//! TSF 文本服务（M1.2 版）。
//!
//! ## 本阶段做到什么
//!
//! 缓冲 `a`–`y`，编码以**组合串**显示在文档里，空格 / 回车把命中字原地替换上去。
//! 候选窗、显示属性（虚线）、光标跟随仍不做，留到 M1.3 / M2。
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
//! 一个容易误读的现象：M1.0 的日志里 Notepad++ 只有 `test-down` 没有 `down`，
//! 是因为那一版 `OnKeyDown` 恒返回 FALSE、一个键都不吃；宿主对「问了说不要」的键
//! 不会再调 `KeyDown`。**一旦在 `OnTestKeyDown` 回答 TRUE，宿主就会接着调
//! `OnKeyDown`**——两条路径都会到齐，只是顺序是「先问后做」。
//!
//! ## 两条路径的分工
//!
//! - `OnTestKey*`：**只回答吃不吃，不碰编码缓冲**。它是「问句」，同一个键可能被问多次。
//! - `OnKey*`：**真正干活**（改缓冲、改组合串、上屏）。
//!
//! 这样即使某个宿主「先问、再调」，也不会把同一个键处理两遍。
//!
//! 唯一的例外是 Shift 采样状态机（`sync_shift`）：**四条回调都要调**。走 test 路径
//! 的宿主在「问了说不要」之后不会调 `OnKeyDown`，把置位动作放在那里等于这一半逻辑
//! 不存在——M1.1 第一版就是这么错的，Shift 切换在 Notepad++ 里完全失效。
//! 状态机本身幂等（见 `keys::ShiftTap`），重复回调不会切两次。

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;

use windows::core::{implement, ComObjectInterface, IUnknown, Interface, Ref, Result, BOOL, GUID};
use windows::Win32::Foundation::{FALSE, LPARAM, TRUE, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CAPITAL, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfDocumentMgr,
    ITfEditSession, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl,
    ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl, TF_CONTEXT_EDIT_CONTEXT_FLAGS,
    TF_ES_READWRITE, TF_ES_SYNC, TF_INVALID_COOKIE, TS_S_ASYNC,
};

use crate::ipc;
use crate::keys::{self, Action, Mode, Mods, ShiftTap, MAX_CODE_LEN};
use crate::preedit::{CompSlot, Goal, PreeditSession};

/// 实例序号，只用于日志区分「第几个宿主进程里的实例」。
static INSTANCE_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 一个宿主进程里的一份文本服务实例。
///
/// 状态**必须**是实例内的：DLL 被注入到每个进程都会 new 一个，
/// 如果放全局，A 窗口的缓冲就会跑到 B 窗口去（预研坑 #5 的根因）。
#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfThreadMgrEventSink,
    ITfCompositionSink
)]
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
    ///
    /// 组合串显示的内容就是它——两者是同一份状态的两种表现，所以缓冲一变就要同步
    /// 组合串，别让它们各自漂移。
    buffer: RefCell<String>,
    /// 中英模式。
    mode: Cell<Mode>,
    /// 单击 Shift 切中英的状态机。
    ///
    /// 判定逻辑在 `keys::ShiftTap`（纯函数，有单测）；这里只负责每次按键回调时
    /// 采样一次物理 Shift 状态喂给它。
    shift_tap: RefCell<ShiftTap>,

    /// 当前组合串句柄。与编辑会话共享（会话可能被异步执行）。
    comp: CompSlot,
    /// 自己在 `ITfCompositionSink` 上的引用，`StartComposition` 要它。
    comp_sink: RefCell<Option<ITfCompositionSink>>,
    /// 组合串所在的 context。失焦时要靠它把残留的组合串撤掉。
    active_ctx: RefCell<Option<ITfContext>>,
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
            comp: Rc::new(RefCell::new(None)),
            comp_sink: RefCell::new(None),
            active_ctx: RefCell::new(None),
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
    /// Notepad++（先问后做的宿主，问了说不要就不再调 `OnKeyDown`）里 Shift 切换
    /// 完全失效：置位那一半从不执行，松手时的判定被整段跳过。
    /// 见 `keys::ShiftTap` 的文档。
    ///
    /// 返回 `true` 表示刚刚切换了中英模式。
    fn sync_shift(&self, pic: &Ref<ITfContext>, vk: u16, is_down: bool) -> bool {
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
        // 上报给 server：模式是**全局**状态（D34）。本地先改是为了让本进程立刻生效，
        // 上报走队列——切模式发生在 Shift 松开的那一刻，那是键路径，不能等 IPC。
        ipc::set_mode(next == Mode::Chinese);
        // 切模式时丢掉半截编码：留着它会让下次输入莫名其妙多出几个字母。
        // 组合串也要跟着撤——否则那串字母会留在文档里，而缓冲已经空了。
        let dropped = self.buffer.borrow().clone();
        if !dropped.is_empty() {
            if let Ok(ctx) = pic.ok() {
                self.run_preedit(ctx, Goal::Finish(None));
            }
        }
        self.clear_buffer();
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
                    // 原样上屏再重新开始，比「忽略这个键」或「卡住不响应」都好——
                    // 用户至少能看出发生了什么。
                    let prev = self.clear_buffer();
                    tsf_info!("#{} 已满 {MAX_CODE_LEN} 码，原样上屏 {prev:?}", self.seq);
                    self.run_preedit(context, Goal::Finish(Some(prev)));
                }
                self.buffer.borrow_mut().push(ch);
                tsf_debug!("#{} 缓冲={:?}", self.seq, self.buffer.borrow());
                self.sync_preedit(context);
            }
            Action::Commit => self.commit(context),
            Action::Punct(p) => {
                // 标点把「当前编码 + 标点」一起写成最终文字。缓冲为空时只写标点。
                let code = self.clear_buffer();
                let mut text = if code.is_empty() {
                    String::new()
                } else {
                    self.resolve(&code)
                };
                text.push(p);
                tsf_info!("#{} 标点 {p}，一次写入 {text:?}", self.seq);
                self.run_preedit(context, Goal::Finish(Some(text)));
            }
            Action::Cancel => {
                let dropped = self.clear_buffer();
                tsf_info!("#{} 取消，丢弃编码 {dropped:?}", self.seq);
                self.run_preedit(context, Goal::Finish(None));
            }
            Action::Backspace => {
                self.buffer.borrow_mut().pop();
                tsf_debug!("#{} 退格后缓冲={:?}", self.seq, self.buffer.borrow());
                self.sync_preedit(context);
            }
        }
    }

    /// 让组合串显示当前缓冲（缓冲空则结束组合串）。
    fn sync_preedit(&self, context: &ITfContext) {
        let text = self.buffer.borrow().clone();
        self.run_preedit(context, Goal::Show(text));
    }

    /// 把缓冲提交上屏。
    fn commit(&self, context: &ITfContext) {
        let code = self.clear_buffer();
        if code.is_empty() {
            return;
        }
        let text = self.resolve(&code);
        self.run_preedit(context, Goal::Finish(Some(text)));
    }

    /// 编码 → 上屏文字。**任何路径都必须返回一段文字**。
    ///
    /// 三种情况：
    ///
    /// | 情况 | 结果 |
    /// |---|---|
    /// | 命中 | 首候选（M1.3 没有候选窗，第 2 个及以后的候选还选不了） |
    /// | 查到了但没这个词 | 编码原样上屏（D27：吃掉的键必须变成输出） |
    /// | server 不可用 / 超时 | 同样原样上屏——比丢字好，也比卡住好 |
    ///
    /// 最后一行是 D18 的落地形态：**宁可这一次降级成英文，绝不冻住宿主**。
    fn resolve(&self, code: &str) -> String {
        match ipc::query(code) {
            Some(entries) if !entries.is_empty() => {
                let word = entries[0].word.clone();
                tsf_info!("#{} 提交 {code} → {word}", self.seq);
                word
            }
            Some(_) => {
                tsf_info!("#{} 无命中 {code}，原样上屏", self.seq);
                code.to_string()
            }
            None => {
                tsf_info!("#{} server 不可用（{code}），原样上屏", self.seq);
                code.to_string()
            }
        }
    }

    /// 请求一次同步编辑会话，把组合串推到目标状态。
    ///
    /// `SYNC` 等的是宿主自己的 UI 线程（TSF 就在那个线程上回调我们），
    /// 不存在跨进程往返——D18 约束的是 IPC，不是这个。
    fn run_preedit(&self, context: &ITfContext, goal: Goal) {
        // 只在「正在显示组合串」时记住 context：失焦时要用它把残留撤掉。
        // `Finish` 之后组合串已经没了，不需要再记。
        if matches!(goal, Goal::Show(_)) {
            *self.active_ctx.borrow_mut() = Some(context.clone());
        }

        let sink = self.comp_sink.borrow().clone();
        if sink.is_none() {
            tsf_debug!("#{} 组合串 sink 未就绪，降级为直接插入", self.seq);
        }
        let session = PreeditSession::new(context.clone(), sink, Rc::clone(&self.comp), goal);
        let iface: ITfEditSession = session.into();
        let flags = TF_CONTEXT_EDIT_CONTEXT_FLAGS(TF_ES_SYNC.0 | TF_ES_READWRITE.0);
        match unsafe { context.RequestEditSession(self.tid.get(), &iface, flags) } {
            Ok(hr) if hr == TS_S_ASYNC => tsf_debug!("#{} 组合串会话排队执行", self.seq),
            Ok(hr) if hr.is_ok() => {}
            Ok(hr) => tsf_error!("#{} 组合串会话返回失败 {hr:?}", self.seq),
            Err(e) => tsf_error!("#{} 组合串会话失败：{e}", self.seq),
        }
        if self.comp.borrow().is_none() {
            // 组合串已结束（或压根没起来），记住的 context 也就没用了。
            self.active_ctx.borrow_mut().take();
        }
    }

    /// 撤销当前组合串并清空缓冲。用于失焦、切窗口这类「输入被打断」的场合。
    ///
    /// 不撤销的话，那串编码字母会留在文档里——用户切回来会看到一个莫名其妙的 `vbg`。
    fn abort_input(&self, why: &str) {
        let dropped = self.buffer.borrow().clone();
        let ctx = self.active_ctx.borrow_mut().take();
        self.clear_buffer();
        let Some(ctx) = ctx else { return };
        if dropped.is_empty() && self.comp.borrow().is_none() {
            return;
        }
        tsf_info!("#{} {why}，撤销组合串（丢弃编码 {dropped:?}）", self.seq);
        self.run_preedit(&ctx, Goal::Finish(None));
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

            // 组合串 sink：`StartComposition` 要一个 `ITfCompositionSink`，
            // 用来在「组合串被别人终止」时通知我们清状态（见 OnCompositionTerminated）。
            // `as_interface_ref()` 是泛型方法（要说明「把哪个接口当 IUnknown」），
            // 这里显式写全类型，随后 AddRef 取一份可长期持有的引用。
            let self_unknown: IUnknown =
                ComObjectInterface::<IUnknown>::as_interface_ref(self).to_owned();
            let comp_sink: ITfCompositionSink = self_unknown.cast()?;
            *self.comp_sink.borrow_mut() = Some(comp_sink);

            *self.thread_mgr.borrow_mut() = Some(thread_mgr.clone());
        }

        // IPC：启动后台连接线程（幂等），并趁机把 server 上的全局模式取回来。
        // 这两件事都发生在 `Activate`——切到某个输入框的瞬间，离真正落键还有
        // 几百毫秒，冷启动 server 与首次握手正好挤在这段空隙里完成。
        ipc::warm_up();
        if let Some(chinese) = ipc::current_mode() {
            self.mode.set(if chinese {
                Mode::Chinese
            } else {
                Mode::English
            });
        }

        tsf_info!(
            "#{} Activate tid={} pid={} —— 按键 sink 与焦点 sink 已挂（{}，IPC {}）",
            self.seq,
            tid,
            std::process::id(),
            if self.mode.get() == Mode::Chinese {
                "中文"
            } else {
                "英文"
            },
            if ipc::is_connected() {
                "已连接"
            } else {
                "未连接"
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

        // 反注册途中不该再往文档里写字，所以这里只丢状态、不撤组合串：
        // 宿主正在切走，TSF 会自己终止它，`OnCompositionTerminated` 会收到通知。
        self.clear_buffer();
        *self.comp.borrow_mut() = None;
        self.active_ctx.borrow_mut().take();
        self.comp_sink.borrow_mut().take();
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
        // 换窗口时撤掉半截输入：不清的话，在 A 窗口敲的 vbg 会跟着跑到 B 窗口，
        // 用户按空格时莫名其妙多出一个「好」；组合串模式下还会在 A 窗口留下一串
        // 「vbg」字母——那串字母是真实写进文档的，不撤就在那儿了。
        self.abort_input("焦点变化");
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
        // 失去前台焦点时撤销组合串，理由同 ThreadMgr 的 OnSetFocus。
        if !foreground.as_bool() {
            self.abort_input("失去前台焦点");
        }
        Ok(())
    }

    /// **问句，不碰编码缓冲。** 与 `OnKeyDown` 必须返回同一个判定结果，
    /// 否则会出现「键被吃掉但没输入」或「输入两次」（预研坑 #2）。
    ///
    /// 唯一在这里推进的状态是 Shift 采样状态机：走 test 路径的宿主不会调
    /// `OnKeyDown`，把它挪过去等于这一半逻辑不存在。状态机本身幂等，
    /// 「先问再调」的宿主重复回调也不会切两次。
    fn OnTestKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnTestKeyDown", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(&pic, vk, true);
            let action = self.decide(vk);
            debug_key(seq, "test-down", vk, lparam, action);
            Ok(bool_of(action))
        })
    }

    fn OnTestKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnTestKeyUp", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(&pic, vk, false);
            debug_key(seq, "test-up", vk, lparam, Action::Pass);
            Ok(FALSE)
        })
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnKeyDown", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(&pic, vk, true);
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

    fn OnKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        let seq = self.seq;
        guarded("OnKeyUp", Ok(FALSE), || {
            let vk = wparam.0 as u16;
            self.sync_shift(&pic, vk, false);
            debug_key(seq, "up", vk, lparam, Action::Pass);
            Ok(FALSE)
        })
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        Ok(FALSE)
    }
}

/// 组合串被**别人**终止时收到通知。
///
/// ## 为什么需要它
///
/// 组合串不是只有我们能结束：宿主换焦点、文档被关、另一个 TIP 抢走组合串，
/// TSF 都会替他终止掉。这时我们的缓冲里还留着 `vbg`，而组合串已经不受我们控制。
/// 不处理的话状态就永久错位——下一次敲字母会往一个已死的对象上写。
///
/// ## 为什么这里可以放心删残留文字
///
/// 我们自己 `EndComposition` 的路径**不会**走到这里（而且我们总是先把槽里的
/// 句柄 take 掉再结束）。再加一道保险：只处理「正是槽里那一个」的组合串，
/// 用裸指针比身份。这样即使某个实现在自己结束之后也回调一次，也匹配不上，
/// 不会把刚提交的汉字删掉。
impl ITfCompositionSink_Impl for TextService_Impl {
    fn OnCompositionTerminated(
        &self,
        ecwrite: u32,
        pcomposition: Ref<ITfComposition>,
    ) -> Result<()> {
        let seq = self.seq;
        guarded("OnCompositionTerminated", Ok(()), || {
            let (ours, theirs) = match (self.comp.borrow().as_ref(), pcomposition.ok()) {
                (Some(mine), Ok(theirs)) => (mine.as_raw(), Some(theirs)),
                _ => (std::ptr::null_mut(), None),
            };
            let Some(theirs) = theirs else {
                tsf_debug!("#{seq} OnCompositionTerminated：找不到被终止的组合串，忽略");
                return Ok(());
            };
            if ours != theirs.as_raw() {
                tsf_debug!("#{seq} OnCompositionTerminated：不是当前组合串，忽略");
                return Ok(());
            }

            // `ecwrite` 就是给我们改文档用的，趁它还有效把残留的编码字母删掉，
            // 否则文档里会永久留下一串 `vbg`。
            unsafe {
                if let Ok(range) = theirs.GetRange() {
                    if let Err(e) = range.SetText(ecwrite, 0, &[]) {
                        tsf_error!("#{seq} 删除组合串残留失败：{e}");
                    }
                }
            }

            *self.comp.borrow_mut() = None;
            self.active_ctx.borrow_mut().take();
            let dropped = self.clear_buffer();
            tsf_info!("#{seq} 组合串被外部终止，已删除残留 {dropped:?}");
            Ok(())
        })
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
        Action::Punct(_) => "punct",
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
