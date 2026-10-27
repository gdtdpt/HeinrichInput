//! 按键判定 —— **纯逻辑，不碰任何 Windows API**。
//!
//! 这个模块的存在理由是 M1.0 真机日志里发现的一条事实：
//!
//! > 同一个 DLL，Notepad++（Scintilla）只回调 `OnTestKeyDown` / `OnTestKeyUp`，
//! > WorkBuddy（Electron/Chromium）只回调 `OnKeyDown` / `OnKeyUp`。
//!
//! 两种宿主走的是**互斥的两条路径**。所以「吃不吃这个键」必须由同一个函数回答，
//! 而且这个函数**不能有副作用、不能依赖任何外部状态**——`OnTestKeyDown` 说「要」
//! 的那一刻键就已经被吃掉了，事后没有反悔的机会（dev-guide 踩坑 #12）。
//!
//! 把它做成纯函数还有一层好处：本文件可以在任意平台上跑单测。

/// 中英模式。单击 Shift 切换。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Chinese,
    English,
}

/// 按键瞬间的修饰键快照。
///
/// 由调用方用 `GetKeyState` 取，再传进来——**不在这里读**，否则本模块就没法单测了。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// 判定结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// 不接管，交还宿主。
    Pass,
    /// 吃掉并把这个字母追加到编码缓冲。
    Buffer(char),
    /// 吃掉并把缓冲上屏。
    Commit,
    /// 吃掉并丢弃缓冲。
    Cancel,
    /// 吃掉并退回一个字符。
    Backspace,
    /// 吃掉，把缓冲（有的话）上屏后紧跟这个全角标点。
    Punct(char),
    /// 吃掉并选**本页**第 n 条候选（0 起，数字键 1–9）。
    Select(usize),
    /// 吃掉并显示**本页**第 n 条候选的码位键位图（0 起，`Ctrl+Shift+1..9`）。
    ///
    /// 与 [`Action::Select`] 分开而不是加个布尔参数：两者做的事完全不同
    /// （一个上屏、一个只画参考图），混在一个变体里早晚会有人写错分支。
    Keycaps(usize),
    /// 吃掉并往前翻一页。已经首页时调用方什么也不做（D38）。
    PagePrev,
    /// 吃掉并往后翻一页。
    PageNext,
    /// 吃掉但什么也不做。用于「按住不放会出事、交还宿主更糟」的键。
    Noop,
}

/// 判定所需的**实例状态**。
///
/// 和 `mods` 分开传，是因为它们来源不同：`Input` 是留住的状态（缓冲、页码），
/// `mods` 是按键那一瞬间采样出来的物理键态。混成一个结构体，测试时容易写出
/// 「状态是这份、修饰键是那份」的两份真相。
///
/// 打成一个结构体而不是三个参数：`decide` 是「吃不吃这个键」的唯一答案来源，
/// 参数一多，两条回调路径（`OnTestKey*` / `OnKey*`）漏传一个就会得到不同的答案，
/// 那正是 dev-guide 第 12 条要防的事故。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Input {
    pub mode: Mode,
    /// 编码缓冲里有几个字符。
    pub buffer_len: usize,
    /// 候选窗这一页上有几条候选。`0` = 没有候选窗。
    ///
    /// 不是「有没有候选」的布尔——数字键要判「按 7 时本页真有第 7 条吗」，
    /// 所以这里给的是**本页条数**。
    pub page_len: usize,
    /// 服务端装了拼音词库（协议能力位 `feature::PINYIN`）。
    ///
    /// 它改变**两条**规则，两条都是「五笔的约束在拼音下不成立」：
    ///
    /// 1. 码长上限从 4 放开：拼音是变长的，`linchuangzhengzhuang` 有 20 个字母。
    /// 2. `'` 才进缓冲——没有拼音词库时它没有任何含义（§6.5）。
    ///
    /// **`z` 不再由它决定**（M3.5 / D62）：`z` 一律进缓冲，由 server 解释成
    /// 拼音声母还是末位通配。这一位必须由 server 告诉 DLL，DLL 自己猜不出来。
    pub pinyin: bool,
}

impl Input {
    /// 此刻窗口里有没有候选。数字键与方向键的归属就看它。
    fn has_candidates(&self) -> bool {
        self.page_len > 0
    }
}

/// 测试用的构造器。只在测试里用得到——生产代码直接写字面量，避免多一层间接。
#[cfg(test)]
impl Input {
    /// 空缓冲、无候选的初始态。
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            buffer_len: 0,
            page_len: 0,
            pinyin: false,
        }
    }

    pub fn with_buffer(mut self, len: usize) -> Self {
        self.buffer_len = len;
        self
    }

    pub fn with_page(mut self, len: usize) -> Self {
        self.page_len = len;
        self
    }

    /// 模拟「服务端装了拼音词库」。
    pub fn with_pinyin(mut self) -> Self {
        self.pinyin = true;
        self
    }
}

/// 五笔 86 的字根键：`a`–`y`，共 25 个。
///
/// `z` 不在其中——86 版的 `z` 是「万能学习键」。它的语义在 M3.5 落地（D62）：
/// **组合态内进缓冲**，由 server 判断它这一段是拼音声母（`zh…`）还是末位通配
/// （`vbz` = 「`vb` 开头的字」）。
///
/// 早先（D42）它是「吃掉不动」，理由是「z 不是字根键，进了缓冲没人解释」。
/// 现在有人解释了，所以那一版的前提失效——但它留下的两条例外仍然成立：
/// 空缓冲放行（没有组合串可打断）、`Shift+z` 放行（临时英文要出大写）。
pub const VK_A: u16 = 0x41;
pub const VK_Y: u16 = 0x59;

/// 万能学习键，见 `VK_A` 上面的说明。
pub const VK_Z: u16 = 0x5A;

/// 单字全码最长 4 码（D15：3 或 4 码，不足四字根补末笔识别码）。
pub const MAX_CODE_LEN: usize = 4;

/// 混输（五笔 + 拼音）时的码长上限。
///
/// 拼音是变长的：`data/pinyin` 里最长的词条是 `linchuangzhengzhuang`（临床症状），
/// 20 个字母。留 4 个字母余量到 24——再长就不可能是哪个词的全拼，
/// 当成「打过头了」原样上屏，与五笔满 4 码的处理一致。
pub const MAX_PINYIN_CODE_LEN: usize = 24;

/// 编译期就把「上限必须覆盖最长词条」钉住：万一有人为了省事把它改回 4，
/// 这条断言会在编译时炸，而不是等真机上打 `linchuang…` 打到一半被上屏。
const _: () = assert!(MAX_PINYIN_CODE_LEN >= 20);

/// 含多段标记或通配时的缓冲上限（M3.5）。
///
/// 两段各 4 码加一个 `'` 正好 9，`vbgv'vbgv` 这类打满的组合也装得下。
/// 只在**缓冲里出现过 `'`、或正要输入的字符是 `'` / `z`** 时启用：
/// 否则纯五笔「满 4 码就原样上屏」会被悄悄放宽，打错 5 个字母不再自动收场。
pub const MAX_EXTENDED_CODE_LEN: usize = MAX_CODE_LEN * 2 + 1;

// 关系用编译期断言钉住，不写成单测里的 `assert!`：常量之间的比较没有运行时信息，
// clippy 会直接判 `assertions_on_constants`，而在 const 块里失败反而更早暴露。
const _: () = assert!(MAX_EXTENDED_CODE_LEN > MAX_CODE_LEN);
const _: () = assert!(MAX_EXTENDED_CODE_LEN < MAX_PINYIN_CODE_LEN);

/// 当前的码长上限。`pinyin` 指服务端是否装了拼音词库。
pub fn max_code_len(pinyin: bool) -> usize {
    if pinyin {
        MAX_PINYIN_CODE_LEN
    } else {
        MAX_CODE_LEN
    }
}

/// Shift 有三个 vkey：通用 `VK_SHIFT`、左 `VK_LSHIFT`、右 `VK_RSHIFT`，
/// 宿主给哪个都可能。
pub const VK_SHIFT: u16 = 0x10;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RSHIFT: u16 = 0xA1;

pub fn is_shift_key(vk: u16) -> bool {
    vk == VK_SHIFT || vk == VK_LSHIFT || vk == VK_RSHIFT
}

/// OEM 键：`-`（下划线键）与 `=`（加号键）。**有候选时是翻页键**。
///
/// 这是五笔输入法用户的肌肉记忆：`-` 往前、`=` 往后。M2.3 只接了 `←/→/PgUp/PgDn`，
/// 而笔记本键盘上 PgUp/PgDn 常要按 Fn，方向键又离手位太远——真机上等于没有翻页。
pub const VK_OEM_MINUS: u16 = 0xBD;
pub const VK_OEM_PLUS: u16 = 0xBB;

/// 单引号。混输下它是**显式拼音标记**（§6.5 / D48）：`hao'` = 只要拼音候选。
pub const VK_QUOTE: u16 = 0xDE;

/// Tab。组合态内吃掉，见 `decide` 里那一支。
pub const VK_TAB: u16 = 0x09;

/// vkey → 五笔码字母。在 `a`–`y` 之外返回 `None`。
pub fn wubi_letter(vk: u16) -> Option<char> {
    if (VK_A..=VK_Y).contains(&vk) {
        Some((b'a' + (vk - VK_A) as u8) as char)
    } else {
        None
    }
}

/// 中文模式下要接管的标点：`vkey + 是否按住 Shift` → 全角字符。
///
/// ## 为什么标点必须由我们吃掉
///
/// 放行给宿主的后果是**丢字**：宿主在组合串中间插入半角符号会终止组合串，
/// 我们那串 `vbg` 就被 `OnCompositionTerminated` 当残留删掉了——用户敲了标点，
/// 想打的字没了。而「先上屏再让宿主出标点」在 TSF 里表达不出来：
/// `OnTestKeyDown` 必须在不知道后果的时候就回答吃不吃，吃了就收不到 `OnKeyDown`
/// 的后续机会（dev-guide 踩坑 #12）。所以只能整个吃掉、自己把标点补出来。
///
/// ## 为什么是表驱动而不是 `ToUnicodeEx`
///
/// `ToUnicodeEx` 能跟随键盘布局，但它有副作用：碰到死键（`^`、`` ` `` 之类）会
/// **改掉进程的死键状态**，而我们会在 `OnTestKeyDown` 与 `OnKeyDown` 里各调一次，
/// 等于把死键吃掉两遍。表驱动的另一个好处是本模块保持纯函数、可在任意平台单测。
/// 代价是非 US 布局的 OEM 键位置不同——记在待验证项里，中国市场的默认布局是 US。
pub fn punct_text(vk: u16, shift: bool) -> Option<char> {
    match (vk, shift) {
        // 逗号 / 句号
        (0xBC, false) => Some('，'),
        (0xBC, true) => Some('《'),
        (0xBE, false) => Some('。'),
        (0xBE, true) => Some('》'),
        // 分号 / 冒号
        (0xBA, false) => Some('；'),
        (0xBA, true) => Some('：'),
        // 斜杠 → 顿号（中文输入法的惯例）/ 问号
        (0xBF, false) => Some('、'),
        (0xBF, true) => Some('？'),
        // Shift + 数字：叹号与圆括号
        (0x31, true) => Some('！'),
        (0x39, true) => Some('（'),
        (0x30, true) => Some('）'),
        // Shift + 6 → 单个省略号（U+2026）。主流输入法这里给的是「……」两个，
        // 用户（D96）明确要半个：要长省略号自己按两次，反过来则拆不开。
        (0x36, true) => Some('…'),
        // 单引号（0xDE）**不在这张表里**：混输下它是显式拼音标记（D48），
        // 由 `decide` 单独接管。`-` / `=`（0xBD / 0xBB）同理——有候选时是翻页键。
        // 其余 OEM 键（`[` `]` `\` `` ` ``）在空缓冲下放行：多数字体下用户要的是
        // 半角原文（写代码、写路径），组合态内则由 `is_text_key` 统一吞掉。
        _ => None,
    }
}

/// 这个键会不会产生一个**可见字符**。
///
/// 用来兜住「组合态内按了个别的字符键」：这类键放行给宿主的唯一后果是宿主在
/// 组合串中间插一个字符、终止组合串，`OnCompositionTerminated` 把打了一半的编码
/// 当残留删掉。用户看到的不是「多了个符号」，而是「正在打的字凭空消失」。
///
/// 只列举**字符键**，不碰功能键：F1–F12、Win、CapsLock 在组合态里仍有
/// 它们自己的含义（刷新、切窗口），吞掉会让用户以为键盘坏了。
///
/// **Tab 不在这张表里，但结论是「吞」**——它走 `decide` 里单独一支。
/// 这张表按「这个键会不会往文档里插字符」分类，而 Tab 的判决依据是
/// 「这个键会不会让组合串消失」，两件事不是同一个判据（见 D49 的修订）。
pub fn is_text_key(vk: u16) -> bool {
    matches!(
        vk,
        // 主键区数字 0–9
        0x30..=0x39
        // 小键盘：数字与 + - * / .
        | 0x60..=0x6F
        // OEM 标点区：; : = , - . / ` 与 Shift 后的配对符号
        | 0xBA..=0xC0
        // [ \ ] ' 与 OEM102
        | 0xDB..=0xDF
        | 0xE2
    )
}

/// 判定一个按键该不该被接管。
///
/// 参数全部来自本进程：`input` 里除 `mods` 外都是实例状态，`mods` 由调用方从
/// `GetKeyState` 取。没有任何跨进程调用——这是 D18「按键路径零阻塞」的前提。
///
/// ## 数字键与方向键只在**有候选**时接管
///
/// 没有候选窗的时候，`1`–`9` 是老老实实的数字、方向键是老实的光标移动，谁都不该
/// 被输入法吃掉。只有组合态（窗口里有候选）时它们才变成「选第 n 条」与「翻页」：
/// 那一刻用户的意图已经被组合串与前缀锁定了，放行反而会破坏组合（D38）。
pub fn decide(input: Input, vk: u16, mods: Mods) -> Action {
    // Ctrl+Alt+1..9：调出本页第 n 条的**码位键位图**（§6.7 / D50）。
    //
    // ## 为什么是 Ctrl+Alt 而不是 Ctrl+Shift（D61）
    //
    // v1.28 用的是 `Ctrl+Shift+1..9`，真机上第一次按就把输入法切走了：`Ctrl+Shift`
    // 是 Windows「在输入语言之间切换」的经典热键，老系统与装了多个输入法/键盘布局
    // 的机器上仍然生效。用户按完弹图，手一松输入法已经换人——比不弹糟糕得多。
    // `Ctrl+Alt` 没有系统级绑定（AltGr 只存在于有 AltGr 的拉丁键盘布局，中文环境
    // 用的是美式/中文布局，两者都没有 AltGr 键）。
    //
    // 必须坐在最前面。下面那条 `if mods.ctrl || mods.alt { Action::Pass }` 会把
    // 所有带 Ctrl 的键一律放行——写在它后面等于写了一段死代码。**而且**它还得
    // 排在「数字键 = 选字」之前，那一支不看修饰键（D38）。两条顺序要求合起来，
    // 只剩这一个位置。
    //
    // 只在有候选窗时接管：没有候选窗，用户手上没有「正在打的码」，键位图无从
    // 展开，`Ctrl+Alt+1` 就该还给宿主。英文模式同理——那一模式下一个键都不该吃。
    //
    // 超出本页条数时**吃掉不动**（`Noop`）而不是放行：放行会让宿主在组合串中间
    // 插字符、终止组合串，把打了一半的编码弄丢——与数字键同一取舍（D38）。
    if mods.ctrl
        && mods.alt
        && !mods.shift
        && input.mode == Mode::Chinese
        && input.has_candidates()
        && (0x31..=0x39).contains(&vk)
    {
        let nth = (vk - 0x31) as usize;
        return if nth < input.page_len {
            Action::Keycaps(nth)
        } else {
            Action::Noop
        };
    }

    // Ctrl / Alt 组合一律放行：Ctrl+C、Ctrl+V、Alt+Tab 被输入法吞掉是灾难性的，
    // 而且用户对它们有强预期。
    if mods.ctrl || mods.alt {
        return Action::Pass;
    }
    if input.mode == Mode::English {
        return Action::Pass;
    }

    let empty = input.buffer_len == 0;

    // `z`：万能学习键（M3.5 / D62）。组合态内**一律进缓冲**，由 server 解释成
    // 拼音声母（`zh…`）还是末位通配（`vbz` = 「`vb` 开头的字」）。
    //
    // D42 原本让它「吃掉不动」，理由是「`z` 不是字根键，进了缓冲没人解释」。
    // 现在 server 会解释，那条理由失效——而「吃掉不动」的代价是 `vbz` 永远打不出来。
    //
    // 两条例外放行：**没装拼音词库时空缓冲**、`Shift+z`（临时英文，要输出大写 `Z`）。
    // 组合态内的 `z` 一律不放行——放行会让宿主在组合串中间插字符、终止组合串，
    // 把已经打出的编码当残留删掉。
    //
    // **装了拼音时空缓冲也必须进缓冲**（v1.64，真机 `zhenbang` → 只查到「henbang」
    // → 出了「很棒」）。D42 说「空缓冲时没有组合串可打断，`z` 就是个普通字母」，
    // 那个前提只在「`z` 不可能是一个码的首键」时成立：五笔里它确实不是字根键，
    // 而拼音里 `zhe` / `zhang` / `zuo` / `zai` 全以它开头。放行的后果是 `z` 被宿主
    // 当普通字符上屏、拼音里少一个字母——用户看到的是「拼音匹配错了字」。
    // 没装拼音时保持 D42：进缓冲只会挂一个永远查不到东西的组合串。
    //
    // D97：判据不再看 Shift。旧代码 `!mods.shift` 时 Shift+z 从这里漏过去、一路
    // Pass 给宿主——组合态内按 Shift+z 同样会把大写 `Z` 插进组合串中间（与
    // Shift+其他字母同一个洞）。现在空缓冲无拼音的临时英文照旧放行，其余一律进缓冲；
    // D98 起 Shift+z 进缓冲的是**大写 `Z`**——大写不参与匹配（见 `decide` 字母
    // 分支的说明），单击 Shift 后与整串一起原样上屏。
    if vk == VK_Z {
        let ch = if mods.shift { 'Z' } else { 'z' };
        return if empty && !input.pinyin {
            Action::Pass
        } else {
            Action::Buffer(ch)
        };
    }

    if let Some(ch) = wubi_letter(vk) {
        // 空缓冲 + Shift：临时英文逃生口（用户拍板保留，D97）——没有组合串可保护，
        // 直接出大写比「先进缓冲再说」更贴近按键意图。
        //
        // **缓冲非空时不再放行**（D97）：放行会让宿主把大写字母插进组合串中间，
        // TSF 终止组合串，`OnCompositionTerminated` 把编码当残留删掉——用户看到的
        // 是「候选窗出来后按 Shift 打字母，大写直接上屏、光标跳走」。中文态从
        // 此没有任何键能绕过缓冲直接上屏。
        //
        // D98 修订：进缓冲的是**大写原字符**，不是降成小写——用户要的是「`gA` 都
        // 进缓冲但匹配不到候选，单击 Shift 后 `gA` 原样上屏」。v1.77 把它降成
        // 小写，结果 `gA` 被当 `ga` 匹配出了候选，恰好违背了这个意图。大写不参与
        // 匹配由 server 侧把守（`heinrich-core` 的 `query_with_limit` 入口对大写
        // 一律空手而归），DLL 只负责把字符原样收进缓冲。
        if mods.shift && empty {
            return Action::Pass;
        }
        return Action::Buffer(if mods.shift {
            ch.to_ascii_uppercase()
        } else {
            ch
        });
    }

    // 单引号 = 显式拼音标记（§6.5 / D48）。只有装了拼音词库它才有意义：
    // 让 `hao'` 把「虚」这类五笔候选全部让开，只留拼音。
    //
    // 空缓冲时放行——没有组合串可打断，用户就是要打一个 `'`。
    // 没装拼音词库时它跟别的符号一样被下面那条兜住，不放行也不进缓冲：
    // 进了缓冲也没人解释它的含义，放行则会在组合串中间插字符、把编码弄丢。
    if vk == VK_QUOTE && !empty && !mods.shift && input.pinyin {
        return Action::Buffer('\'');
    }

    // 标点：中文模式下由我们接手，保证「字 + 标点」一起出、不丢编码。
    // 放在字母之后、编辑键之前——两组 vkey 不重叠，顺序只影响可读性。
    // 也放在数字键之前：`Shift+1` 是「！」，不是「选第 1 条」。
    if let Some(p) = punct_text(vk, mods.shift) {
        return Action::Punct(p);
    }

    // 数字键：本页有第 n 条就选它，没有就吃掉不动。
    //
    // 「没有就吃掉不动」不是偷懒：这时把 `7` 放行给宿主，宿主会在组合串中间插入
    // 半角数字、终止组合串，`OnCompositionTerminated` 把我们那串 `vbg` 当残留删掉——
    // 用户按了个不存在的候选号，结果是打了一半的字没了。
    if input.has_candidates() {
        if (0x31..=0x39).contains(&vk) {
            let nth = (vk - 0x31) as usize;
            return if nth < input.page_len {
                Action::Select(nth)
            } else {
                Action::Noop
            };
        }
        match vk {
            // ← / PgUp / `-` 往前，→ / PgDn / `=` 往后（D38）。
            // 翻到头由调用方判空。
            0x25 | 0x21 | VK_OEM_MINUS => return Action::PagePrev,
            0x27 | 0x22 | VK_OEM_PLUS => return Action::PageNext,
            // ↑ / ↓ 暂时没有纵向导航可用（单行布局），但也不能放行——光标一动
            // 组合串就停在原地。吃掉、不动，是这里唯一不会留下错位的选择。
            0x26 | 0x28 => return Action::Noop,
            _ => {}
        }
    }

    match vk {
        // 空格 / 回车：有编码才提交，空缓冲时是正常空格 / 换行。
        0x20 | 0x0D if !empty => Action::Commit,
        // Esc：有编码才取消，空缓冲时交给宿主（很多程序用它关窗口）。
        0x1B if !empty => Action::Cancel,
        // 退格：空缓冲时交还宿主，否则会删不掉文档里已有的字。
        0x08 if !empty => Action::Backspace,
        // 剩下的方向键 / Home / End（有候选时 `←→↑↓` 与 PgUp/PgDn 已被上面接走）。
        // 组合态下光标一动，组合串就停在原地，下一个字母还会往那个旧位置写，
        // 屏幕上出现「光标在这、字却在那」的错位。没有候选窗时（M1.2 起的老行为）
        // 折中成「吃掉并取消」：代价是一次方向键被吞，换来绝不留下错位的组合串。
        0x21..=0x28 if !empty => Action::Cancel,
        // Tab：组合态内吃掉，不动。
        //
        // D49 最初把 Tab 归进「放行也不要紧的功能键」，理由是「它们不会往文档里
        // 插字符，不会打断组合串」。前半句对，**后半句是错的**：Tab 让宿主切焦点，
        // TSF 随即终止组合串，`OnCompositionTerminated` 把已经打出的编码当残留删掉
        // （`text_service` 里那个 `SetText(ecwrite, 0, &[])`）。用户看到的是
        // 「按了一下 Tab，正在打的字没了」——与当年 `-`/`=` 的症状同源，
        // 只是没有那个多余的符号。
        //
        // 所以判据要换成「这个键会不会让组合串消失」，而不是「它会不会插字符」。
        // 代价是 Excel 里换格之前得先上屏（空格或回车），也就是多按一下；
        // 按 D31 / D42 的一贯取舍，「吃掉没反应」远比「打的字被打断」轻。
        //
        // 空缓冲时 Tab 是老老实实的制表符（写代码、表格里换格都靠它）；
        // `Ctrl+Tab` 在上面 `mods.ctrl` 那一关就放行了，不经过这里。
        VK_TAB if !empty => Action::Noop,
        // 兜底：组合态内所有**字符键**一律吃掉，不动。
        //
        // 这一条原先不存在，代价是每次按 `-` `=` `[` `]` `\` 之类都会把打了一半的
        // 字弄没：宿主在组合串中间插入半角字符，TSF 随即终止组合串，
        // `OnCompositionTerminated` 把已经打出的编码当残留删掉。用户看到的是
        // 「符号先冒出来，然后我正在打的字消失了」——符号甚至排在编码前面。
        //
        // 「想在组合态里打一个符号」本来就是伪需求：先上屏再打就是了。
        // 空缓冲时 `is_text_key` 一律放行，写代码、写路径完全不受影响。
        _ if is_text_key(vk) && !empty => Action::Noop,
        _ => Action::Pass,
    }
}

/// 「单击 Shift 切中英」的状态机。**纯逻辑**：Shift 的物理状态由调用方采样后传进来。
///
/// ## 为什么不直接按 vkey 判上下
///
/// 最初的实现是「收到 Shift 的 down 就置位、收到 up 就读一次」。实测有两处靠不住：
///
/// 1. 它假设 down 与 up 都经 `OnKeyDown` / `OnKeyUp` 到达。但 Notepad++（Scintilla）
///    只回调 `OnTestKeyDown` / `OnTestKeyUp`，`OnKeyDown` 从不被调用——置位那一半
///    永远不会执行，松手时的判定被整段跳过。
/// 2. M1.0 的真机日志里，Notepad++ 对 Alt 只留下了 `test-down`，**没有对应的
///    `test-up`**。也就是说宿主不一定会把修饰键的松开事件送进来。
///
/// 所以改成「每次按键回调都采样一次物理状态，与上次比对」：
///
/// - 看到「上次没按、这次按着」→ 置位；
/// - 看到「上次按着、这次没按」→ 若期间没夹杂别的键，就是一次单击；
/// - 松开事件没送到也不致命——那个「上次按着、这次没按」的比对会在**下一个按键
///   回调**上补做，只是判定晚一拍（见 `late_detection_when_up_missing` 测试）。
#[derive(Default, Debug)]
pub struct ShiftTap {
    /// 上次回调时采样到的 Shift 是否按下。
    seen_down: bool,
    /// 本次 Shift 按下之后没有夹杂别的键。
    armed: bool,
}

impl ShiftTap {
    /// 计入一次按键回调。
    ///
    /// - `shift_now`：回调发生时 `GetKeyState(VK_SHIFT)` 的结果
    /// - `vk` / `is_down`：这次回调对应的键与方向
    ///
    /// 返回 `true` 表示刚刚完成一次单击 Shift，调用方应当切换中英。
    ///
    /// 幂等：同一个键被重复回调（宿主可能「先问再调」）不会切两次。
    pub fn observe(&mut self, shift_now: bool, vk: u16, is_down: bool) -> bool {
        let was_down = self.seen_down;
        self.seen_down = shift_now;

        let mut tapped = false;
        if shift_now && !was_down {
            self.armed = true;
        } else if !shift_now && was_down && self.armed {
            tapped = true;
            self.armed = false;
        }

        // 任何别的键按下都意味着这不再是「单独按了一下 Shift」，否则按住 Shift
        // 打一个大写字母，松手时会把模式切走。
        //
        // 必须放在上面之后：松开事件缺失时，补齐判定发生在那个键的 down 回调上，
        // 先判后清才不会把这次单击吞掉。
        if is_down && !is_shift_key(vk) {
            self.armed = false;
        }
        tapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Mods {
        Mods::default()
    }
    /// 中文模式、`n` 个字码、无候选窗。
    fn zh(n: usize) -> Input {
        Input::new(Mode::Chinese).with_buffer(n)
    }
    /// 中文模式、`n` 个字码、本页 `page` 条候选。
    fn zh_page(n: usize, page: usize) -> Input {
        zh(n).with_page(page)
    }
    fn en(n: usize) -> Input {
        Input::new(Mode::English).with_buffer(n)
    }
    fn shift() -> Mods {
        Mods {
            ctrl: false,
            alt: false,
            shift: true,
        }
    }
    /// Ctrl+Alt，其余不按。键位图的触发组合（D61）。
    fn ctrl_alt() -> Mods {
        Mods {
            ctrl: true,
            alt: true,
            shift: false,
        }
    }
    /// Ctrl+Shift，其余不按。**曾经**是键位图的触发组合（D61 换掉）。
    fn ctrl_shift() -> Mods {
        Mods {
            ctrl: true,
            alt: false,
            shift: true,
        }
    }

    const V: u16 = 0x56; // 'V'
    const Z: u16 = 0x5A; // 'Z'
    const SPACE: u16 = 0x20;
    const ENTER: u16 = 0x0D;
    const ESC: u16 = 0x1B;
    const BACK: u16 = 0x08;
    const SHIFT: u16 = 0x10;

    #[test]
    fn letters_a_to_y_are_buffered() {
        for vk in VK_A..=VK_Y {
            assert!(matches!(decide(zh(0), vk, m()), Action::Buffer(_)));
        }
    }

    /// `z` 是万能学习键（M3.5 / D62）：**组合态内一律进缓冲**，由 server 判断
    /// 它是拼音声母还是末位通配。
    ///
    /// 空缓冲是唯一看「装没装拼音」的地方（v1.64）：装了拼音就必须进缓冲，
    /// 否则 `zhe` / `zhang` / `zhenbang` 这一类整片打不出来；没装拼音时
    /// 进缓冲只会挂一个查不到东西的组合串，保持 D42 的放行。
    #[test]
    fn z_enters_the_buffer_once_pinyin_is_installed() {
        for has_pinyin in [false, true] {
            let base = zh(0);
            let input = if has_pinyin { base.with_pinyin() } else { base };
            let want = if has_pinyin {
                Action::Buffer('z')
            } else {
                Action::Pass
            };
            assert_eq!(
                decide(input, Z, m()),
                want,
                "空缓冲（pinyin={has_pinyin}）：装了拼音时 z 是声母，放行会丢字母"
            );
            assert_eq!(
                decide(input.with_buffer(3), Z, m()),
                Action::Buffer('z'),
                "组合态内（pinyin={has_pinyin}）"
            );
        }
        // Shift+z 空缓冲无拼音仍是临时英文（Pass）；装了拼音时与不按 Shift 同判据
        // （进缓冲）；组合态内一律进缓冲（D97——中文态不再有直接上屏的口子）。
        // D98：按着 Shift 进缓冲的是大写 `Z`，不参与匹配、原样上屏。
        assert_eq!(decide(zh(0), Z, shift()), Action::Pass);
        assert_eq!(decide(zh(0).with_pinyin(), Z, shift()), Action::Buffer('Z'));
        assert_eq!(decide(zh(2), Z, shift()), Action::Buffer('Z'));
        assert_eq!(
            decide(Input::new(Mode::English).with_pinyin(), Z, m()),
            Action::Pass
        );
    }

    /// v1.64 真机回归：`zhenbang` 的第一键不能漏掉。
    ///
    /// 曾经的判据是「空缓冲就放行」，于是 `z` 被宿主当普通字符上屏、进组合串的
    /// 只剩 `henbang`——拼音侧查到的是「很棒」。这条测试直接照抄那一串按键。
    #[test]
    fn pinyin_starting_with_z_keeps_its_first_letter() {
        let mut input = zh(0).with_pinyin();
        let mut buf = String::new();
        for ch in "zhenbang".chars() {
            let vk = match ch {
                'z' => Z,
                c => c.to_ascii_uppercase() as u16,
            };
            match decide(input, vk, m()) {
                Action::Buffer(c) => {
                    buf.push(c);
                    input = input.with_buffer(buf.chars().count());
                }
                other => panic!("`{ch}` 的判定是 {other:?}，应当进缓冲"),
            }
        }
        assert_eq!(buf, "zhenbang", "组合串必须与敲进去的一模一样");
    }

    /// `wubi_letter` 只管 25 个五笔字根键，`z` 不归它——`z` 走 `decide` 里单独一支。
    #[test]
    fn wubi_letter_excludes_z() {
        assert_eq!(wubi_letter(Z), None);
        assert_eq!(wubi_letter(V), Some('v'));
        assert_eq!(wubi_letter(VK_Y + 1), None, "y 之后就是 z，不在字根键里");
    }

    /// 码长上限随拼音放开：拼音词条最长 20 个字母（`linchuangzhengzhuang`），
    /// 「上限够覆盖最长词条」那条由 `MAX_PINYIN_CODE_LEN` 上的编译期断言保证。
    #[test]
    fn code_len_limit_follows_pinyin() {
        assert_eq!(max_code_len(false), MAX_CODE_LEN);
        assert_eq!(max_code_len(true), MAX_PINYIN_CODE_LEN);
        assert!(max_code_len(true) > max_code_len(false), "混输必须放开码长");
        // 多段 / 通配专用上限（与 MAX_CODE_LEN 的关系由文件头的编译期断言钉住）
        assert_eq!(MAX_EXTENDED_CODE_LEN, 9, "两段各 4 码 + 一个 '");
    }

    #[test]
    fn commit_and_cancel_need_a_buffer() {
        assert_eq!(decide(zh(0), SPACE, m()), Action::Pass);
        assert_eq!(decide(zh(3), SPACE, m()), Action::Commit);
        assert_eq!(decide(zh(0), ENTER, m()), Action::Pass);
        assert_eq!(decide(zh(3), ENTER, m()), Action::Commit);
        assert_eq!(decide(zh(0), ESC, m()), Action::Pass);
        assert_eq!(decide(zh(3), ESC, m()), Action::Cancel);
    }

    /// 空缓冲时退格必须交还宿主，否则文档里已有的字删不掉。
    #[test]
    fn backspace_only_eaten_with_a_buffer() {
        assert_eq!(decide(zh(0), BACK, m()), Action::Pass);
        assert_eq!(decide(zh(2), BACK, m()), Action::Backspace);
    }

    /// **没有候选窗**时方向键吃掉并取消（D31 原样）：吃了才不会留下错位的组合串，
    /// 空缓冲时必须放行，否则用户连光标都移不动。
    #[test]
    fn navigation_cancels_when_no_candidates() {
        for vk in [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28] {
            assert_eq!(decide(zh(2), vk, m()), Action::Cancel, "{vk:#04x}");
            assert_eq!(decide(zh(0), vk, m()), Action::Pass, "{vk:#04x}");
        }
    }

    #[test]
    fn ctrl_and_alt_always_pass() {
        for (ctrl, alt) in [(true, false), (false, true), (true, true)] {
            let mods = Mods {
                ctrl,
                alt,
                shift: false,
            };
            let cands = zh_page(2, 5);
            assert_eq!(decide(cands, V, mods), Action::Pass, "字母");
            assert_eq!(decide(cands, SPACE, mods), Action::Pass, "空格");
            assert_eq!(decide(cands, BACK, mods), Action::Pass, "退格");
            // 数字键唯一的例外：`Ctrl+Alt+1..9` 是键位图（D61）。其余组合照旧放行。
            let digit = if ctrl && alt {
                Action::Keycaps(0)
            } else {
                Action::Pass
            };
            assert_eq!(decide(cands, 0x31, mods), digit, "数字键");
            assert_eq!(decide(cands, 0x27, mods), Action::Pass, "右方向");
        }
    }

    /// Shift+字母 = 临时英文——**只限空缓冲**（D97 用户拍板保留）。
    #[test]
    fn shift_letters_pass_through() {
        assert_eq!(decide(zh(0), V, shift()), Action::Pass);
        // 但 Shift 不影响空格提交。
        assert_eq!(decide(zh(2), SPACE, shift()), Action::Commit);
    }

    /// D97/D98 真机回归：候选窗出来后按 Shift 打字母，大写曾直接上屏、组合串被
    /// 终止、光标跳走。现在缓冲非空时 Shift+字母一律进缓冲，且**保留大写原字符**
    /// （D98）——大写匹配不到任何候选，单击 Shift 后整串原样上屏。
    /// v1.77 曾把它降成小写，`gA` 被当 `ga` 匹配出候选，不是用户要的行为。
    #[test]
    fn shift_letters_enter_buffer_mid_composition() {
        assert_eq!(decide(zh(2), V, shift()), Action::Buffer('V'));
        assert_eq!(decide(zh(2), 0x41, shift()), Action::Buffer('A'), "VK 'A'");
        // 不按 Shift 照旧小写
        assert_eq!(decide(zh(2), V, m()), Action::Buffer('v'));
    }

    #[test]
    fn english_mode_passes_everything() {
        for vk in [V, Z, SPACE, ENTER, ESC, BACK, SHIFT] {
            assert_eq!(decide(en(2), vk, m()), Action::Pass);
        }
        // 英文模式下即便窗口里还挂着候选，数字键与方向键也不该被吃掉
        let en_page = en(2).with_page(5);
        assert_eq!(decide(en_page, 0x31, m()), Action::Pass);
        assert_eq!(decide(en_page, 0x25, m()), Action::Pass);
    }

    // ---------------------------------------------------------------
    // 标点（D33）
    // ---------------------------------------------------------------

    const COMMA: u16 = 0xBC;
    const PERIOD: u16 = 0xBE;
    const SEMI: u16 = 0xBA;
    const SLASH: u16 = 0xBF;

    #[test]
    fn punctuation_becomes_fullwidth() {
        assert_eq!(decide(zh(0), COMMA, m()), Action::Punct('，'));
        assert_eq!(decide(zh(0), PERIOD, m()), Action::Punct('。'));
        assert_eq!(decide(zh(0), SEMI, m()), Action::Punct('；'));
        assert_eq!(decide(zh(0), SLASH, m()), Action::Punct('、'));
    }

    /// 缓冲里有编码时标点照样吃掉：上屏字 + 标点由 `text_service` 一次写完。
    #[test]
    fn punctuation_is_consumed_mid_composition() {
        assert_eq!(decide(zh(3), COMMA, m()), Action::Punct('，'));
        assert_eq!(decide(zh(1), SLASH, m()), Action::Punct('、'));
    }

    #[test]
    fn shift_punctuation_maps_to_its_pair() {
        let s = shift();
        assert_eq!(decide(zh(0), SEMI, s), Action::Punct('：'));
        assert_eq!(decide(zh(0), SLASH, s), Action::Punct('？'));
        assert_eq!(decide(zh(0), COMMA, s), Action::Punct('《'));
        assert_eq!(decide(zh(0), PERIOD, s), Action::Punct('》'));
        assert_eq!(decide(zh(0), 0x31, s), Action::Punct('！'));
        assert_eq!(decide(zh(0), 0x39, s), Action::Punct('（'));
        assert_eq!(decide(zh(0), 0x30, s), Action::Punct('）'));
        // D96：单个省略号，不是主流输入法的「……」两个
        assert_eq!(decide(zh(0), 0x36, s), Action::Punct('…'));
        assert_eq!(
            decide(zh(2), 0x36, s),
            Action::Punct('…'),
            "组合态内同样吃掉"
        );
    }

    /// 表里没有的键在**空缓冲**时一律放行——写代码、写路径要的是半角原文。
    #[test]
    fn untabled_keys_pass_through() {
        for vk in [0xBB, 0xBD, 0xDB, 0xDD, 0xDC, 0xC0, 0xDE, 0xE2] {
            assert_eq!(decide(zh(0), vk, m()), Action::Pass, "{vk:#04x} 不该被吃掉");
        }
        // 不带 Shift 的数字键永远是数字
        for vk in 0x30..=0x39 {
            assert_eq!(decide(zh(0), vk, m()), Action::Pass, "{vk:#04x}");
        }
    }

    /// 组合态内所有**字符键**一律吞掉。
    ///
    /// 放行的后果不是「多了个符号」，而是「正在打的字没了」：宿主在组合串中间
    /// 插字符会终止组合串，`OnCompositionTerminated` 把编码当残留删掉。
    /// 真机上表现为「符号先冒出来，编码随后消失」。
    #[test]
    fn text_keys_are_swallowed_mid_composition() {
        for vk in [0xBB, 0xBD, 0xDB, 0xDD, 0xDC, 0xC0, 0xE2, 0x30, 0x35, 0x6B] {
            assert_eq!(
                decide(zh(3), vk, m()),
                Action::Noop,
                "{vk:#04x} 有编码时必须吞掉"
            );
        }
        // 功能键不受影响：它们有自己的含义，吞掉会让用户以为键盘坏了
        //
        // Tab 曾经在这张表里，后来移走了——它虽然「不插字符」，但**会切焦点**，
        // 焦点一动组合串就被 TSF 终止、编码被当残留删掉。见 `tab_is_eaten_mid_composition`。
        for vk in [
            0x70, /* F1 */
            0x7B, /* F12 */
            0x14, /* CapsLock */
        ] {
            assert_eq!(decide(zh(3), vk, m()), Action::Pass, "{vk:#04x} 不该被吞");
        }
    }

    const TAB: u16 = 0x09;

    /// Tab 在组合态内吃掉：放行 = 切焦点 = 组合串被终止 = 编码被删。
    ///
    /// 这一条修的是 D49 的一处误判——那份记录把 Tab 与 F1/CapsLock 归在一起，
    /// 理由是「它们不会往文档里插字符，不会打断组合串」。Tab 会打断组合串。
    #[test]
    fn tab_is_eaten_mid_composition() {
        assert_eq!(decide(zh(3), TAB, m()), Action::Noop, "组合态内吃掉不动");
        // 有编码但没候选窗（打了一半的死码）同样吃掉，不然编码照样没
        assert_eq!(decide(zh(2), TAB, m()), Action::Noop);
        // 空缓冲时它就是个制表符：写代码、表格换格都靠它
        assert_eq!(decide(zh(0), TAB, m()), Action::Pass);
        // `Ctrl+Tab` 是宿主的切标签，`mods.ctrl` 那一关就放行了
        let c = Mods {
            ctrl: true,
            alt: false,
            shift: false,
        };
        assert_eq!(decide(zh(3), TAB, c), Action::Pass);
        // 英文模式照旧全放行
        assert_eq!(decide(en(3), TAB, m()), Action::Pass);
    }

    /// `Ctrl+Alt+1..9` 调出键位图（§6.7 / D61）。三条边界：
    /// 必须越过 `mods.ctrl` 那一关、必须排在「数字键 = 选字」之前、没有候选窗时放行。
    #[test]
    fn ctrl_alt_digit_opens_keycaps() {
        // 本页 5 条时按 1/3 分别指向第 1/3 条
        assert_eq!(decide(zh_page(3, 5), 0x31, ctrl_alt()), Action::Keycaps(0));
        assert_eq!(decide(zh_page(3, 5), 0x33, ctrl_alt()), Action::Keycaps(2));
        // 超出本页条数：吃掉不动，绝不能放行（放行会把组合串打断）
        assert_eq!(decide(zh_page(3, 2), 0x35, ctrl_alt()), Action::Noop);
        // 没有候选窗：还给宿主
        assert_eq!(decide(zh(3), 0x31, ctrl_alt()), Action::Pass);
        assert_eq!(decide(zh(0), 0x31, ctrl_alt()), Action::Pass);
        // 只按 Ctrl（没有 Alt）仍是宿主的键
        let c = Mods {
            ctrl: true,
            alt: false,
            shift: false,
        };
        assert_eq!(decide(zh_page(3, 5), 0x31, c), Action::Pass);
        // 旧的 Ctrl+Shift 必须彻底让开：它是 Windows 切换输入语言的热键（D61）
        assert_eq!(decide(zh_page(3, 5), 0x31, ctrl_shift()), Action::Pass);
        // 加上 Shift 就还回去（`Ctrl+Alt+Shift+1` 是别的东西）
        let cas = Mods {
            ctrl: true,
            alt: true,
            shift: true,
        };
        assert_eq!(decide(zh_page(3, 5), 0x31, cas), Action::Pass);
        // 英文模式下一个键都不吃
        assert_eq!(
            decide(Input::new(Mode::English).with_page(5), 0x31, ctrl_alt()),
            Action::Pass
        );
    }

    /// 普通数字键不受影响：不按 Ctrl 时仍然是「选第 n 条」。
    #[test]
    fn plain_digit_still_selects() {
        assert_eq!(decide(zh_page(3, 5), 0x32, m()), Action::Select(1));
    }

    #[test]
    fn punctuation_passes_in_english_mode() {
        assert_eq!(decide(en(0), COMMA, m()), Action::Pass);
        assert_eq!(decide(en(3), PERIOD, m()), Action::Pass);
    }

    /// Ctrl/Alt + 标点必须放行：`Ctrl+,` 之类是宿主的功能键。
    #[test]
    fn punctuation_with_ctrl_passes() {
        let c = Mods {
            ctrl: true,
            alt: false,
            shift: false,
        };
        assert_eq!(decide(zh(3), COMMA, c), Action::Pass);
    }

    /// Shift 键本身永远不被吃掉——它是修饰键，吃了会用不了大写和符号。
    #[test]
    fn shift_key_itself_passes() {
        let s = shift();
        assert_eq!(decide(zh(2), SHIFT, s), Action::Pass);
    }

    // ---------------------------------------------------------------
    // 数字键选词（M2.3）
    // ---------------------------------------------------------------

    /// 有候选时 1–9 选本页对应条；超出本页条数吃掉不动。
    #[test]
    fn digits_select_within_the_page() {
        let page = zh_page(3, 5);
        for n in 1..=5u16 {
            assert_eq!(
                decide(page, 0x30 + n, m()),
                Action::Select(n as usize - 1),
                "数字 {n}"
            );
        }
        for n in 6..=9u16 {
            assert_eq!(
                decide(page, 0x30 + n, m()),
                Action::Noop,
                "本页只有 5 条，第 {n} 个键吃掉不动"
            );
        }
    }

    /// 本页只剩 2 条（末页）时，3 号以后的键都不选。
    #[test]
    fn digits_are_limited_by_page_length() {
        let last_page = zh_page(3, 2);
        assert_eq!(decide(last_page, 0x31, m()), Action::Select(0));
        assert_eq!(decide(last_page, 0x32, m()), Action::Select(1));
        assert_eq!(decide(last_page, 0x33, m()), Action::Noop);
    }

    /// 没候选窗时数字键不选词，但也**不能放行**：有编码时放行会把数字插进
    /// 组合串中间、把正在打的字弄丢。空缓冲时它才是普通数字。
    #[test]
    fn digits_pass_only_without_a_buffer() {
        for vk in 0x31..=0x39 {
            assert_eq!(decide(zh(0), vk, m()), Action::Pass, "{vk:#04x} 空缓冲");
            assert_eq!(decide(zh(3), vk, m()), Action::Noop, "{vk:#04x} 组合态内");
        }
    }

    // ---------------------------------------------------------------
    // `-` / `=` 翻页（D49）
    // ---------------------------------------------------------------

    const MINUS: u16 = 0xBD;
    const EQUAL: u16 = 0xBB;

    /// 有候选窗时 `-` 往前、`=` 往后。这是五笔用户的肌肉记忆，也是笔记本上
    /// 唯一不用 Fn 的翻页方式（PgUp/PgDn 常要按 Fn，方向键离手位太远）。
    #[test]
    fn minus_and_equal_page_when_candidates_exist() {
        let page = zh_page(3, 5);
        assert_eq!(decide(page, MINUS, m()), Action::PagePrev);
        assert_eq!(decide(page, EQUAL, m()), Action::PageNext);
        // Shift 组合也按同一套走：`Shift+-` 是 `_`，但在候选窗前面它不可能是
        // 「我要一个下划线」——那时用户正在选词。
        assert_eq!(decide(page, MINUS, shift()), Action::PagePrev);
        assert_eq!(decide(page, EQUAL, shift()), Action::PageNext);
    }

    /// 没有候选窗时这两个键仍是原文：`-` 与 `=` 在写代码时天天用到。
    #[test]
    fn minus_and_equal_are_plain_keys_without_candidates() {
        assert_eq!(decide(zh(0), MINUS, m()), Action::Pass);
        assert_eq!(decide(zh(0), EQUAL, m()), Action::Pass);
        // 有编码但没候选（打了一半的死码）：吞掉，别让它打断组合串
        assert_eq!(decide(zh(3), MINUS, m()), Action::Noop);
        assert_eq!(decide(zh(3), EQUAL, m()), Action::Noop);
    }

    // ---------------------------------------------------------------
    // `'` 显式拼音标记（§6.5 / D48）
    // ---------------------------------------------------------------

    const QUOTE: u16 = 0xDE;

    #[test]
    fn quote_marks_pinyin_when_pinyin_is_available() {
        let py = zh(2).with_pinyin();
        assert_eq!(decide(py, QUOTE, m()), Action::Buffer('\''), "进缓冲当标记");
        // 空缓冲时它就是个普通单引号
        assert_eq!(decide(zh(0).with_pinyin(), QUOTE, m()), Action::Pass);
        // Shift+' 是双引号，但它同样不能插进组合串中间——按字符键兜底吞掉
        assert_eq!(decide(py, QUOTE, shift()), Action::Noop);
    }

    /// 没装拼音词库时 `'` 没有任何含义：既不进缓冲（没人解释它），也不放行
    /// （会在组合串中间插字符、把编码弄丢）。
    #[test]
    fn quote_is_swallowed_without_pinyin() {
        assert_eq!(decide(zh(2), QUOTE, m()), Action::Noop);
        assert_eq!(decide(zh(0), QUOTE, m()), Action::Pass);
    }

    // ---------------------------------------------------------------
    // 翻页（D38）
    // ---------------------------------------------------------------

    /// 有候选时 ←/→/PgUp/PgDn 是翻页，↑/↓ 吃掉不动。
    #[test]
    fn arrows_page_when_candidates_exist() {
        let page = zh_page(3, 5);
        assert_eq!(decide(page, 0x25, m()), Action::PagePrev, "←");
        assert_eq!(decide(page, 0x27, m()), Action::PageNext, "→");
        assert_eq!(decide(page, 0x21, m()), Action::PagePrev, "PgUp");
        assert_eq!(decide(page, 0x22, m()), Action::PageNext, "PgDn");
        assert_eq!(decide(page, 0x26, m()), Action::Noop, "↑");
        assert_eq!(decide(page, 0x28, m()), Action::Noop, "↓");
    }

    /// Home / End 仍然是取消：它们会把光标扔到文档两端，组合串必然错位。
    #[test]
    fn home_and_end_still_cancel_during_composition() {
        let page = zh_page(3, 5);
        assert_eq!(decide(page, 0x24, m()), Action::Cancel, "Home");
        assert_eq!(decide(page, 0x23, m()), Action::Cancel, "End");
    }

    // ---------------------------------------------------------------
    // ShiftTap：单击 Shift 切中英
    // ---------------------------------------------------------------

    #[test]
    fn tap_toggles_on_release() {
        let mut t = ShiftTap::default();
        assert!(!t.observe(true, SHIFT, true), "按下不算单击");
        assert!(t.observe(false, SHIFT, false), "松开时判定为单击");
        assert!(!t.observe(false, SHIFT, false), "重复的 up 不得再切一次");
    }

    /// 按住 Shift 打一个大写字母，松手时不能把模式切走。
    #[test]
    fn held_shift_with_a_letter_does_not_toggle() {
        let mut t = ShiftTap::default();
        assert!(!t.observe(true, SHIFT, true));
        assert!(!t.observe(true, 0x41, true), "Shift+A 是组合，不是单击");
        assert!(!t.observe(true, 0x41, false));
        assert!(!t.observe(false, SHIFT, false), "松手时 armed 已被清掉");
    }

    /// 宿主没送 Shift 的 key-up 时，判定会在下一个按键回调上补做。
    ///
    /// 这正是 Notepad++ 的实际情况：M1.0 日志里它只留下了 Alt 的 test-down。
    #[test]
    fn late_detection_when_up_missing() {
        let mut t = ShiftTap::default();
        assert!(!t.observe(true, SHIFT, true), "按下 Shift");
        // 松开事件缺失，直接来了 'A' 的 down
        assert!(t.observe(false, 0x41, true), "在下一个按键上补判出单击");
        assert!(!t.observe(false, 0x41, false), "同一键的 up 不得再切一次");
    }

    /// 连按两次 Shift 应当切两次（中→英→中）。
    #[test]
    fn two_taps_toggle_twice() {
        let mut t = ShiftTap::default();
        t.observe(true, SHIFT, true);
        assert!(t.observe(false, SHIFT, false));
        t.observe(true, SHIFT, true);
        assert!(t.observe(false, SHIFT, false));
    }

    /// 宿主「先问再调」会把同一个键回调两遍，判定必须幂等。
    #[test]
    fn double_callback_is_idempotent() {
        let mut t = ShiftTap::default();
        // test-down 与 down 都送进来
        assert!(!t.observe(true, SHIFT, true));
        assert!(!t.observe(true, SHIFT, true));
        // test-up 与 up 都送进来
        assert!(t.observe(false, SHIFT, false));
        assert!(!t.observe(false, SHIFT, false));
    }
}
