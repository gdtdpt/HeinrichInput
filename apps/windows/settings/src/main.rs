//! 亨利输入法设置窗口（M4a：词组页）。
//!
//! ```text
//! heinrich-settings.exe                           # 编辑 %APPDATA%\Heinrich\words.tsv
//! heinrich-settings.exe --words out\t.tsv --data dist\data\wubi86
//! heinrich-settings.exe --selftest                # 无界面自测（数据层）
//! heinrich-settings.exe --selftest-ui             # 窗口层自测（z 序 + 命中测试）
//! ```
//!
//! 它只做一件事：读写 `words.tsv`。生效靠 server 那每秒一次的 mtime 检查，
//! 所以这里**不需要**管道、不需要协议、不需要重启任何东西（D65）。

#![cfg(windows)]

mod lex;
mod model;
mod ui;

use std::path::PathBuf;

struct Args {
    data: Option<PathBuf>,
    words: Option<PathBuf>,
    selftest: bool,
    selftest_ui: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        data: None,
        words: None,
        selftest: false,
        selftest_ui: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data" => args.data = it.next().map(PathBuf::from),
            "--words" => args.words = it.next().map(PathBuf::from),
            "--selftest" => args.selftest = true,
            "--selftest-ui" => args.selftest_ui = true,
            other => {
                if other == "--help" || other == "-h" {
                    println!(
                        "用法：\n  heinrich-settings [--words <文件>] [--data <五笔目录>]\n  heinrich-settings --selftest[|-ui]"
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
        return match ui::selftest_ui(args.data) {
            Ok(()) => {
                println!("设置窗口自测通过（z 序 + 命中 + 面板保存流程）");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("窗口自测失败：{e}");
                std::process::ExitCode::FAILURE
            }
        };
    }

    match ui::App::new(args.data, args.words) {
        Ok(app) => match ui::run(Box::new(app)) {
            Ok(_) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("启动失败：{e}");
                std::process::ExitCode::FAILURE
            }
        },
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
