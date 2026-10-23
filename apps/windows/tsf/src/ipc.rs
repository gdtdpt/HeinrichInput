//! DLL → server 的 IPC 客户端。
//!
//! ## 这条模块存在的理由：D18「按键路径零阻塞」
//!
//! TSF 在**宿主的 UI 线程**上同步回调 `OnKeyDown`。在这里等一次 IPC，
//! 就是让记事本/Chrome 的界面跟着一起等。server 冷启动要装载 13.6 万条词库
//! （约 300 ms），被挂起、被杀、磁盘卡顿都可能发生——任何一次都会被用户感知成
//! 「打字卡住」。
//!
//! 所以这里的规则有两条，缺一不可：
//!
//! 1. **键路径上的每次往返都有硬上限**（[`KEY_BUDGET`]）。超时就直接放弃这次
//!    查询，把编码原样上屏（D27），绝不再等。
//! 2. **连接、重连、拉起 server 全在后台线程里**。键路径只做 `try_lock`：
//!    拿不到锁（后台正在连）就立刻放弃，连 50 ms 都不花。
//!
//! ## 超时之后为什么把整条连接丢掉
//!
//! 重叠 I/O 超时后管道里可能还剩着半个响应，继续复用会读到错位的帧——一条
//! 错位的连接比没有连接难查得多。直接把句柄关掉：server 那边看到断开，
//! 后台线程在 200 ms 内重连。代价是这一键降级成「原样上屏」。

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT,
    HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, ResetEvent, WaitForSingleObject,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use heinrich_proto::{
    body_len, feature, source as proto_source, Entry, Frame, Mode, ProtoKeyRoots, MAX_ENTRIES,
    PROTO_VERSION,
};

/// 键路径上的往返预算。本地管道正常是**几十微秒**级，50 ms 已经是「server 出事」
/// 的量级；再大就会让用户感觉到卡。
const KEY_BUDGET: Duration = Duration::from_millis(50);

/// 后台线程握手预算。server 冷启动要装载词库（约 300 ms），给足余量——
/// 这是后台线程，等多久都不影响打字。
const GREET_BUDGET: Duration = Duration::from_millis(3000);

/// 后台线程轮询间隔。
const WORKER_TICK: Duration = Duration::from_millis(200);

/// 拉起 server 的最小间隔。防止「server 起不来」时每个 tick 都去 CreateProcess。
const SPAWN_BACKOFF: Duration = Duration::from_secs(5);

/// server **拒绝**握手后的重试间隔。协议版本不匹配在重装 DLL 前不会自愈，
/// 200 ms 一轮只会把日志刷满、把 server 管道当磨刀石；5 s 足够等一次升级。
const REJECT_RETRY: Duration = Duration::from_secs(5);

/// 本地候选缓存条数上限。满了直接清空——这是个纯粹的热路径优化，
/// 不需要 LRU 的精确性。
const CACHE_CAP: usize = 64;

/// 候选缓存的有效期。
///
/// 这份缓存的存在理由是「连打同一个码时顺序不跳」（D55/D57），而那是**亚秒级**的
/// 事——3 秒足够覆盖，多出来的时间只是在替用户挡掉一次几毫秒的本机 IPC。
///
/// 不设上限会有一个真问题：server 按 mtime 热重载 `words.tsv`，但 DLL 这份缓存
/// 没有任何推送通道。于是**加词之前打过的码**，加完词再打永远看到旧结果——
/// 而且因为命中缓存，server 日志里连一条 `Query` 都不会出现（F45 的真机现象：
/// 「微信」保存成功、`tmwy` 却还是四个生僻字）。缓存过期是这条链上唯一不升协议
/// 就能补的洞。
const CACHE_TTL: Duration = Duration::from_secs(3);

/// 待上报的上屏记录条数上限。
///
/// 溢出丢**最旧**的：学习数据丢几条不影响打字，但让它无限涨会拖死宿主进程
/// （server 被杀一整天、用户又一直在打字，就是这个场景）。512 条足够覆盖
/// 「server 挂了几分钟」的典型情况。
const RECORD_QUEUE_CAP: usize = 512;

/// 一次最多发多少条上屏记录，避免一次占用连接太久挡住查询。
const RECORD_BATCH: usize = 64;

/// 上报单条记录的写入预算。后台线程，给得比键路径宽松。
const RECORD_BUDGET: Duration = Duration::from_millis(500);

/// 方案 id。M1.3 只有一档；握手时拿它比对，防止「装的是别的方案」这种错位。
const SCHEME_ID: &str = "wubi86";

struct Conn {
    pipe: HANDLE,
    event: HANDLE,
}

// 句柄本身可以在线程间传递，windows-rs 只是不替我们担保这一点。
unsafe impl Send for Conn {}

impl Drop for Conn {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.pipe);
            let _ = CloseHandle(self.event);
        }
    }
}

static CONN: Mutex<Option<Conn>> = Mutex::new(None);
static CACHE: Mutex<Option<CacheState>> = Mutex::new(None);
static PENDING_MODE: Mutex<Option<Mode>> = Mutex::new(None);
/// server 推来的字根表（§6.7）。`None` = 还没收到，此时键位图相关按键一律放行。
///
/// 数据在一整条连接里不变，收到就留着；断线重连时新连接会再推一次。
/// 不清空：万一重连期间用户按了 `Ctrl+Shift+N`，用上一次的表画总比不弹好。
static ROOTS: Mutex<Option<Vec<ProtoKeyRoots>>> = Mutex::new(None);
/// 服务端最近一次握手声明的能力位（`Frame::Welcome::features`）。
///
/// 键路径要读它（`z` 归谁、码长上限），而握手在后台线程——所以存在原子变量里。
/// 连不上时保持上一次的值：宁可沿用「有拼音」也别在一次掉线后把 `z` 突然锁死。
static FEATURES: AtomicU8 = AtomicU8::new(0);
/// 待上报的上屏记录：`(编码, 词, 提交时刻)`。
///
/// 为什么要排队而不是「打完直接发」：上屏发生在键路径上（D18 一行都不能等），
/// 而 server 可能正被杀掉、正在冷启动、管道正忙。排队 + 后台重试让这两件事解耦；
/// server 起来后这批记录会整批补上（对应 M2.4 验收项 ②「强杀 server 不丢记录」）。
static PENDING_RECORDS: Mutex<VecDeque<(String, String, i64)>> = Mutex::new(VecDeque::new());
static WORKER: Once = Once::new();
static SPAWN_FAILED_LOGGED: AtomicBool = AtomicBool::new(false);
/// 因队列满被丢弃的记录条数（只用于日志，不需要精确）。
static RECORDS_DROPPED: AtomicU32 = AtomicU32::new(0);

/// 启动后台连接线程。幂等，`Activate` 每次都可以调。
///
/// 放在 `Activate` 而不是第一次按键上：`Activate` 发生在用户切到某个输入框时，
/// 而真正开始打字通常还有几百毫秒，足够冷启动完成。这样首次上屏就大概率是
/// 真候选，而不是「原样上屏的字母」。
pub fn warm_up() {
    WORKER.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("heinrich-ipc".into())
            .spawn(worker);
        tsf_debug!("IPC 后台线程已启动");
    });
}

/// 一次查询的答复。
///
/// 比「一串候选」多两个字段，都是为了 §6.2 的分区显示：候选窗要把**首屏**
/// （判定语言）与**折叠区**（另一种语言）分开画，而判定在 server 做——
/// DLL 手上只有两份词库里查到的东西，自己判不出这一串到底是五笔还是拼音。
///
/// 字段是 `words` / `pinyin` / `primary_len` 而不是协议类型，是为了不让
/// `heinrich_proto` 的类型渗进 `text_service`（那一层只认字符串与 `keys::Mode`）。
#[derive(Clone, Debug)]
pub struct Reply {
    pub words: Vec<String>,
    /// 与 `words` 等长的五笔码提示（§6.3）。空串 = 这条不画 chip。
    ///
    /// 给不给由 server 定：它知道这段候选是五笔还是拼音查出来的、`hint.mode` 是
    /// 什么。DLL 这层只负责「非空就画」，不重复实现一遍规则。
    pub codes: Vec<String>,
    /// 与 `words` 等长的 chip 词中位置（D64）：chip 挂在第几个字符之后，
    /// `0` = 词尾。多段组合的 chip 贴着拼音段的字。
    pub hint_ats: Vec<u8>,
    /// 首屏候选来自拼音（`Frame::Result::source`）
    pub pinyin: bool,
    /// 前多少条属于首屏；`words` 的其余部分是折叠区
    pub primary_len: usize,
}

impl Reply {
    fn from_result(entries: Vec<Entry>, source: u8, primary_len: u16) -> Self {
        let words: Vec<String> = entries.iter().map(|e| e.word.clone()).collect();
        let codes: Vec<String> = entries.iter().map(|e| e.code.clone()).collect();
        let hint_ats: Vec<u8> = entries.into_iter().map(|e| e.hint_at).collect();
        // 协议里的 `primary_len` 是 server 算的，仍然夹一次：万一 server 版本对不上
        // 给了个越界值，DLL 这边最多画错分区，不能因此切片 panic。
        let primary_len = (primary_len as usize).min(words.len());
        Self {
            words,
            codes,
            hint_ats,
            pinyin: source == proto_source::PINYIN,
            primary_len,
        }
    }
}

/// 查一个码的候选。**永不阻塞超过 [`KEY_BUDGET`]**。
///
/// 返回 `None` 表示「没问到」（server 不可用 / 超时 / 后台正在重连）——
/// 调用方应当把编码原样上屏。返回 `Some(空)` 表示「问到了，没这个词」。
pub fn query(code: &str) -> Option<Reply> {
    if let Some(hit) = cache_get(code) {
        return Some(hit);
    }
    let reply = request(
        &Frame::Query {
            code: code.to_string(),
            limit: MAX_ENTRIES as u8,
        },
        KEY_BUDGET,
    )?;
    match reply {
        Frame::Result {
            entries,
            source,
            primary_len,
            ..
        } => {
            let reply = Reply::from_result(entries, source, primary_len);
            cache_put(code, &reply);
            Some(reply)
        }
        other => {
            tsf_debug!("查询 {code} 收到意外应答 {other:?}");
            None
        }
    }
}

/// 服务端是否装了拼音词库（混输可用）。**读的是最近一次握手的结果**。
pub fn has_pinyin() -> bool {
    FEATURES.load(Ordering::Relaxed) & feature::PINYIN != 0
}

/// server 推来的字根表（§6.7）。`None` = 还没收到过。
///
/// 返回克隆而不是引用：表本身只有几百字节，克隆一次比在键路径上「拿锁 + 持锁
/// 进绘制」安全得多——绘制要走 DWrite/D2D，那里万一批异常，锁就再也放不回来了。
pub fn key_roots() -> Option<Vec<ProtoKeyRoots>> {
    ROOTS.lock().ok().and_then(|g| g.clone())
}

/// 取 server 上的中英模式：`Some(true)` = 中文，`None` = 问不到，调用方保持不动。
///
/// 只该在 `Activate` 这类「不在按键热路径」的地方调用。对外只暴露一个布尔，
/// 是为了不让协议里的类型渗进 `text_service`（那一层只认 `keys::Mode`）。
pub fn current_mode() -> Option<bool> {
    match request(
        &Frame::Hello {
            proto: PROTO_VERSION,
            scheme_id: SCHEME_ID.to_string(),
        },
        KEY_BUDGET,
    )? {
        Frame::Welcome { mode, features, .. } => {
            FEATURES.store(features, Ordering::Relaxed);
            Some(mode == Mode::Chinese)
        }
        _ => None,
    }
}

/// 上报模式变更。**入队即返回**，真正的发送在后台线程——切模式发生在
/// Shift 松开的那一刻，那是键路径，一秒都不能多花。
pub fn set_mode(chinese: bool) {
    if let Ok(mut p) = PENDING_MODE.lock() {
        *p = Some(if chinese {
            Mode::Chinese
        } else {
            Mode::English
        });
    }
}

/// 上报一次上屏（§5.1：**只统计上屏，不统计翻页经过、不统计预览**）。
///
/// 三步都是微秒级、都不等网络：入队 → 丢掉该码的本地缓存 → 返回。真正的发送在
/// 后台线程（[`flush_pending_records`]）。这条函数会被键路径直接调用，所以
/// **绝不能**在这里做任何可能阻塞的事（D18）。
///
/// 调用方只在「真的上屏了一个候选词」时调它：原样吐字母（无命中 / server 不可用 /
/// Shift 切模式）不算，那些不是用户的选择，记进去会把 n 灌成噪声。
pub fn record(code: &str, word: &str) {
    // 多段编码（`hao'vb`）**只标脏、不上报**（M3.5 / D62）。
    //
    // `Record` 的语义是「这个码 → 这个词」，server 拿它按 `(码, 词)` 累加学习分。
    // 而多段那次查询用的码是 `hao'vb`，之后用户再打永远是单段的 `hao` 或 `vb`，
    // 这条记录再也命中不了——它不参与任何排序，只会在学习库里堆垃圾行。
    // 各段单独打时本来就会各自记分，多段不记不会丢信息。
    //
    // 缓存仍要标脏：那一份 `hao'vb` 的结果已经过时，留着会让同一个码的连打顺序
    // 与「刚刚选过什么」脱节。
    let report = !code.contains('\'');
    if report {
        let at = unix_now();
        if let Ok(mut q) = PENDING_RECORDS.lock() {
            if q.len() >= RECORD_QUEUE_CAP {
                q.pop_front();
                RECORDS_DROPPED.fetch_add(1, Ordering::Relaxed);
                // 只在第一次丢时记一条，避免 server 长时间不在时刷满日志。
                if RECORDS_DROPPED.load(Ordering::Relaxed) == 1 {
                    tsf_error!("上屏记录队列已满（{RECORD_QUEUE_CAP} 条），开始丢最旧的");
                }
            }
            q.push_back((code.to_string(), word.to_string(), at));
        }
    }
    // 本地缓存里这一份已经过时，但**不立刻删**（D55）：连打同一个码时顺序必须
    // 稳定，否则用户按着「2」而第 2 条换了人。标脏，等他打别的码再让它失效。
    cache_mark_stale(code);
}

fn request(frame: &Frame, budget: Duration) -> Option<Frame> {
    // try_lock 是关键：后台线程可能正拿着锁重连，键路径绝不能在这里等。
    let mut guard = CONN.try_lock().ok()?;
    let conn = guard.as_ref()?;
    match exchange(conn, frame, budget) {
        Ok(f) => Some(f),
        Err(e) => {
            // 走 `warn` 而不是 `debug`：这一条正是「server 不可用、本次按键降级成
            // 直接上屏」的现场证据。用户在 `info` 级排障时看不到 debug 明细，
            // 却一定会问「为什么一个候选都不出」——答案就在这一行。
            tsf_warn!("IPC 往返失败（{e}），丢弃连接等后台重连");
            // 丢掉整条连接：超时后管道里可能残留半个响应，复用会读到错位的帧。
            *guard = None;
            None
        }
    }
}

/// 一次最多跳过多条推送帧。推送只在握手后有一条；给 4 是「就算对端疯了一样推，
/// 也不会把这次按键的预算耗光」。
const MAX_PUSH_SKIP: usize = 4;

fn exchange(conn: &Conn, req: &Frame, budget: Duration) -> io::Result<Frame> {
    let deadline = Instant::now() + budget;
    write_all(conn, &req.encode(), deadline)?;

    // 一条请求对应一条应答——但协议 v5 起 server 会**主动推**一帧字根表
    // （排在 `Welcome` 后面）。它静静躺在管道里，不跳过去的话下一次读应答就会
    // 读到它，整条管道从此错位一帧：查询拿到上一条的应答、模式变更拿到候选……
    // 所以这里只跳过 `Roots`，其余帧一律当应答返回。
    for _ in 0..MAX_PUSH_SKIP {
        let frame = read_frame(conn, deadline)?;
        if let Frame::Roots { keys } = frame {
            let n = keys.len();
            if let Ok(mut g) = ROOTS.lock() {
                *g = Some(keys);
            }
            tsf_debug!("收到字根表 {n} 键");
            continue;
        }
        return Ok(frame);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "连续收到推送帧，没有应答",
    ))
}

/// 读一整帧（含长度前缀）。
fn read_frame(conn: &Conn, deadline: Instant) -> io::Result<Frame> {
    let mut header = [0u8; 4];
    read_exact(conn, &mut header, deadline)?;
    let len =
        body_len(header).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let mut whole = vec![0u8; 4 + len];
    whole[..4].copy_from_slice(&header);
    read_exact(conn, &mut whole[4..], deadline)?;
    Frame::decode(&whole).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn write_all(conn: &Conn, mut buf: &[u8], deadline: Instant) -> io::Result<()> {
    while !buf.is_empty() {
        // `ReadFile`/`WriteFile` 的形参是 `Option<&mut [u8]>`（API 的历史形状），
        // 写的时候它只读这块内存，不会再写回来。
        let n = unsafe { io_once(conn, buf.as_ptr() as *mut u8, buf.len(), true, deadline)? };
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "写入 0 字节"));
        }
        buf = &buf[n..];
    }
    Ok(())
}

fn read_exact(conn: &Conn, mut buf: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { io_once(conn, buf.as_mut_ptr(), buf.len(), false, deadline)? };
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "对端关闭"));
        }
        buf = &mut buf[n..];
    }
    Ok(())
}

/// 一次带截止时间的重叠读写。
///
/// # Safety
///
/// `ptr`/`len` 必须描述一块在本次调用期间有效、且没有其他别名额在用的内存。
unsafe fn io_once(
    conn: &Conn,
    ptr: *mut u8,
    len: usize,
    write: bool,
    deadline: Instant,
) -> io::Result<usize> {
    let slice = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
    // OVERLAPPED 放堆上：超时放弃时它可能仍被内核引用，栈上的地址会被复用。
    // 那种情况下干脆泄漏这一小块（每次超时 40 字节），换「不写坏别人的内存」。
    let mut ol = Box::new(OVERLAPPED::default());
    ol.hEvent = conn.event;
    unsafe {
        let _ = ResetEvent(conn.event);
    }

    let mut done = 0u32;
    let result = unsafe {
        if write {
            WriteFile(
                conn.pipe,
                Some(slice),
                Some(&mut done as *mut u32),
                Some(&mut *ol),
            )
        } else {
            ReadFile(
                conn.pipe,
                Some(slice),
                Some(&mut done as *mut u32),
                Some(&mut *ol),
            )
        }
    };

    let pending = match result {
        Ok(()) => return Ok(done as usize),
        Err(e) => e,
    };
    let code = win32_code(&pending);
    if code != ERROR_IO_PENDING.0 && code != ERROR_SEM_TIMEOUT.0 {
        return Err(io::Error::other(pending.to_string()));
    }

    let remain = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(u32::MAX as u128) as u32;
    if unsafe { WaitForSingleObject(conn.event, remain) } != WAIT_OBJECT_0 {
        unsafe {
            let _ = CancelIoEx(conn.pipe, Some(&*ol as *const _));
        }
        std::mem::forget(ol);
        return Err(io::Error::new(io::ErrorKind::TimedOut, "等待 server 超时"));
    }

    let mut transferred = 0u32;
    unsafe { GetOverlappedResult(conn.pipe, &*ol, &mut transferred, false) }
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(transferred as usize)
}

/// 从 `windows-core` 的 `Error` 里取回 Win32 错误码。
///
/// `Error::code()` 给的是 `HRESULT`，由 `HRESULT_FROM_WIN32` 包装过
/// （`0x8007_0000 | code`）。我们只关心 `ERROR_FILE_NOT_FOUND` 这类小于
/// 0xFFFF 的系统错误码，取低 16 位即可；不做这个转换就会把「管道还不存在」
/// 这种正常情况当成真错误刷进日志。
fn win32_code(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

// ---------------------------------------------------------------------------
// 后台线程：连 / 重连 / 拉起 server / 冲刷模式变更
// ---------------------------------------------------------------------------

fn worker() {
    let mut last_spawn: Option<Instant> = None;
    loop {
        let extra = ensure_connected(&mut last_spawn);
        flush_pending_mode();
        // 连上之后同 tick 就试着补发：server 刚起来时那批积压最该尽快落地。
        flush_pending_records();
        std::thread::sleep(WORKER_TICK + extra);
    }
}

/// 尝试建连。返回这轮**额外**要睡的时间（被拒时放慢，见 [`REJECT_RETRY`]）。
fn ensure_connected(last_spawn: &mut Option<Instant>) -> Duration {
    // 只在无人持锁且确实空着时动手；拿不到锁说明键路径正在用，下一 tick 再说。
    let need = match CONN.try_lock() {
        Ok(g) => g.is_none(),
        Err(_) => false,
    };
    if !need {
        return Duration::ZERO;
    }

    match open() {
        Ok(conn) => {
            match CONN.lock() {
                Ok(mut g) => {
                    // 期间可能已经被别的路径连上了，别覆盖。
                    if g.is_none() {
                        *g = Some(conn);
                    }
                }
                Err(p) => {
                    *p.into_inner() = Some(conn);
                }
            }
            SPAWN_FAILED_LOGGED.store(false, Ordering::Relaxed);
            Duration::ZERO
        }
        Err(fail) => {
            // 只有「连管道都没连上」才拉起 server（D63）。server 在但拒绝握手时
            // 拉起它，得到的是又一个绑不上管道、转身退出的进程——空转到天荒地老。
            if fail.should_spawn() {
                let may_spawn = last_spawn.is_none_or(|t| t.elapsed() >= SPAWN_BACKOFF);
                if may_spawn {
                    *last_spawn = Some(Instant::now());
                    spawn_server();
                }
            }
            fail.retry_delay()
        }
    }
}

/// `open()` 失败的形态。**只有 `NoServer` 才值得拉起 server**（D63）：
///
/// 实测踩过：旧版 DLL（协议 4）对上新版 server（协议 5），握手被 `Bye` 拒绝，
/// 旧代码把一切失败都当成「server 没跑」，于是每 5 s 拉起一个新 server——
/// 而新 server 绑不上已被占用的管道、立刻退出，形成「拉起 → 退出 → 再拉起」
/// 的死循环，一次不匹配能空转出几百个进程。
///
/// 只做分类不带数据：具体错误在发生地（`open`）就地记日志，这里不重复。
enum OpenFail {
    /// `CreateFileW` 都没成功：管道不存在或全忙，server 大概率没跑。
    NoServer,
    /// 管道通了，但收到 `Frame::Bye`：协议版本 / 方案不匹配。重装 DLL 前不会好，
    /// 拉起 server 无济于事。
    Rejected,
    /// 管道通了，但握手在读应答时断掉：server 写完 `Bye` 就断管道（v1.31 前的
    /// server 行为）或 server 中途崩了。server 大概率还在，重试即可，不拉起。
    Broken,
}

impl OpenFail {
    /// 这次失败之后要不要去拉起 server。
    fn should_spawn(&self) -> bool {
        matches!(self, OpenFail::NoServer)
    }

    /// 失败后这次 tick 额外睡多久（`WORKER_TICK` 之外）。
    fn retry_delay(&self) -> Duration {
        match self {
            // 管道都没有，按原来的节奏快速重试，拉起节奏由 SPAWN_BACKOFF 管。
            OpenFail::NoServer => Duration::ZERO,
            // 被拒就慢下来：这个状态以「分钟」为单位，5 s 一轮足够了。
            OpenFail::Rejected => REJECT_RETRY,
            // 断在半路：给 server 一点喘息，但别太久——它可能只是重启中。
            OpenFail::Broken => Duration::from_secs(1),
        }
    }
}

/// 「server 拒绝握手」只报一次错，之后降为 debug。这个状态在重装 DLL 前
/// 不会消失，5 s 一条 error 也会把日志刷满。
static REJECT_LOGGED: AtomicBool = AtomicBool::new(false);

/// 建连接并完成握手。
fn open() -> Result<Conn, OpenFail> {
    let name = pipe_name();
    let wide = to_wide(&name);
    let pipe = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            0x8000_0000 | 0x4000_0000, // GENERIC_READ | GENERIC_WRITE
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            None,
        )
    };
    let pipe = match pipe {
        Ok(h) => h,
        Err(e) => {
            // 管道不存在 / 全忙都是「server 还没准备好」，属于正常路径，不写日志刷屏。
            if win32_code(&e) != ERROR_FILE_NOT_FOUND.0 && win32_code(&e) != ERROR_PIPE_BUSY.0 {
                tsf_debug!("打开管道 {name} 失败：{e}");
            }
            return Err(OpenFail::NoServer);
        }
    };

    let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok();
    let Some(event) = event else {
        unsafe {
            let _ = CloseHandle(pipe);
        }
        // 到不了握手那一步，但管道是通的：按「server 在」处理，不拉起。
        tsf_debug!("CreateEventW 失败，本轮放弃");
        return Err(OpenFail::Broken);
    };
    let conn = Conn { pipe, event };

    match exchange(
        &conn,
        &Frame::Hello {
            proto: PROTO_VERSION,
            scheme_id: SCHEME_ID.to_string(),
        },
        GREET_BUDGET,
    ) {
        Ok(Frame::Welcome {
            entry_count,
            mode,
            features,
            ..
        }) => {
            FEATURES.store(features, Ordering::Relaxed);
            tsf_info!(
                "已连上 server（pid 未知，{} 条词条，{}，{}）",
                entry_count,
                if mode == Mode::Chinese {
                    "中文"
                } else {
                    "英文"
                },
                if features & feature::PINYIN != 0 {
                    "混输可用"
                } else {
                    "纯五笔"
                }
            );
            REJECT_LOGGED.store(false, Ordering::Relaxed);
            Ok(conn)
        }
        Ok(Frame::Bye { reason }) => {
            // 方案或协议不匹配：不是重连能解决的问题，更不是拉起 server 能解决的。
            if REJECT_LOGGED.swap(true, Ordering::Relaxed) {
                tsf_debug!("server 拒绝握手（已报过一次）：{reason}");
            } else {
                tsf_error!(
                    "server 拒绝握手：{reason} —— DLL 与 server 版本不匹配，\
                     重启宿主进程（或重新安装）加载新 DLL 后恢复"
                );
            }
            Err(OpenFail::Rejected)
        }
        Ok(other) => {
            tsf_error!("握手收到意外应答：{other:?}");
            Err(OpenFail::Broken)
        }
        Err(e) => {
            tsf_debug!("握手失败：{e}");
            Err(OpenFail::Broken)
        }
    }
}

fn flush_pending_mode() {
    let pending = match PENDING_MODE.try_lock() {
        Ok(mut g) => g.take(),
        Err(_) => None,
    };
    let Some(mode) = pending else { return };
    // 用较短的预算：这是后台线程，但也没必要为一个「顺手同步」耗到 50 ms 以上。
    match request(&Frame::SetMode { mode }, KEY_BUDGET) {
        Some(Frame::Mode { .. }) => tsf_debug!("模式已同步到 server"),
        _ => {
            // 同步失败不重试：本地模式才是键路径的真相，下次切模式会再报一次。
            tsf_debug!("模式同步失败（server 不可用）");
        }
    }
}

/// 把待上报的上屏记录冲给 server。
///
/// `Frame::Record` 是**单向**帧（server 不回），所以这里只写不读——用 `exchange`
/// 会一直等一条永远不会来的应答，把 500 ms 预算耗光并误判成「连接坏了」。
fn flush_pending_records() {
    let batch: Vec<(String, String, i64)> = match PENDING_RECORDS.try_lock() {
        Ok(mut q) => {
            let n = q.len().min(RECORD_BATCH);
            q.drain(..n).collect()
        }
        // 键路径正拿着锁（说明刚有人上屏），下一 tick 再说。
        Err(_) => return,
    };
    if batch.is_empty() {
        return;
    }

    let mut guard = match CONN.try_lock() {
        Ok(g) => g,
        Err(_) => {
            requeue(batch);
            return;
        }
    };
    let Some(conn) = guard.as_ref() else {
        // 没连上：整批留着，等 server 起来再发。**这是「不丢记录」的关键一步。**
        requeue(batch);
        return;
    };

    let deadline = Instant::now() + RECORD_BUDGET;
    for (i, (code, word, at)) in batch.iter().enumerate() {
        let frame = Frame::Record {
            code: code.clone(),
            word: word.clone(),
            at_unix: *at,
        };
        if let Err(e) = write_all(conn, &frame.encode(), deadline) {
            // 写了一半就断：管道里可能留着半个帧，复用会读到错位数据。
            // 丢掉整条连接（后台会重连），剩下的记录放回队列。
            tsf_debug!(
                "上报上屏记录失败（{e}），剩下 {} 条留待重试",
                batch.len() - i
            );
            requeue(batch[i..].to_vec());
            *guard = None;
            return;
        }
    }
    tsf_debug!("已上报 {} 条上屏记录", batch.len());
}

/// 把记录放回队首（保持原来的先后顺序）。
fn requeue(items: Vec<(String, String, i64)>) {
    if items.is_empty() {
        return;
    }
    if let Ok(mut q) = PENDING_RECORDS.lock() {
        for it in items.into_iter().rev() {
            if q.len() >= RECORD_QUEUE_CAP {
                // 已经满了：放不回去的只能丢，记一笔。
                RECORDS_DROPPED.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            q.push_front(it);
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 拉起同目录下的 `heinrich-server.exe`。
///
/// **不读注册表**：setup 已经保证 dist 里 `heinrich_win.dll` 与
/// `heinrich-server.exe` 同级，DLL 用 `GetModuleFileName` 就能算出来。
fn spawn_server() {
    let Some(exe) = server_exe_path() else {
        tsf_error!("找不到 heinrich-server.exe（与 DLL 同目录）");
        return;
    };
    // CREATE_NO_WINDOW：server 没有控制台需求，绝不能给用户闪一个黑窗口。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new(&exe)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => tsf_info!("已拉起 server（pid {}）：{}", child.id(), exe.display()),
        Err(e) => {
            if !SPAWN_FAILED_LOGGED.swap(true, Ordering::Relaxed) {
                tsf_error!("拉起 server 失败：{e}（{}）", exe.display());
            }
        }
    }
}

fn server_exe_path() -> Option<PathBuf> {
    let dir = dll_path()?.parent()?.to_path_buf();
    let exe = dir.join("heinrich-server.exe");
    if exe.exists() {
        Some(exe)
    } else {
        None
    }
}

/// 本 DLL 的完整路径。
///
/// 不能直接用 `GetModuleFileNameW(None)`——那给的是**宿主进程的 exe**
/// （notepad++.exe），不是我们。必须先用 `GetModuleHandleExW(FROM_ADDRESS)`
/// 拿自己的 HMODULE。
fn dll_path() -> Option<PathBuf> {
    let mut hmod = Default::default();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(marker_address()),
            &mut hmod,
        )
    }
    .ok()?;

    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetModuleFileNameW(Some(hmod), &mut buf) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

/// 取一个「一定位于本 DLL 内」的地址，供 `GetModuleHandleExW` 反查模块。
fn marker_address() -> *const u16 {
    dll_path as *const u16
}

fn pipe_name() -> String {
    let mut sid = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut sid);
    }
    format!(r"\\.\pipe\heinrich-{sid}")
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// 候选缓存
// ---------------------------------------------------------------------------

/// 候选缓存的全部状态。
///
/// 合在一个结构里而不是拆成几个 `static`：`query` 要做「判断能不能用这份缓存 →
/// 清脏 → 取」，这几步必须在同一个视图下完成，否则两个会话线程会互相踩。
#[derive(Default)]
struct CacheState {
    map: HashMap<String, Cached>,
    /// 上屏后标脏的码——排序已过时，但连打期间先不刷新（D57）。
    stale: HashSet<String>,
    /// 上一次**提交**的码（`record` 时才更新）。
    ///
    /// 判据是「提交过」而不是「查过」：D55 用「查了别的码」当信号，结果打 `fcu`
    /// 之后第一个字母 `f` 就已经算是「别的码」，脏标记当场被清掉，连打期间位置
    /// 照样会跳。提交才是那个真正改变了学习数据的动作。
    last_commit: Option<String>,
}

/// 一条缓存：候选 + 什么时候取回来的（[`CACHE_TTL`] 要按它算年龄）。
struct Cached {
    reply: Reply,
    at: Instant,
}

fn cache_get(code: &str) -> Option<Reply> {
    let now = Instant::now();
    let mut g = CACHE.lock().ok()?;
    let st = g.get_or_insert_with(CacheState::default);
    // 这条被提交过 ⇒ 它的排序可能已经变了。只有「提交后用户已经去打过别的码」
    // 才重问；否则就是连打同一个码，此时换顺序等于让手指按错位置。
    if st.stale.contains(code) && st.last_commit.as_deref() != Some(code) {
        st.stale.remove(code);
        st.map.remove(code);
        return None;
    }
    let hit = st.map.get(code)?;
    // 放太久就作废：这期间词库/配置可能被热重载过（见 CACHE_TTL 的说明）。
    if now.duration_since(hit.at) > CACHE_TTL {
        st.map.remove(code);
        st.stale.remove(code);
        return None;
    }
    Some(hit.reply.clone())
}

fn cache_put(code: &str, reply: &Reply) {
    if let Ok(mut g) = CACHE.lock() {
        let st = g.get_or_insert_with(CacheState::default);
        if st.map.len() >= CACHE_CAP {
            st.map.clear();
            st.stale.clear();
        }
        st.map.insert(
            code.to_string(),
            Cached {
                reply: reply.clone(),
                at: Instant::now(),
            },
        );
        // 刚查到的是一手数据，顺手把脏标记去掉。
        st.stale.remove(code);
    }
}

/// 上屏之后调用：把这个码的缓存**标脏**，等提交过别的码之后再真正丢掉。
///
/// 不能立刻 `remove`——那会让连打同一个码时每次查询都重新问 server，而 server
/// 那边刚好因为这次上屏改了顺序，于是用户还按着「2」的时候第 2 条已经变成别的
/// 词了（真机实测：连打「云」到第 12 次它上位，用户继续按 2 选到了「去」，反而
/// 把「云」打回第 2）。延迟到「提交过别的码」，排序改动仍然生效，只是不在连打
/// 中途生效。
fn cache_mark_stale(code: &str) {
    if let Ok(mut g) = CACHE.lock() {
        let st = g.get_or_insert_with(CacheState::default);
        st.stale.insert(code.to_string());
        st.last_commit = Some(code.to_string());
    }
}

/// 仅供排错：当前是否连上（`UnadviseSink` 之类的地方会用）。
pub fn is_connected() -> bool {
    CONN.try_lock().map(|g| g.is_some()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试之间共享 `CACHE` 这个 `static`，串行化一下（只影响这一个模块）。
    static SERIAL: Mutex<()> = Mutex::new(());

    fn reply(words: &[&str]) -> Reply {
        Reply {
            words: words.iter().map(|w| w.to_string()).collect(),
            codes: vec![String::new(); words.len()],
            hint_ats: vec![0; words.len()],
            pinyin: false,
            primary_len: words.len(),
        }
    }

    fn reset() {
        if let Ok(mut g) = CACHE.lock() {
            *g = None;
        }
    }

    /// D63：只有「管道都没连上」才拉起 server。被 `Bye` 拒绝时拉起一百次
    /// 也修不了版本不匹配——实测它每 5 s 空转出一个 server 进程。
    #[test]
    fn spawn_only_when_the_pipe_was_never_opened() {
        assert!(OpenFail::NoServer.should_spawn());
        assert_eq!(OpenFail::NoServer.retry_delay(), Duration::ZERO);

        assert!(!OpenFail::Rejected.should_spawn());
        assert_eq!(OpenFail::Rejected.retry_delay(), REJECT_RETRY);

        let broken = OpenFail::Broken;
        assert!(!broken.should_spawn());
        assert!(broken.retry_delay() > Duration::ZERO);
        assert!(
            broken.retry_delay() < REJECT_RETRY,
            "断在半路可能只是重启中，别等太久"
        );
    }

    /// D57 的核心：上屏标脏之后，**同一个码**继续命中旧缓存（连打不跳位），
    /// 只有「提交过别的码」之后回去才重问。
    #[test]
    fn stale_entry_survives_the_same_code_and_expires_after_another_commit() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();

        cache_put("fcu", &reply(&["去", "云"]));
        assert_eq!(
            cache_get("fcu").map(|r| r.words),
            Some(vec!["去".into(), "云".into()])
        );

        // 上屏一次：只标脏，不删。
        cache_mark_stale("fcu");
        assert!(
            cache_get("fcu").is_some(),
            "连打同一个码时必须继续用旧序，否则用户按着「2」而第 2 条换了人"
        );

        // 又打一次并上屏（还是同一个码）——依旧不刷新。
        cache_mark_stale("fcu");
        assert!(cache_get("fcu").is_some(), "连打期间顺序绝对不动");

        // 去打了别的码并上屏 ⇒ fcu 的脏条目才算真的过时。
        cache_mark_stale("vbg");
        assert!(
            cache_get("fcu").is_none(),
            "提交过别的码之后，回去才看到新序"
        );

        reset();
    }

    /// D55 → D57 修的就是这一条：**只是查了别的码**不该让脏条目失效。
    ///
    /// 打 `fcu` 之后第一个字母 `f` 在缓存里就是「另一个码」，D55 拿它当换码信号，
    /// 于是脏标记在用户还没提交任何东西的时候就被清掉了——连打照样跳位。
    #[test]
    fn querying_another_code_does_not_expire_it() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();

        cache_put("fcu", &reply(&["去", "云"]));
        cache_mark_stale("fcu");

        // 打下一个词时先输入的是 f、fc——它们都是「别的码」，但没有提交。
        assert!(cache_get("f").is_none(), "没查过的码本来就该 miss");
        assert!(cache_get("fc").is_none());
        assert!(
            cache_get("fcu").is_some(),
            "只查不提交，不算换码；否则连打期间照样跳位"
        );

        reset();
    }

    /// 从没被提交过的码永远有效——它的排序不会因为别人上屏而变。
    #[test]
    fn clean_entries_are_never_invalidated() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();

        cache_put("vbg", &reply(&["好"]));
        cache_put("fcu", &reply(&["去"]));
        cache_mark_stale("fcu");
        cache_mark_stale("hao");

        assert!(cache_get("vbg").is_some(), "没上屏过的码不该被清");
        assert!(cache_get("fcu").is_none(), "上屏过的那个才该清");

        reset();
    }

    /// 缓存放久了自己过期（[`CACHE_TTL`]）。
    ///
    /// 词库热重载没有推送通道，这份缓存是唯一会让「加完词再打同一个码」看不到新词的
    /// 地方（F45：加「微信」前打过 `tmwy`，加完再打还是四个生僻字，而且因为命中缓存，
    /// server 日志里连 `Query` 都没有）。
    #[test]
    fn entry_expires_after_ttl() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset();

        cache_put("tmwy", &reply(&["䄲", "𢓇"]));
        assert!(cache_get("tmwy").is_some(), "刚取回来的当然能用");

        // 直接把时间戳往回拨，免得测试真睡 3 秒
        if let Ok(mut g) = CACHE.lock() {
            let st = g.get_or_insert_with(CacheState::default);
            let e = st.map.get_mut("tmwy").expect("刚放进去的条目");
            e.at = Instant::now() - CACHE_TTL - Duration::from_millis(1);
        }
        assert!(
            cache_get("tmwy").is_none(),
            "超过有效期必须重问 server，否则加完词还是旧结果"
        );

        reset();
    }
}
