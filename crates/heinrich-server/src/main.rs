//! 亨利输入法 · 常驻引擎宿主。
//!
//! ## 它解决什么问题
//!
//! `lexicon.tsv` 有 13.6 万条、2.5 MB，装载约 300 ms。TSF DLL 会被注入到**每一个**
//! 需要中文输入的进程（Chrome 的每个 tab、explorer、每个编辑器），在那里装载
//! 等于每个进程都卡 300 ms、每个进程都多背一份索引。
//!
//! 所以词库、排序、未来的候选窗和 SQLite 学习库都放这里，DLL 只做按键与协议搬运（D1）。
//!
//! ## 用法
//!
//! ```text
//! heinrich-server.exe                      # 常驻（正常形态，由 DLL 惰性拉起）
//! heinrich-server.exe --debug-sleep 5000   # 每次查询前睡 5 秒，用来验 D18 零阻塞
//! heinrich-server.exe --idle-exit 0        # 不自动退出，调完手工结束
//! heinrich-server.exe --selftest           # 装载词库 + 跑几条查询后退出（无管道）
//! ```
//!
//! 日志：`%LOCALAPPDATA%\Heinrich\logs\server.log`。

#[allow(unused_macros)]
#[macro_use]
mod log;

mod host;

#[cfg(windows)]
mod pipe;

use std::path::PathBuf;
use std::time::Duration;

use host::Host;

struct Args {
    scheme_dir: Option<PathBuf>,
    debug_sleep: Duration,
    idle_exit: Duration,
    selftest: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Args {
            scheme_dir: None,
            debug_sleep: Duration::ZERO,
            idle_exit: Duration::from_secs(30 * 60),
            selftest: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--scheme-dir" => {
                    args.scheme_dir =
                        Some(PathBuf::from(it.next().ok_or("--scheme-dir 需要一个路径")?));
                }
                "--debug-sleep" => {
                    let ms: u64 = it
                        .next()
                        .ok_or("--debug-sleep 需要毫秒数")?
                        .parse()
                        .map_err(|_| "--debug-sleep 需要一个整数（毫秒）")?;
                    args.debug_sleep = Duration::from_millis(ms);
                }
                "--idle-exit" => {
                    let secs: u64 = it
                        .next()
                        .ok_or("--idle-exit 需要秒数")?
                        .parse()
                        .map_err(|_| "--idle-exit 需要一个整数（秒）")?;
                    args.idle_exit = Duration::from_secs(secs);
                }
                "--selftest" => args.selftest = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("不认识的参数：{other}")),
            }
        }
        Ok(args)
    }
}

fn print_help() {
    println!("亨利输入法 · 常驻引擎宿主");
    println!();
    println!("  --scheme-dir <dir>   方案目录（默认 <exe 目录>\\data\\wubi86）");
    println!("  --debug-sleep <ms>   每次查询前故意睡若干毫秒（验零阻塞）");
    println!("  --idle-exit <secs>   无连接多久后退出，0 = 不退出（默认 1800）");
    println!("  --selftest           只装载词库并跑几条查询，然后退出");
}

/// 默认方案目录：与 exe 同级的 `data/wubi86`。
///
/// 故意**不读注册表**：setup 已经保证 dist 目录里的相对结构，读注册表只会多一处
/// 可能与实际安装位置不一致的状态。（DLL 侧找 server 也走「同目录」这条规则。）
fn default_scheme_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("data").join("wubi86"))
}

fn main() {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误：{e}");
            print_help();
            std::process::exit(2);
        }
    };

    let dir = match args.scheme_dir.clone().or_else(default_scheme_dir) {
        Some(d) => d,
        None => {
            eprintln!("无法确定方案目录，请用 --scheme-dir 指定");
            std::process::exit(2);
        }
    };

    let host = match Host::load(&dir) {
        Ok(h) => h,
        Err(e) => {
            srv_error!("{e}");
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    if args.selftest {
        std::process::exit(selftest(&host));
    }

    #[cfg(windows)]
    {
        let cfg = pipe::Config {
            idle_exit: args.idle_exit,
            debug_sleep: args.debug_sleep,
        };
        std::process::exit(pipe::run(host, cfg));
    }

    #[cfg(not(windows))]
    {
        eprintln!("本程序当前只有 Windows 形态（命名管道传输）");
        std::process::exit(2);
    }
}

/// 无界面自测：装载成功 + 几条查询的答案对得上，就返回 0。
///
/// 与 DLL 的自测一样，这是给 `check.ps1` 用的——不需要真的开一个管道客户端。
fn selftest(host: &Host) -> i32 {
    println!("方案 {} · {} 条词条", host.scheme_id(), host.entry_count());
    let cases = [("vbg", "好"), ("q", "我"), ("vbgf", "好玩")];
    let mut failed = 0;
    for (code, want) in cases {
        let got = host.query(code, 1);
        let first = got.first().map(|e| e.word.as_str()).unwrap_or("（无）");
        let ok = first == want;
        println!("  {code:<6} → {first}  {}", if ok { "✓" } else { "✗" });
        if !ok {
            failed += 1;
        }
    }
    // 无命中必须是空列表，不能是前缀兜底的结果
    let empty = host.query("abc", 9).is_empty();
    println!("  abc    → 无命中  {}", if empty { "✓" } else { "✗" });
    if !empty {
        failed += 1;
    }
    if failed == 0 {
        println!("自测通过");
        0
    } else {
        println!("自测失败 {failed} 项");
        1
    }
}
