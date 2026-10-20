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
}

/// 五笔 86 的字根键：`a`–`y`，共 25 个。
///
/// `z` 不在其中——86 版的 `z` 是「万能学习键」，查任意位置的字根，
/// M1.1 不实现。**吃掉一个键却不出字是不可接受的**，所以 `z` 直接放行。
pub const VK_A: u16 = 0x41;
pub const VK_Y: u16 = 0x59;

/// 单字全码最长 4 码（D15：3 或 4 码，不足四字根补末笔识别码）。
pub const MAX_CODE_LEN: usize = 4;

/// Shift 有三个 vkey：通用 `VK_SHIFT`、左 `VK_LSHIFT`、右 `VK_RSHIFT`，
/// 宿主给哪个都可能。
pub const VK_SHIFT: u16 = 0x10;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RSHIFT: u16 = 0xA1;

pub fn is_shift_key(vk: u16) -> bool {
    vk == VK_SHIFT || vk == VK_LSHIFT || vk == VK_RSHIFT
}

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
        // 单引号（0xDE）**故意不接管**：它是 §6.5 的显式拼音标记键（D11），
        // M3 要用它表示「这段按拼音解析」。现在先吃掉会在 M3 时产生行为变更。
        // 其余 OEM 键（`=` `-` `[` `]` `\` `` ` `` `<` `>` `|` `~`）在多数字体下
        // 用户要的是半角原文（写代码、写路径），一律放行。
        _ => None,
    }
}

/// 判定一个按键该不该被接管。
///
/// 参数全部来自本进程：`mode` 与 `buffer_len` 是实例状态，`mods` 由调用方从
/// `GetKeyState` 取。没有任何跨进程调用——这是 D18「按键路径零阻塞」的前提。
pub fn decide(mode: Mode, buffer_len: usize, vk: u16, mods: Mods) -> Action {
    // Ctrl / Alt 组合一律放行：Ctrl+C、Ctrl+V、Alt+Tab 被输入法吞掉是灾难性的，
    // 而且用户对它们有强预期。
    if mods.ctrl || mods.alt {
        return Action::Pass;
    }
    if mode == Mode::English {
        return Action::Pass;
    }

    let empty = buffer_len == 0;

    if let Some(ch) = wubi_letter(vk) {
        // 按住 Shift 打字母 → 放行，宿主会输出大写字母。
        // 这等于一个「临时英文」逃生口，比切模式快，也不占额外按键。
        if mods.shift {
            return Action::Pass;
        }
        return Action::Buffer(ch);
    }

    // 标点：中文模式下由我们接手，保证「字 + 标点」一起出、不丢编码。
    // 放在字母之后、编辑键之前——两组 vkey 不重叠，顺序只影响可读性。
    if let Some(p) = punct_text(vk, mods.shift) {
        return Action::Punct(p);
    }

    match vk {
        // 空格 / 回车：有编码才提交，空缓冲时是正常空格 / 换行。
        0x20 | 0x0D if !empty => Action::Commit,
        // Esc：有编码才取消，空缓冲时交给宿主（很多程序用它关窗口）。
        0x1B if !empty => Action::Cancel,
        // 退格：空缓冲时交还宿主，否则会删不掉文档里已有的字。
        0x08 if !empty => Action::Backspace,
        // 方向键 / Home / End / PgUp / PgDn（0x21–0x28）：组合态下光标一动，
        // 组合串就停在原地，下一个字母还会往那个旧位置写，屏幕上出现「光标在这、
        // 字却在那」的错位。M1.2 既没有候选窗也不做组合态光标，折中成
        // 「吃掉并取消」：代价是一次方向键被吞，换来的是绝不留下错位的组合串。
        0x21..=0x28 if !empty => Action::Cancel,
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
            assert!(matches!(
                decide(Mode::Chinese, 0, vk, m()),
                Action::Buffer(_)
            ));
        }
    }

    /// z 是万能学习键，M1.1 不实现，必须放行而不是吃掉。
    #[test]
    fn z_is_not_consumed() {
        assert_eq!(decide(Mode::Chinese, 0, Z, m()), Action::Pass);
        assert_eq!(decide(Mode::Chinese, 3, Z, m()), Action::Pass);
    }

    #[test]
    fn commit_and_cancel_need_a_buffer() {
        assert_eq!(decide(Mode::Chinese, 0, SPACE, m()), Action::Pass);
        assert_eq!(decide(Mode::Chinese, 3, SPACE, m()), Action::Commit);
        assert_eq!(decide(Mode::Chinese, 0, ENTER, m()), Action::Pass);
        assert_eq!(decide(Mode::Chinese, 3, ENTER, m()), Action::Commit);
        assert_eq!(decide(Mode::Chinese, 0, ESC, m()), Action::Pass);
        assert_eq!(decide(Mode::Chinese, 3, ESC, m()), Action::Cancel);
    }

    /// 空缓冲时退格必须交还宿主，否则文档里已有的字删不掉。
    #[test]
    fn backspace_only_eaten_with_a_buffer() {
        assert_eq!(decide(Mode::Chinese, 0, BACK, m()), Action::Pass);
        assert_eq!(decide(Mode::Chinese, 2, BACK, m()), Action::Backspace);
    }

    /// 组合态下方向键一律「吃掉并取消」：吃了才不会留下错位的组合串，
    /// 空缓冲时必须放行，否则用户连光标都移不动。
    #[test]
    fn navigation_cancels_only_during_composition() {
        for vk in [0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28] {
            assert_eq!(
                decide(Mode::Chinese, 2, vk, m()),
                Action::Cancel,
                "{vk:#04x}"
            );
            assert_eq!(decide(Mode::Chinese, 0, vk, m()), Action::Pass, "{vk:#04x}");
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
            assert_eq!(decide(Mode::Chinese, 2, V, mods), Action::Pass, "字母");
            assert_eq!(decide(Mode::Chinese, 2, SPACE, mods), Action::Pass, "空格");
            assert_eq!(decide(Mode::Chinese, 2, BACK, mods), Action::Pass, "退格");
        }
    }

    /// Shift+字母 = 临时英文，放行让宿主输出大写。
    #[test]
    fn shift_letters_pass_through() {
        let mods = Mods {
            ctrl: false,
            alt: false,
            shift: true,
        };
        assert_eq!(decide(Mode::Chinese, 0, V, mods), Action::Pass);
        // 但 Shift 不影响空格提交。
        assert_eq!(decide(Mode::Chinese, 2, SPACE, mods), Action::Commit);
    }

    #[test]
    fn english_mode_passes_everything() {
        for vk in [V, Z, SPACE, ENTER, ESC, BACK, SHIFT] {
            assert_eq!(decide(Mode::English, 2, vk, m()), Action::Pass);
        }
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
        assert_eq!(decide(Mode::Chinese, 0, COMMA, m()), Action::Punct('，'));
        assert_eq!(decide(Mode::Chinese, 0, PERIOD, m()), Action::Punct('。'));
        assert_eq!(decide(Mode::Chinese, 0, SEMI, m()), Action::Punct('；'));
        assert_eq!(decide(Mode::Chinese, 0, SLASH, m()), Action::Punct('、'));
    }

    /// 缓冲里有编码时标点照样吃掉：上屏字 + 标点由 `text_service` 一次写完。
    #[test]
    fn punctuation_is_consumed_mid_composition() {
        assert_eq!(decide(Mode::Chinese, 3, COMMA, m()), Action::Punct('，'));
        assert_eq!(decide(Mode::Chinese, 1, SLASH, m()), Action::Punct('、'));
    }

    #[test]
    fn shift_punctuation_maps_to_its_pair() {
        let s = Mods {
            ctrl: false,
            alt: false,
            shift: true,
        };
        assert_eq!(decide(Mode::Chinese, 0, SEMI, s), Action::Punct('：'));
        assert_eq!(decide(Mode::Chinese, 0, SLASH, s), Action::Punct('？'));
        assert_eq!(decide(Mode::Chinese, 0, COMMA, s), Action::Punct('《'));
        assert_eq!(decide(Mode::Chinese, 0, PERIOD, s), Action::Punct('》'));
        assert_eq!(decide(Mode::Chinese, 0, 0x31, s), Action::Punct('！'));
        assert_eq!(decide(Mode::Chinese, 0, 0x39, s), Action::Punct('（'));
        assert_eq!(decide(Mode::Chinese, 0, 0x30, s), Action::Punct('）'));
    }

    /// 表里没有的键一律放行——写代码、写路径要的是半角原文。
    #[test]
    fn untabled_keys_pass_through() {
        for vk in [0xBB, 0xBD, 0xDB, 0xDD, 0xDC, 0xC0, 0xDE, 0xE2] {
            assert_eq!(
                decide(Mode::Chinese, 0, vk, m()),
                Action::Pass,
                "{vk:#04x} 不该被吃掉"
            );
        }
        // 单引号留给 §6.5 的显式拼音标记（D11）
        assert_eq!(decide(Mode::Chinese, 2, 0xDE, m()), Action::Pass);
        // 不带 Shift 的数字键永远是数字
        for vk in 0x30..=0x39 {
            assert_eq!(decide(Mode::Chinese, 0, vk, m()), Action::Pass, "{vk:#04x}");
        }
    }

    #[test]
    fn punctuation_passes_in_english_mode() {
        assert_eq!(decide(Mode::English, 0, COMMA, m()), Action::Pass);
        assert_eq!(decide(Mode::English, 3, PERIOD, m()), Action::Pass);
    }

    /// Ctrl/Alt + 标点必须放行：`Ctrl+,` 之类是宿主的功能键。
    #[test]
    fn punctuation_with_ctrl_passes() {
        let c = Mods {
            ctrl: true,
            alt: false,
            shift: false,
        };
        assert_eq!(decide(Mode::Chinese, 3, COMMA, c), Action::Pass);
    }

    /// Shift 键本身永远不被吃掉——它是修饰键，吃了会用不了大写和符号。
    #[test]
    fn shift_key_itself_passes() {
        let mods = Mods {
            ctrl: false,
            alt: false,
            shift: true,
        };
        assert_eq!(decide(Mode::Chinese, 2, SHIFT, mods), Action::Pass);
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
