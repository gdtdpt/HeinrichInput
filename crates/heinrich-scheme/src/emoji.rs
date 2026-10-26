//! 词组 → emoji 的两层映射（§6.4，D9 / D66）。
//!
//! ```toml
//! [[emoji]]
//! word = "好评"
//! emoji = "👍"
//!
//! [[emoji]]
//! code = "vbgu"      # 按五笔码绑：命中该码的候选都带此 emoji
//! emoji = "👍"
//!
//! [[emoji]]
//! word = "开心"
//! emoji = "😊"
//! enabled = false    # 停用（吃掉内置层同 key 的条目）
//! ```
//!
//! ## 为什么这里又手写一个解析器
//!
//! 与 `Manifest` / `Config` 同一条理由：本 crate 零第三方依赖（D8 的连锁约束），
//! 而这份文件只有「`[[emoji]]` + 四个键」这一种形状。手写的好处是**能给出带行号的
//! 警告**并且宽容——用户手改时把 `emoji = "👍"` 写成别的，坏行只影响它自己。
//!
//! ## 合并语义（对应 M4c 验收 ② ③ ⑤）
//!
//! - 用户层按 key（`word` 或 `code`）**覆盖**内置层；
//! - `enabled = false` 等于「吃掉」同 key 的内置值（不是「不覆盖」，是**删掉**）；
//! - 同一个词只留一个 emoji：按词绑定优先于按码绑定（词更具体）；
//! - 后写的条目覆盖先写的（文件内顺序就是优先级，用户改绑不必删旧行）。

use std::collections::HashMap;
use std::path::Path;

use crate::code::Code;
use crate::hash::sha256_hex;

/// 一条映射。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiEntry {
    /// 按词绑定。`None` = 这条没写 `word`。
    pub word: Option<String>,
    /// 按五笔码绑定。`None` = 这条没写 `code`。
    pub code: Option<String>,
    pub emoji: String,
    /// `false` 表示停用（吃掉内置层同 key 的条目）。
    pub enabled: bool,
    /// 条目在文件里的起始行号（1 起），警告用。
    pub line: usize,
}

/// 一份映射文件（内置或用户层），宽容解析：坏行进 [`Self::warnings`] 而不是失败。
#[derive(Clone, Debug, Default)]
pub struct EmojiLayer {
    pub entries: Vec<EmojiEntry>,
    /// 头部声明的条目数（生成器写的，用户层没有）。
    pub entry_count: Option<usize>,
    /// 头部声明的正文哈希（生成器写的，用户层没有）。
    pub sha256: Option<String>,
    pub warnings: Vec<String>,
}

impl EmojiLayer {
    /// 解析一份 TOML 子集。**永不失败**：读不懂的行记警告后跳过。
    pub fn parse(text: &str) -> Self {
        let mut out = Self::default();
        let mut cur: Option<Draft> = None;

        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[emoji]]" {
                if let Some(d) = cur.take() {
                    out.finish(d, line_no);
                }
                cur = Some(Draft::new(line_no));
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                out.warnings
                    .push(format!("第 {line_no} 行：缺少 '='，已跳过：{line}"));
                continue;
            };
            let key = key.trim();
            let value = value.trim();

            let Some(draft) = cur.as_mut() else {
                // 头部元数据（entry_count / sha256）与将来可能加的顶层键：
                // 认识的记下来，不认识的忽略——它不是条目，不该产生警告噪音。
                match key {
                    "entry_count" => {
                        out.entry_count = value.parse::<usize>().ok();
                    }
                    "sha256" => {
                        out.sha256 = unquote(value).map(str::to_string);
                    }
                    _ => {}
                }
                continue;
            };

            match key {
                "word" => match unquote(value) {
                    Some(s) if !s.is_empty() => draft.word = Some(s.to_string()),
                    _ => out.warnings.push(format!(
                        "第 {line_no} 行：word 必须是带引号的非空字符串，已跳过这个值"
                    )),
                },
                "code" => match unquote(value) {
                    Some(s) if !s.is_empty() => draft.code = Some(s.to_string()),
                    _ => out.warnings.push(format!(
                        "第 {line_no} 行：code 必须是带引号的非空字符串，已跳过这个值"
                    )),
                },
                "emoji" => match unquote(value) {
                    Some(s) => draft.emoji = Some(s.to_string()),
                    None => out
                        .warnings
                        .push(format!("第 {line_no} 行：emoji 必须带引号")),
                },
                "enabled" => match value {
                    "true" => draft.enabled = true,
                    "false" => draft.enabled = false,
                    other => out.warnings.push(format!(
                        "第 {line_no} 行：enabled 只认 true / false，收到 `{other}`"
                    )),
                },
                other => out
                    .warnings
                    .push(format!("第 {line_no} 行：不认识的键 `{other}`，已忽略")),
            }
        }
        if let Some(d) = cur.take() {
            out.finish(d, usize::MAX);
        }
        out
    }

    /// 从文件读。文件不存在返回空层（**不算错**：用户层就是「可选」）。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                (Self::parse(text), None)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("读取 {} 失败：{e}", path.display())),
            ),
        }
    }

    fn finish(&mut self, d: Draft, _end_line: usize) {
        let line = d.line;
        let Draft {
            word,
            code,
            emoji,
            enabled,
            ..
        } = d;
        if word.is_none() && code.is_none() {
            self.warnings.push(format!(
                "第 {line} 行起的条目既没有 word 也没有 code，已跳过"
            ));
            return;
        }
        let Some(emoji) = emoji else {
            self.warnings.push(format!(
                "第 {line} 行起的条目缺少 emoji，已跳过（word={word:?} code={code:?}）"
            ));
            return;
        };
        if !valid_emoji(&emoji) {
            self.warnings
                .push(format!("第 {line} 行：`{emoji}` 不是单个 emoji，已跳过"));
            return;
        }
        if word.as_ref().is_some_and(|w| w.chars().count() < 2) {
            self.warnings.push(format!(
                "第 {line} 行：`{}` 是单字，单字不挂 emoji（§6.4），已跳过",
                word.unwrap_or_default()
            ));
            return;
        }
        if let Some(c) = &code {
            // 只做**形状**校验（a–y、≤4 码）。「这个码在不在码表里」是
            // `heinrich-cli emoji check` 的活——解析层看不到词库。
            if Code::parse(c).is_none() {
                self.warnings.push(format!(
                    "第 {line} 行：`{c}` 不是合法五笔码（只允许 a–y、最多 4 码），已跳过"
                ));
                return;
            }
        }
        self.entries.push(EmojiEntry {
            word,
            code,
            emoji,
            enabled,
            line,
        });
    }

    /// 核对生成器写在头部的正文哈希。返回 `None` = 一致或没写；
    /// `Some(原因)` = 不一致（文件被手改过 / 版本混装）。
    ///
    /// 只核对「按词绑定且不带 code」的条目：内置表全是这个形状，用户层不写头部。
    pub fn verify(&self) -> Option<String> {
        let declared = self.sha256.as_ref()?;
        let mut body = String::new();
        for e in self.entries.iter().filter(|e| e.code.is_none()) {
            if let Some(w) = &e.word {
                body.push_str(w);
                body.push('\t');
                body.push_str(&e.emoji);
                body.push('\n');
            }
        }
        let actual = sha256_hex(body.as_bytes());
        if actual == *declared {
            None
        } else {
            Some(format!(
                "emoji 映射表正文与声明的哈希不符（声明 {declared}，实际 {actual}）"
            ))
        }
    }
}

#[derive(Debug)]
struct Draft {
    line: usize,
    word: Option<String>,
    code: Option<String>,
    emoji: Option<String>,
    enabled: bool,
}

impl Draft {
    fn new(line: usize) -> Self {
        Self {
            line,
            word: None,
            code: None,
            emoji: None,
            enabled: true,
        }
    }
}

/// 合并后的查表结构。**不可变**：热重载整份重建，不做原地改。
#[derive(Clone, Debug, Default)]
pub struct EmojiMap {
    by_word: HashMap<String, String>,
    by_code: HashMap<String, String>,
}

impl EmojiMap {
    /// 按「内置层 → 用户层」的顺序叠加。后写的覆盖先写的；停用条目删除同 key。
    pub fn build(layers: &[&EmojiLayer]) -> Self {
        let mut map = Self::default();
        for layer in layers {
            for e in &layer.entries {
                if let Some(word) = &e.word {
                    if e.enabled {
                        map.by_word.insert(word.clone(), e.emoji.clone());
                    } else {
                        map.by_word.remove(word);
                    }
                }
                if let Some(code) = &e.code {
                    if e.enabled {
                        map.by_code.insert(code.clone(), e.emoji.clone());
                    } else {
                        map.by_code.remove(code);
                    }
                }
            }
        }
        map
    }

    /// 查一个候选该挂哪个 emoji。
    ///
    /// `word` 是候选词，`code` 是它的五笔码（没有就传空串）。规则：
    /// ① 单字不挂（§6.4：一个词只挂一个，而单字候选一行好几个，挂上会让行宽翻倍）；
    /// ② 按词绑定优先于按码绑定——词比码具体，两者都给时以词为准。
    pub fn lookup(&self, word: &str, code: &str) -> Option<&str> {
        if word.chars().count() < 2 {
            return None;
        }
        if let Some(e) = self.by_word.get(word) {
            return Some(e.as_str());
        }
        if code.is_empty() {
            return None;
        }
        self.by_code.get(code).map(String::as_str)
    }

    /// 按「这枚 emoji 本机画得出来吗」把映射筛一遍，返回被丢掉的键数。
    ///
    /// 判据由调用方给（server 传本机字体探测的结果）：本 crate 平台中立，
    /// 不认识 DirectWrite，只认识「保留还是丢掉」这个布尔。
    ///
    /// 丢掉的是**键**，不只是那一条显示：装不出来的 emoji 连候选都不该出现，
    /// 否则用户选中它，文档里就是一排方块。
    pub fn retain_supported(&mut self, supported: impl Fn(&str) -> bool) -> usize {
        let before = self.by_word.len() + self.by_code.len();
        self.by_word.retain(|_, e| supported(e));
        self.by_code.retain(|_, e| supported(e));
        before - (self.by_word.len() + self.by_code.len())
    }

    pub fn word_len(&self) -> usize {
        self.by_word.len()
    }

    pub fn code_len(&self) -> usize {
        self.by_code.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_word.is_empty() && self.by_code.is_empty()
    }

    /// 生效中的全部按词绑定（`emoji list` 用），按词排序。
    pub fn words(&self) -> Vec<(&str, &str)> {
        let mut v: Vec<(&str, &str)> = self
            .by_word
            .iter()
            .map(|(k, e)| (k.as_str(), e.as_str()))
            .collect();
        v.sort();
        v
    }

    /// 生效中的全部按码绑定（`emoji list` 用），按码排序。
    pub fn codes(&self) -> Vec<(&str, &str)> {
        let mut v: Vec<(&str, &str)> = self
            .by_code
            .iter()
            .map(|(k, e)| (k.as_str(), e.as_str()))
            .collect();
        v.sort();
        v
    }
}

/// 一个字符是不是 emoji（按 Unicode emoji 属性的主干区间）。
///
/// 不是「所有 ≥ U+2000 的字符」：汉字、全角标点、箭头、括号都在那个范围里，
/// 而 CLDR 的中文注解表确实给 `{`、《 之类写了关键词（`{` → 大括号 | 左花括号），
/// 不过滤的话「括号」会被挂上 `《`。
///
/// 再剔掉两类「单个字符无法独立显示」的：区域指示符（旗帜的一半）与肤色修饰符
/// （`🏻`，只能贴在别人身上）。生成器与装载期共用这一份判据——两份必然漂移。
pub fn is_emoji(c: char) -> bool {
    let cp = c as u32;
    if (0x1F000..=0x1FAFF).contains(&cp) {
        return !(0x1F1E6..=0x1F1FF).contains(&cp) && !(0x1F3FB..=0x1F3FF).contains(&cp);
    }
    (0x2600..=0x27BF).contains(&cp)
        || (0x2300..=0x23FF).contains(&cp)
        || matches!(
            cp,
            0x2B1B | 0x2B1C | 0x2B50 | 0x2B55 | 0x3030 | 0x303D | 0x3297 | 0x3299
        )
}

/// 一个 emoji 值是不是「单个 emoji」。
///
/// 认两种：单码位（`👍`），或「基字符 + `U+FE0F` 变体选择符」（`❤️`）。
/// 后者在 CLDR 与用户手写里都常见，宽度也只算一枚；ZWJ 序列（👨‍👩‍👧）不接受——
/// 它在候选行里会撑到两三枚宽，而 §6.4 的约束是「不给候选行加宽」。
pub fn valid_emoji(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_emoji(first) {
        return false;
    }
    match chars.next() {
        None => true,
        Some('\u{FE0F}') => chars.next().is_none(),
        Some(_) => false,
    }
}

fn unquote(v: &str) -> Option<&str> {
    let v = v.trim();
    let rest = v.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 装不出来的 emoji 必须连键一起丢掉：只丢显示的话，用户选中它就是方块。
    #[test]
    fn retain_supported_drops_keys_entirely() {
        let builtin = EmojiLayer::parse(
            "[[emoji]]\nword = \"你好\"\nemoji = \"👋\"\n\n[[emoji]]\nword = \"您好\"\nemoji = \"🫂\"\n",
        );
        let mut map = EmojiMap::build(&[&builtin]);
        assert_eq!(map.word_len(), 2);

        let dropped = map.retain_supported(|e| e != "🫂");
        assert_eq!(dropped, 1);
        assert_eq!(map.lookup("你好", ""), Some("👋"));
        assert_eq!(map.lookup("您好", ""), None);
    }

    fn layer(text: &str) -> EmojiLayer {
        EmojiLayer::parse(text)
    }

    #[test]
    fn 解析三种形态() {
        let l = layer(
            r#"
[[emoji]]
word = "好评"
emoji = "👍"

[[emoji]]
code = "vbgu"
emoji = "👍"

[[emoji]]
word = "开心"
emoji = "😊"
enabled = false
"#,
        );
        assert_eq!(l.entries.len(), 3);
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.entries[0].word.as_deref(), Some("好评"));
        assert_eq!(l.entries[1].code.as_deref(), Some("vbgu"));
        assert!(!l.entries[2].enabled);
    }

    /// 验收 ③：`enabled = false` 要真的把内置值吃掉，而不是「不覆盖」。
    #[test]
    fn 停用条目吃掉内置值() {
        let builtin = layer("[[emoji]]\nword = \"开心\"\nemoji = \"😊\"\n");
        let user = layer("[[emoji]]\nword = \"开心\"\nemoji = \"😊\"\nenabled = false\n");
        let map = EmojiMap::build(&[&builtin, &user]);
        assert_eq!(map.lookup("开心", ""), None);
    }

    /// 验收 ②：用户层改绑要压过内置层。
    #[test]
    fn 用户层改绑覆盖内置() {
        let builtin = layer("[[emoji]]\nword = \"好评\"\nemoji = \"👍\"\n");
        let user = layer("[[emoji]]\nword = \"好评\"\nemoji = \"👏\"\n");
        let map = EmojiMap::build(&[&builtin, &user]);
        assert_eq!(map.lookup("好评", ""), Some("👏"));
    }

    /// 验收 ④：单字候选不挂 emoji（内置与用户层都拦）。
    #[test]
    fn 单字不挂() {
        let l = layer("[[emoji]]\nword = \"好\"\nemoji = \"👍\"\n");
        assert!(l.entries.is_empty(), "单字条目应在解析期就被拒");
        assert_eq!(l.warnings.len(), 1);

        let good = layer("[[emoji]]\nword = \"好评\"\nemoji = \"👍\"\n");
        let map = EmojiMap::build(&[&good]);
        assert_eq!(map.lookup("好", ""), None);
        assert_eq!(map.lookup("好评", ""), Some("👍"));
    }

    /// 验收 ⑤：同一个词只出一个 emoji；按词绑定压过按码绑定。
    #[test]
    fn 一词一emoji且按词优先() {
        let l = layer(
            r#"
[[emoji]]
code = "vbg"
emoji = "👌"

[[emoji]]
word = "好评"
emoji = "👍"
"#,
        );
        let map = EmojiMap::build(&[&l]);
        // 同时命中词与码：只出一个，且是词绑定的那个
        assert_eq!(map.lookup("好评", "vbg"), Some("👍"));
        // 只有码命中
        assert_eq!(map.lookup("好运", "vbg"), Some("👌"));
        assert_eq!(map.word_len(), 1);
        assert_eq!(map.code_len(), 1);
    }

    /// 按码绑定不能把「同一个码下的所有候选」变成同一个 emoji 之外的东西：
    /// 它本来就是「该码的候选都带这个 emoji」。
    #[test]
    fn 按码绑定对同码候选一律生效() {
        let l = layer("[[emoji]]\ncode = \"vbgu\"\nemoji = \"👍\"\n");
        let map = EmojiMap::build(&[&l]);
        assert_eq!(map.lookup("好评", "vbgu"), Some("👍"));
        assert_eq!(map.lookup("好坏", "vbgu"), Some("👍"));
        assert_eq!(map.lookup("好评", "vbgf"), None);
    }

    #[test]
    fn 坏行只影响它自己() {
        let l = layer(
            r#"
[[emoji]]
word = "好评"
emoji = "👍"

[[emoji]]
word = "开心"
emoji = 52

[[emoji]]
emoji = "😊"

[[emoji]]
word = "生气"
emoji = "😠"
colour = "red"
"#,
        );
        assert_eq!(l.entries.len(), 2, "好与生气两条有效");
        assert!(l.warnings.len() >= 2, "{:?}", l.warnings);
        // 未知键只警告、不影响条目
        assert_eq!(l.entries[1].emoji, "😠");
    }

    #[test]
    fn 后写覆盖先写() {
        let l = layer(
            "[[emoji]]\nword = \"好评\"\nemoji = \"👍\"\n\n[[emoji]]\nword = \"好评\"\nemoji = \"👏\"\n",
        );
        let map = EmojiMap::build(&[&l]);
        assert_eq!(map.lookup("好评", ""), Some("👏"));
    }

    #[test]
    fn emoji_值的合法形状() {
        assert!(valid_emoji("👍"));
        assert!(valid_emoji("❤\u{FE0F}"));
        assert!(!valid_emoji(""), "空串不行");
        assert!(!valid_emoji("好"), "汉字不是 emoji");
        assert!(!valid_emoji("👍👍"), "两个 emoji 会把候选行加宽两倍");
        assert!(!valid_emoji("👨\u{200D}👩\u{200D}👧"), "ZWJ 序列不收");
    }

    /// 生成器的头部哈希能自校验：改一个字符就报出来。
    #[test]
    fn 头部哈希能发现被手改() {
        let body = "好评\t👍\n";
        let digest = sha256_hex(body.as_bytes());
        let text = format!(
            "entry_count = 1\nsha256 = \"{digest}\"\n\n[[emoji]]\nword = \"好评\"\nemoji = \"👍\"\n"
        );
        let l = layer(&text);
        assert_eq!(l.entry_count, Some(1));
        assert_eq!(l.verify(), None, "原样应通过");

        let touched = text.replace("👍", "👏");
        let l2 = layer(&touched);
        assert!(l2.verify().is_some(), "改过就该报不一致");
    }
}
