//! TSF 文本服务（M1.1 版）。
//!
//! ## 本阶段做到什么
//!
//! 「字上得去」：缓冲 `a`–`y`，空格 / 回车把命中字经 `ITfEditSession` 写进文档。
//! 组合串（虚线、光标跟随）与候选窗仍不做，那是 M1.2。
//!
//! ## 实测：宿主只走其中一条回调路径（M1.0 真机日志）
//!
//! 同一份 DLL、同一台机器，两个宿主的按键事件落在**不同的方法**上：
//!
//! | 宿主 | 实际收到的回调 |
//! |---|---|
//! | Notepad++（Scintilla） | 只有 `OnTestKeyDown` / `OnTestKeyUp` |
//! | WorkBuddy（Electron/Chromium） | 只有 `OnKeyDown` / `OnKeyUp` |
//!
//! 这不是巧合，是两种键路由风格：前者先问「你要不要这个键」，不要就自己处理、
//! 不再回调；后者直接调 `ITfKeystrokeMgr::KeyDown`。
//!
//! ⇒ 所以两条路径都调 `keys::decide()`（同一个纯函数），**答案必须一致**。
//! 这也正是 dev-guide 第 12 条「吃键集 ⊆ 出字集」的实机证据：`OnTestKeyDown`
//! 返回 TRUE 的那一刻键就已经被吃掉了，没有事后反悔的机会。
//!
//! ## 两条路径的分工
//!
//! - `OnTestKey*`：**只回答吃不吃，不碰编码缓冲**。它是「问句」，同一个键可能被问多次。
//! - `OnKey*`：**真正干活**（改缓冲、上屏）。
//!
//! 这样即使某个宿主「先问、再调」，也不会把同一个键处理两遍。
//!
//! 唯一的例外是 Shift 采样状态机（`sync_shift`）：**四条回调都要调**。走 test 路径
//! 的宿主（Notepad++）永远不调 `OnKeyDown`，把置位动作放在那里等于这一半逻辑
//! 不存在——M1.1 第一版就是这么错的，Shift 切换在 Notepad++ 里完全失效。
//! 状态机本身幂等（见 `keys::ShiftTap`），重复回调不会切两次。

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};

use windows::core::{implement, ComObjectInterface, Interface, Ref, Result, BOOL, GUID};
use windows::Win32::Foundation::{FALSE, LPARAM, TRUE, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CAPITAL, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfContext, ITfDocumentMgr, ITfEditSession, ITfKeyEventSink, ITfKeyEventSink_Impl,
    ITfKeystrokeMgr, ITfSource, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl,
    TF_CONTEXT_EDIT_CONTEXT_FLAGS, TF_ES_READWRITE, TF_ES_SYNC, TF_INVALID_COOKIE, TS_S_ASYNC,
};

use crate::commit::CommitSession;
use crate::keys::{self, Action, Mode, Mods, ShiftTap, MAX_CODE_LEN};
use crate::tinydict;

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

    /// 当前编码缓冲（五笔字母），上限 [`MAX_CODE_LEN`] 码。
    buffer: RefCell<String>,
    /// 中英模式。
    mode: Cell<Mode>,
    /// 单击 Shift 切中英的状态机。
    ///
    /// 判定逻辑在 `keys::ShiftTap`（纯函数，有单测）；这里只负责每次按键回调时
    /// 采样一次物理 Shift 状态喂给它。
    shift_tap: RefCell<ShiftTap>,
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
            buffer: RefCell::new(String::new()),
            mode: Cell::new(Mode::Chinese),
            shift_tap: RefCell::new(ShiftTap::default()),
        }
    }

    fn buffer_len(&self) -> usize {
        self.buffer.borrow().chars().count()
    }

    fn clear_buffer(&self) -> String {
        std::mem::take(&mut *self.buffer.borrow_mut())
    }

    /// 当前位置的修饰键快照。`GetKeyState` 读本线程输入队列，开销可忽略。
    fn mods(&self) -> Mods {
        Mods {
            ctrl: key_down(VK_CONTROL.0),
            alt: key_down(VK_MENU.0),
            shift: key_down(VK_SHIFT.0),
        }
    }

    /// 用当前状态跑一次判定。两条回调路径都走这里。
    fn decide(&self, vk: u16) -> Action {
        keys::decide(self.mode.get(), self.buffer_len(), vk, self.mods())
    }

    /// 每次按键回调的入口都调一次：采样物理 Shift 状态，推进「单击切中英」状态机。
    ///
    /// **四条回调路径全都要调**。M1.1 第一版只在 `OnKeyDown` 里置位，结果
    /// Notepad++（只走 `OnTestKeyDown` / `OnTestKeyUp`）里 Shift 切换完全失效：
    /// 置位那一半从不执行，松手时的判定被整段跳过。见 `keys::ShiftTap` 的文档。
    ///
    /// 返回 `true` 表示刚刚切换了中英模式。
    fn sync_shift(&self, vk: u16, is_down: bool) -> bool {
        let shift_now = key_down(VK_SHIFT.0);
        let tapped = self.shift_tap.borrow_mut().observe(shift_now, vk, is_down);
        tsf_debug!(
            "#{} shift 采样 down={} is_down={} vk={vk:#04x} -> {}",
            self.seq,
            shift_now,
            is_down,
            if tapped { "单击" } else { "-" }
        );
        if !tapped {
            return false;
        }
        let next = match self.mode.get() {
            Mode::Chinese => Mode::English,
            Mode::English => Mode::Chinese,
        };
        self.mode.set(next);
        // 切模式时丢掉半截编码：留着它会让下次输入莫名其妙多出几个字母。
        let dropped = self.clear_buffer();
        tsf_info!(
            "#{} 切换为{}模式{}",
            self.seq,
            if next == Mode::Chinese {
                "中文"
            } else {
                "英文"
            },
            if dropped.is_empty() {
                String::new()
            } else {
                format!("（丢弃编码 {dropped:?}）")
            }
        );
        true
    }

    /// 执行判定结果。**只有 `OnKey*` 会调它**，`OnTestKey*` 绝不调。
    fn apply(&self, context: &ITfContext, action: Action) {
        match action {
            Action::Pass => {}
            Action::Buffer(ch) => {
                if self.buffer_len() >= MAX_CODE_LEN {
                    // 已满 4 码还来第 5 个键：五笔全码最长 4 位，不可能命中。
                    // 原样上屏再重新开始，比"忽略这个键"或"卡住不响应"都好——
                    // 用户至少能看出发生了什么。
                    let prev = self.clear_buffer();
                    tsf_info!("#{} 已满 {MAX_CODE_LEN} 码，原样上屏 {prev:?}", self.seq);
                    self.commit_text(context, &prev);
                }
                self.buffer.borrow_mut().push(ch);
                tsf_debug!("#{} 缓冲={:?}", self.seq, self.buffer.borrow());
            }
            Action::Commit => self.flush(context),
            Action::Cancel => {
                let dropped = self.clear_buffer();
                tsf_info!("#{} 取消，丢弃编码 {dropped:?}", self.seq);
            }
            Action::Backspace => {
                self.buffer.borrow_mut().pop();
                tsf_debug!("#{} 退格后缓冲={:?}", self.seq, self.buffer.borrow());
            }
        }
    }

    /// 把缓冲提交上屏。
    fn flush(&self, context: &ITfContext) {
        let code = self.clear_buffer();
        if code.is_empty() {
            return;
        }
        match tinydict::lookup(&code) {
            Some(word) => {
                tsf_info!("#{} 提交 {code} → {word}", self.seq);
                self.commit_text(context, word);
            }
            None => {
                // 没命中也必须出字。吃掉了 4 个键却什么都不写，在严格宿主里
                // 那几个字母就永久消失了（踩坑 #12）。原样上屏同时给了用户
                // 逃生口：打错了就重新打。
                tsf_info!("#{} 无命中 {code}，原样上屏", self.seq);
                self.commit_text(context, &code);
            }
        }
    }

    /// 请求一次同步编辑会话，把 `text` 写到光标处。
    fn commit_text(&self, context: &ITfContext, text: &str) {
        if text.is_empty() {
            return;
        }
        let session: ITfEditSession = CommitSession::new(context.clone(), text).into();
        // SYNC：等宿主自己的 UI 线程完成写入。不涉及跨进程往返，见 `commit.rs`。
        let flags = TF_CONTEXT_EDIT_CONTEXT_FLAGS(TF_ES_SYNC.0 | TF_ES_READWRITE.0);
        let r = unsafe { context.RequestEditSession(self.tid.get(), &session, flags) };
        match r {
            Ok(hr) if hr == TS_S_ASYNC => {
                tsf_debug!("#{} 编辑会话排队执行", self.seq);
            }
            Ok(hr) if hr.is_ok() => {}
            Ok(hr) => tsf_error!("#{} RequestEditSession 返回失败 {hr:?}", self.seq),
            Err(e) => tsf_error!("#{} RequestEditSession 失败：{e}", self.seq),
        }
    }
}

impl Default for TextService {
    fn default() -> Self {
        Self::new()
    }
}

/// 修饰键是否按下。`GetKeyState` 最高位为 1 表示当前按下。
fn key_down(vk: u16) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
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

            // 焦点 sink：用来在切窗口时清缓冲，否则在 A 窗口打的字会跑到 B 窗口（预研坑 #5）。
            let source: ITfSource = thread_mgr.cast()?;
            let cookie = source.AdviseSink(&ITfThreadMgrEventSink::IID, self.as_interface_ref())?;
            self.thread_mgr_cookie.set(cookie);

            // 按键 sink：唯一真正关心按键的地方。
            let keystroke_mgr: ITfKeystrokeMgr = thread_mgr.cast()?;
            keystroke_mgr.AdviseKeyEventSink(tid, self.as_interface_ref(), true)?;
            self.keysink_advertised.set(true);

            *self.thread_mgr.borrow_mut() = Some(thread_mgr.clone());
        }

        tsf_info!(
            "#{} Activate tid={} pid={} —— 按键 sink 与焦点 sink 已挂（{}）",
            self.seq,
            tid,
            std::process::id(),
            if self.mode.get() == Mode::Chinese {
                "中文"
            } else {
                "英文"
            }
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

        self.clear_buffer();
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
        // 换窗口时丢掉半截编码：不清的话，在 A 窗口敲的 vbg 会跟着跑到 B 窗口，
        // 用户按空格时莫名其妙多出一个「好」。
        if !self.buffer.borrow().is_empty() {
            let dropped = self.clear_buffer();
            tsf_debug!("#{} 焦点变化，丢弃编码 {dropped:?}", self.seq);
        }
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
        // 失去前台焦点时清缓冲，理由同 ThreadMgr 的 OnSetFocus。
        if !foreground.as_bool() {
            self.clear_buffer();
        }
        Ok(())
    }

    /// **问句，不碰编码缓冲。** 与 `OnKeyDown` 必须返回同一个判定结果，
    /// 否则会出现「键被吃掉但没输入」或「输入两次」（预研坑 #2）。
    ///
    /// 唯一在这里推进的状态是 Shift 采样状态机：走 test 路径的宿主不会调
    /// `OnKeyDown`，把它挪过去等于这一半逻辑不存在。状态机本身幂等，
    /// 「先问再调」的宿主重复回调也不会切两次。
    fn OnTestKeyDown(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnTestKeyDown", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(vk, true);
            let action = self.decide(vk);
            debug_key(seq, "test-down", vk, lparam, action);
            Ok(bool_of(action))
        })
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnTestKeyUp", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(vk, false);
            debug_key(seq, "test-up", vk, lparam, Action::Pass);
            Ok(FALSE)
        })
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnKeyDown", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(vk, true);
            let action = self.decide(vk);
            debug_key(seq, "down", vk, lparam, action);
            if action == Action::Pass {
                return Ok(FALSE);
            }

            // 说吃就一定要吃到底：即使拿不到 context 也返回 TRUE，
            // 否则与 `OnTestKeyDown` 的回答不一致（会出现输入两次或丢键）。
            let context = match pic.ok() {
                Ok(c) => c,
                Err(e) => {
                    tsf_error!("#{seq} 拿不到 ITfContext（{e}），按键被丢弃");
                    return Ok(TRUE);
                }
            };
            self.apply(context, action);
            Ok(TRUE)
        })
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnKeyUp", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(vk, false);
            debug_key(seq, "up", vk, lparam, Action::Pass);
            Ok(FALSE)
        })
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        Ok(FALSE)
    }
}

fn bool_of(action: Action) -> BOOL {
    if action == Action::Pass {
        FALSE
    } else {
        TRUE
    }
}

/// 每次按键写一行 `debug` 日志。
///
/// M1.0 曾把按键压在 `info` 级，因为那一阶段唯一的验收标准是「日志里能看到 vkey」。
/// M1.1 有了真正的行为，验收看的是「提交了什么」，按键退回 `debug`；
/// 需要逐键排查时设 `HEINRICH_TSF_LOG=debug` 即可。
fn debug_key(seq: u32, kind: &str, vk: u16, lparam: LPARAM, action: Action) {
    let is_down = kind.ends_with("down");
    // bit 30 是「此前该键是否已按下」。对 key-down 它正是自动重复标志；
    // 但对 key-up 它**恒为 1**（因为刚才确实按着），拿它判重复会让每一个
    // up 事件都标上 REPEAT。所以只在 down 类事件上看这一位。
    let repeat = is_down && (lparam.0 >> 30) & 1 == 1;
    tsf_debug!(
        "#{seq} {kind} vk={vk:#04x} {} {} {} -> {}",
        describe_vk(vk),
        modifiers_text(),
        if repeat { "REPEAT" } else { "" },
        action_name(action)
    );
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::Pass => "pass",
        Action::Buffer(_) => "buffer",
        Action::Commit => "commit",
        Action::Cancel => "cancel",
        Action::Backspace => "backspace",
    }
}

/// 修饰键状态文本，供日志用。
fn modifiers_text() -> String {
    let mut s = String::new();
    for (vk, ch) in [(VK_CONTROL.0, 'C'), (VK_MENU.0, 'A'), (VK_SHIFT.0, 'S')] {
        if key_down(vk) {
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

/// vkey → 人能读的名字。M1.1 全靠这个看「键到底进没进来」。
///
/// 覆盖范围按「会用到」定：字母数字、五笔用到的标点（`;` 与 `'`，
/// 后者是 §6.5 的显式拼音标记）、编辑键、修饰键、方向键、功能键。
/// 未收录的落到 `(0x..)`，不会丢信息——只是要多查一次表。
fn describe_vk(vk: u16) -> String {
    let named = match vk {
        0x08 => "BACKSPACE",
        0x09 => "TAB",
        0x0D => "ENTER",
        0x10 => "SHIFT",
        0x11 => "CTRL",
        0x12 => "ALT",
        0x14 => "CAPSLOCK",
        0x1B => "ESC",
        0x20 => "SPACE",
        0x21 => "PGUP",
        0x22 => "PGDN",
        0x23 => "END",
        0x24 => "HOME",
        0x25 => "LEFT",
        0x26 => "UP",
        0x27 => "RIGHT",
        0x28 => "DOWN",
        0x2D => "INSERT",
        0x2E => "DELETE",
        0x5B => "LWIN",
        0x5C => "RWIN",
        0x5D => "APPS",
        0x90 => "NUMLOCK",
        0x91 => "SCROLLLOCK",
        0xA0 => "LSHIFT",
        0xA1 => "RSHIFT",
        0xA2 => "LCTRL",
        0xA3 => "RCTRL",
        0xA4 => "LALT",
        0xA5 => "RALT",
        // 标点：OEM 键的 vkey 与键盘布局无关，映射是固定的。
        0xBA => "OEM_1(;)",
        0xBB => "OEM_PLUS(=)",
        0xBC => "OEM_COMMA(,)",
        0xBD => "OEM_MINUS(-)",
        0xBE => "OEM_PERIOD(.)",
        0xBF => "OEM_2(/)",
        0xC0 => "OEM_3(`)",
        0xDB => "OEM_4([)",
        0xDC => "OEM_5(\\)",
        0xDD => "OEM_6(])",
        0xDE => "OEM_7(')",
        0xE2 => "OEM_102(<\\)",
        _ => {
            return match vk {
                0x41..=0x5A => format!("'{}'", (b'A' + (vk - 0x41) as u8) as char),
                0x30..=0x39 => format!("'{}'", (b'0' + (vk - 0x30) as u8) as char),
                0x60..=0x69 => format!("NUM{}", vk - 0x60),
                0x70..=0x87 => format!("F{}", vk - 0x6F),
                other => format!("({other:#04x})"),
            }
        }
    };
    named.to_string()
}
