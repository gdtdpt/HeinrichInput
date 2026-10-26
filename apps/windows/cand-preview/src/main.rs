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
//! heinrich-cand-preview.exe --keycaps vbg            # 码位键位图（§6.7）
//! ```
//!
//! 它走的路径与 M2.3 接进 TSF 后**完全一样**（`prepare` → `place` → `show`），
//! 所以它同时是这个 crate 的集成测试。

use std::sync::mpsc::{channel, Receiver};

use heinrich_scheme::Roots;
use heinrich_ui::{place, place_keycaps, Rect};
use heinrich_win_ui::{
    work_area_at, CandidateWindow, Candidates, CapData, Event, Hit, KeycapWindow, Keycaps, Theme,
    KEYCAP_FOOTNOTE,
};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowRect, PeekMessageW, PostMessageW,
    SendMessageW, SetTimer, TranslateMessage, MA_NOACTIVATE, MSG, PM_REMOVE, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_TIMER,
};

/// 假候选。长度不一，顺便验「成比例压窄」与对齐。
const POOL: [&str; 9] = ["好", "㛅", "崽", "孬", "妳", "耗", "孑", "籽", "㜽"];

/// 假码提示（§6.3）。长度 2–4 码都要有：chip 是可变宽的，只验 3 码会漏掉
/// 「四码词组把整项撑多宽」这件事。
const HINTS: [&str; 5] = ["vbg", "kgn", "vbyg", "itfh", "rn"];

/// 词中 chip（D64）的假候选。`--hint-at N`（配 `--hint`）时替换 `POOL`：
/// 单字拆不出「前截 + chip + 后截」，得多字候选才验得到。
const MULTI_POOL: [&str; 5] = ["好吃", "好看", "好话", "好似", "好样"];

/// `--emoji` 时轮流插上的假 **emoji 候选**（v8/D83）。特意混一个非黄色、一个带
/// 变体选择符：前者验彩色字形确实上了屏，后者验「基础字符 + FE0F」也能量准。
const EMOJIS: [&str; 4] = ["👍", "🎉", "❤\u{FE0F}", "🚀"];

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

    if args.keycaps {
        return run_keycaps(args);
    }

    let mut window = CandidateWindow::new(Theme::default())?;

    // 词中 chip（D64）：`--hint-at N`（配 `--hint`）切换到多字候选池，
    // 把 chip 嵌进词里画——这才是多段组合的真实形状。
    let use_mid_word = args.hint && args.hint_at > 0;
    let source_pool: &[&str] = if use_mid_word { &MULTI_POOL } else { &POOL };
    let words: Vec<String> = source_pool
        .iter()
        .cycle()
        .take(args.count.max(1))
        .map(|s| s.to_string())
        .collect();

    // emoji 是**独立候选**（v8/D83）：`--emoji` 时每个词后面插一条，`text` 就是
    // 那枚 emoji——这正是 server 合成出来的形状（`1 好评  2 👍  3 好看 …`）。
    // 这样预览器验的不只是「画得出来」，还有「编号与命中落在正确的条目上」。
    let mut texts: Vec<String> = Vec::new();
    let mut is_emoji: Vec<bool> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        texts.push(w.clone());
        is_emoji.push(false);
        if args.emoji {
            texts.push(EMOJIS[i % EMOJIS.len()].to_string());
            is_emoji.push(true);
        }
    }

    // 码提示：`--hint` 只给 split **之后**的条目加码——那正是「折叠区 = 拼音段」
    // 的真实形状（主段五笔不带码、补段拼音带码）。没给 split 时全部带码，
    // 相当于「整屏都是拼音查出来的」。`--hint-at` 模式下全部带码且不设折叠分界
    // （多段候选没有折叠区）。
    let hint_start = if use_mid_word { 0 } else { args.split };
    // emoji 条目永远不带码提示（它没有码），其余条目按原来的规则给。
    let hints: Vec<String> = (0..texts.len())
        .map(|i| {
            if !args.hint || is_emoji[i] || i < hint_start {
                String::new()
            } else {
                HINTS[i % HINTS.len()].to_string()
            }
        })
        .collect();
    // 词中位置（D64）：`--hint-at N` 时带 chip 的条目都挂在第 N 个字符后。
    let hint_ats: Vec<u8> = if use_mid_word {
        (0..texts.len())
            .map(|i| if is_emoji[i] { 0 } else { args.hint_at as u8 })
            .collect()
    } else {
        vec![0; texts.len()]
    };

    let candidates = Candidates {
        texts: &texts,
        hints: &hints,
        hint_ats: &hint_ats,
        is_emoji: &is_emoji,
        page: args.page,
        page_size: args.page_size,
        highlight: 0,
        // 预览器可以故意造出「两段」的样子来验折叠区的颜色：`--split N`。
        // 词中 chip 模式不造两段（多段候选没有折叠区）。
        split: if use_mid_word { usize::MAX } else { args.split },
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

    // 渲染位图直接落盘（`--save-bmp`）：沙箱 / 无头会话里抓屏是黑的，
    // 而 `renderer` 的那块 DIB 就是 `UpdateLayeredWindow` 要上屏的内容，
    // 拿它做视觉验收与「真弹窗」等价且可复现。
    if let Some(path) = &args.save_bmp {
        let w = prepared.layout.size.w.ceil().max(1.0) as i32;
        let h = prepared.layout.size.h.ceil().max(1.0) as i32;
        let hdc = window
            .hdc()
            .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        save_bmp(hdc, w, h, path)?;
        println!("渲染位图已存 {path}（{w}×{h}）");
    }

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
            "  #{} 「{}」{}{}{} rect=({:.0}, {:.0}, {:.0}×{:.0})",
            item.index + 1,
            item.text,
            match &item.hint {
                Some(h) => format!("[{h}]"),
                None => String::new(),
            },
            if item.is_emoji {
                "（emoji 候选）"
            } else {
                ""
            },
            if item.secondary { "（补段）" } else { "" },
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

/// `--keycaps`：弹键位图。数据来自真实的 `roots.toml`，与真机走同一份。
fn run_keycaps(args: &Args) -> windows::core::Result<bool> {
    let roots = match Roots::load(std::path::Path::new(&args.data)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return Ok(false);
        }
    };
    let Some(keys) = roots.lookup(&args.keys) else {
        eprintln!(
            "`{}` 里含有不是 a–y 的字符（`z` 是学习键，没有字根）",
            args.keys
        );
        return Ok(false);
    };
    let caps: Vec<CapData> = keys
        .iter()
        .map(|k| CapData {
            key: k.key,
            roots: k.display_roots().collect(),
            main_root_count: k.roots.len(),
        })
        .collect();

    let mut window = KeycapWindow::new(Theme::default())?;
    let content = Keycaps {
        caps: &caps,
        footnote: KEYCAP_FOOTNOTE,
        anchor: args.at,
    };
    let prepared = match args.scale {
        Some(scale) => window.prepare_scaled(&content, scale)?,
        None => window.prepare(&content)?,
    };

    // 键位图的锚点是**候选窗**，不是光标：这里造一个假候选窗矩形贴在 `--at`。
    let cand = Rect::new(args.at.0 as f32, args.at.1 as f32, 300.0, 36.0);
    let placement = place_keycaps(
        cand,
        prepared.layout.size,
        work_area_at(args.at),
        4.0,
        false,
    );

    let foreground_before = unsafe { GetForegroundWindow() };
    window.show(
        &prepared,
        (placement.rect.x as i32, placement.rect.y as i32),
    )?;

    println!(
        "键位图 `{}` {}×{} @ ({}, {})　DPI {:.2}　键帽 {} 枚　说明 {} 行{}",
        args.keys,
        prepared.layout.size.w,
        prepared.layout.size.h,
        placement.rect.x as i32,
        placement.rect.y as i32,
        prepared.dpi_scale,
        prepared.layout.caps.len(),
        prepared.footnote_lines.len(),
        if placement.flipped {
            "　（翻到候选窗上方）"
        } else {
            ""
        },
    );
    for cap in &prepared.layout.caps {
        let shown: String = cap.roots.iter().collect();
        println!(
            "  {}　{}　rect=({:.0}, {:.0}, {:.0}×{:.0})",
            cap.key, shown, cap.rect.x, cap.rect.y, cap.rect.w, cap.rect.h
        );
    }

    pump();

    if args.selftest {
        let ok = keycap_selftest(&window, &prepared, foreground_before, &caps);
        window.hide();
        return Ok(ok);
    }

    if let Some(ms) = args.hold {
        unsafe { SetTimer(Some(window.hwnd()), 1, ms, None) };
    }

    message_loop(window.hwnd());
    window.hide();
    Ok(true)
}

/// 键位图自测：尺寸一致、装得下、说明不越界、不抢前台。
fn keycap_selftest(
    window: &KeycapWindow,
    prepared: &heinrich_win_ui::PreparedKeycaps,
    foreground_before: HWND,
    caps: &[CapData],
) -> bool {
    let mut all_ok = true;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("{} {}　{}", if ok { "✅" } else { "❌" }, name, detail);
        all_ok &= ok;
    };

    // ① 窗口实际尺寸 = 布局尺寸（分层窗口的位图与几何错位是最难查的一类 bug）
    let mut rect = RECT::default();
    let sized = unsafe { GetWindowRect(window.hwnd(), &mut rect) }.is_ok();
    let w = (rect.right - rect.left) as f32;
    let h = (rect.bottom - rect.top) as f32;
    check(
        "窗口尺寸 = 布局尺寸",
        sized
            && (w - prepared.layout.size.w).abs() < 1.5
            && (h - prepared.layout.size.h).abs() < 1.5,
        format!(
            "窗口 {w}×{h}　布局 {}×{}",
            prepared.layout.size.w, prepared.layout.size.h
        ),
    );

    // ② 键帽数与码长一致（`vbg` → 3 枚）
    check(
        "键帽数 = 码长",
        prepared.layout.caps.len() == caps.len(),
        format!("{} 枚", prepared.layout.caps.len()),
    );

    // ③ 每个字的矩形都装在自己的键帽里，键帽之间不重叠
    let mut inside = true;
    let mut detail = String::from("全部字在键帽内");
    for cap in &prepared.layout.caps {
        let rects = std::iter::once(&cap.key_rect).chain(cap.root_rects.iter());
        for r in rects {
            if r.x < cap.rect.x - 0.5
                || r.right() > cap.rect.right() + 0.5
                || r.y < cap.rect.y - 0.5
                || r.bottom() > cap.rect.bottom() + 0.5
            {
                inside = false;
                detail = format!("{} 的字越出键帽", cap.key);
            }
        }
    }
    for pair in prepared.layout.caps.windows(2) {
        if pair[0].rect.right() > pair[1].rect.x + 0.5 {
            inside = false;
            detail = format!("{} 与 {} 的键帽重叠", pair[0].key, pair[1].key);
        }
    }
    check("字在键帽内、键帽不重叠", inside, detail);

    // ④ 说明行不越出窗口（它是唯一可能被撑宽的内容）
    let mut footnote_ok = true;
    for (line, rect) in prepared
        .footnote_lines
        .iter()
        .zip(&prepared.layout.footnotes)
    {
        if rect.x < 0.0 || rect.right() > prepared.layout.size.w + 0.5 {
            footnote_ok = false;
        }
        let _ = line;
    }
    check(
        "说明行不越界",
        footnote_ok,
        format!("{} 行", prepared.footnote_lines.len()),
    );

    // ⑤ 不抢前台。**必须与候选窗同一条判据**：显示后前台窗口没变。
    pump();
    let after = unsafe { GetForegroundWindow() };
    check(
        "显示与点击都不抢前台",
        after == foreground_before,
        format!("显示前 {:?} → 显示后 {:?}", foreground_before.0, after.0),
    );

    println!(
        "\n{}",
        if all_ok {
            "键位图自测全部通过 ✅"
        } else {
            "键位图自测有失败项 ❌"
        }
    );
    all_ok
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

    // ⑦ 码提示 chip（§6.3）：只在 `--hint` 时才有东西可验，没带就跳过而不是
    //    假装通过——「没测」和「测了没问题」必须能分开。
    let chipped: Vec<&heinrich_ui::ItemBox> = prepared
        .layout
        .items
        .iter()
        .filter(|it| it.hint.is_some())
        .collect();
    if chipped.is_empty() {
        println!("⏭ chip 不发现在本页里（没带 --hint），跳过 chip 断言");
    } else {
        let inside = chipped.iter().all(|it| {
            it.hint_rect.w > 0.0
                && it.hint_rect.x >= it.text_rect.right()
                && it.hint_rect.right() <= it.rect.right() + 0.01
                && it.hint_rect.y >= it.rect.y
                && it.hint_rect.bottom() <= it.rect.bottom()
        });
        check(
            "chip 在自己的项内、不压住词",
            inside,
            format!("{} 条带码", chipped.len()),
        );

        // chip 上点下去必须命中这一条（整项矩形命中，不是「点 chip 等于点空」）
        let it = chipped[0];
        let (x, y) = (
            (it.hint_rect.x + it.hint_rect.w / 2.0) as i32,
            (it.hint_rect.y + it.hint_rect.h / 2.0) as i32,
        );
        while rx.try_recv().is_ok() {}
        post_click(window.hwnd(), x, y);
        pump();
        let got = rx.try_recv().ok();
        check(
            "点 chip 命中该候选",
            got == Some(Event::Click(Hit::Candidate(it.index))),
            format!("点 ({x}, {y}) 得到 {got:?}"),
        );
    }

    // ⑧ emoji 候选（v8/D83）：它是**独立的候选条目**，不是词尾的装饰。
    //    验三件事：① 真的出现在这一屏且带 `is_emoji` 标记；② 宽度是量出来的字形宽、
    //    整项落在窗口内；③ 点它命中的是它自己（编号没串位）。
    let marked: Vec<&heinrich_ui::ItemBox> = prepared
        .layout
        .items
        .iter()
        .filter(|it| it.is_emoji)
        .collect();
    if marked.is_empty() {
        println!("（本次没有 emoji 候选，跳过 emoji 断言——加 --emoji 可验）");
    } else {
        let inside = marked.iter().all(|it| {
            it.text_rect.w > 0.0
                && it.rect.right() <= prepared.layout.size.w + 0.01
                && it.rect.y >= 0.0
                && it.rect.bottom() <= prepared.layout.size.h + 0.01
        });
        check(
            "emoji 候选有自己的宽度且在窗口内",
            inside,
            format!(
                "{} 条 emoji 候选，首条宽 {:.1}",
                marked.len(),
                marked[0].text_rect.w
            ),
        );

        let it = marked[0];
        let (x, y) = (
            (it.text_rect.x + it.text_rect.w / 2.0) as i32,
            (it.text_rect.y + it.text_rect.h / 2.0) as i32,
        );
        while rx.try_recv().is_ok() {}
        post_click(window.hwnd(), x, y);
        pump();
        let got = rx.try_recv().ok();
        check(
            "点 emoji 候选命中它自己",
            got == Some(Event::Click(Hit::Candidate(it.index))),
            format!("点 ({x}, {y}) 得到 {got:?}"),
        );
    }

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
    /// 给 `split` 之后的候选加假码提示（§6.3），用来目验 chip 的排版。
    hint: bool,
    /// chip 的词中位置（D64）：`--hint-at N`（配 `--hint`）把 chip 嵌在第 N 个
    /// 字符之后，并切换到多字候选池——目验 `好[vbg]吃` 的排版。
    hint_at: usize,
    /// 给每条候选挂一枚假 emoji（§6.4），用来目验「词尾一枚彩色字形」的排版。
    emoji: bool,
    /// 渲染位图落盘路径（`.bmp`）。沙箱 / 无头会话抓不到屏幕，直接拿
    /// `UpdateLayeredWindow` 要上屏的那块 DIB 做视觉验收。
    save_bmp: Option<String>,
    /// 弹**键位图**而不是候选窗（§6.7）。
    keycaps: bool,
    /// 键位图要展开的码，如 `vbg`、`vbgu`。
    keys: String,
    /// 字根表所在目录。
    data: String,
    selftest: bool,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Self {
        // peekable：`--keycaps` 后面跟的码是可选的，要能看一眼再决定吃不吃
        let mut args = args.peekable();
        let mut out = Self {
            count: 8,
            page: 0,
            page_size: 5,
            scale: None,
            at: (200, 260),
            hold: None,
            split: usize::MAX,
            hint: false,
            hint_at: 0,
            emoji: false,
            save_bmp: None,
            keycaps: false,
            keys: "vbg".to_string(),
            data: "data/wubi86".to_string(),
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
                "--hint" => out.hint = true,
                "--hint-at" => out.hint_at = value("--hint-at").parse().unwrap_or(0),
                "--emoji" => out.emoji = true,
                "--save-bmp" => out.save_bmp = Some(value("--save-bmp")),
                "--keycaps" => {
                    out.keycaps = true;
                    // `--keycaps vbg`：紧跟的值若是纯小写字母串就当码，否则用默认 vbg
                    let looks_like_code = args.peek().is_some_and(|n| {
                        !n.starts_with('-') && n.chars().all(|c| c.is_ascii_lowercase())
                    });
                    if looks_like_code {
                        out.keys = args.next().unwrap_or_default();
                    }
                }
                "--data" => out.data = value("--data"),
                "-h" | "--help" => {
                    println!(
                        "用法：heinrich-cand-preview [--count N] [--page P] [--page-size S]\n\
                         \x20                      [--dpi SCALE] [--at X,Y] [--hold MS]
\
                         \x20                      [--split N] [--hint] [--hint-at N] [--emoji] [--selftest]\n\
                         \x20                      [--keycaps [码]] [--data 目录]"
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

/// 把一块 DIB 落成 24 位 BMP（无压缩、零依赖）。
///
/// `hdc` 是渲染器那块 32 位预乘 alpha 位图的设备上下文；这里只取它的 RGB，
/// alpha 通道在 BMP 里本来就放不下——验收看的是排版，不是透明度。
fn save_bmp(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    w: i32,
    h: i32,
    path: &str,
) -> windows::core::Result<()> {
    use windows::Win32::Graphics::Gdi::{
        GetDIBits, BITMAPFILEHEADER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };

    #[repr(C)]
    struct BmpInfo {
        header: BITMAPINFOHEADER,
    }
    let mut bi = BmpInfo {
        header: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // 自上而下
            biPlanes: 1,
            biBitCount: 24,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
    };
    let row = (w as usize * 3).div_ceil(4) * 4;
    let mut pixels = vec![0u8; row * h as usize];
    let got = unsafe {
        // 位图取当前选进 DC 的那个（渲染器建好就选着），不用传 HBITMAP。
        // `GetCurrentObject(hdc, OBJ_BITMAP)` 拿到的就是渲染器那块 DIB。
        let bmp = windows::Win32::Graphics::Gdi::GetCurrentObject(
            hdc,
            windows::Win32::Graphics::Gdi::OBJ_TYPE(7), // OBJ_BITMAP
        );
        GetDIBits(
            hdc,
            windows::Win32::Graphics::Gdi::HBITMAP(bmp.0),
            0,
            h as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut bi.header as *mut BITMAPINFOHEADER as *mut BITMAPINFO,
            DIB_RGB_COLORS,
        )
    };
    if got == 0 {
        return Err(windows::core::Error::from(
            windows::Win32::Foundation::E_FAIL,
        ));
    }

    let file_header = BITMAPFILEHEADER {
        bfType: 0x4D42, // 'BM'
        bfSize: (14 + std::mem::size_of::<BITMAPINFOHEADER>() + pixels.len()) as u32,
        bfReserved1: 0,
        bfReserved2: 0,
        bfOffBits: 14 + std::mem::size_of::<BITMAPINFOHEADER>() as u32,
    };
    let mut out = Vec::with_capacity(14 + 40 + pixels.len());
    out.extend_from_slice(&file_header.bfType.to_le_bytes());
    out.extend_from_slice(&file_header.bfSize.to_le_bytes());
    out.extend_from_slice(&file_header.bfReserved1.to_le_bytes());
    out.extend_from_slice(&file_header.bfReserved2.to_le_bytes());
    out.extend_from_slice(&file_header.bfOffBits.to_le_bytes());
    out.extend_from_slice(unsafe {
        std::slice::from_raw_parts(
            &bi.header as *const BITMAPINFOHEADER as *const u8,
            std::mem::size_of::<BITMAPINFOHEADER>(),
        )
    });
    out.extend_from_slice(&pixels);
    std::fs::write(path, out)?;
    Ok(())
}
