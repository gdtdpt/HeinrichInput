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
}

/// 五笔 86 的字根键：`a`–`y`，共 25 个。
///
/// `z` 不在其中——86 版的 `z` 是「万能学习键」，查任意位置的字根，
/// M1.1 不实现。**吃掉一个键却不出字是不可接受的**，所以 `z` 直接放行。
pub const VK_A: u16 = 0x41;
pub const VK_Y: u16 = 0x59;

/// 单字全码最长 4 码（D15：3 或 4 码，不足四字根补末笔识别码）。
pub const MAX_CODE_LEN: usize = 4;

/// vkey → 五笔码字母。在 `a`–`y` 之外返回 `None`。
pub fn wubi_letter(vk: u16) -> Option<char> {
    if (VK_A..=VK_Y).contains(&vk) {
        Some((b'a' + (vk - VK_A) as u8) as char)
    } else {
        None
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

    match vk {
        // 空格 / 回车：有编码才提交，空缓冲时是正常空格 / 换行。
        0x20 | 0x0D if !empty => Action::Commit,
        // Esc：有编码才取消，空缓冲时交给宿主（很多程序用它关窗口）。
        0x1B if !empty => Action::Cancel,
        // 退格：空缓冲时交还宿主，否则会删不掉文档里已有的字。
        0x08 if !empty => Action::Backspace,
        _ => Action::Pass,
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
}
