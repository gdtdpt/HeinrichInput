//! DLL 内的文件日志。
//!
//! ## 为什么不能直接 println
//!
//! TSF DLL 会被注入到**每一个**加载输入法的进程（chrome 的每个 tab、explorer、
//! VS Code…）。这些进程大多没有控制台，`println!` 写 stdout 不会报错也不会有人看到。
//! 所以日志必须落文件。
//!
//! ## 多写者正确性
//!
//! 同一个日志文件会被 N 个进程同时写。这里刻意**不用**「打开一次、长期持有」的写法：
//! 那种写法下，一旦另一个进程插入内容，本进程的文件指针就过期了，后续写入会从
//! 过时的位置覆盖对方（这在 Mino 项目上实测丢过整段日志）。
//!
//! 这里每次写入都是独立的 open → write → close，并且用 `append(true)`——
//! 它在 Windows 上映射为 `FILE_APPEND_DATA` 访问权，由内核保证「定位到末尾 + 写入」
//! 是原子的，不会互相覆盖。
//!
//! ## 不做的事
//!
//! - 不在 `DllMain` 里调用任何本模块的函数（loader lock 死锁风险）。
//! - 不弹窗、不写调试输出以外的地方。
//! - 日志文件写不进去就静默放弃——输入法不能因为日志失败而影响打字。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// 日志级别。数值越大越啰嗦。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
}

/// 当前级别，进程内只解析一次（`HEINRICH_TSF_LOG` 环境变量，默认 `info`）。
static LEVEL: OnceLock<Level> = OnceLock::new();
/// 日志文件路径，解析失败时为空（表示关闭日志）。
static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
/// 避免「路径不可用」这件事被重复告警。
static WARNED: AtomicU8 = AtomicU8::new(0);

fn level() -> Level {
    *LEVEL.get_or_init(|| {
        // 1) 环境变量优先，显式设了就以它为准。
        let raw = std::env::var("HEINRICH_TSF_LOG").unwrap_or_default();
        let from_env = match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" => Some(Level::Off),
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "debug" | "trace" => Some(Level::Debug),
            "info" => Some(Level::Info),
            _ => None,
        };
        if let Some(l) = from_env {
            return l;
        }
        // 2) 没设环境变量时看标志文件。DLL 跑在别人的进程里，改环境变量要去
        //    系统设置里翻、还要重启宿主；一个标志文件用 `tools\tsf.ps1 debug on`
        //    就能开关，排障成本低得多。
        if debug_marker().is_some_and(|p| p.exists()) {
            return Level::Debug;
        }
        // 3) 默认 info。写错时不静默变 off，否则「日志是空的」会被误判成没收到按键。
        Level::Info
    })
}

/// 诊断开关文件：`%LOCALAPPDATA%\Heinrich\debug.on`，存在即把级别提到 `debug`。
fn debug_marker() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("debug.on"))
}

fn path() -> Option<&'static PathBuf> {
    PATH.get_or_init(|| {
        let base = std::env::var_os("LOCALAPPDATA")?;
        let dir = PathBuf::from(base).join("Heinrich").join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join("tsf.log"))
    })
    .as_ref()
}

pub fn enabled(l: Level) -> bool {
    l <= level()
}

/// 写入一行。`args` 用 `format_args!` 传，未启用时不做格式化。
pub fn log(l: Level, args: std::fmt::Arguments<'_>) {
    if !enabled(l) {
        return;
    }
    let Some(path) = path() else {
        if WARNED.swap(1, Ordering::Relaxed) == 0 {
            // 此时连日志都没有，只能认命：不打扰用户。
        }
        return;
    };
    let tag = match l {
        Level::Error => 'E',
        Level::Warn => 'W',
        Level::Info => 'I',
        Level::Debug => 'D',
        Level::Off => return,
    };
    let line = format!(
        "{} [{}] {} {}\n",
        timestamp(),
        std::process::id(),
        tag,
        args
    );
    // 独立 open → write → close：见模块文档「多写者正确性」。
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// `2026-10-20 13:45:01.123`
fn timestamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

pub fn info(args: std::fmt::Arguments<'_>) {
    log(Level::Info, args);
}

pub fn debug(args: std::fmt::Arguments<'_>) {
    log(Level::Debug, args);
}

pub fn error(args: std::fmt::Arguments<'_>) {
    log(Level::Error, args);
}

/// 三个便捷宏。用 `format_args!` 传参：未达到级别时不发生格式化开销。
#[macro_export]
macro_rules! tsf_info {
    ($($arg:tt)*) => { $crate::log::info(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! tsf_debug {
    ($($arg:tt)*) => { $crate::log::debug(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! tsf_error {
    ($($arg:tt)*) => { $crate::log::error(format_args!($($arg)*)) };
}
