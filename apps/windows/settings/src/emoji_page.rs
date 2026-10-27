//! emoji 页的数据层（M4d / D85）：两层映射的合并视图 + 用户层的三个写操作。
//!
//! 刻意不依赖任何 Win32（与 `model.rs` / `general.rs` 同一条纪律）：
//! `--selftest` 直接跑这一层，窗口只负责把 [`EmojiRow`] 画进 ListView、
//! 把编辑框的内容交回 [`EmojiPage`] 的方法。
//!
//! ## 写盘口径
//!
//! 内置表（`data/wubi86/emoji.toml`）永远只读；一切改动都落用户层
//! `%APPDATA%\Heinrich\emoji.user.toml`。用户层是**机器拥有的文件**（头部注释是
//! 我们的模板），所以整份重写、临时文件 + 改名——server 那边每秒 stat 一次，
//! 不能让它读到半个文件。
//!
//! ## 单字（D85）
//!
//! 内置表由生成器保证只挂词组；用户层显式给单字绑就照办。数据层不做拦截，
//! 只把 `single` 标出来给界面提示「那一行会变宽」。

use std::path::{Path, PathBuf};

use heinrich_scheme::emoji::{valid_emoji, EmojiEntry, EmojiLayer, EmojiMap};

/// 列表里的一行：合并视图（内置 + 用户层叠加后的现状）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiRow {
    /// 按词绑定的词（与 `code` 二者必有其一）。
    pub word: Option<String>,
    /// 按码绑定的五笔码。
    pub code: Option<String>,
    /// 当前生效的 emoji（停用时是「恢复后会用的那个」）。
    pub emoji: String,
    /// 内置层的值；`None` = 内置没有这条（用户新增的绑定）。
    pub builtin_emoji: Option<String>,
    /// 用户层有没有这条（改绑过 / 停用过 / 新增）。
    pub user: bool,
    /// 当前是否生效（用户层 `enabled = false` 且吃掉了内置 → `false`）。
    pub enabled: bool,
}

impl EmojiRow {
    /// 列表第一列：词，或「码 …」形状。
    pub fn key_cell(&self) -> String {
        match (&self.word, &self.code) {
            (Some(w), _) => w.clone(),
            (None, Some(c)) => format!("码 {c}"),
            (None, None) => String::new(),
        }
    }

    /// 「绑定」列：按词 / 按码；单字再补一个记号，提示那一行会变宽。
    pub fn bind_cell(&self) -> String {
        let base = if self.word.is_some() {
            "按词"
        } else {
            "按码"
        };
        match self.word.as_deref() {
            Some(w) if w.chars().count() == 1 => format!("{base}·单字"),
            _ => base.to_string(),
        }
    }

    /// 「状态」列：内置 / 已改绑 / 已停用 / 新增。
    /// 内置没有这条、纯用户新增的绑定（`builtin_emoji` 为空）。
    ///
    /// 这一类的「撤销」是**删掉绑定**，不是「恢复内置」——界面上那个按钮的名字
    /// 要跟着换（v1.67）：给单字挂了一枚想撤掉，看到「恢复内置」根本想不到是它。
    pub fn is_user_only(&self) -> bool {
        self.user && self.builtin_emoji.is_none()
    }

    pub fn state_cell(&self) -> &'static str {
        match (self.user, self.enabled) {
            (false, true) => "内置",
            (true, true) => {
                if self.builtin_emoji.is_some() {
                    "已改绑"
                } else {
                    "新增"
                }
            }
            (_, false) => "已停用",
        }
    }
}

/// 校验「新 emoji」框里填的东西，通过就返回去掉首尾空白的那一枚。
///
/// 三种不合格各有各的说法，**不能合成一句「要单个 emoji」**：真机上用户往框里
/// 敲了「好」，界面先跑去探字形、报「本机字体画不出来（候选窗里会是方块）」，
/// 于是「点了改绑没反应」——他填的压根不是一个 emoji，报的却是字形问题。
pub fn check_input(text: &str) -> Result<String, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("还没填 emoji：点「浏览 emoji…」选一枚，或粘贴一个 emoji".into());
    }
    if !valid_emoji(s) {
        let n = s.chars().count();
        return Err(if n > 1 {
            format!("「{s}」是 {n} 个字符：一条绑定只挂一枚 emoji（如 👍）")
        } else {
            format!("「{s}」不是 emoji：一条绑定只挂一枚 emoji（如 👍），点「浏览 emoji…」选")
        });
    }
    Ok(s.to_string())
}

/// 「绑到」框：填一个词或一个五笔码，**自动判向**，返回对应的 key。
///
/// 为什么自动判而不是让用户选「按词 / 按码」：两个输入域几乎不可能重叠
/// （词是汉字、码是 a–y 的字母），多一个开关就多一次「我该选哪个」的停顿。
/// 判错了也有救——报错里给出两种写法的例子。
pub fn parse_target(text: &str) -> Result<Key, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("还没填「绑到」：写一个词（如 好、好评），或一个五笔码（如 vb）".into());
    }
    // 词与码里出现引号/反斜杠时直接拒绝：解析层的 TOML 子集不认转义
    // （中文词、a–y 的码本来就不会有），写出去一个解析不回的文件才是事故。
    if s.contains('"') || s.contains('\\') {
        return Err("词或码里不能有引号和反斜杠".into());
    }
    if s.chars().all(is_han) {
        let n = s.chars().count();
        if n > 8 {
            return Err(format!("「{s}」有 {n} 个字：一条绑定最多挂 8 个字的词"));
        }
        return Ok(Key::Word(s.to_string()));
    }
    let lower = s.to_ascii_lowercase();
    if lower.chars().all(|c| ('a'..='y').contains(&c)) {
        let n = lower.chars().count();
        if n > 4 {
            return Err(format!("五笔码最长 4 位（「{s}」有 {n} 位）"));
        }
        return Ok(Key::Code(lower));
    }
    Err(format!(
        "「{s}」看不懂：填中文词（如 好、好评），或五笔码（如 vb，字母 a–y）"
    ))
}

/// 汉字判据：基本区 + 扩展 A + 兼容表意字（够覆盖词库里的词）。
fn is_han(c: char) -> bool {
    matches!(c,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}')
}

/// emoji 页模型：两份层 + 指向它们的路径。
pub struct EmojiPage {
    /// 内置表路径（保留给错误信息与将来的「打开内置表」入口）
    #[allow(dead_code)]
    pub builtin_path: PathBuf,
    pub user_path: PathBuf,
    pub builtin: EmojiLayer,
    pub user: EmojiLayer,
    /// 读内置层的错误（缺文件不算错，那是「没装 emoji 数据」的降级）。
    pub builtin_err: Option<String>,
    /// 读用户层的错误（不存在不算错）。
    pub user_err: Option<String>,
}

impl EmojiPage {
    pub fn load(builtin_path: PathBuf, user_path: PathBuf) -> Self {
        let (builtin, builtin_err) = EmojiLayer::load(&builtin_path);
        let (user, user_err) = EmojiLayer::load(&user_path);
        Self {
            builtin_path,
            user_path,
            builtin,
            user,
            builtin_err,
            user_err,
        }
    }

    /// 合并视图。排序：按词绑定的按词排、按码绑定的按码排，词在前——
    /// 与 `heinrich-cli emoji list` 同一口径，搜索时两边都能按前缀找到。
    pub fn rows(&self) -> Vec<EmojiRow> {
        let mut out: Vec<EmojiRow> = Vec::new();
        let mut push = |word: Option<String>,
                        code: Option<String>,
                        builtin: Option<String>,
                        user: bool,
                        enabled: bool,
                        emoji: String| {
            out.push(EmojiRow {
                word,
                code,
                emoji,
                builtin_emoji: builtin,
                user,
                enabled,
            });
        };

        // 内置的每一条：看用户层有没有同 key 的覆盖
        let user_word = |w: &str| {
            self.user
                .entries
                .iter()
                .find(|e| e.word.as_deref() == Some(w))
        };
        let user_code = |c: &str| {
            self.user
                .entries
                .iter()
                .find(|e| e.code.as_deref() == Some(c))
        };
        for e in &self.builtin.entries {
            if let Some(w) = &e.word {
                match user_word(w) {
                    Some(u) => push(
                        Some(w.clone()),
                        None,
                        Some(e.emoji.clone()),
                        true,
                        u.enabled,
                        u.emoji.clone(),
                    ),
                    None => push(
                        Some(w.clone()),
                        None,
                        Some(e.emoji.clone()),
                        false,
                        e.enabled,
                        e.emoji.clone(),
                    ),
                }
            }
            if let Some(c) = &e.code {
                match user_code(c) {
                    Some(u) => push(
                        None,
                        Some(c.clone()),
                        Some(e.emoji.clone()),
                        true,
                        u.enabled,
                        u.emoji.clone(),
                    ),
                    None => push(
                        None,
                        Some(c.clone()),
                        Some(e.emoji.clone()),
                        false,
                        e.enabled,
                        e.emoji.clone(),
                    ),
                }
            }
        }
        // 用户层独有的（内置没有的 key）：改绑的新词、单字、新增按码绑定
        for e in &self.user.entries {
            let covered = match (&e.word, &e.code) {
                (Some(w), _) => self
                    .builtin
                    .entries
                    .iter()
                    .any(|b| b.word.as_deref() == Some(w)),
                (None, Some(c)) => self
                    .builtin
                    .entries
                    .iter()
                    .any(|b| b.code.as_deref() == Some(c)),
                (None, None) => false,
            };
            // 既没词也没码的条目解析期就拦掉了，这里只是防御
            if !covered && (e.word.is_some() || e.code.is_some()) {
                push(
                    e.word.clone(),
                    e.code.clone(),
                    None,
                    true,
                    e.enabled,
                    e.emoji.clone(),
                );
            }
        }

        out.sort_by(|a, b| match (&a.word, &b.word) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.code.cmp(&b.code),
        });
        out
    }

    /// 当前生效的查表结构。界面目前只画合并视图，用不到它；单测靠它断言
    /// 「写盘之后查表语义跟着变」（这正是用户在意的东西）。
    #[allow(dead_code)]
    pub fn map(&self) -> EmojiMap {
        EmojiMap::build(&[&self.builtin, &self.user])
    }

    /// 内置表里出现过的全部 emoji（去重、排序）——选择器与字形探测的数据源。
    pub fn builtin_choices(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .builtin
            .entries
            .iter()
            .map(|e| e.emoji.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// 选择器展示的全集：内置表用过的 ∪ 用户层写过的。
    ///
    /// 只列内置表就给「配一枚表里没有的 emoji」留了死角——用户手动往用户层
    /// 写过的那几枚（比如他自己加的 🫠）也该出现在网格里，否则他想把同一枚
    /// 用到第二个词上还得再粘一次。
    pub fn choices_for_picker(&self) -> Vec<String> {
        let mut v = self.builtin_choices();
        v.extend(self.user.entries.iter().map(|e| e.emoji.clone()));
        v.retain(|e| !e.is_empty());
        v.sort();
        v.dedup();
        v
    }

    /// 改绑 / 新增：把 key 指到 `emoji`。校验不过就返回人话，**不写盘**。
    pub fn rebind(&mut self, key: &Key, emoji: &str) -> Result<(), String> {
        let emoji = check_input(emoji)?;
        // 词与码里出现引号/反斜杠时直接拒绝：解析层的 TOML 子集不认转义
        // （中文词、a–y 的码本来就不会有），写出去一个解析不回的文件才是事故。
        let text = match key {
            Key::Word(w) => w.as_str(),
            Key::Code(c) => c.as_str(),
        };
        if text.contains('"') || text.contains('\\') {
            return Err("词或码里不能有引号和反斜杠".into());
        }
        self.remove_user_entries(key);
        self.user.entries.push(EmojiEntry {
            word: key.word().map(str::to_string),
            code: key.code().map(str::to_string),
            emoji,
            enabled: true,
            line: 0,
        });
        self.save()
    }

    /// 停用 / 恢复：一律写一条带 `enabled` 的用户层条目。
    ///
    /// v1.67 之前这里对「用户新增的 key」直接报错（当时的理由是「没有内置值可回退，
    /// 停用等于删」），结果是给单字绑了一枚之后**按钮是灰的**——用户想先关掉它
    /// 再观察候选，界面上没有任何入口。其实停用对用户新增的条目完全成立：条目留在
    /// 用户层，只是 `enabled = false`，候选里不再挂 emoji；删不删是另一件事
    /// （界面上的「删除绑定」）。
    pub fn set_disabled(&mut self, key: &Key, disabled: bool) -> Result<(), String> {
        // 恢复时用「当前这枚 emoji」：用户新增的条目取用户层，内置条目取内置值
        let emoji = match self.user_entry(key) {
            Some(u) => u.emoji.clone(),
            None => match self.builtin_entry(key) {
                Some(b) => b.emoji.clone(),
                None => return Err("这条还没有绑定，没有可停用的东西".to_string()),
            },
        };
        // 启用后与内置值一样时直接删条目：留着一条「和内置完全相同」的用户层记录，
        // 状态列会显示「已改绑」，看着像改过却什么也没变。
        if !disabled && self.builtin_entry(key).is_some_and(|b| b.emoji == emoji) {
            return self.restore(key);
        }
        self.remove_user_entries(key);
        self.user.entries.push(EmojiEntry {
            word: key.word().map(str::to_string),
            code: key.code().map(str::to_string),
            emoji,
            enabled: !disabled,
            line: 0,
        });
        self.save()
    }

    /// 删掉用户层里这个 key 的全部条目。
    ///
    /// 两种语义同一个动作，看内置有没有这个 key：内置有 = 恢复内置值；
    /// 内置没有（用户新增）= **解绑**，那一行直接从列表里消失。界面上按钮的
    /// 名字由 [`EmojiRow::is_user_only`] 决定。
    pub fn restore(&mut self, key: &Key) -> Result<(), String> {
        self.remove_user_entries(key);
        self.save()
    }

    fn builtin_entry(&self, key: &Key) -> Option<&EmojiEntry> {
        self.builtin.entries.iter().find(|e| key.matches(e))
    }

    fn user_entry(&self, key: &Key) -> Option<&EmojiEntry> {
        self.user.entries.iter().find(|e| key.matches(e))
    }

    fn remove_user_entries(&mut self, key: &Key) {
        self.user.entries.retain(|e| !key.matches(e));
    }

    /// 整份重写用户层：模板头（与 `heinrich-cli emoji reset` 同一份说明）+ 条目。
    /// 临时文件 + 改名，server 的每秒 stat 读不到半个文件。
    fn save(&self) -> Result<(), String> {
        let mut text = String::from(HEADER);
        for e in &self.user.entries {
            text.push_str("[[emoji]]\n");
            if let Some(w) = &e.word {
                text.push_str(&format!("word = \"{}\"\n", w));
            }
            if let Some(c) = &e.code {
                text.push_str(&format!("code = \"{}\"\n", c));
            }
            text.push_str(&format!("emoji = \"{}\"\n", e.emoji));
            if !e.enabled {
                text.push_str("enabled = false\n");
            }
            text.push('\n');
        }
        if let Some(dir) = self.user_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("创建目录 {} 失败：{e}", dir.display()))?;
        }
        let tmp = self.user_path.with_extension("toml.tmp");
        std::fs::write(&tmp, text.as_bytes())
            .map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.user_path)
            .map_err(|e| format!("落盘 {} 失败：{e}", self.user_path.display()))?;
        Ok(())
    }
}

/// 一条绑定的 key（按词或按码）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Word(String),
    Code(String),
}

impl Key {
    pub fn word(&self) -> Option<&str> {
        match self {
            Key::Word(w) => Some(w),
            Key::Code(_) => None,
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            Key::Code(c) => Some(c),
            Key::Word(_) => None,
        }
    }

    /// 直接写回「绑到」框的样子（词就是词，码就是码——不带前缀）。
    pub fn label(&self) -> String {
        match self {
            Key::Word(w) => w.clone(),
            Key::Code(c) => c.clone(),
        }
    }

    /// 给人看的名字：码加一个「码 」前缀，免得和音节混。
    pub fn display(&self) -> String {
        match self {
            Key::Word(w) => w.clone(),
            Key::Code(c) => format!("码 {c}"),
        }
    }

    fn matches(&self, e: &EmojiEntry) -> bool {
        match self {
            Key::Word(w) => e.word.as_deref() == Some(w.as_str()),
            Key::Code(c) => e.code.as_deref() == Some(c.as_str()),
        }
    }
}

/// 用户层头部说明（与 `heinrich-cli emoji reset` 写出的样例同一口径）。
const HEADER: &str = "\
# 亨利输入法 · 词组 emoji 用户层（§6.4）
# 保存即生效（≤ 1 秒），不用重启输入法。整份删掉 = 回到只用内置表。
# 这份文件通常由设置窗口的 emoji 页维护；手改也认，格式同下。
#
";

/// `emoji.enabled` 现在的值（没写 = 开）。
pub fn emoji_enabled(config_path: &Path) -> bool {
    heinrich_scheme::Config::load(config_path).emoji_enabled()
}

/// 总开关写盘。**开 = 删行**（默认就是开，写死 `true` 等于把默认值钉在文件里，
/// 与通用页「恢复默认 = 删行」同一纪律）；关 = 写 `false`。
pub fn set_emoji_enabled(config_path: &Path, on: bool) -> std::io::Result<()> {
    heinrich_scheme::edit_config_file(
        config_path,
        &[("emoji.enabled", if on { None } else { Some("false") })],
    )
}

/// 用户层默认位置：`%APPDATA%\Heinrich\emoji.user.toml`（与 server 的 `--emoji` 默认一致）。
pub fn default_user_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Heinrich").join("emoji.user.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 输入框的三种不合格（v1.64）：空、不是 emoji、多枚。文案要说得清是哪一种——
    /// 真机上把「好」报成「本机字体画不出来」，用户以为自己操作对了却没反应。
    #[test]
    fn check_input_says_which_kind_of_wrong() {
        assert!(check_input("  ").unwrap_err().contains("还没填"));
        let e = check_input("好").unwrap_err();
        assert!(e.contains("不是 emoji"), "{e}");
        let e = check_input("👍👍").unwrap_err();
        assert!(e.contains("是 2 个字符"), "{e}");
        assert_eq!(check_input(" 👍 ").unwrap(), "👍", "两端空白要吃掉");
        assert_eq!(check_input("❤️").unwrap(), "❤️", "变体选择符算一枚");
    }

    /// 「绑到」框的自动判向（v1.65）：汉字 → 词、a–y → 码，掺了别的一律报错。
    ///
    /// 这个函数决定了「能不能把 emoji 绑到单字上」——单字就是长度 1 的词，
    /// 不需要任何特殊分支；当年绑不上是因为界面上没地方填词（列表按词列，
    /// 没绑过的词不在列表里），不是数据层不收。
    #[test]
    fn parse_target_picks_word_or_code() {
        assert_eq!(parse_target(" 好 ").unwrap(), Key::Word("好".into()));
        assert_eq!(parse_target("好评").unwrap(), Key::Word("好评".into()));
        assert_eq!(parse_target(" VB ").unwrap(), Key::Code("vb".into()));
        assert_eq!(parse_target("vbkt").unwrap(), Key::Code("vbkt".into()));
        // 空 / 混写 / 太长 / 引号，四种各有各的说法
        assert!(parse_target(" ").unwrap_err().contains("还没填"));
        assert!(parse_target("好a").unwrap_err().contains("看不懂"));
        assert!(parse_target("abcdefgh").unwrap_err().contains("最长 4 位"));
        assert!(parse_target("vbtkz").unwrap_err().contains("看不懂"));
        assert!(parse_target("一二三四五六七八九")
            .unwrap_err()
            .contains("最多挂 8"));
        assert!(parse_target("a\"b").unwrap_err().contains("引号"));
        // z 不是码字母（只做通配），这里必须判成「看不懂」而不是一个查不到的码
        assert!(parse_target("vz").unwrap_err().contains("看不懂"));
    }

    /// `rebind` 也必须走同一条校验：两个字符拼一起、ZWJ 序列都不收。
    #[test]
    fn rebind_rejects_non_emoji_input() {
        let d = temp_dir("rebind-check");
        let mut page = EmojiPage::load(d.join("emoji.toml"), d.join("user.toml"));
        let key = Key::Word("好评".into());
        assert!(page.rebind(&key, "好").is_err());
        assert!(page.rebind(&key, "👍👍").is_err());
        assert!(page.rebind(&key, "").is_err());
        assert!(page.rebind(&key, "👍").is_ok());
    }

    /// 用户新增的条目也能停用（v1.67）。当年这里直接返回错误——「没有内置值可回退，
    /// 停用等于删」——界面上「停用」是灰的，给单字挂了一枚想先关掉它就没路走。
    #[test]
    fn 用户新增的绑定也能停用与恢复() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("user-disable").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp.clone(), up.clone());
        let key = Key::Word("好".into());
        page.rebind(&key, "👍").unwrap();
        let row = page
            .rows()
            .into_iter()
            .find(|r| r.key_cell() == "好")
            .expect("单字「好」应当在列表里");
        assert!(row.is_user_only(), "内置没有「好」，这是纯用户新增");
        assert_eq!(row.state_cell(), "新增");

        page.set_disabled(&key, true).unwrap();
        let text = std::fs::read_to_string(&up).unwrap();
        assert!(text.contains("word = \"好\""), "{text}");
        assert!(text.contains("👍"), "停用不能丢掉那枚 emoji：\n{text}");
        assert!(text.contains("enabled = false"), "{text}");

        let back = EmojiPage::load(bp.clone(), up.clone());
        let row = back
            .rows()
            .into_iter()
            .find(|r| r.key_cell() == "好")
            .unwrap();
        assert_eq!(row.state_cell(), "已停用");
        assert_eq!(row.emoji, "👍", "停用时显示的是恢复后会用的那枚");
        assert!(!row.enabled);

        // 再打开：还是同一枚，状态回「新增」
        page.set_disabled(&key, false).unwrap();
        let back = EmojiPage::load(bp, up);
        let row = back
            .rows()
            .into_iter()
            .find(|r| r.key_cell() == "好")
            .unwrap();
        assert_eq!(row.state_cell(), "新增");
        assert!(row.enabled);
        assert_eq!(row.emoji, "👍");
    }

    /// 解绑：用户新增的条目删干净，行从列表消失（内置条目则回到内置值）。
    #[test]
    fn 解绑删掉用户新增的条目() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("unbind").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp.clone(), up.clone());
        page.rebind(&Key::Word("好".into()), "👍").unwrap();
        page.restore(&Key::Word("好".into())).unwrap();
        assert!(
            !page.rows().iter().any(|r| r.key_cell() == "好"),
            "解绑后这一行应当消失"
        );
        // 内置条目走同一条路 = 回到内置值，行还在
        page.rebind(&Key::Word("好评".into()), "🎉").unwrap();
        page.restore(&Key::Word("好评".into())).unwrap();
        let row = EmojiPage::load(bp, up)
            .rows()
            .into_iter()
            .find(|r| r.key_cell() == "好评")
            .unwrap();
        assert_eq!(row.emoji, "👍");
        assert_eq!(row.state_cell(), "内置");
    }

    /// 没有任何绑定的 key 停用要给出人话，而不是悄悄写一条空 emoji 的条目。
    #[test]
    fn 停用没有绑定的目标会报错() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("disable-missing").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp, up.clone());
        let e = page
            .set_disabled(&Key::Word("没有这条".into()), true)
            .unwrap_err();
        assert!(e.contains("还没有绑定"), "{e}");
        assert!(!up.exists() || !std::fs::read_to_string(&up).unwrap().contains("没有这条"));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        // 计数器：测试并行跑，同名目录会被另一个线程 remove_dir_all 掉
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "heinrich-emoji-page-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn builtin_with(entries: &str) -> PathBuf {
        let d = temp_dir("builtin");
        let p = d.join("emoji.toml");
        std::fs::write(&p, entries).unwrap();
        p
    }

    const BUILTIN: &str = "\
[[emoji]]
word = \"你好\"
emoji = \"👋\"

[[emoji]]
word = \"好评\"
emoji = \"👍\"

[[emoji]]
code = \"vbg\"
emoji = \"👌\"
";

    #[test]
    fn 合并视图三个状态() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("merged").join("emoji.user.toml");
        std::fs::write(
            &up,
            "[[emoji]]\nword = \"你好\"\nemoji = \"👏\"\n\n[[emoji]]\nword = \"好评\"\nemoji = \"👍\"\nenabled = false\n",
        )
        .unwrap();
        let page = EmojiPage::load(bp, up);
        let rows = page.rows();
        let you = rows.iter().find(|r| r.key_cell() == "你好").unwrap();
        assert_eq!(you.emoji, "👏");
        assert_eq!(you.state_cell(), "已改绑");
        let hao = rows.iter().find(|r| r.key_cell() == "好评").unwrap();
        assert_eq!(hao.emoji, "👍", "停用时显示的是恢复后会用的值");
        assert_eq!(hao.state_cell(), "已停用");
        assert!(!hao.enabled);
        let code = rows.iter().find(|r| r.key_cell() == "码 vbg").unwrap();
        assert_eq!(code.state_cell(), "内置");
        assert_eq!(code.bind_cell(), "按码");
    }

    #[test]
    fn 改绑落盘且能读回() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("rebind").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp.clone(), up.clone());
        page.rebind(&Key::Word("你好".into()), "🎉").unwrap();
        // 没有 .tmp 残留
        assert!(!up.with_extension("toml.tmp").exists());
        // 重读：合并视图与生效查表都是新值
        let back = EmojiPage::load(bp, up.clone());
        let rows = back.rows();
        let you = rows.iter().find(|r| r.key_cell() == "你好").unwrap();
        assert_eq!(you.emoji, "🎉");
        assert_eq!(you.state_cell(), "已改绑");
        assert_eq!(back.map().lookup("你好", ""), Some("🎉"));
        let _ = std::fs::remove_dir_all(up.parent().unwrap());
    }

    #[test]
    fn 停用与恢复内置() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("disable").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp.clone(), up.clone());
        page.set_disabled(&Key::Word("你好".into()), true).unwrap();
        let rows = EmojiPage::load(bp.clone(), up.clone()).rows();
        assert_eq!(
            rows.iter()
                .find(|r| r.key_cell() == "你好")
                .unwrap()
                .state_cell(),
            "已停用"
        );
        assert_eq!(
            EmojiPage::load(bp.clone(), up.clone())
                .map()
                .lookup("你好", ""),
            None,
            "停用后查表就是没有"
        );
        page.restore(&Key::Word("你好".into())).unwrap();
        let rows = EmojiPage::load(bp, up.clone()).rows();
        let you = rows.iter().find(|r| r.key_cell() == "你好").unwrap();
        assert_eq!(you.state_cell(), "内置");
        assert_eq!(you.emoji, "👋");
        let _ = std::fs::remove_dir_all(up.parent().unwrap());
    }

    /// 用户新增的绑定（内置没有的词 / 单字）：停用 = 删掉，恢复也是删掉。
    #[test]
    fn 用户新增与单字条目() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("new").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp.clone(), up.clone());
        // 单字放行（D85）
        page.rebind(&Key::Word("好".into()), "👍").unwrap();
        let rows = EmojiPage::load(bp.clone(), up.clone()).rows();
        let hao = rows.iter().find(|r| r.key_cell() == "好").unwrap();
        assert_eq!(hao.bind_cell(), "按词·单字");
        assert_eq!(hao.state_cell(), "新增");
        assert_eq!(
            EmojiPage::load(bp.clone(), up.clone())
                .map()
                .lookup("好", ""),
            Some("👍")
        );

        // 新增条目也能停用（v1.67 修正）：条目留在用户层、`enabled = false`，
        // 候选里不再挂这枚——早先这里直接报错，界面上的「停用」因此是灰的。
        page.set_disabled(&Key::Word("好".into()), true).unwrap();
        let text = std::fs::read_to_string(&up).unwrap();
        assert!(text.contains("enabled = false"), "{text}");
        assert!(
            EmojiPage::load(bp.clone(), up.clone())
                .map()
                .lookup("好", "")
                .is_none(),
            "停用后不该再命中"
        );
        // 启用回来还是那一枚
        page.set_disabled(&Key::Word("好".into()), false).unwrap();
        assert_eq!(
            EmojiPage::load(bp.clone(), up.clone()).map().lookup("好", ""),
            Some("👍")
        );
        // 真正撤掉绑定 = restore（界面上叫「删除绑定」）
        page.restore(&Key::Word("好".into())).unwrap();
        assert!(EmojiPage::load(bp, up.clone())
            .map()
            .lookup("好", "")
            .is_none());
        let _ = std::fs::remove_dir_all(up.parent().unwrap());
    }

    #[test]
    fn 非法emoji不写盘() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("bad").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp, up.clone());
        assert!(page.rebind(&Key::Word("你好".into()), "").is_err());
        assert!(page.rebind(&Key::Word("你好".into()), "👍👍").is_err());
        assert!(page.rebind(&Key::Word("你好".into()), "abc").is_err());
        assert!(!up.exists(), "校验不过不该碰盘");
        let _ = std::fs::remove_dir_all(up.parent().unwrap());
    }

    /// 词里出现引号/反斜杠直接拒绝（解析层的 TOML 子集不认转义，
    /// 写出去一个解析不回的文件才是事故）。
    #[test]
    fn 特殊字符拒绝() {
        let bp = builtin_with(BUILTIN);
        let up = temp_dir("escape").join("emoji.user.toml");
        let mut page = EmojiPage::load(bp, up.clone());
        assert!(page
            .rebind(&Key::Word("说\"你好\"".into()), "👍")
            .unwrap_err()
            .contains("引号"));
        assert!(page
            .rebind(&Key::Code("vb\\g".into()), "👍")
            .unwrap_err()
            .contains("反斜杠"));
        assert!(!up.exists(), "校验不过不该碰盘");
        let _ = std::fs::remove_dir_all(up.parent().unwrap());
    }

    /// 三份用户文件互不干扰（M4d 验收 ③ 的数据层部分）：
    /// 改 emoji 用户层不许碰 config.toml / words.tsv。
    #[test]
    fn 三份用户文件都保留() {
        let d = temp_dir("three");
        let cfg = d.join("config.toml");
        std::fs::write(&cfg, "# 手写注释\nhint.mode = \"off\"\n").unwrap();
        let words = d.join("words.tsv");
        std::fs::write(&words, "微信\ttmwy\t\t\n").unwrap();
        let bp = builtin_with(BUILTIN);
        let up = d.join("emoji.user.toml");

        let mut page = EmojiPage::load(bp, up);
        page.rebind(&Key::Word("你好".into()), "🎉").unwrap();
        set_emoji_enabled(&cfg, false).unwrap();

        assert!(std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("# 手写注释"));
        assert!(std::fs::read_to_string(&words).unwrap().starts_with("微信"));
        assert!(!emoji_enabled(&cfg));
        // 再删行恢复默认 = 开
        set_emoji_enabled(&cfg, true).unwrap();
        assert!(emoji_enabled(&cfg), "开 = 删行（默认值）");
        assert!(!std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("emoji.enabled"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
