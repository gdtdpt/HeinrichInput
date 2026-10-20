//! TSF 文本服务（M1.0 版）。
//!
//! ## 本阶段只做一件事
//!
//! 「证明注册对了、接口接上了、按键进得来」。所以：
//!
//! - `OnTestKeyDown` / `OnKeyDown` **一律返回 FALSE（不消费）**，
//!   用户在记事本里打字照常出英文——这是**正确的中间态**，不是 bug。
//! - 每次按键写一行日志，验证 vkey 真的到了。
//! - 组合串、候选窗、server 通信全部不做（M1.1 起逐步加）。
//!
//! 好处是把「注册对不对」和「接口写对没」分开诊断：注册错了语言列表里没名字，
//! 接口错了日志里没按键，两种故障不会混在一起。

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};

use windows::core::{implement, ComObjectInterface, Interface, Ref, Result, BOOL, GUID};
use windows::Win32::Foundation::{FALSE, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CAPITAL, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfContext, ITfDocumentMgr, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl,
    ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl, TF_INVALID_COOKIE,
};

/// 实例序号，只用于日志区分「第几个宿主进程里的实例」。
static INSTANCE_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 一个宿主进程里的一份文本服务实例。
///
/// 状态**必须**是实例内的：DLL 被注入到每个进程都会 new 一个，
/// 如果放全局，A 窗口的缓冲就会跑到 B 窗口去（预研坑 #5 的根因）。
#[implement(ITfTextInputProcessorEx, ITfKeyEventSink, ITfThreadMgrEventSink)]
pub struct TextService {
    /// 本次激活的线程 id，撤按键 sink 时需要它。
    tid: Cell<u32>,
    /// 线程管理器，`Deactivate` 时用来撤 sink。
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    /// `ITfSource::AdviseSink` 返回的 cookie。
    thread_mgr_cookie: Cell<u32>,
    /// 按键 sink 是否已挂上。
    keysink_advertised: Cell<bool>,
    /// 实例序号。
    seq: u32,
}

impl TextService {
    pub fn new() -> Self {
        let seq = INSTANCE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        Self {
            tid: Cell::new(0),
            thread_mgr: RefCell::new(None),
            thread_mgr_cookie: Cell::new(TF_INVALID_COOKIE),
            keysink_advertised: Cell::new(false),
            seq,
        }
    }
}

impl Default for TextService {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 挂 sink / 撤 sink
//
// 这两个方法放在**宏生成的 `TextService_Impl`** 上，而不是 `TextService` 上：
// `as_interface_ref()`（把自己当 IUnknown 交给系统）只在 `_Impl` 上才有。
// 字段访问靠 `_Impl` 对 `TextService` 的 Deref 自动穿透。
// ---------------------------------------------------------------------------

impl TextService_Impl {
    fn activate_inner(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        self.tid.set(tid);

        unsafe {
            let thread_mgr = ptim.ok()?;

            // 焦点 sink：M1.0 只观察；M1.1 起用来清缓冲，
            // 否则在 A 窗口打的字会跑到 B 窗口（预研坑 #5）。
            let source: ITfSource = thread_mgr.cast()?;
            let cookie = source.AdviseSink(&ITfThreadMgrEventSink::IID, self.as_interface_ref())?;
            self.thread_mgr_cookie.set(cookie);

            // 按键 sink：M1.0 唯一真正关心的东西。
            let keystroke_mgr: ITfKeystrokeMgr = thread_mgr.cast()?;
            keystroke_mgr.AdviseKeyEventSink(tid, self.as_interface_ref(), true)?;
            self.keysink_advertised.set(true);

            *self.thread_mgr.borrow_mut() = Some(thread_mgr.clone());
        }

        tsf_info!(
            "#{} Activate tid={} pid={} —— 按键 sink 与焦点 sink 已挂",
            self.seq,
            tid,
            std::process::id()
        );
        Ok(())
    }

    fn deactivate_inner(&self) -> Result<()> {
        let Some(thread_mgr) = self.thread_mgr.borrow_mut().take() else {
            // 没激活过就 Deactivate：TSF 允许这么调。
            return Ok(());
        };

        unsafe {
            if self.keysink_advertised.get() {
                let keystroke_mgr: ITfKeystrokeMgr = thread_mgr.cast()?;
                if let Err(e) = keystroke_mgr.UnadviseKeyEventSink(self.tid.get()) {
                    // 清理路径失败只记日志，不向上抛：抛 Err 会让 TSF
                    // 认为 Deactivate 失败（预研坑 #6）。
                    tsf_error!("#{} UnadviseKeyEventSink 失败：{e}", self.seq);
                }
                self.keysink_advertised.set(false);
            }

            let cookie = self.thread_mgr_cookie.get();
            if cookie != TF_INVALID_COOKIE {
                let source: ITfSource = thread_mgr.cast()?;
                if let Err(e) = source.UnadviseSink(cookie) {
                    tsf_error!("#{} UnadviseSink 失败：{e}", self.seq);
                }
                self.thread_mgr_cookie.set(TF_INVALID_COOKIE);
            }
        }

        tsf_info!("#{} Deactivate pid={}", self.seq, std::process::id());
        Ok(())
    }
}

/// 拦截 panic。
///
/// **这一层不是可选的**：`panic = "abort"` 会让宿主进程直接消失；
/// 即使改成 unwind，panic 跨出 `extern "system"` 边界也是未定义行为，
/// Rust 会直接 abort。所以每个 COM 方法体都必须包在这里面。
fn guarded<T>(what: &'static str, fallback: T, body: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(v) => v,
        Err(_) => {
            tsf_error!("panic 被拦截于 {}，返回降级值", what);
            fallback
        }
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        guarded("Activate", Ok(()), || {
            // 失败也不抛 Err：TSF 一旦认为初始化失败，可能再也不给激活机会。
            let _ = self.activate_inner(ptim, tid);
            Ok(())
        })
    }

    fn Deactivate(&self) -> Result<()> {
        guarded("Deactivate", Ok(()), || {
            let _ = self.deactivate_inner();
            Ok(())
        })
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, dwflags: u32) -> Result<()> {
        tsf_debug!("#{} ActivateEx flags={dwflags:#x}", self.seq);
        // Win8+ 走这条；不转调 Activate 会漏掉一次初始化。
        self.Activate(ptim, tid)
    }
}

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    fn OnInitDocumentMgr(&self, pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        tsf_debug!("#{} OnInitDocumentMgr null={}", self.seq, pdim.is_null());
        Ok(())
    }

    fn OnUninitDocumentMgr(&self, pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        tsf_debug!("#{} OnUninitDocumentMgr null={}", self.seq, pdim.is_null());
        Ok(())
    }

    fn OnSetFocus(
        &self,
        pdimfocus: Ref<ITfDocumentMgr>,
        pdimprevfocus: Ref<ITfDocumentMgr>,
    ) -> Result<()> {
        tsf_debug!(
            "#{} OnSetFocus focus_null={} prev_null={}",
            self.seq,
            pdimfocus.is_null(),
            pdimprevfocus.is_null()
        );
        // M1.1 在这里清缓冲 + 结束组合串。
        Ok(())
    }

    fn OnPushContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        tsf_debug!("#{} OnPushContext", self.seq);
        Ok(())
    }

    fn OnPopContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        tsf_debug!("#{} OnPopContext", self.seq);
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, foreground: BOOL) -> Result<()> {
        tsf_debug!(
            "#{} KeyEventSink::OnSetFocus foreground={}",
            self.seq,
            foreground.as_bool()
        );
        Ok(())
    }

    /// 与 `OnKeyDown` 必须返回同一个判定结果，否则会出现
    /// 「键被吃掉但没输入」或「输入两次」（预研坑 #2）。
    ///
    /// M1.0 两者都返回 FALSE：不接管任何键。
    fn OnTestKeyDown(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnTestKeyDown", Ok(FALSE), || {
            log_key(seq, "test-down", wparam, lparam);
            Ok(FALSE)
        })
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnTestKeyUp", Ok(FALSE), || {
            log_key(seq, "test-up", wparam, lparam);
            Ok(FALSE)
        })
    }

    fn OnKeyDown(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnKeyDown", Ok(FALSE), || {
            log_key(seq, "down", wparam, lparam);
            // M1.0：不消费。
            Ok(FALSE)
        })
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnKeyUp", Ok(FALSE), || {
            log_key(seq, "up", wparam, lparam);
            Ok(FALSE)
        })
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        Ok(FALSE)
    }
}

/// 把一次按键写进日志。`lparam` 按 Win32 位域解读：bit 30 = 之前是否已按下。
fn log_key(seq: u32, kind: &str, wparam: WPARAM, lparam: LPARAM) {
    let vk = wparam.0 as u16;
    let repeat = (lparam.0 >> 30) & 1 == 1;
    // M1.0 刻意用 info 级：这一阶段唯一的验收标准就是「日志里能看到每个 vkey」。
    // 若压在 debug 级，用户必须给每个宿主进程单独设 HEINRICH_TSF_LOG 才看得到，
    // 验收等于做不成。M1.1 有了真正的组合串之后降回 debug。
    tsf_info!(
        "#{seq} {kind} vk={vk:#04x} {} {} {}",
        describe_vk(vk),
        modifiers(),
        if repeat { "REPEAT" } else { "" }
    );
}

/// 修饰键状态。`GetKeyState` 读本线程输入队列，开销可忽略。
fn modifiers() -> String {
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) < 0 };
    let mut s = String::new();
    for (vk, ch) in [(VK_CONTROL, 'C'), (VK_MENU, 'A'), (VK_SHIFT, 'S')] {
        if down(vk) {
            s.push(ch);
        }
    }
    if unsafe { GetKeyState(VK_CAPITAL.0 as i32) & 1 == 1 } {
        s.push('L');
    }
    if s.is_empty() {
        "-".to_string()
    } else {
        s
    }
}

/// vkey → 人能读的名字。M1.0 全靠这个看「键到底进没进来」。
fn describe_vk(vk: u16) -> String {
    match vk {
        0x41..=0x5A => format!("'{}'", (b'A' + (vk - 0x41) as u8) as char),
        0x30..=0x39 => format!("'{}'", (b'0' + (vk - 0x30) as u8) as char),
        0x20 => "SPACE".to_string(),
        0x0D => "ENTER".to_string(),
        0x1B => "ESC".to_string(),
        0x08 => "BACKSPACE".to_string(),
        0x09 => "TAB".to_string(),
        0x14 => "CAPSLOCK".to_string(),
        0x10 => "SHIFT".to_string(),
        0x11 => "CTRL".to_string(),
        0x12 => "ALT".to_string(),
        0xBA => "OEM_1(;)".to_string(),
        0xDE => "OEM_7(')".to_string(),
        0x2E => "DELETE".to_string(),
        other => format!("({other:#04x})"),
    }
}
