//! 命名管道服务端（Windows 传输层）。
//!
//! 管道名 `\\.\pipe\heinrich-<SessionId>`：**按会话隔离**。多用户同时登录时各自
//! 有独立的 server 与管道，谁也不会把候选查到别人的会话里去。
//!
//! 单实例靠同名互斥体 `Local\HeinrichServer-<SessionId>`：`Local\` 本身就是会话
//! 作用域，再带上会话 id 只是把意图写明白——别的会话不该看到这个名字。
//!
//! ## 不做的事
//!
//! - **不阻塞客户端**：这里所有等待都在 server 自己的线程里。客户端的超时由 DLL
//!   侧用重叠 I/O 实现（D18）。
//! - **不做 ACL 定制**：默认 DACL 只允许创建者（以及管理员/系统）访问，而管道名
//!   已经按会话隔离，M1.3 够用。M4 做设置界面时若要「服务化」再补显式安全描述符。
//! - **不退出在「没人连」以外的场合**：`--idle-exit` 到点且连接数为 0 才退。

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::windows::io::{FromRawHandle, IntoRawHandle};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, HANDLE,
};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};

use heinrich_proto::{body_len, Frame};

use crate::host::Host;

/// `HANDLE` 本身不实现 `Send`（windows-rs 不替我们担保跨线程安全）。
///
/// 这里担保：每个句柄只交给**一个**线程使用（一连接一线程），
/// 不存在两个线程同时读写同一个管道实例的情况。
struct PipeHandle(HANDLE);

unsafe impl Send for PipeHandle {}

/// 管道与互斥体共用的名字后缀，保证两者总是同一个会话。
fn session_suffix() -> String {
    let mut sid = 0u32;
    unsafe {
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut sid);
    }
    sid.to_string()
}

fn pipe_name() -> String {
    format!(r"\\.\pipe\heinrich-{}", session_suffix())
}

/// 共享活动计数：空闲退出用。
#[derive(Default)]
struct Activity {
    /// 当前连接数。
    live: AtomicUsize,
    /// 最近一次活动的 Unix 毫秒。
    last_ms: AtomicU64,
    /// 累计接受过的连接数（日志里看「DLL 有没有来连过」）。
    accepted: AtomicU64,
}

impl Activity {
    fn touch(&self) {
        self.last_ms.store(now_ms(), Ordering::Relaxed);
    }

    fn idle_ms(&self) -> u64 {
        now_ms().saturating_sub(self.last_ms.load(Ordering::Relaxed))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct Config {
    /// 无连接持续多久后退出。0 表示不自动退出。
    pub idle_exit: Duration,
    /// 每次查询前故意睡多久（`--debug-sleep`）。用来验 D18：server 卡住时宿主不能卡。
    pub debug_sleep: Duration,
}

/// 跑起来，直到空闲退出或被结束。返回进程退出码。
pub fn run(host: Host, cfg: Config) -> i32 {
    let suffix = session_suffix();
    let name = pipe_name();

    let Some(_guard) = single_instance(&format!(r"Local\HeinrichServer-{suffix}")) else {
        srv_info!("已有同会话实例在跑，本进程退出");
        return 0;
    };

    srv_info!(
        "启动 pid={} 管道={} 方案={} 词条={} 模式={} 空闲退出={:?} 调试延迟={:?}",
        std::process::id(),
        name,
        host.scheme_id(),
        host.entry_count(),
        match host.mode() {
            heinrich_proto::Mode::Chinese => "中文",
            heinrich_proto::Mode::English => "英文",
        },
        cfg.idle_exit,
        cfg.debug_sleep
    );

    let shared = Arc::new(Mutex::new(host));
    let activity = Arc::new(Activity::default());
    activity.touch();

    if !cfg.idle_exit.is_zero() {
        spawn_monitor(Arc::clone(&activity), cfg.idle_exit, Arc::clone(&shared));
    }

    let debug_sleep = cfg.debug_sleep;
    loop {
        let Some(handle) = create_pipe(&name) else {
            srv_error!("创建管道失败（{name}）");
            return 1;
        };
        if !connect(&handle) {
            srv_warn!("等待客户端连接失败");
            unsafe {
                let _ = CloseHandle(handle);
            }
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        activity.accepted.fetch_add(1, Ordering::Relaxed);
        activity.live.fetch_add(1, Ordering::Relaxed);
        activity.touch();
        srv_debug!(
            "客户端已连接（第 {} 个，当前 {} 个）",
            activity.accepted.load(Ordering::Relaxed),
            activity.live.load(Ordering::Relaxed)
        );

        let shared = Arc::clone(&shared);
        let activity_for_thread = Arc::clone(&activity);
        // 每个连接一个线程：DLL 是**每进程**一份客户端，Chrome 十来个进程就是
        // 十来条连接，串行处理会让一个 `--debug-sleep` 卡住所有宿主。
        let handle = PipeHandle(handle);
        thread::spawn(move || {
            serve(handle, &shared, debug_sleep, &activity_for_thread);
            activity_for_thread.live.fetch_sub(1, Ordering::Relaxed);
            activity_for_thread.touch();
            srv_debug!(
                "客户端断开，剩余连接 {}",
                activity_for_thread.live.load(Ordering::Relaxed)
            );
        });
    }
}

/// 取单实例互斥体。已有实例时返回 `None`。
fn single_instance(name: &str) -> Option<HANDLE> {
    let wide = wide(name);
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }.ok()?;
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }
    // 句柄随进程存活，故意不关（进程退出时内核自动回收）。
    Some(handle)
}

fn create_pipe(name: &str) -> Option<HANDLE> {
    let wide = wide(name);
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        None
    } else {
        Some(handle)
    }
}

/// 等服务端连上。阻塞——但只阻塞 server 自己的线程。
fn connect(handle: &HANDLE) -> bool {
    match unsafe { ConnectNamedPipe(*handle, None) } {
        Ok(()) => true,
        // 客户端在 ConnectNamedPipe 之前就连上了，系统报 ERROR_PIPE_CONNECTED，
        // 这不是失败，是「已经连好了」。
        // （`Error::code()` 是 `HRESULT_FROM_WIN32` 包装过的，取低 16 位才是系统错误码。）
        Err(e) => (e.code().0 as u32) & 0xFFFF == ERROR_PIPE_CONNECTED.0,
    }
}

fn serve(handle: PipeHandle, shared: &Mutex<Host>, debug_sleep: Duration, activity: &Activity) {
    let handle = handle.0;
    // 把裸句柄交给 std 的 File 只是为了能用 Read/Write；所有权仍归本函数，
    // 结尾用 into_raw_handle 取回来继续 Disconnect/复用。
    let mut file = unsafe { File::from_raw_handle(handle.0) };
    // 字根表每条连接只发一次：客户端断线重连会开一条新连接，那时再给。
    let mut roots_sent = false;

    loop {
        let frame = match read_frame(&mut file) {
            Ok(f) => f,
            Err(e) if is_disconnect(&e) => break,
            Err(e) => {
                srv_warn!("读帧失败：{e}");
                break;
            }
        };
        activity.touch();
        srv_debug!("收到 {frame:?}");

        let reply = {
            // 先取锁再睡：`--debug-sleep` 要模拟的是「server 卡住」，锁外睡就假了。
            let mut host = match shared.lock() {
                Ok(g) => g,
                Err(poisoned) => {
                    // 某个连接线程 panic 过。宿主数据还能用，继续服务。
                    srv_warn!("宿主状态被污染，继续使用");
                    poisoned.into_inner()
                }
            };
            if !debug_sleep.is_zero() {
                srv_debug!("按 --debug-sleep 睡 {:?}", debug_sleep);
                thread::sleep(debug_sleep);
            }
            let reply = host.handle(frame);
            // 握手成功后紧跟一帧字根表（v5 起）。**必须在同一次持锁里取**：
            // `roots_frame` 只是读，但它和 `handle` 之间若松开锁，另一条连接的
            // 线程可能插进来，那时再看 `roots_sent` 已经是别人的决定了。
            let roots = if !roots_sent && matches!(reply, Some(Frame::Welcome { .. })) {
                host.roots_frame()
            } else {
                None
            };
            (reply, roots)
        };
        let (reply, roots) = reply;

        let Some(reply) = reply else { continue };
        srv_debug!("回 {reply:?}");
        let bytes = reply.encode();
        if let Err(e) = file.write_all(&bytes).and_then(|()| file.flush()) {
            srv_warn!("写回失败：{e}");
            break;
        }
        activity.touch();

        // 字根表跟在 `Welcome` **后面**发：客户端握手是「发 Hello、读一帧」，
        // 先发它会让 `open()` 把字根表当成握手应答（`握手收到意外应答`）。
        // 放在后面，那帧就静静躺在管道里，等客户端下一次读应答时跳过去（ipc.rs）。
        if let Some(roots) = roots {
            let bytes = roots.encode();
            if let Err(e) = file.write_all(&bytes).and_then(|()| file.flush()) {
                srv_warn!("字根表发送失败：{e}");
                break;
            }
            roots_sent = true;
            srv_debug!("已下发字根表 {} 字节", bytes.len());
        }

        if matches!(reply, Frame::Bye { .. }) {
            // 给客户端留出**读走 `Bye`** 的时间（D63）。立刻断管道的话，客户端那头
            // 的读会以 ERROR_NO_DATA 收场——它永远看不到拒绝原因，日志里只有一行
            // 莫名的「握手失败」，而真正的原因（协议版本不匹配）就埋没了。
            // 本机管道一个来回远小于 100 ms，也不至于让 server 卡在一个死连接上。
            std::thread::sleep(std::time::Duration::from_millis(100));
            break;
        }
    }

    let raw = file.into_raw_handle();
    let handle = HANDLE(raw);
    unsafe {
        let _ = DisconnectNamedPipe(handle);
        // 句柄不关：主循环要拿它继续 `ConnectNamedPipe` 接下一个客户端。
        // （`create_pipe` 只在一个实例彻底不可用时才会重新建。）
    }
}

fn read_frame(file: &mut File) -> std::io::Result<Frame> {
    let mut header = [0u8; 4];
    file.read_exact(&mut header)?;
    let len =
        body_len(header).map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e.to_string()))?;
    let mut whole = Vec::with_capacity(4 + len);
    whole.extend_from_slice(&header);
    whole.resize(4 + len, 0);
    file.read_exact(&mut whole[4..])?;
    Frame::decode(&whole).map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e.to_string()))
}

fn is_disconnect(e: &std::io::Error) -> bool {
    // 客户端被强杀时读取会以 broken pipe / 对端关闭的形式失败；
    // 这些都只是「连接没了」，不是 server 的问题。
    matches!(
        e.kind(),
        ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
    )
}

fn spawn_monitor(activity: Arc<Activity>, idle_exit: Duration, shared: Arc<Mutex<Host>>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(2));
        if activity.live.load(Ordering::Relaxed) != 0 {
            continue;
        }
        let idle = activity.idle_ms();
        if idle >= idle_exit.as_millis() as u64 {
            srv_info!("空闲 {idle} ms，退出");
            // 退出前把 WAL 落干净（§5.5）。这一步要在 `process::exit` **之前**做完：
            // exit 不走析构，SQLite 连接不会自己 close。
            match shared.lock() {
                Ok(mut h) => h.shutdown(),
                Err(p) => p.into_inner().shutdown(),
            }
            std::process::exit(0);
        }
    });
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
