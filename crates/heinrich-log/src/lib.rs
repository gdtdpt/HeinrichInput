//! 亨利五笔的共享文件日志。
//!
//! 两个写者（注入型 DLL 的 `tsf.log`、常驻 server 的 `server.log`）用的是同一套
//! 排版、级别与轮转规则，所以实现在这里，各自的 `log.rs` 只留一层薄壳
//! （级别枚举、宏名、启动 banner 的内容）。
//!
//! ## 为什么必须每次 open → write → close
//!
//! 同一个日志文件会被 N 个进程同时写（DLL 被注入进 chrome 的每个 tab、explorer、
//! 编辑器…）。「打开一次、长期持有」的写法下，一旦另一个进程插入内容，本进程的
//! 文件指针就过期了，后续写入会从过时的位置覆盖对方（Mino 项目实测丢过整段日志）。
//!
//! 每次独立 `open(append)` → `write` → `close`，`append(true)` 在 Windows 上映射为
//! `FILE_APPEND_DATA`，由内核保证「定位到末尾 + 写入」原子，不会互相覆盖。
//!
//! ## 轮转：按天分文件，不做 rename
//!
//! 常见做法「写满就改名 `xxx.log → xxx.log.1`」在这里是**错的**：rename 需要先
//! 关掉所有写者，而写者是几十个我们控制不了生命周期的外部进程——改名那一刻在
//! 写入的进程会写进一个已经不存在的路径（旧句柄），日志直接丢。
//!
//! 所以文件名里带日期：`tsf-20261023.log`。跨天不需要任何动作，路径自己就变了，
//! 「本来就要 open 一个新文件」这件事实天然完成了切分。
//!
//! ## 保留与上限
//!
//! - 每个进程首次写日志时扫一遍目录，删掉 `{stem}*.log` 里超过 [`KEEP_DAYS`] 天
//!   没被写过的文件。扫描放在「每进程一次」，不放在每行写入上。
//! - 单日文件超过 [`SOFT_CAP`] 后只写 `warn` / `error`；超过 [`HARD_CAP`] 停写。
//!   这是**总量闸**，不是去重：触发次数一条不少地落盘，直到当天用满预算。
//!
//! ## 不做的事
//!
//! - 不在 `DllMain` 里调用（loader lock 死锁风险）。
//! - 不弹窗、不重试、不因为日志失败影响打字：任何 IO 错误一律静默放弃。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// 日志级别。数值越大越啰嗦。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
}

impl Level {
    pub fn tag(self) -> char {
        match self {
            Level::Error => 'E',
            Level::Warn => 'W',
            Level::Info => 'I',
            Level::Debug => 'D',
            Level::Off => '-',
        }
    }

    /// 给 banner 用的名字，和 `HEINRICH_*_LOG` 接受的字面量同形。
    pub fn name(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }

    /// 解析 `HEINRICH_*_LOG` 的取值；`None` 表示「这个字符串不认识」。
    pub fn parse(raw: &str) -> Option<Level> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" => Some(Level::Off),
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" | "trace" => Some(Level::Debug),
            _ => None,
        }
    }
}

/// 单日文件超过这个大小后只写 `warn` / `error`。
pub const SOFT_CAP: u64 = 8 * 1024 * 1024;
/// 单日文件的硬上限，超过就当天停写。
///
/// 为什么还需要硬上限：`SOFT_CAP` 之后 `error` 仍然放行，而**真正的刷屏源往往就是
/// error**（例如「server 拒绝握手：协议版本不匹配」这类不会自愈的故障，每个宿主
/// 进程每 5 秒一条）。只有软阈值的话，一个坏版本能让当天文件涨到几百 MB。
pub const HARD_CAP: u64 = 32 * 1024 * 1024;
/// 按天文件保留多少天（按最后写入时间算，不是文件名里的日期）。
pub const KEEP_DAYS: u64 = 7;

const SECS_PER_DAY: u64 = 24 * 60 * 60;

/// 一个日志目的地。用 `static` 声明，则天然是进程内单例。
///
/// 字段全是 `OnceLock` / 原子量，所以 `const fn new` 可以直接初始化，
/// 不需要 `lazy_static` 那类机制——DLL 里少一件要在 loader lock 下小心的事。
pub struct Spec {
    /// 级别开关的环境变量名，例如 `HEINRICH_TSF_LOG`。
    env_var: &'static str,
    /// 文件名主干，例如 `tsf` → `tsf-20261023.log`。
    stem: &'static str,
    level: OnceLock<Level>,
    dir: OnceLock<Option<PathBuf>>,
    /// 「本进程扫过一次目录了」。
    swept: AtomicBool,
    /// 已经写过「越过软阈值」提示的日期（yyyymmdd），避免每行都写一遍。
    soft_noted: AtomicU32,
}

impl Spec {
    pub const fn new(env_var: &'static str, stem: &'static str) -> Self {
        Self {
            env_var,
            stem,
            level: OnceLock::new(),
            dir: OnceLock::new(),
            swept: AtomicBool::new(false),
            soft_noted: AtomicU32::new(0),
        }
    }

    /// 当前级别，进程内只解析一次。
    ///
    /// 顺序：环境变量 → `%LOCALAPPDATA%\Heinrich\debug.on` 标志文件 → `info`。
    /// 写错时不静默变 `off`，否则「日志是空的」会被误判成「没收到按键」。
    pub fn level(&self) -> Level {
        *self.level.get_or_init(|| {
            if let Some(l) = Level::parse(&std::env::var(self.env_var).unwrap_or_default()) {
                return l;
            }
            if debug_marker().is_some_and(|p| p.exists()) {
                return Level::Debug;
            }
            Level::Info
        })
    }

    pub fn enabled(&self, l: Level) -> bool {
        l != Level::Off && l <= self.level()
    }

    /// `%LOCALAPPDATA%\Heinrich\logs`，解析失败为 `None`（表示这次不写）。
    pub fn dir(&self) -> Option<&Path> {
        self.dir
            .get_or_init(|| {
                let base = std::env::var_os("LOCALAPPDATA")?;
                let dir = PathBuf::from(base).join("Heinrich").join("logs");
                fs::create_dir_all(&dir).ok()?;
                Some(dir)
            })
            .as_deref()
    }

    /// 今天该写的文件：`<stem>-<yyyymmdd>.log`。
    pub fn today_path(&self) -> Option<PathBuf> {
        let (day, _) = now();
        Some(self.dir()?.join(format!("{}-{:08}.log", self.stem, day)))
    }

    pub fn log(&self, l: Level, args: std::fmt::Arguments<'_>) {
        if !self.enabled(l) {
            return;
        }
        let Some(dir) = self.dir() else { return };
        self.sweep_once(dir);

        let (day, stamp) = now();
        let path = dir.join(format!("{}-{:08}.log", self.stem, day));
        let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) else {
            return;
        };
        // append 打开后 metadata 就是当前长度：一次 GetFileSize，够便宜。
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len >= HARD_CAP {
            return;
        }
        if len >= SOFT_CAP && l > Level::Warn {
            // 提示只写一次（每次进程每天一次）。它自己也可能写不进去，那就算了。
            if self.soft_noted.swap(day, Ordering::Relaxed) != day {
                let note = format!(
                    "{} [{}] W 当日日志已到 {} MiB，此后只记 warn/error；{} MiB 处停写。\n",
                    stamp,
                    std::process::id(),
                    SOFT_CAP / (1024 * 1024),
                    HARD_CAP / (1024 * 1024)
                );
                let _ = f.write_all(note.as_bytes());
            }
            return;
        }

        let line = format!("{} [{}] {} {}\n", stamp, std::process::id(), l.tag(), args);
        let _ = f.write_all(line.as_bytes());
    }

    /// 清掉过期文件。每个进程只扫一次：调用点在 `log` 的第一行之后，
    /// 但用原子量挡住重复扫描——高频调用（`debug` 级每 200 ms 一条）不能再付目录遍历。
    fn sweep_once(&self, dir: &Path) {
        if self.swept.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(cutoff) =
            SystemTime::now().checked_sub(Duration::from_secs(KEEP_DAYS * SECS_PER_DAY))
        else {
            return;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let prefix = format!("{}-", self.stem);
        let legacy = format!("{}.log", self.stem);
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // 只碰自己的文件：`tsf-*.log` 与旧格式 `tsf.log`。`server-*.log` 归 server 管。
            if !name.starts_with(&prefix) && name != legacy {
                continue;
            }
            let Ok(md) = entry.metadata() else { continue };
            let Ok(mtime) = md.modified() else { continue };
            if mtime < cutoff {
                // 多进程可能同时删，失败（已被别人删掉）忽略即可。
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// 诊断开关文件：`%LOCALAPPDATA%\Heinrich\debug.on`，存在即把级别提到 `debug`。
///
/// DLL 跑在别人的进程里，改环境变量要去系统设置里翻、还要重启宿主；
/// 一个标志文件用 `tools\tsf.ps1 debug on` 就能开关，排障成本低得多。
fn debug_marker() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("debug.on"))
}

/// 今天的 `yyyymmdd`（用于文件名）与一行时间戳。
#[cfg(windows)]
fn now() -> (u32, String) {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    let day = t.wYear as u32 * 10_000 + t.wMonth as u32 * 100 + t.wDay as u32;
    let stamp = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    );
    (day, stamp)
}

#[cfg(not(windows))]
fn now() -> (u32, String) {
    // 非 Windows 只是为了能 `cargo check` 过；两个产物在那边都不跑。
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    ((secs / SECS_PER_DAY) as u32 + 1, format!("t={secs}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_ordering_and_parsing() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert_eq!(Level::parse("DEBUG"), Some(Level::Debug));
        assert_eq!(Level::parse(" warning "), Some(Level::Warn));
        assert_eq!(Level::parse("nope"), None);
        assert_eq!(Level::Off.tag(), '-');
    }

    #[test]
    fn spec_is_const_constructible() {
        static S: Spec = Spec::new("HEINRICH_TEST_LOG", "test");
        // 只验「能声明成 static 并取到路径形状」，不落盘。
        assert_eq!(S.env_var, "HEINRICH_TEST_LOG");
        assert_eq!(S.stem, "test");
    }

    #[test]
    fn today_path_shape() {
        static S: Spec = Spec::new("HEINRICH_TEST_LOG", "tape");
        // 沙箱里 LOCALAPPDATA 未必存在；两种情况都不应 panic。
        if let Some(p) = S.today_path() {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            assert!(
                name.starts_with("tape-") && name.ends_with(".log"),
                "{name}"
            );
        }
    }
}
