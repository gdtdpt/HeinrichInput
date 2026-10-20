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

use std::collections::HashMap;
use std::io;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

use heinrich_proto::{body_len, Entry, Frame, Mode, MAX_ENTRIES, PROTO_VERSION};

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

/// 本地候选缓存条数上限。满了直接清空——这是个纯粹的热路径优化，
/// 不需要 LRU 的精确性。
const CACHE_CAP: usize = 64;

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
static CACHE: Mutex<Option<HashMap<String, Vec<Entry>>>> = Mutex::new(None);
static PENDING_MODE: Mutex<Option<Mode>> = Mutex::new(None);
static WORKER: Once = Once::new();
static SPAWN_FAILED_LOGGED: AtomicBool = AtomicBool::new(false);

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

/// 查一个码的候选。**永不阻塞超过 [`KEY_BUDGET`]**。
///
/// 返回 `None` 表示「没问到」（server 不可用 / 超时 / 后台正在重连）——
/// 调用方应当把编码原样上屏。返回 `Some(vec![])` 表示「问到了，没这个词」。
pub fn query(code: &str) -> Option<Vec<Entry>> {
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
        Frame::Result { entries, .. } => {
            cache_put(code, &entries);
            Some(entries)
        }
        other => {
            tsf_debug!("查询 {code} 收到意外应答 {other:?}");
            None
        }
    }
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
        Frame::Welcome { mode, .. } => Some(mode == Mode::Chinese),
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

fn request(frame: &Frame, budget: Duration) -> Option<Frame> {
    // try_lock 是关键：后台线程可能正拿着锁重连，键路径绝不能在这里等。
    let mut guard = CONN.try_lock().ok()?;
    let conn = guard.as_ref()?;
    match exchange(conn, frame, budget) {
        Ok(f) => Some(f),
        Err(e) => {
            tsf_debug!("IPC 往返失败（{e}），丢弃连接等后台重连");
            // 丢掉整条连接：超时后管道里可能残留半个响应，复用会读到错位的帧。
            *guard = None;
            None
        }
    }
}

fn exchange(conn: &Conn, req: &Frame, budget: Duration) -> io::Result<Frame> {
    let deadline = Instant::now() + budget;
    write_all(conn, &req.encode(), deadline)?;

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
        ensure_connected(&mut last_spawn);
        flush_pending_mode();
        std::thread::sleep(WORKER_TICK);
    }
}

fn ensure_connected(last_spawn: &mut Option<Instant>) {
    // 只在无人持锁且确实空着时动手；拿不到锁说明键路径正在用，下一 tick 再说。
    let need = match CONN.try_lock() {
        Ok(g) => g.is_none(),
        Err(_) => false,
    };
    if !need {
        return;
    }

    if let Some(conn) = open() {
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
        return;
    }

    // 连不上 ⇒ 大概率 server 没跑。拉起它，但别太勤。
    let may_spawn = last_spawn.is_none_or(|t| t.elapsed() >= SPAWN_BACKOFF);
    if may_spawn {
        *last_spawn = Some(Instant::now());
        spawn_server();
    }
}

/// 建连接并完成握手。失败返回 `None`（调用方决定要不要拉起 server）。
fn open() -> Option<Conn> {
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
            let code = win32_code(&e);
            // 管道不存在 / 全忙都是「server 还没准备好」，属于正常路径，不写日志刷屏。
            if code != ERROR_FILE_NOT_FOUND.0 && code != ERROR_PIPE_BUSY.0 {
                tsf_debug!("打开管道 {name} 失败：{e}");
            }
            return None;
        }
    };

    let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok();
    let Some(event) = event else {
        unsafe {
            let _ = CloseHandle(pipe);
        }
        return None;
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
            entry_count, mode, ..
        }) => {
            tsf_info!(
                "已连上 server（pid 未知，{} 条词条，{}）",
                entry_count,
                if mode == Mode::Chinese {
                    "中文"
                } else {
                    "英文"
                }
            );
            Some(conn)
        }
        Ok(Frame::Bye { reason }) => {
            // 方案或协议不匹配：不是重连能解决的问题，记一条错误级的日志。
            tsf_error!("server 拒绝握手：{reason}");
            None
        }
        Ok(other) => {
            tsf_error!("握手收到意外应答：{other:?}");
            None
        }
        Err(e) => {
            tsf_debug!("握手失败：{e}");
            None
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

fn cache_get(code: &str) -> Option<Vec<Entry>> {
    CACHE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(code).cloned()))
}

fn cache_put(code: &str, entries: &[Entry]) {
    if let Ok(mut g) = CACHE.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        if map.len() >= CACHE_CAP {
            map.clear();
        }
        map.insert(code.to_string(), entries.to_vec());
    }
}

/// 仅供排错：当前是否连上（`UnadviseSink` 之类的地方会用）。
pub fn is_connected() -> bool {
    CONN.try_lock().map(|g| g.is_some()).unwrap_or(false)
}
