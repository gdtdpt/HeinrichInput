//! DLL 内的文件日志 —— `log.rs` 这层只做三件事：定级别开关、定宏名、写启动 banner。
//!
//! 排版、按天分文件、保留天数、单日体积上限都在 `heinrich-log` 里实现，
//! 和 server 的 `server.log` 共用同一套规则（两处各写一份会慢慢走样）。
//!
//! 文件位置：`%LOCALAPPDATA%\Heinrich\logs\tsf-<yyyymmdd>.log`
//!
//! 为什么不能直接 `println!`：TSF DLL 会被注入到**每一个**加载输入法的进程
//! （chrome 的每个 tab、explorer、VS Code…），这些进程大多没有控制台。
//!
//! 为什么是「每次写入独立 open → append → close」：见 `heinrich-log` 的模块文档。
//! 一句话：多写者共用文件时，长期持有文件指针会从过时位置覆盖别人的内容。
//!
//! ## 不做的事
//!
//! - 不在 `DllMain` 里调用任何本模块的函数（loader lock 死锁风险）。
//! - 不弹窗、不写调试输出以外的地方。
//! - 日志文件写不进去就静默放弃——输入法不能因为日志失败而影响打字。

use std::sync::atomic::{AtomicBool, Ordering};

pub use heinrich_log::Level;

/// 本进程的日志目的地。`static` 保证级别解析、目录解析各只做一次。
static SPEC: heinrich_log::Spec = heinrich_log::Spec::new("HEINRICH_TSF_LOG", "tsf");
/// 「已经写过 banner 了」。
static BANNER: AtomicBool = AtomicBool::new(false);

/// 当前级别（banner 里要用它的名字）。
pub fn level() -> Level {
    SPEC.level()
}

/// 写入一行。`args` 用 `format_args!` 传，未启用时不做格式化。
pub fn log(l: Level, args: std::fmt::Arguments<'_>) {
    if !SPEC.enabled(l) {
        return;
    }
    banner_once();
    SPEC.log(l, args);
}

/// 每个宿主进程的第一条日志前插一行「我是谁」。
///
/// 排障时最常问的两个问题是「这个 pid 是哪个程序的」和「它加载的是哪版 DLL」。
/// 没有这一行，只能靠时间戳和宿主进程表去猜；而版本不匹配（ABI / 协议）恰恰是
/// 最常见的一类故障，猜错的代价是把「旧 DLL 还在跑」误判成「新代码有 bug」。
fn banner_once() {
    if BANNER.swap(true, Ordering::Relaxed) {
        return;
    }
    let abi = crate::HeinrichTsfAbiVersion();
    SPEC.log(
        Level::Info,
        format_args!(
            "==== 亨利五笔 DLL：{}，ABI {}，协议 v{}，日志 {} ====",
            crate::who(),
            abi,
            heinrich_proto::PROTO_VERSION,
            level().name()
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

/// 四个便捷宏。用 `format_args!` 传参：未达到级别时不发生格式化开销。
#[macro_export]
macro_rules! tsf_info {
    ($($arg:tt)*) => { $crate::log::info(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! tsf_warn {
    ($($arg:tt)*) => { $crate::log::warn(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! tsf_debug {
    ($($arg:tt)*) => { $crate::log::debug(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! tsf_error {
    ($($arg:tt)*) => { $crate::log::error(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_and_function_agree_on_level() {
        assert_eq!(Level::Warn.tag(), 'W');
        assert!(Level::Error <= Level::Warn);
    }
}
