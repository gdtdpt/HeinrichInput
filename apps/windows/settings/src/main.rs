//! 亨利输入法设置窗口（M4b 四页：通用 / 词组 / emoji / 学习数据）。
//!
//! ```text
//! heinrich-settings.exe                           # 打开在通用页
//! heinrich-settings.exe --words t.tsv --config c.toml --db learn.db --page 1
//! heinrich-settings.exe --selftest                # 无界面自测（数据层）
//! heinrich-settings.exe --selftest-ui             # 窗口层自测（四页 z 序 / 命中 / 写盘）
//! ```
//!
//! 它碰四个文件：`words.tsv`（读写）、`config.toml`（定向改写）、
//! `emoji.user.toml`（整份重写，内置表只读）、`learn.db`（只读 + 清零某词的学习分）。
//! 都不经过管道、不需要协议：server 每秒 stat 一次前三个、每次查询直接读第四个
//! （D65），所以改完不用重启任何东西。`--page` 是给截图验收用的
//! （`PrintWindow` 只能截当前可见的那一页）。

#![cfg(windows)]
// `--selftest` 要把结果打到 stdout，所以 debug 构建保留控制台；release 打成
// Windows 子系统——否则从输入法（或开始菜单）拉起来时，背后会多开一个黑框
// 命令行窗口，看起来像程序没退干净（真机反馈就是这个）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod emoji_page;
mod emoji_picker;
mod general;
mod learn;
mod lex;
mod model;
mod theme;
mod ui;

use std::path::PathBuf;

struct Args {
    data: Option<PathBuf>,
    words: Option<PathBuf>,
    config: Option<PathBuf>,
    db: Option<PathBuf>,
    /// emoji 用户层（默认 %APPDATA%\Heinrich\emoji.user.toml）。自测用。
    emoji: Option<PathBuf>,
    /// 启动时停在哪一页（0 通用 / 1 词组 / 2 emoji / 3 学习数据）。截图验收要能直接打开某一页。
    page: usize,
    /// 起来后自动弹一次 emoji 选择器（截图验收用）
    picker: bool,
    selftest: bool,
    selftest_ui: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        data: None,
        words: None,
        config: None,
        db: None,
        emoji: None,
        page: 0,
        picker: false,
        selftest: false,
        selftest_ui: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => args.data = it.next().map(PathBuf::from),
            "--words" => args.words = it.next().map(PathBuf::from),
            "--config" => args.config = it.next().map(PathBuf::from),
            "--db" => args.db = it.next().map(PathBuf::from),
            "--emoji" => args.emoji = it.next().map(PathBuf::from),
            "--page" => args.page = it.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0),
            "--picker" => args.picker = true,
            "--selftest" => args.selftest = true,
            "--selftest-ui" => args.selftest_ui = true,
            other => {
                if other == "--help" || other == "-h" {
                    println!(
                        "用法：\n  heinrich-settings [--words <文件>] [--data <五笔目录>]\n                   [--config <config.toml>] [--db <learn.db>] [--emoji <emoji.user.toml>]\n                   [--page 0|1|2|3] [--picker]\n  heinrich-settings --selftest[|-ui]"
                    );
                    std::process::exit(0);
                }
                eprintln!("忽略不认识的参数：{other}");
            }
        }
    }
    args
}

fn main() -> std::process::ExitCode {
    let args = parse_args();
    if args.selftest {
        return match selftest(args.data) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("自测失败：{e}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    // 必须在建任何窗口之前：不做声明的话进程是 DPI 不感知的，Windows 会把整张窗口
    // 按 96 DPI 渲染再位图放大，在 125%/200% 缩放下字全是糊的（截图验收时一眼可见）。
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }

    if args.selftest_ui {
        // 自测一律在临时目录里跑，绝不碰 %APPDATA% / %LOCALAPPDATA%——
        // 拿用户的真配置和真学习库来自测，出错就是改别人的数据。
        return match ui::selftest_ui(args.data) {
            Ok(()) => {
                println!("设置窗口自测通过（四页 z 序/命中 + 通用页写盘与恢复默认 + 学习页列表/搜索 + 词组页面板 + emoji 页改绑/停用/恢复）");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("窗口自测失败：{e}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    // 已经开着就把它叫到前台，自己退出。两个入口（开始菜单、系统设置的「选项」）
    // 都启动这个 exe，而它没有单实例锁——两个窗口各持一份内存里的词表，后保存的
    // 那份会把先保存的改动当成「已删除」。放在 `--selftest-ui` 之后：自测要自己建窗。
    if let Some(pid) = ui::focus_existing() {
        println!("已有设置窗口（pid={pid}），已切到前台");
        return std::process::ExitCode::SUCCESS;
    }

    match ui::App::new(args.data, args.words, args.config, args.db, args.emoji) {
        Ok(mut app) => {
            app.set_initial_page(args.page);
            app.set_open_picker(args.picker);
            match ui::run(Box::new(app)) {
                Ok(_) => std::process::ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("启动失败：{e}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("启动失败：{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// 无界面自测：模型层的读写、推导、原子落盘。
///
/// 窗口本身靠 `win-ui-selfscreenshot` 那套截图验收（`PrintWindow`），不在这里。
fn selftest(data: Option<PathBuf>) -> Result<(), String> {
    let dir = std::env::temp_dir().join("heinrich-settings-selftest");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建临时目录失败：{e}"))?;
    let path = dir.join("words.tsv");

    // 1. 空文件 → 0 条，不报错
    let mut m = model::Model::load(path.clone()).map_err(|e| format!("读空文件失败：{e}"))?;
    assert!(m.words.is_empty(), "空文件应读出 0 条");

    // 2. 手填一条、留空一条
    m.upsert(heinrich_scheme::user::UserWord {
        word: "好吃".into(),
        pinyin: None,
        code: None,
        weight: None,
    });
    m.upsert(heinrich_scheme::user::UserWord {
        word: "微信".into(),
        pinyin: Some("weixin".into()),
        code: Some("tmwy".into()),
        weight: Some(1_700_000.0),
    });
    m.save().map_err(|e| format!("保存失败：{e}"))?;

    // 3. 落盘是「临时文件 + 改名」：不该留下 .tmp
    let leftover = path.with_extension("tsv.tmp");
    assert!(!leftover.exists(), "保存后不该留下 {}", leftover.display());

    // 4. 读回来列数与内容都对
    let back = model::Model::load(path.clone()).map_err(|e| format!("重读失败：{e}"))?;
    assert_eq!(back.words.len(), 2, "应有 2 条");
    assert_eq!(back.words[1].word, "微信");
    assert_eq!(back.words[1].code.as_deref(), Some("tmwy"));

    // 5. 有词库就验推导（拼音反查 + 码按规则推）
    let lex = data
        .as_ref()
        .and_then(|d| lex::find_data_dir(Some(d.clone())))
        .and_then(|d| lex::Lexicons::load(&d).ok());
    match lex {
        Some(lex) => {
            let rows = back.rows(Some(&lex));
            let haochi = rows
                .iter()
                .find(|r| r.raw.word == "好吃")
                .ok_or("找不到「好吃」")?;
            assert_eq!(
                haochi.pinyin.as_deref(),
                Some("haochi"),
                "拼音应反查到 haochi"
            );
            assert_eq!(haochi.code.as_deref(), Some("vbkt"), "码应按规则推成 vbkt");
            assert!(haochi.weight > 0.0, "权重应取到中位数");
            assert!(haochi.pinyin_cell().ends_with('*'), "推导的拼音应带 *");
            let wx = rows.iter().find(|r| r.raw.word == "微信").unwrap();
            assert_eq!(wx.pinyin_cell(), "weixin", "手填的拼音不带 *");
        }
        None => println!("（没给 --data 或词库装不上，跳过推导断言）"),
    }

    // 6. 删除 + 恢复
    let (at, w) = m.remove("微信").ok_or("删除失败")?;
    assert_eq!(at, 1);
    m.insert_at(at, w);
    assert_eq!(m.words.len(), 2, "恢复后应回到 2 条");

    // 7. 坏行不影响其它词（空词的行走警告，其余照读）
    std::fs::write(&path, "好\tvbg\t\t\n\tvbg\t\t\n好吃\t\tvbkt\n").unwrap();
    let messy = model::Model::load(path.clone()).map_err(|e| e.to_string())?;
    assert_eq!(messy.words.len(), 2, "坏行应被跳过");
    assert!(!messy.warnings.is_empty(), "坏行应有警告");

    let _ = std::fs::remove_dir_all(&dir);
    println!("设置窗口自测通过（7 项）");
    Ok(())
}
