//! TSF UI 线程的自投递通知窗口（D101 重连补弹、D104 光标复量）。
//!
//! 有些动作不能当场做完，只能「等这一轮消息处理完再说」：
//!
//! | 消息 | 谁发起 | 为什么不能当场做 |
//! |---|---|---|
//! | [`WM_APP_RECONNECTED`] | 后台 IPC 线程 | 它不拥有 UI，不能碰候选窗 |
//! | [`WM_APP_RECHECK_CARET`] | UI 线程自己 | 按键那一刻宿主的排版还没落定（D104） |
//!
//! 复量不再「量到不同的值就挪窗」（D105）：它也要过 `gate` 那道闸门，否则被拒的
//! 错值会在下一轮消息里被原样捡回来——D105 的现场就是这样，复量两次数值一模一样。
//!
//! 两条路都落在同一个 message-only 窗口上：`PostMessageW` 把消息排进宿主的消息
//! 队列，宿主的消息泵派发回 UI 线程，窗口过程里再调 `TextService` 的对应入口。
//!
//! 为什么不用 `PostThreadMessageW`：线程消息虽然不需要窗口，但宿主的消息循环
//! 千差万别（有的走 `IsDialogMessage`、有的过滤无窗口消息），丢在哪一环都无从
//! 排查；message-only 窗口是 Win32 里「跨线程投递回本线程」的标准做法，宿主
//! 的 `GetMessage` / `DispatchMessage` 天然会处理它。
//!
//! ## 与 D55 的关系
//!
//! D55 禁止「同一码的候选序列中途被替换」（连打顺序必须稳定）。补弹只发生在
//! **当前没有任何候选窗**的时刻——是从无到有，不是替换；判定在
//! `Core::on_reconnected`（text_service.rs）里做。复量只挪窗口、不换候选，
//! 与 D55 不冲突。

use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Once;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, RegisterClassW,
    SetWindowLongPtrW, CREATESTRUCTW, GWLP_USERDATA, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_NCCREATE, WM_NCDESTROY, WNDCLASSW,
};

/// 后台线程 → TSF UI 线程：「重连成功，若缓冲非空且无候选窗，补弹」。
pub const WM_APP_RECONNECTED: u32 = WM_APP + 1;
/// UI 线程 → 自己：「闸门没收下这次量到的位置，下一轮消息里再量一次」（D104/D105）。
pub const WM_APP_RECHECK_CARET: u32 = WM_APP + 2;

const CLASS_NAME: PCWSTR = w!("HeinrichNotifyV1");

static REGISTER_ONCE: Once = Once::new();
static CLASS_ATOM: AtomicU16 = AtomicU16::new(0);

/// message-only 通知窗。每个 `TextService` 活跃实例一个，`Activate` 建、
/// `Deactivate` 销毁；调用方只拿它的 `HWND` 值去 `PostMessageW`。
pub struct NotifyWindow {
    hwnd: HWND,
    /// 与窗口共生命周期的 `Weak<Core>`（`WM_NCCREATE` 存进 `GWLP_USERDATA`）。
    /// `Drop` 在销毁窗口**之后**回收，窗口过程不会摸到悬垂指针。
    userdata: *mut Weak<crate::text_service::Core>,
}

// 只在拥有它的 UI 线程上被触碰（窗口消息只派发给属主线程），跨线程仅传递
// `hwnd` 这个整数——与 `ipc.rs` 的 `HANDLE` 同一性质。
unsafe impl Send for NotifyWindow {}

impl NotifyWindow {
    /// 建一个不可见的 message-only 窗口。必须在 TSF UI 线程上调用。
    pub fn new(core: &Rc<crate::text_service::Core>) -> Result<Self> {
        register_class()?;
        let hinstance = unsafe { GetModuleHandleW(None)? };
        let userdata = Box::into_raw(Box::new(Rc::downgrade(core)));
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!("亨利通知窗"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(HINSTANCE(hinstance.0)),
                // 窗口过程在 `WM_NCCREATE` 里把它存进 `GWLP_USERDATA`。
                Some(userdata.cast::<core::ffi::c_void>()),
            )
        }?;
        Ok(Self { hwnd, userdata })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

impl Drop for NotifyWindow {
    fn drop(&mut self) {
        // 先销毁窗口：窗口过程还持有 userdata 裸指针，顺序反了就是 use-after-free。
        if !self.hwnd.0.is_null() {
            let _ = unsafe { DestroyWindow(self.hwnd) };
        }
        // 窗口创建失败时 userdata 没被 `WM_NCCREATE` 认领过，也没人回收它——就地丢弃。
        if !self.userdata.is_null() {
            unsafe { drop(Box::from_raw(self.userdata)) };
            self.userdata = std::ptr::null_mut();
        }
    }
}

fn register_class() -> Result<()> {
    REGISTER_ONCE.call_once(|| {
        let Ok(hinstance) = (unsafe { GetModuleHandleW(None) }) else {
            return;
        };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(notify_wndproc),
            hInstance: HINSTANCE(hinstance.0),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        let registered = unsafe { RegisterClassW(&wc) };
        CLASS_ATOM.store(registered, Ordering::Release);
    });
    if CLASS_ATOM.load(Ordering::Acquire) == 0 {
        return Err(windows::core::Error::from(
            windows::Win32::Foundation::E_FAIL,
        ));
    }
    Ok(())
}

/// 把消息交给 `Core`。升级失败（实例已销毁）就当没发生——这是**唯一**
/// 允许的失败方式：窗口过程跨 `extern "system"` 边界，任何 panic 都会带走宿主进程。
fn with_core(hwnd: HWND, action: impl FnOnce(&Rc<crate::text_service::Core>)) {
    let ptr =
        unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Weak<crate::text_service::Core>;
    let Some(weak) = (unsafe { ptr.as_ref() }) else {
        return;
    };
    let Some(core) = weak.upgrade() else {
        return;
    };
    action(&core);
}

unsafe extern "system" fn notify_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
            LRESULT(1)
        }
        WM_APP_RECONNECTED => {
            with_core(hwnd, |core| core.on_reconnected());
            LRESULT(0)
        }
        WM_APP_RECHECK_CARET => {
            with_core(hwnd, |core| core.on_recheck_caret());
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // 只清指针、不回收 Box：Box 的所有权在 `NotifyWindow` 手里，
            // 它的 `Drop` 会在 `DestroyWindow` 返回后统一回收。
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
