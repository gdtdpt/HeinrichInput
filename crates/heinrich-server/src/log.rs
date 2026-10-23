//! server 的文件日志 —— 这层只做三件事：定级别开关、定宏名、写启动 banner。
//!
//! 排版、按天分文件、保留天数、单日体积上限都在 `heinrich-log` 里实现，
//! 与 DLL 的 `tsf.log` 共用同一套规则。
//!
//! 落盘位置：`%LOCALAPPDATA%\Heinrich\logs\server-<yyyymmdd>.log`
//!
//! 和 DLL 分开存是因为两者的读者不同：`tsf.log` 看「宿主有没有把键送进来」，
//! `server.log` 看「server 活着吗、为什么退出了、连了几个客户端」。
//!
//! 写法沿用同一条约束：**每次写入独立 open → append → close**。多写者的 `append`
//! 在 Windows 上映射为 `FILE_APPEND_DATA`，内核保证原子追加；常驻进程持有文件
//! 指针的写法会从过时位置覆盖别人写入的内容。

use std::sync::atomic::{AtomicBool, Ordering};

pub use heinrich_log::Level;

static SPEC: heinrich_log::Spec = heinrich_log::Spec::new("HEINRICH_SERVER_LOG", "server");
static BANNER: AtomicBool = AtomicBool::new(false);

pub fn level() -> Level {
    SPEC.level()
}

pub fn log(l: Level, args: std::fmt::Arguments<'_>) {
    if !SPEC.enabled(l) {
        return;
    }
    banner_once();
    SPEC.log(l, args);
}

/// 首条日志前插一行版本信息：`server.log` 里的错误九成要配着「这是哪一版」
/// 才能读，而升级 dist 之后新旧 server 会并存一段时间（D1 的惰性拉起不保证
/// 立刻换掉正在跑的旧进程）。
fn banner_once() {
    if BANNER.swap(true, Ordering::Relaxed) {
        return;
    }
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unknown>".into());
    SPEC.log(
        Level::Info,
        format_args!(
            "==== 亨利五笔 server：pid {}，协议 v{}，日志 {}，exe {} ====",
            std::process::id(),
            heinrich_proto::PROTO_VERSION,
            level().name(),
            exe
        ),
    );
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
