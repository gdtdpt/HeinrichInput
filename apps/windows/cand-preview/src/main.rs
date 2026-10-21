//! 候选窗预览器：拿一批假候选把候选窗弹出来。
//!
//! 为什么需要它：候选窗只在**别人的输入框**里出现，验收时既没法截图也没法稳定
//! 复现 DPI 与长候选。有了这个 exe，圆角、间距、两端箭头、125%/150% DPI 全部
//! 可以自己开一遍、自己截图看，不用装输入法、不用打字。
//!
//! ```text
//! heinrich-cand-preview.exe                          # 100% DPI，第 1 页
//! heinrich-cand-preview.exe --dpi 1.5 --page 1       # 150% DPI，第 2 页
//! heinrich-cand-preview.exe --selftest               # 自动校验，不打扰人
//! ```
//!
//! 它走的路径与 M2.3 接进 TSF 后**完全一样**（`prepare` → `place` → `show`），
//! 所以它同时是这个 crate 的集成测试。

use std::sync::mpsc::{channel, Receiver};

use heinrich_ui::{place, Rect};
use heinrich_win_ui::{work_area_at, CandidateWindow, Candidates, Event, Hit, Theme};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowRect, PeekMessageW, PostMessageW,
    SendMessageW, SetTimer, TranslateMessage, MA_NOACTIVATE, MSG, PM_REMOVE, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_TIMER,
};

/// 假候选。长度不一，顺便验「成比例压窄」与对齐。
const POOL: [&str; 9] = ["好", "㛅", "崽", "孬", "妳", "耗", "孑", "籽", "㜽"];

fn main() -> std::process::ExitCode {
    let args = Args::parse(std::env::args().skip(1));
    match run(&args) {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(e) => {
            eprintln!("失败：{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// 返回 `Ok(false)` 表示自测没全过。
fn run(args: &Args) -> windows::core::Result<bool> {
    // 预览器是独立进程，DPI 感知归自己管（DLL 里绝不能这么干，见 crate 文档）。
    match heinrich_win_ui::enable_per_monitor_dpi() {
        Ok(()) => eprintln!("DPI 感知：PerMonitorV2"),
        Err(e) => eprintln!("DPI 感知设置失败：{e}"),
    }
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;

    let mut window = CandidateWindow::new(Theme::default())?;

    let texts: Vec<String> = POOL
        .iter()
        .cycle()
        .take(args.count.max(1))
        .map(|s| s.to_string())
        .collect();

    let candidates = Candidates {
        texts: &texts,
        page: args.page,
        page_size: args.page_size,
        highlight: 0,
        // 预览器可以故意造出「两段」的样子来验折叠区的颜色：`--split N`。
        split: args.split,
        anchor: args.at,
    };
    let prepared = match args.scale {
        Some(scale) => window.prepare_scaled(&candidates, scale)?,
        None => window.prepare(&candidates)?,
    };

    // 把 `--at` 当光标位置：窗口贴在它下面。越界时 place() 会翻到上方或夹进工作区。
    let caret = Rect::new(args.at.0 as f32, args.at.1 as f32, 2.0, 20.0);
    let placement = place(caret, prepared.layout.size, work_area_at(args.at), 2.0);

    let (tx, rx) = channel();
    window.set_on_event(move |e| {
        let _ = tx.send(e);
    });

    let foreground_before = unsafe { GetForegroundWindow() };
    window.show(
        &prepared,
        (placement.rect.x as i32, placement.rect.y as i32),
    )?;

    println!(
        "窗口 {}×{} @ ({}, {})　DPI 缩放 {:.2}　候选 {} 条 / 第 {} 页　箭头 左={} 右={}{}",
        prepared.layout.size.w,
        prepared.layout.size.h,
        placement.rect.x as i32,
        placement.rect.y as i32,
        prepared.dpi_scale,
        prepared.layout.items.len(),
        args.page + 1,
        prepared.layout.prev.is_some(),
        prepared.layout.next.is_some(),
        if placement.flipped {
            "　（翻到光标上方）"
        } else {
            ""
        },
    );
    for item in &prepared.layout.items {
        println!(
            "  #{} 「{}」 rect=({:.0}, {:.0}, {:.0}×{:.0})",
            item.index + 1,
            item.text,
            item.rect.x,
            item.rect.y,
            item.rect.w,
            item.rect.h
        );
    }

    pump();

    if args.selftest {
        let ok = selftest(&window, &prepared, foreground_before, &rx);
        window.hide();
        return Ok(ok);
    }

    if let Some(ms) = args.hold {
        // 定时器消息由本循环自己截，不进窗口过程：候选窗不吃键盘，也没有
        // 「按任意键退出」这种入口，只能靠它自己走。
        unsafe { SetTimer(Some(window.hwnd()), 1, ms, None) };
    }

    message_loop(window.hwnd());
    window.hide();
    Ok(true)
}

/// 自动校验：不激活、命中、箭头。
fn selftest(
    window: &CandidateWindow,
    prepared: &heinrich_win_ui::Prepared,
    foreground_before: HWND,
    rx: &Receiver<Event>,
) -> bool {
    let mut all_ok = true;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("{} {}　{}", if ok { "✅" } else { "❌" }, name, detail);
        all_ok &= ok;
    };

    // ① 窗口实际尺寸必须等于布局尺寸。
    //
    // 这条看着多余，其实是最容易静默出错的一条：`UpdateLayeredWindow` 的尺寸与
    // `layout()` 算出来的哪怕差一个像素，内容就会被系统拉伸/裁掉，而屏幕上只会
    // 显得「字有点胖」，谁也说不清是哪一步错了。
    let mut rect = RECT::default();
    let got_rect = unsafe { GetWindowRect(window.hwnd(), &mut rect) }.is_ok();
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    let expect_w = prepared.layout.size.w.ceil() as i32;
    let expect_h = prepared.layout.size.h.ceil() as i32;
    check(
        "窗口尺寸 = 布局尺寸",
        got_rect && w == expect_w && h == expect_h,
        format!("实际 {w}×{h}，布局 {expect_w}×{expect_h}"),
    );

    // ② 显示之后前台窗口没变
    let after = unsafe { GetForegroundWindow() };
    check(
        "显示不抢前台",
        after == foreground_before,
        format!("之前 {foreground_before:?} / 之后 {after:?}"),
    );

    // ③ 鼠标点上来也拒绝激活
    let ret = unsafe {
        SendMessageW(
            window.hwnd(),
            WM_MOUSEACTIVATE,
            Some(WPARAM(0)),
            Some(LPARAM(0)),
        )
    };
    check(
        "WM_MOUSEACTIVATE → MA_NOACTIVATE",
        ret.0 == MA_NOACTIVATE as isize,
        format!("返回 {}（期望 {}）", ret.0, MA_NOACTIVATE),
    );

    // ④ 每个候选的矩形中心都能点中，且回的是**全局**序号
    for item in &prepared.layout.items {
        let (x, y) = (
            (item.rect.x + item.rect.w / 2.0) as i32,
            (item.rect.y + item.rect.h / 2.0) as i32,
        );
        while rx.try_recv().is_ok() {}
        post_click(window.hwnd(), x, y);
        pump();
        let got = rx.try_recv().ok();
        check(
            &format!("命中候选 #{}「{}」", item.index + 1, item.text),
            got == Some(Event::Click(Hit::Candidate(item.index))),
            format!("点 ({x}, {y}) 得到 {got:?}"),
        );
    }

    // ⑤ 箭头命中，且与候选分开
    for arrow in [prepared.layout.prev.as_ref(), prepared.layout.next.as_ref()]
        .into_iter()
        .flatten()
    {
        let (x, y) = (
            (arrow.rect.x + arrow.rect.w / 2.0) as i32,
            (arrow.rect.y + arrow.rect.h / 2.0) as i32,
        );
        while rx.try_recv().is_ok() {}
        post_click(window.hwnd(), x, y);
        pump();
        let got = rx.try_recv().ok();
        check(
            &format!("命中箭头 {:?}", arrow.dir),
            got == Some(Event::Click(Hit::Arrow(arrow.dir))),
            format!("点 ({x}, {y}) 得到 {got:?}"),
        );
    }

    // ⑥ 窗口内空白处（内边距上）不该命中任何东西
    while rx.try_recv().is_ok() {}
    post_click(window.hwnd(), 1, 1);
    pump();
    let got = rx.try_recv().ok();
    check(
        "内边距不命中",
        got.is_none(),
        format!("点 (1, 1) 得到 {got:?}"),
    );

    println!(
        "\n{}",
        if all_ok {
            "自测通过"
        } else {
            "自测未通过（见上面的 ❌）"
        }
    );
    all_ok
}

fn post_click(hwnd: HWND, x: i32, y: i32) {
    let packed = ((y as isize & 0xFFFF) << 16) | (x as isize & 0xFFFF);
    unsafe { PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(packed)) }.ok();
}

fn pump() {
    let mut msg = MSG::default();
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn message_loop(hwnd: HWND) {
    let mut msg = MSG::default();
    loop {
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        if msg.message == WM_TIMER && msg.hwnd == hwnd {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

struct Args {
    count: usize,
    page: usize,
    page_size: usize,
    /// `None` = 按真实显示器 DPI。
    scale: Option<f32>,
    at: (i32, i32),
    hold: Option<u32>,
    /// 折叠区第一条的全局序号（D48）。默认 `usize::MAX` = 只有一段；
    /// `--split N` 让后半截画淡一档，用来目验混输的两段分界。
    split: usize,
    selftest: bool,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Self {
        let mut out = Self {
            count: 8,
            page: 0,
            page_size: 5,
            scale: None,
            at: (200, 260),
            hold: None,
            split: usize::MAX,
            selftest: false,
        };
        while let Some(arg) = args.next() {
            let mut value = |name: &str| -> String {
                args.next().unwrap_or_else(|| {
                    eprintln!("{name} 后面缺一个值");
                    std::process::exit(2)
                })
            };
            match arg.as_str() {
                "--count" => out.count = value("--count").parse().unwrap_or(8),
                "--page" => out.page = value("--page").parse().unwrap_or(0),
                "--page-size" => out.page_size = value("--page-size").parse().unwrap_or(5),
                "--dpi" => out.scale = value("--dpi").parse().ok(),
                "--at" => {
                    let v = value("--at");
                    let mut it = v.split(',');
                    out.at = (
                        it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(200),
                        it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(260),
                    );
                }
                "--hold" => out.hold = value("--hold").parse().ok(),
                "--split" => {
                    out.split = value("--split").parse().unwrap_or(usize::MAX);
                }
                "--selftest" => out.selftest = true,
                "-h" | "--help" => {
                    println!(
                        "用法：heinrich-cand-preview [--count N] [--page P] [--page-size S]\n\
                         \x20                      [--dpi SCALE] [--at X,Y] [--hold MS]
\
                         \x20                      [--split N] [--selftest]"
                    );
                    std::process::exit(0);
                }
                other => {
                    eprintln!("不认识的参数：{other}（--help 看用法）");
                    std::process::exit(2);
                }
            }
        }
        out
    }
}
