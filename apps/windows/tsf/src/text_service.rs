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

use heinrich_ui::{Dir, Rect};
use heinrich_win_ui::{Event, Hit};
use windows::core::{
    implement, ComObjectInterface, IUnknown, Interface, Ref, Result, BOOL, BSTR, GUID,
};
use windows::Win32::Foundation::{FALSE, HWND, LPARAM, TRUE, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CAPITAL, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfCompartmentMgr, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext,
    ITfDocumentMgr, ITfEditSession, ITfFnConfigure, ITfFnConfigure_Impl, ITfFunction_Impl,
    ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfLangBarItemButton,
    ITfLangBarItemMgr, ITfSource, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl,
    GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION, TF_CONTEXT_EDIT_CONTEXT_FLAGS, TF_ES_READ,
    TF_ES_READWRITE, TF_ES_SYNC, TF_INVALID_COOKIE, TS_S_ASYNC,
};

use crate::cand;
use crate::caret::CaretProbe;
use crate::ipc;
use crate::keycap;
use crate::keys::{self, Action, Mode, Mods, ShiftTap};
use crate::langbar;
use crate::preedit::{CompSlot, Goal, PreeditSession};

/// 实例序号，只用于日志区分「第几个宿主进程里的实例」。
static INSTANCE_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 一个宿主进程里的一份文本服务实例。
///
/// ## 为什么状态装在 `Rc<Core>` 里，而不是直接放在本结构体上
///
/// 候选窗是**独立窗口**，用户点它的时候是宿主在 `DispatchMessage` 里回调我们的
/// 窗口过程——那不在任何 TSF 回调栈里。也就是说「点选提交」这条路径拿不到
/// `&self`（COM 对象在 vtable 后面，没有安全的入口）。把可变状态放进 `Rc<Core>`，
/// 窗口过程的闭包就能持有一个 `Weak<Core>`，点选时升级成 `Rc` 再走同一套
/// 「起编辑会话 → 上屏」的代码，不必为点击写第二份提交逻辑。
///
/// 状态**必须**是实例内的：DLL 被注入到每个进程都会 new 一个，
/// 如果放全局，A 窗口的缓冲就会跑到 B 窗口去（预研坑 #5 的根因）。
#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfThreadMgrEventSink,
    ITfCompositionSink,
    ITfFnConfigure
)]
pub struct TextService {
    core: Rc<Core>,
}

/// 所有实例状态。见 [`TextService`] 的文档：它要被窗口过程的闭包共享，
/// 所以不能直接长在 COM 对象上。
pub struct Core {
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

    /// 当前编码缓冲（五笔字母 / 拼音字母），上限随混输切换：
    /// 纯五笔 4 码（[`keys::MAX_CODE_LEN`]），有拼音词库时 24 个字母
    /// （[`keys::MAX_PINYIN_CODE_LEN`]）。
    ///
    /// 组合串显示的内容就是它——两者是同一份状态的两种表现，所以缓冲一变就要同步
    /// 组合串，别让它们各自漂移。
    buffer: RefCell<String>,
    /// 中英模式。`Rc` 共享给语言栏按钮（左键切换同一份），见 `langbar`。
    mode: Rc<Cell<Mode>>,
    /// 语言栏按钮（任务栏角标入口）。`Activate` 时挂、`Deactivate` 时摘；
    /// 模式切换时 `notify` 让图标重取。None = 还没激活或挂载失败。
    langbar: RefCell<Option<windows::core::ComObject<langbar::LangBarButton>>>,
    /// 单击 Shift 切中英的状态机。
    ///
    /// 判定逻辑在 `keys::ShiftTap`（纯函数，有单测）；这里只负责每次按键回调时
    /// 采样一次物理 Shift 状态喂给它。
    shift_tap: RefCell<ShiftTap>,

    /// 当前组合串句柄。与编辑会话共享（会话可能被异步执行）。
    comp: CompSlot,
    /// 自己在 `ITfCompositionSink` 上的引用，`StartComposition` 要它。
    comp_sink: RefCell<Option<ITfCompositionSink>>,
    /// 组合串所在的 context。失焦时要靠它把残留的组合串撤掉；
    /// 光标定位与点选提交也要靠它。
    active_ctx: RefCell<Option<ITfContext>>,

    /// 候选窗 + 这一页的状态。见 [`crate::cand`]。
    cands: Rc<RefCell<cand::Cands>>,
    /// 码位键位图（§6.7）。懒建、按需弹，与候选窗同生共死。
    keycaps: RefCell<keycap::KeycapPanel>,
}

/// 把 COM 对象上的字段访问与私有方法调用透到 [`Core`]：`self.buffer`、`self.apply(..)`
/// 这类写法在两个 `Deref` 之后自动落到 `Core` 上（`#[implement]` 已经为
/// `TextService_Impl` 生成了指向 `TextService` 的 `Deref`）。
impl std::ops::Deref for TextService {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.core
    }
}

impl TextService {
    pub fn new() -> Self {
        Self { core: Core::new() }
    }
}

impl Core {
    fn new() -> Rc<Self> {
        let seq = INSTANCE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let core = Rc::new(Self {
            tid: Cell::new(0),
            thread_mgr: RefCell::new(None),
            thread_mgr_cookie: Cell::new(TF_INVALID_COOKIE),
            keysink_advertised: Cell::new(false),
            seq,
            buffer: RefCell::new(String::new()),
            mode: Rc::new(Cell::new(Mode::Chinese)),
            langbar: RefCell::new(None),
            shift_tap: RefCell::new(ShiftTap::default()),
            comp: Rc::new(RefCell::new(None)),
            comp_sink: RefCell::new(None),
            active_ctx: RefCell::new(None),
            cands: Rc::new(RefCell::new(cand::Cands::new(seq))),
            keycaps: RefCell::new(keycap::KeycapPanel::new(seq)),
        });

        // 候选窗的点击回调。只持有 `Weak` 有两个原因：
        // ① 窗口在 `Core` 里，回调再持有 `Rc<Core>` 就成环，`Core` 永不释放；
        // ② 窗口销毁得晚于 `Core` 的可能性必须被容忍——升级失败就当没发生。
        let weak = Rc::downgrade(&core);
        core.cands.borrow_mut().set_on_event(move |event| {
            if let Some(core) = weak.upgrade() {
                core.on_candidate_event(event);
            }
        });
        core
    }

    fn buffer_len(&self) -> usize {
        self.buffer.borrow().chars().count()
    }

    fn clear_buffer(&self) -> String {
        std::mem::take(&mut *self.buffer.borrow_mut())
    }

    /// 当前允许的缓冲长度。`next` 是**即将进入缓冲**的那个字符。
    ///
    /// 三条规则，按「这段编码最可能有多长」排：
    ///
    /// | 情形 | 上限 |
    /// |---|---|
    /// | 缓冲里出现过 `'`，或正要输入的字符是 `'` / `z` | [`keys::MAX_EXTENDED_CODE_LEN`]（多段 / 通配） |
    /// | 装了拼音词库 | [`keys::MAX_PINYIN_CODE_LEN`]（拼音最长词条 20 个字母） |
    /// | 纯五笔 | 4（五笔全码定长） |
    ///
    /// 第一行必须看 `next` 而不能只看已有缓冲：`hao'` 的 `'` 是在第 4 个键进来的，
    /// 那时纯五笔的 4 码上限已经满了，不放宽就永远打不出第二段。
    /// 反过来，一旦缓冲里有过 `'` 就**一直**放宽——第二段还得打完。
    fn max_code_len(&self, next: char) -> usize {
        let buf = self.buffer.borrow();
        if next == '\'' || next == 'z' || buf.contains('\'') {
            keys::MAX_EXTENDED_CODE_LEN
        } else {
            keys::max_code_len(ipc::has_pinyin())
        }
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
    ///
    /// `page_len` 也要喂进去：数字键与方向键归谁，取决于此刻窗口里有没有候选。
    fn decide(&self, vk: u16) -> Action {
        let input = keys::Input {
            mode: self.mode.get(),
            buffer_len: self.buffer_len(),
            page_len: self.cands.borrow().page_len(),
            pinyin: ipc::has_pinyin(),
        };
        keys::decide(input, vk, self.mods())
    }

    /// 语言栏按钮还在就通知它重取图标（模式变了）。
    ///
    /// 只报 `TF_LBI_ICON`：状态位没变，带 `TF_LBI_STATUS` 会让系统整条重建
    /// （见 `langbar::LangBarButton::notify_icon` 的注释）。
    fn notify_langbar(&self) {
        if let Some(obj) = &*self.langbar.borrow() {
            obj.notify_icon();
        }
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
        // 语言栏按钮的图标跟着模式走（任务栏角标上的「亨/英」）。
        self.notify_langbar();
        let flushed = self.flush_codes_as_letters(pic);
        // 窗口跟着撤：切到英文模式后还挂着一屏中文候选，等于骗用户按空格。
        self.clear_popups();
        tsf_info!(
            "#{} 切换为{}模式{}",
            self.seq,
            if next == Mode::Chinese {
                "中文"
            } else {
                "英文"
            },
            match &flushed {
                Some(code) => format!("（缓冲 {code:?} 原样上屏）"),
                None => String::new(),
            }
        );
        true
    }

    /// 切模式时把半截编码**原样**写进文档，返回写进去的内容。
    ///
    /// 为什么吐字母而不是吐候选（D41）：走进这条路的几乎只有一种情形——用户以为
    /// 自己在英文模式，敲了几个字母才看到中文候选，于是单击 Shift 切英文。他要的
    /// 是「我刚按的那几个键」，替他上汉字等于替他做了他没做的决定。对误触 Shift
    /// 也更宽容：留在文档里的至少是他自己敲的东西。
    ///
    /// 不走 `resolve`，所以也不会去查 server：这条路径上任何 IPC 都在 Shift 松手
    /// 的那一瞬，等它不值得。
    fn flush_codes_as_letters(&self, pic: &Ref<ITfContext>) -> Option<String> {
        let code = self.clear_buffer();
        if code.is_empty() {
            return None;
        }
        match pic.ok() {
            // 走 `finish`：先撤窗再写文档，与空格/点选同一条出口。
            Ok(context) => {
                self.finish(context, code.clone());
                Some(code)
            }
            // 宿主已经切走、拿不到 context：只能丢，但记 error 而不是 info，
            // 否则「明明打了字却不见了」在日志里查不到。
            Err(e) => {
                tsf_error!(
                    "#{} 切模式时拿不到 context（{e}），缓冲 {code:?} 丢失",
                    self.seq
                );
                None
            }
        }
    }

    /// 执行判定结果。**只有 `OnKey*` 会调它**，`OnTestKey*` 绝不调。
    fn apply(&self, context: &ITfContext, action: Action) {
        match action {
            // `Noop` 是「键已经吃了，但什么也不做」：吃掉是为了不让宿主在组合串
            // 中间插字符（那会终止组合串、把编码当残留丢掉），不做是因为此刻
            // 没有合法的动作可做（翻到头了、按下不存在的高亮号）。
            Action::Pass | Action::Noop => {}
            Action::Buffer(ch) => self.type_letter(context, ch),
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
                self.finish(context, text);
            }
            Action::Cancel => {
                let dropped = self.clear_buffer();
                tsf_info!("#{} 取消，丢弃编码 {dropped:?}", self.seq);
                self.abort(context);
            }
            Action::Backspace => {
                self.buffer.borrow_mut().pop();
                tsf_debug!("#{} 退格后缓冲={:?}", self.seq, self.buffer.borrow());
                self.sync_preedit(context);
                self.refresh_candidates(context, None);
            }
            Action::Select(nth) => self.select(context, nth),
            Action::Keycaps(nth) => self.show_keycaps(nth),
            Action::PagePrev => self.flip_page(Dir::Prev),
            Action::PageNext => self.flip_page(Dir::Next),
        }
    }

    /// 敲一个字母：缓冲 + 组合串 + 候选窗（D37：每个字母都查一次）。
    fn type_letter(&self, context: &ITfContext, ch: char) {
        // **首个字母进来之前先量一次光标。**
        //
        // 这时文档还没被我们改过，宿主的排版信息是干净的，`GetTextExt` 一定答得出。
        // 组合串一写进去，Chromium 的排版就进入「脏」状态，紧接着问会给
        // `TF_E_NOLAYOUT`（见 `caret` 模块文档）——那正是 Chrome 里第一键
        // 看不到窗口的原因。所以留一份「动手前的位置」当兜底。
        //
        // 只在 `buffer_len() == 0`（一段新编码的第一个键）做，多花的那一次会话
        // 摊到每个词上只有一次。
        let pre_caret = if self.buffer_len() == 0 {
            self.caret_rect(context)
        } else {
            None
        };

        let limit = self.max_code_len(ch);
        if self.buffer_len() + 1 > limit {
            // 已满码还来下一个键：纯五笔是「五笔全码最长 4 位，不可能命中」，
            // 混输时是「拼音 24 个字母还查不到，基本是打错了」。
            // 原样上屏再重新开始，比「忽略这个键」或「卡住不响应」都好——
            // 用户至少能看出发生了什么。
            let prev = self.clear_buffer();
            tsf_info!("#{} 已满 {} 码，原样上屏 {prev:?}", self.seq, limit);
            self.finish(context, prev);
        }
        self.buffer.borrow_mut().push(ch);
        tsf_debug!("#{} 缓冲={:?}", self.seq, self.buffer.borrow());
        // 顺序有讲究：组合串是本地的（快），候选要过 IPC（最多 50 ms）。
        // 先让用户看见编码长了，再去等候选——反过来的话，慢 server 会让
        // 光标后面的字晚半拍才出现。
        self.sync_preedit(context);
        self.refresh_candidates(context, pre_caret);
    }

    /// 按当前编码问一次候选，并让窗口跟着更新。
    ///
    /// ## 三种结果，三种处理（D37）
    ///
    /// | 结果 | 窗口 | 理由 |
    /// |---|---|---|
    /// | 有候选 | 换成新候选，回第 1 页 | 正常路径 |
    /// | 问到了但没这个词 | 关掉 | 没有东西可显示，留一个空壳更奇怪 |
    /// | 超时 / server 挂了 | **保留上一帧** | 清空会让窗口一闪一闪；上屏时仍原样吐字母 |
    ///
    /// `fallback_caret`：重画时如果问不到光标（宿主排版脏），用这个位置代替。
    fn refresh_candidates(&self, context: &ITfContext, fallback_caret: Option<Rect>) {
        // 编码一变，上一张键位图就过期了（它画的是**上一个码**的字根）。
        // 放在最前面而不是跟着 `clear` 走：新码也查到候选时不会走 `clear`，
        // 但那张图同样不能留（§6.7 的「换码即关闭」）。
        self.keycaps.borrow_mut().hide();
        let code = self.buffer.borrow().clone();
        if code.is_empty() {
            self.clear_popups();
            return;
        }
        match ipc::query(&code) {
            Some(reply) if reply.words.is_empty() => {
                self.clear_popups();
                tsf_debug!("#{} {code} 无候选", self.seq);
            }
            Some(reply) => {
                // 候选窗按 server 给的顺序画：主段在前、折叠区在后（D48）。
                // `primary_len` 交给窗口当分界，折叠区那一截淡一档——用户得能
                // 一眼看出「这条是五笔打出来的」还是「这条是拼音猜的」。
                tsf_debug!(
                    "#{} {code} → {} 条，主段{} {} 条，{} 条带码提示",
                    self.seq,
                    reply.words.len(),
                    if reply.pinyin { "拼音" } else { "五笔" },
                    reply.primary_len,
                    reply.codes.iter().filter(|c| !c.is_empty()).count()
                );
                self.cands.borrow_mut().update(
                    reply.words,
                    reply.codes,
                    reply.hint_ats,
                    reply.emojis,
                    reply.primary_len,
                );
                self.show_candidates(context, fallback_caret);
            }
            None => tsf_debug!("#{} {code} 候选查询无应答，保留上一帧", self.seq),
        }
    }

    /// 让候选窗贴着光标显示。
    ///
    /// 光标有两处来源：现场量一次，量不到就用调用方给的 `fallback_caret`
    /// （首键前预取的那一个）。两个都没有才不显示——猜一个位置会盖住用户正在
    /// 看的那一行字，比没有窗口更糟。
    fn show_candidates(&self, context: &ITfContext, fallback_caret: Option<Rect>) {
        let caret = match (self.caret_rect(context), fallback_caret) {
            (Some(caret), _) => caret,
            (None, Some(caret)) => {
                tsf_debug!("#{} 沿用动手前量到的光标 {caret:?}", self.seq);
                caret
            }
            (None, None) => {
                tsf_debug!("#{} 拿不到光标位置，本次不显示候选窗", self.seq);
                return;
            }
        };
        self.cands.borrow_mut().show(caret);
    }

    /// 问一次光标矩形（屏幕坐标）。
    ///
    /// 开一次**只读**编辑会话：`GetTextExt` 要 cookie，cookie 只在会话里有效。
    /// 会话是 `TF_ES_SYNC`，返回时结果已经填好。
    fn caret_rect(&self, context: &ITfContext) -> Option<Rect> {
        let (probe, slot) = CaretProbe::new(context.clone(), Rc::clone(&self.comp), self.tid.get());
        let iface = CaretProbe::into_session(probe);
        let flags = TF_CONTEXT_EDIT_CONTEXT_FLAGS(TF_ES_SYNC.0 | TF_ES_READ.0);
        if let Err(e) = unsafe { context.RequestEditSession(self.tid.get(), &iface, flags) } {
            tsf_debug!("#{} 光标会话请求失败：{e}", self.seq);
            return None;
        }
        // 先拷出来再返回：`Option<Rect>` 是 `Copy`，把它作为尾表达式会把
        // `Ref` 临时值的生命周期拉到函数末尾，而 `slot` 在那之前就 drop 了。
        let rect = *slot.borrow();
        rect
    }

    /// 按本页序号选词（数字键 1–9）。
    fn select(&self, context: &ITfContext, nth: usize) {
        let word = self.cands.borrow().word_on_page(nth);
        let code = self.clear_buffer();
        match word {
            Some(word) => {
                tsf_info!("#{} 数字键选第 {} 条 {code} → {word}", self.seq, nth + 1);
                ipc::record(&code, &word);
                self.finish(context, word);
            }
            None => {
                // 到不了这里：`decide` 只在 `nth < page_len` 时给 `Select`。
                // 真到了就按上屏处理，绝不丢键（D27）。
                tsf_info!(
                    "#{} 第 {} 条不存在，编码原样上屏 {code:?}",
                    self.seq,
                    nth + 1
                );
                let text = if code.is_empty() {
                    String::new()
                } else {
                    self.resolve(&code)
                };
                self.finish(context, text);
            }
        }
    }

    fn flip_page(&self, dir: Dir) {
        self.cands.borrow_mut().flip(dir);
    }

    /// 显示本页第 `nth` 条候选的码位键位图（`Ctrl+Shift+N`，§6.7）。
    ///
    /// **码的优先级**（见 [`crate::keycap`]）：候选自己的 `code` → 当前缓冲。
    /// 拼音段能拿到「好 → vbg」这种反查码；五笔段没有码提示，用户手上那串字母
    /// 本身就是码。
    fn show_keycaps(&self, nth: usize) {
        // 一次借用里把三样东西取出来就立刻放开：`keycap.show` 要建窗、要量字，
        // 期间若还持有 `cands` 的借用，任何一次回调重入都会 panic。
        let (code, anchor, flipped) = {
            let c = self.cands.borrow();
            let Some(anchor) = c.rect() else {
                tsf_debug!("#{} 候选窗没显示，键位图不弹", self.seq);
                return;
            };
            let code = match c.code_on_page(nth) {
                Some(code) if !code.is_empty() => code,
                Some(_) => self.buffer.borrow().clone(),
                None => {
                    tsf_debug!("#{} 本页没有第 {} 条", self.seq, nth + 1);
                    return;
                }
            };
            (code, anchor, c.flipped())
        };
        if code.is_empty() {
            tsf_debug!("#{} 手上没有码，键位图不弹", self.seq);
            return;
        }
        tsf_info!("#{} 键位图：第 {} 条 → {code}", self.seq, nth + 1);
        self.keycaps.borrow_mut().show(&code, anchor, flipped);
    }

    /// 候选窗与键位图一起收掉。
    ///
    /// 键位图贴在候选窗下沿——候选窗没了它还留着，就是一个飘在屏幕上、
    /// 与任何东西都对不上的空框。
    fn clear_popups(&self) {
        self.cands.borrow_mut().clear();
        self.keycaps.borrow_mut().hide();
    }

    /// 候选窗报上来的事件。**跑在窗口过程里**，不在任何 TSF 回调栈上。
    fn on_candidate_event(&self, event: Event) {
        match event {
            Event::Click(Hit::Candidate(global)) => {
                // 只借一下取值，绝不把借用带进 `finish`——那会二次借同一个
                // `RefCell`，panic 一旦跨出 wndproc 就是宿主进程 abort。
                let word = self.cands.try_borrow().ok().and_then(|c| c.word_at(global));
                let code = self.buffer.borrow().clone();
                let ctx = self.active_ctx.borrow().clone();
                let Some(ctx) = ctx else {
                    self.clear_popups();
                    return;
                };
                match word {
                    Some(word) => {
                        tsf_info!("#{} 点选第 {} 条 {code} → {word}", self.seq, global + 1);
                        self.clear_buffer();
                        self.finish(&ctx, word);
                    }
                    None => {
                        tsf_info!("#{} 点选的第 {} 条不存在，忽略", self.seq, global + 1);
                    }
                }
            }
            // 箭头只翻页、不选词（D39）。
            Event::Click(Hit::Arrow(dir)) => {
                if let Ok(mut cands) = self.cands.try_borrow_mut() {
                    cands.flip(dir);
                }
            }
            // 分层窗口更新时系统可能同步发 DPI 变化；这时重画会重入窗口过程，
            // 所以只记日志，位置留给下一次按键重新算（每次 `prepare` 都重新取
            // 光标所在屏的 DPI，最多滞后一个键）。
            Event::DpiChanged(dpi) => {
                tsf_debug!("#{} 显示器 DPI 变为 {dpi}，下一个键重新定位", self.seq);
            }
        }
    }

    /// 让组合串显示当前缓冲（缓冲空则结束组合串）。
    fn sync_preedit(&self, context: &ITfContext) {
        let text = self.buffer.borrow().clone();
        self.run_preedit(context, Goal::Show(text));
    }

    /// 把缓冲提交上屏（空格 / 回车）。
    fn commit(&self, context: &ITfContext) {
        let code = self.clear_buffer();
        if code.is_empty() {
            return;
        }
        let text = self.resolve(&code);
        self.finish(context, text);
    }

    /// 这次输入成功结束：关掉候选窗，把文字写进文档。
    ///
    /// **所有**「上屏」路径都要走它（包括点选与数字键），这样「窗口什么时候消失」
    /// 只有一个答案，不会出现「字上去了、窗口还挂着」的残影。
    fn finish(&self, context: &ITfContext, text: String) {
        self.clear_popups();
        self.run_preedit(context, Goal::Finish(Some(text)));
    }

    /// 这次输入被取消：关窗、抹掉组合串、不留残留。
    fn abort(&self, context: &ITfContext) {
        self.clear_popups();
        self.run_preedit(context, Goal::Finish(None));
    }

    /// 编码 → 上屏文字。**任何路径都必须返回一段文字**。
    ///
    /// 四种情况：
    ///
    /// | 情况 | 结果 |
    /// |---|---|
    /// | 窗口里正显示着候选 | 当前候选（空格上屏的就是高亮那条，与数字键、点选同一份数据） |
    /// | 缓存/查询命中 | 首候选（窗口已关的情况：候选窗建不出来时的降级） |
    /// | 查到了但没这个词 | 编码原样上屏（D27：吃掉的键必须变成输出） |
    /// | server 不可用 / 超时 | 同样原样上屏——比丢字好，也比卡住好 |
    ///
    /// 最后一行是 D18 的落地形态：**宁可这一次降级成英文，绝不冻住宿主**。
    ///
    /// 只有**前两行**会 `ipc::record` 上报学习数据（§5.1：只统计上屏，而且必须是
    /// 用户真的选了一个词）。后两行吐的是编码字母，那不是选择，记进去只会把 n 灌成噪声。
    fn resolve(&self, code: &str) -> String {
        // 先问窗口：它手里那批候选正是用户刚刚看着的那一屏，用它才谈得上
        // 「所见即所得」。取完立刻离开借用，后面的 IPC 可能慢。
        let shown = self.cands.borrow().current();
        if let Some(word) = shown {
            tsf_info!("#{} 提交 {code} → {word}（候选窗）", self.seq);
            ipc::record(code, &word);
            return word;
        }
        match ipc::query(code) {
            Some(reply) if !reply.words.is_empty() => {
                let word = reply.words[0].clone();
                tsf_info!("#{} 提交 {code} → {word}", self.seq);
                ipc::record(code, &word);
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
        // 窗口先撤，再撤组合串：**顺序反了会看到残影**——组合串没了、窗口还挂着
        // 一屏候选，而此刻缓冲已空，再按数字键什么也不会发生。
        self.clear_popups();
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

        // 幂等：TSF 可能先走 `Activate` 再走 `ActivateEx`（后者转调前者）。
        // 重复初始化会重复挂 sink，并把语言栏条目 `AddItem` 两次——两个同 GUID
        // 条目并存时，一次点击翻两次模式（看起来"点了没反应"），图标也会被系统
        // 在两条之间来回顶掉。
        if self.thread_mgr_cookie.get() != TF_INVALID_COOKIE {
            tsf_debug!(
                "#{} 重复 Activate（cookie={}），跳过初始化",
                self.seq,
                self.thread_mgr_cookie.get()
            );
            return Ok(());
        }

        unsafe {
            let thread_mgr = ptim.ok()?;
            // 每一步失败都标注阶段名：宿主里「激活了却没功能」只有这份日志可查。
            let stage = |name: &'static str, e: windows::core::Error| -> windows::core::Error {
                tsf_error!("#{} Activate·{name} 失败：{e}", self.seq);
                e
            };

            // 焦点 sink：用来在切窗口时清缓冲，否则在 A 窗口打的字会跑到 B 窗口（预研坑 #5）。
            let source: ITfSource = thread_mgr.cast()?;
            let cookie = source
                .AdviseSink(&ITfThreadMgrEventSink::IID, self.as_interface_ref())
                .map_err(|e| stage("挂焦点 sink", e))?;
            self.thread_mgr_cookie.set(cookie);

            // 按键 sink：唯一真正关心按键的地方。
            let keystroke_mgr: ITfKeystrokeMgr = thread_mgr.cast()?;
            keystroke_mgr
                .AdviseKeyEventSink(tid, self.as_interface_ref(), true)
                .map_err(|e| stage("挂按键 sink", e))?;
            self.keysink_advertised.set(true);

            // 组合串 sink：`StartComposition` 要一个 `ITfCompositionSink`，
            // 用来在「组合串被别人终止」时通知我们清状态（见 OnCompositionTerminated）。
            // `as_interface_ref()` 是泛型方法（要说明「把哪个接口当 IUnknown」），
            // 这里显式写全类型，随后 AddRef 取一份可长期持有的引用。
            let self_unknown: IUnknown =
                ComObjectInterface::<IUnknown>::as_interface_ref(self).to_owned();
            let comp_sink: ITfCompositionSink = self_unknown.cast()?;
            *self.comp_sink.borrow_mut() = Some(comp_sink);

            // 语言栏按钮：任务栏角标入口（左键切中英，右键菜单开设置）。
            // 失败只记日志——这个按钮是加分项，挂不上不该拖死整个输入法。
            match self.init_langbar(thread_mgr) {
                Ok(()) => {}
                Err(e) => tsf_error!("#{} 语言栏按钮挂载失败：{e}", self.seq),
            }

            *self.thread_mgr.borrow_mut() = Some(thread_mgr.clone());
        }

        // IPC：启动后台连接线程（幂等），并趁机把 server 上的全局模式取回来。
        // 这两件事都发生在 `Activate`——切到某个输入框的瞬间，离真正落键还有
        // 几百毫秒，冷启动 server 与首次握手正好挤在这段空隙里完成。
        ipc::warm_up();
        if let Some(chinese) = ipc::current_mode() {
            let next = if chinese {
                Mode::Chinese
            } else {
                Mode::English
            };
            if next != self.mode.get() {
                self.mode.set(next);
                // server 上的全局模式优先：图标与系统 compartment 都得跟着改，
                // 否则角标还停在旧模式的字样上，要按一下 Shift 才纠正。
                self.notify_langbar();
            }
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

    /// 挂语言栏按钮。`thread_mgr` 还活着时才能调（`AddItem` 要从它拿管理器）。
    fn init_langbar(&self, tim: &ITfThreadMgr) -> Result<()> {
        unsafe {
            let mgr: ITfLangBarItemMgr = tim.cast()?;

            // 防双条目：同一个 GUID 在语言栏里只能有一条。若之前那条还在
            // （`Activate` 与 `ActivateEx` 都进来过、或上次 Deactivate 没摘干净），
            // 先摘掉——两个同 GUID 条目并存时，一次点击会翻两次模式（点了没反应），
            // 图标也会被系统在两条之间来回顶。
            if let Some(old) = self.langbar.borrow_mut().take() {
                tsf_error!(
                    "#{} 语言栏：发现残留的旧条目，先摘除（重复 Activate？）",
                    self.seq
                );
                let old_item: ITfLangBarItemButton = old.to_interface();
                if let Err(e) = mgr.RemoveItem(&old_item) {
                    tsf_error!("#{} 语言栏：摘除旧条目失败：{e}", self.seq);
                }
            }

            let obj = windows::core::ComObject::new(langbar::LangBarButton::new(self.mode.clone()));

            // 输入模式 compartment：Windows 靠它判断「当前是中文还是英文」。
            // 不同步的话系统托盘指示器会按旧值重画角标格，把我们的图标顶掉
            // （真机表现：按几下 Shift 图标消失）。缺了只记日志——它不该
            // 挡住整个语言栏按钮的挂载。
            match tim.cast::<ITfCompartmentMgr>() {
                Ok(cm) => {
                    match cm.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION) {
                        Ok(compartment) => obj.set_compartment(compartment),
                        Err(e) => {
                            tsf_error!("#{} 取输入模式 compartment 失败：{e}", self.seq)
                        }
                    }
                }
                Err(e) => tsf_error!("#{} 拿不到 ITfCompartmentMgr：{e}", self.seq),
            }

            let item: ITfLangBarItemButton = obj.to_interface();
            mgr.AddItem(&item)?;
            tsf_info!(
                "#{} 语言栏：AddItem 成功；该格当前持有者 {}",
                self.seq,
                langbar::describe_guid_owner(&mgr)
            );
            // 与小狼毫一致：挂上后明确声明「要显示」，把 HIDDEN 位清掉。
            // 这一步顺带把当前模式写进 compartment（`notify` 里做）。
            obj.set_shown(true);
            // 初始对齐：`set_shown` 在状态没变时不会发通知，而 compartment 里
            // 可能还留着上一个宿主的模式，这里显式同步一次。
            obj.notify();
            *self.langbar.borrow_mut() = Some(obj);
        }
        Ok(())
    }

    fn deactivate_inner(&self) -> Result<()> {
        let Some(thread_mgr) = self.thread_mgr.borrow_mut().take() else {
            // 没激活过就 Deactivate：TSF 允许这么调。
            return Ok(());
        };

        // 先摘语言栏按钮——必须在 thread_mgr 还活着时做。
        if let Some(obj) = self.langbar.borrow_mut().take() {
            unsafe {
                match thread_mgr.cast::<ITfLangBarItemMgr>() {
                    Ok(mgr) => {
                        let item: ITfLangBarItemButton = obj.to_interface();
                        if let Err(e) = mgr.RemoveItem(&item) {
                            tsf_error!("#{} RemoveItem 失败：{e}", self.seq);
                        } else {
                            tsf_info!("#{} 语言栏：RemoveItem 成功", self.seq);
                        }
                    }
                    Err(e) => tsf_error!("#{} 取 ITfLangBarItemMgr 失败：{e}", self.seq),
                }
            }
        }

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
        self.clear_popups();
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
            // 但失败原因必须留下——否则「激活了却没功能」无从排查。
            if let Err(e) = self.activate_inner(ptim, tid) {
                tsf_error!("#{} Activate 内部失败：{e}", self.seq);
            }
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
            // 组合串没了，候选窗也得跟着撤：否则屏幕上留着一屏对着「已经不存在的
            // 编码」的候选，点它还会往文档里写字。
            self.clear_popups();
            tsf_info!("#{seq} 组合串被外部终止，已删除残留 {dropped:?}");
            Ok(())
        })
    }
}

/// 系统设置页上那个「选项」按钮的落点。
///
/// ## 它决定按钮**存不存在**
///
/// Windows 的输入法管理界面按微软文档的约定 `CoCreateInstance(CLSID,
/// IID_ITfFnConfigure)` 拿这个接口：**拿得到就显示「选项」，拿不到就不显示**。
/// v1.42 之前「删除」左边那块空白不是我们漏画，是系统根本不知道我们能配置。
/// 实测对照：微软拼音 `{B115690A…}`、微软五笔 `{a028ae76…}` 在这个 IID 上都是
/// `S_OK`，它们都有「选项」；我们那时是 `E_NOINTERFACE`。
///
/// ## 为什么放在文本服务对象上，而不是单独做一个 COM 类
///
/// 系统要的是「同一个 CLSID 上多问一个 IID」，不是第二个类。放在这里，
/// `DllGetClassObject` → `ClassFactory::CreateInstance` → `QueryInterface`
/// 这条既有链路一行都不用改，也不必给注册器加一次注册。
impl ITfFunction_Impl for TextService_Impl {
    /// 系统若把这个函数名显示出来（旧版「文本服务和输入语言」的属性对话框会），
    /// 用户看到的应当是「亨利五笔 设置」这样的词，而不是类名。
    fn GetDisplayName(&self) -> Result<BSTR> {
        Ok(BSTR::from(format!(
            "{} · 设置",
            heinrich_tip_id::DISPLAY_NAME
        )))
    }
}

/// `Show` 只做一件事：把设置窗口叫到用户面前。
///
/// **无论失败与否都返回 `S_OK`**。理由有两条：
/// ① 系统这边只是「打开配置界面」，我们失败时它没有任何补救手段，弹一个
///    错误框只是把失败转嫁给用户；
/// ② 这个调用发生在 SystemSettings 的 UI 线程上，返回错误码可能让它显示
///    「无法打开设置」的通用提示——而真正的失败原因（exe 不在、被拦截）
///    只有写进 tsf.log 才查得动。
impl ITfFnConfigure_Impl for TextService_Impl {
    fn Show(&self, _hwndparent: HWND, langid: u16, _rguidprofile: *const GUID) -> Result<()> {
        tsf_info!(
            "#{} 系统设置请求打开配置窗口（langid=0x{langid:04X}）",
            self.seq
        );
        guarded("ITfFnConfigure::Show", Ok(()), || {
            // 不把父窗口传下去：设置窗是独立顶层窗口，自己管自己的 Z 序与焦点；
            // 认成一个可能随时被销毁的 SystemSettings 子窗口反而会跟着它一起消失。
            if let Err(e) = crate::launch::open_settings() {
                tsf_error!("#{} 打开设置窗口失败：{e}", self.seq);
            }
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
        Action::Select(_) => "select",
        Action::Keycaps(_) => "keycaps",
        Action::PagePrev => "page-prev",
        Action::PageNext => "page-next",
        Action::Noop => "noop",
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
