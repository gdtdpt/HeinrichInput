//! server 的文件日志。
//!
//! 与 DLL 的 `tsf.log` 分开落盘（`server.log`），因为两者的读者不同：
//! `tsf.log` 看的是「宿主有没有把键送进来」，`server.log` 看的是「server 活着吗、
//! 为什么退出了、连了几个客户端」。
//!
//! 写法沿用同一条约束：**每次写入独立 open → append → close**。多写者的 `append`
//! 在 Windows 上映射为 `FILE_APPEND_DATA`，内核保证原子追加；常驻进程持有文件
//! 指针的写法会从过时位置覆盖别人写入的内容。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
}

static LEVEL: OnceLock<Level> = OnceLock::new();
static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static WARNED: AtomicU8 = AtomicU8::new(0);

fn level() -> Level {
    *LEVEL.get_or_init(|| {
        // 环境变量优先；没设就跟着 `tools\tsf.ps1 debug on` 的标志文件走，
        // 免得排障时要在两个入口之间来回切。
        let raw = std::env::var("HEINRICH_SERVER_LOG").unwrap_or_default();
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" => return Level::Off,
            "error" => return Level::Error,
            "warn" | "warning" => return Level::Warn,
            "debug" | "trace" => return Level::Debug,
            "info" => return Level::Info,
            _ => {}
        }
        if marker().is_some_and(|p| p.exists()) {
            return Level::Debug;
        }
        Level::Info
    })
}

fn marker() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("debug.on"))
}

pub fn path() -> Option<&'static PathBuf> {
    PATH.get_or_init(|| {
        let base = std::env::var_os("LOCALAPPDATA")?;
        let dir = PathBuf::from(base).join("Heinrich").join("logs");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join("server.log"))
    })
    .as_ref()
}

fn enabled(l: Level) -> bool {
    l <= level()
}

pub fn log(l: Level, args: std::fmt::Arguments<'_>) {
    if !enabled(l) {
        return;
    }
    let Some(p) = path() else {
        if WARNED.swap(1, Ordering::Relaxed) == 0 {
            // 连日志都写不进去，只能认命：输入法不能因为日志失败而影响打字。
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
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(windows)]
fn timestamp() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(not(windows))]
fn timestamp() -> String {
    // 非 Windows 只是为了能 `cargo check` 过；server 的正常形态只在 Windows 跑。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("t={secs}")
}

pub fn info(args: std::fmt::Arguments<'_>) {
    log(Level::Info, args);
}
pub fn warn(args: std::fmt::Arguments<'_>) {
    log(Level::Warn, args);
}
pub fn debug(args: std::fmt::Arguments<'_>) {
    log(Level::Debug, args);
}
pub fn error(args: std::fmt::Arguments<'_>) {
    log(Level::Error, args);
}

#[macro_export]
macro_rules! srv_info {
    ($($arg:tt)*) => { $crate::log::info(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! srv_warn {
    ($($arg:tt)*) => { $crate::log::warn(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! srv_debug {
    ($($arg:tt)*) => { $crate::log::debug(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! srv_error {
    ($($arg:tt)*) => { $crate::log::error(format_args!($($arg)*)) };
}
