//! 「这枚 emoji 在本机能画出字形吗」——一个只干这件事的小 crate。
//!
//! 起因：`您好 → 🫂`（U+1FAC2，Emoji 13.0）在候选窗里显示成方块。本机
//! `seguiemj.ttf` 是 2019 年那版（覆盖到 Emoji 12），字体里根本没有这个字形，
//! DirectWrite 找不到就画 .notdef。emoji 表是按 CLDR 全量生成的，所以**必须**
//! 在装载期按本机字体过一遍，否则候选窗和文档里都会出现一排方块。
//!
//! # 为什么不用 GDI
//!
//! `GetGlyphIndicesW` 按 **UTF-16 码元**逐个查，非 BMP 字符会被拆成代理对的两个
//! 码元，而字体的 cmap 里映射的是码位本身（U+1F44B），不是那两个代理——于是
//! **所有**彩色 emoji 都被报成「没有字形」，连 `👋` 都是。这是假阴性，实测过：
//! 用它筛完 2395 条只剩 228 条，而其中绝大多数明明能正常显示。
//!
//! 判据只能是 DirectWrite 的 [`IDWriteFont::HasCharacter`]（接受 UCS-4 码位）。
//!
//! # 为什么要先拨成一个集合
//!
//! `IDWriteFont` 不是 `Send`，而 server 是按连接开线程的。所以这里**探测一次、
//! 立刻丢掉 COM 对象**，只留下「哪些码位有字形」的 `HashSet<char>`——那是个
//! 普通值，能随 `EmojiMap` 一起放在任何地方。

use std::collections::HashSet;

/// 候选窗渲染 emoji 时 DirectWrite 会回退到的字体族。
///
/// 换掉它这个 crate 就失去意义：判的必须是**真正要拿来画的那一款**。
const EMOJI_FAMILY: &str = "Segoe UI Emoji";

/// 本机 emoji 字体的字形覆盖。
///
/// `supported` 为 `None` 表示**判不出来**（找不到字体族、或不在 Windows 上），
/// 此时 [`supports`](Self::supports) 一律返回 `true`：宁可在极端环境下照旧显示，
/// 也不要因为探测失败把整批 emoji 意外关掉。
#[derive(Debug, Clone, Default)]
pub struct EmojiSupport {
    supported: Option<HashSet<char>>,
    /// 已经问过字体的码位（含判为「没有字形」的），重载时不必再问。
    ///
    /// 与 `supported` 的不变式：`supported` 为 `Some` 时 `probed` 包含自己探过的
    /// 全部码位；探测失败（`probe` 返回 `None`）时两者都不动，下次重载再试。
    probed: HashSet<char>,
}

impl EmojiSupport {
    /// 探测一批码位在本机字体里有没有字形。
    ///
    /// 只探测传进来的那些（去重后通常一千出头）；不缓存跨进程，
    /// 因为装了新字体重启输入法就该生效。
    pub fn detect<I: IntoIterator<Item = char>>(candidates: I) -> Self {
        let mut s = Self::default();
        s.extend(candidates);
        s
    }

    /// 补探一批码位，**只问上次没问过的那些**（D99）。
    ///
    /// 返回本次真正问过字体的码位数（0 = 全是老面孔，一次 COM 都没进）。
    ///
    /// 存在的理由：emoji 用户层是**每改一次设置就整份重建**的，而整份重建要问
    /// 两千多个码位、每枚一次 `HasCharacter`，实测 110ms——期间宿主锁被独占，
    /// 连着两三个键一起超时。用户改一行绑定实际只涉及 1 枚新码位，问它就够了。
    pub fn extend<I: IntoIterator<Item = char>>(&mut self, candidates: I) -> usize {
        let fresh: HashSet<char> = candidates
            .into_iter()
            .filter(|c| !self.probed.contains(c))
            .collect();
        if fresh.is_empty() {
            return 0;
        }
        // 判不出来时一律放行（见类型注释），且**不记 probed**：装上新字体后
        // 下一次重载能自愈，而不是永久沿用「全部放行」。
        let Some(found) = probe(&fresh) else {
            return 0;
        };
        self.supported
            .get_or_insert_with(HashSet::new)
            .extend(found);
        self.probed.extend(fresh.iter().copied());
        fresh.len()
    }

    /// 一段 emoji（可能带变体选择符）**每个码位**都有字形才算可用。
    ///
    /// 只看基字符是不够的：`❤️` = U+2764 + U+FE0F，缺了后面的变体选择符，
    /// 画出来是单色心形而不是彩色那张。
    pub fn supports(&self, emoji: &str) -> bool {
        match &self.supported {
            None => true,
            Some(set) => emoji.chars().all(|c| set.contains(&c)),
        }
    }

    /// 探测到了多少个「有字形」的码位；`None` = 没探测成功。
    pub fn known(&self) -> Option<usize> {
        self.supported.as_ref().map(HashSet::len)
    }
}

/// 现场判一枚 emoji（M4d 设置窗口的改绑框用）：[`EmojiSupport::detect`] 只认识
/// 探测时给过的那批码位，用户现场粘贴进来的不在里面，得单独问一次字体。
///
/// 一次完整的 COM 探测只查这一个 emoji，毫秒级；变体选择符 `U+FE0F` 不查
/// ——它有没有字形不随字体变，彩色与否取决于基字符那一 位。
/// `None` = 判不出来（找不到字体族 / 不在 Windows），调用方按「放行」处理。
#[cfg(windows)]
pub fn probe_one(emoji: &str) -> Option<bool> {
    with_emoji_font(|font| {
        let mut ok = true;
        for c in emoji.chars() {
            if c == '\u{FE0F}' {
                continue;
            }
            match unsafe { font.HasCharacter(c as u32) } {
                Ok(found) if found.as_bool() => {}
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        Some(ok)
    })
}

#[cfg(not(windows))]
pub fn probe_one(_emoji: &str) -> Option<bool> {
    None
}

#[cfg(windows)]
fn with_emoji_font<T>(
    f: impl FnOnce(&windows::Win32::Graphics::DirectWrite::IDWriteFont) -> Option<T>,
) -> Option<T> {
    use windows::core::HSTRING;
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, DWRITE_FACTORY_TYPE_SHARED,
        DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    };

    // SAFETY：只调用 DirectWrite 的只读接口；`factory` / `collection` / `family` /
    // `font` 都是 RAII 的 COM 指针，出作用域即释放。`GetSystemFontCollection`
    // 的原始签名要求先给一个空槽，所以用 `Option` 接。
    unsafe {
        let factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
        let mut collection: Option<IDWriteFontCollection> = None;
        factory
            .GetSystemFontCollection(&mut collection, false)
            .ok()?;
        let collection = collection?;

        let name = HSTRING::from(EMOJI_FAMILY);
        let mut index = 0u32;
        let mut exists = windows::core::BOOL(0);
        collection
            .FindFamilyName(&name, &mut index, &mut exists)
            .ok()?;
        if !exists.as_bool() {
            return None;
        }
        let family = collection.GetFontFamily(index).ok()?;
        let font = family
            .GetFirstMatchingFont(
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
            )
            .ok()?;
        f(&font)
    }
}

#[cfg(windows)]
fn probe(wanted: &HashSet<char>) -> Option<HashSet<char>> {
    with_emoji_font(|font| {
        let mut ok = HashSet::with_capacity(wanted.len());
        for &c in wanted {
            if let Ok(found) = unsafe { font.HasCharacter(c as u32) } {
                if found.as_bool() {
                    ok.insert(c);
                }
            }
        }
        Some(ok)
    })
}

#[cfg(not(windows))]
fn probe(_wanted: &HashSet<char>) -> Option<HashSet<char>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 判不出来时（非 Windows / 字体缺失）必须全部放行，否则功能会被静默关掉。
    #[test]
    fn unknown_font_means_everything_supported() {
        let s = EmojiSupport::default();
        assert!(s.supports("👍"));
        assert!(s.supports("❤\u{FE0F}"));
        assert_eq!(s.known(), None);
    }

    /// 每个码位都要有字形：缺变体选择符就是单色心形，与彩色那张不是一回事。
    #[test]
    fn every_codepoint_must_be_covered() {
        let s = EmojiSupport {
            supported: Some(['\u{2764}'].into_iter().collect()),
            probed: HashSet::new(),
        };
        assert!(s.supports("❤"));
        assert!(!s.supports("❤\u{FE0F}"));
        assert!(!s.supports("👍"));
    }

    /// 空批次不进 COM，也不能把「还没探过」翻成「全都不支持」——否则整批 emoji
    /// 会被静默关掉。
    #[test]
    fn empty_batch_probes_nothing() {
        let mut s = EmojiSupport::default();
        assert_eq!(s.extend(std::iter::empty()), 0);
        assert_eq!(s.known(), None);
        assert!(s.supports("👍"));
    }
}
