//! 五笔字根表 `data/wubi86/roots.toml` 的专用读取与校验。
//!
//! 服务于 PROJECT.md §6.7 的码位键位图：回答「这个字母在键盘上属于哪个键区、
//! 那个键上有哪些字根」，**不回答「这个字是怎么拆的」**——后者需要笔顺与字型
//! 数据，`docs/design/probe-report.md` 已把那条路证伪（R3）。
//!
//! 解析风格与 `manifest.rs` 一致：手写极简、只认本文件实际用到的语法
//! （`[[key]]` 表头 + 标量 + 单行字符数组），其它写法一律报错而不静默忽略。
//! 引入完整 TOML 解析器不划算：为零依赖付出的代价远大于收益。

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::code::Code;
use crate::scheme::Scheme;

/// 86 版公开口径的主字根数（不含键帽变体）。
pub const EXPECTED_ROOT_COUNT: usize = 130;
/// 字根键的字母范围：`z` 是学习键，没有字根。
pub const ALPHABET: &str = "abcdefghijklmnopqrstuvwxy";

/// 一个键位上的全部字根信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRoots {
    /// 字母键，`a`–`y`。
    pub key: char,
    /// 区位号（11–55），与五笔教材的分区一致。
    pub zone: u8,
    /// 助记口诀。
    pub mnemonic: String,
    /// 一级简码字，用于与码表交叉校验。
    pub primary: char,
    /// 主字根，计入 [`EXPECTED_ROOT_COUNT`]。
    pub roots: Vec<char>,
    /// 同键次要变体，不计入总数，只在键帽上以更淡的字号补充显示。
    pub variants: Vec<char>,
}

impl KeyRoots {
    /// 键帽上要显示的字根序列：主字根在前、变体在后。
    pub fn display_roots(&self) -> impl Iterator<Item = char> + '_ {
        self.roots
            .iter()
            .copied()
            .chain(self.variants.iter().copied())
    }
}

/// 全部 25 个键位，按文件顺序（区位序）保存。
#[derive(Debug, Clone, Default)]
pub struct Roots {
    keys: Vec<KeyRoots>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootsError {
    pub line: usize,
    pub reason: String,
}

impl fmt::Display for RootsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "字根表：{}", self.reason)
        } else {
            write!(f, "字根表第 {} 行：{}", self.line, self.reason)
        }
    }
}

impl std::error::Error for RootsError {}

/// 一条校验结果。CLI 与 `check.ps1` 都只读 `pass`。
#[derive(Debug, Clone)]
pub struct CheckItem {
    pub name: &'static str,
    pub pass: bool,
    pub detail: String,
}

impl Roots {
    /// 从目录装载 `<dir>/roots.toml`。
    pub fn load(dir: &Path) -> Result<Self, RootsError> {
        let path = dir.join("roots.toml");
        let text = std::fs::read_to_string(&path).map_err(|e| RootsError {
            line: 0,
            reason: format!("读不到 {}：{e}", path.display()),
        })?;
        Self::parse(&text)
    }

    /// 解析字根表正文。
    pub fn parse(text: &str) -> Result<Self, RootsError> {
        let mut keys: Vec<KeyRoots> = Vec::new();
        let mut cur: Option<(usize, BTreeMap<String, String>)> = None;

        for (idx, raw) in text.lines().enumerate() {
            let line_no = idx + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[key]]" {
                if let Some((start, fields)) = cur.take() {
                    keys.push(build(start, fields)?);
                }
                cur = Some((line_no, BTreeMap::new()));
                continue;
            }
            if line.starts_with('[') {
                return Err(err(line_no, "只支持 `[[key]]` 这一种表头"));
            }
            let Some((k, v)) = line.split_once('=') else {
                return Err(err(line_no, "缺少 '='"));
            };
            let Some((_, fields)) = cur.as_mut() else {
                return Err(err(line_no, "键值出现在 `[[key]]` 之前"));
            };
            let k = k.trim();
            if k.is_empty() {
                return Err(err(line_no, "键为空"));
            }
            if fields.insert(k.to_string(), v.trim().to_string()).is_some() {
                return Err(err(line_no, format!("`{k}` 在同一键位里重复")));
            }
        }
        if let Some((start, fields)) = cur.take() {
            keys.push(build(start, fields)?);
        }
        if keys.is_empty() {
            return Err(err(0, "没有任何 [[key]] 条目"));
        }
        Ok(Self { keys })
    }

    pub fn keys(&self) -> &[KeyRoots] {
        &self.keys
    }

    /// 按字母取键位。
    pub fn get(&self, key: char) -> Option<&KeyRoots> {
        self.keys.iter().find(|k| k.key == key)
    }

    pub fn total_roots(&self) -> usize {
        self.keys.iter().map(|k| k.roots.len()).sum()
    }

    /// 把一个编码逐位展开成键位序列；出现 `z` 或非字母时返回 `None`。
    pub fn lookup(&self, code: &str) -> Option<Vec<&KeyRoots>> {
        let mut out = Vec::with_capacity(code.chars().count());
        for ch in code.chars() {
            out.push(self.get(ch)?);
        }
        Some(out)
    }

    /// 四项结构校验，不依赖码表。
    pub fn validate(&self) -> Vec<CheckItem> {
        let mut items = Vec::new();

        // ① 25 键齐全、字母落在 a–y、区位不重复
        let mut missing: Vec<char> = ALPHABET
            .chars()
            .filter(|c| self.get(*c).is_none())
            .collect();
        let extra: Vec<char> = self
            .keys
            .iter()
            .map(|k| k.key)
            .filter(|c| !ALPHABET.contains(*c))
            .collect();
        let mut zones: Vec<u8> = self.keys.iter().map(|k| k.zone).collect();
        zones.sort_unstable();
        let zone_dup = zones.windows(2).any(|w| w[0] == w[1]);
        let mut detail = format!("{} 键", self.keys.len());
        if !missing.is_empty() {
            missing.sort_unstable();
            detail.push_str(&format!("，缺 {}", missing.iter().collect::<String>()));
        }
        if !extra.is_empty() {
            detail.push_str(&format!("，多 {}", extra.iter().collect::<String>()));
        }
        if zone_dup {
            detail.push_str("，区位重复");
        }
        items.push(CheckItem {
            name: "25 键齐全",
            pass: missing.is_empty() && extra.is_empty() && !zone_dup && self.keys.len() == 25,
            detail,
        });

        // ② 字根总数
        let total = self.total_roots();
        items.push(CheckItem {
            name: "字根总数",
            pass: total == EXPECTED_ROOT_COUNT,
            detail: format!("{total} 条（86 版口径 {EXPECTED_ROOT_COUNT}）"),
        });

        // ③ 助记非空
        let empty: Vec<String> = self
            .keys
            .iter()
            .filter(|k| k.mnemonic.trim().is_empty())
            .map(|k| k.key.to_string())
            .collect();
        items.push(CheckItem {
            name: "助记非空",
            pass: empty.is_empty(),
            detail: if empty.is_empty() {
                format!("{} 键均有助记", self.keys.len())
            } else {
                format!("缺助记：{}", empty.join(" "))
            },
        });

        // ④ 字根不跨键、不键内重复
        let mut seen: BTreeMap<char, char> = BTreeMap::new();
        let mut dups: Vec<String> = Vec::new();
        for k in &self.keys {
            let mut local: Vec<char> = Vec::new();
            for r in k.display_roots() {
                if local.contains(&r) {
                    dups.push(format!("{} 键内重复 `{r}`", k.key));
                }
                local.push(r);
                if let Some(prev) = seen.insert(r, k.key) {
                    if prev != k.key {
                        dups.push(format!("`{r}` 同时在 {prev} 与 {}", k.key));
                    }
                }
            }
        }
        items.push(CheckItem {
            name: "字根无重复",
            pass: dups.is_empty(),
            detail: if dups.is_empty() {
                format!("{} 个字根（含变体）唯一", seen.len())
            } else {
                dups.join("；")
            },
        });

        items
    }

    /// 第五项：一级简码与码表的单字母码一致。需要装载好的方案。
    pub fn check_primary(&self, scheme: &Scheme) -> CheckItem {
        let mut bad: Vec<String> = Vec::new();
        for k in &self.keys {
            let code_text = k.key.to_string();
            let hit = Code::parse(&code_text).is_some_and(|code| {
                scheme.exact_indices(&code).iter().any(|&i| {
                    let w = &scheme.entry(i).word;
                    let mut it = w.chars();
                    it.next() == Some(k.primary) && it.next().is_none()
                })
            });
            if !hit {
                bad.push(format!("{} 应为「{}」", k.key, k.primary));
            }
        }
        CheckItem {
            name: "一级简码与码表一致",
            pass: bad.is_empty(),
            detail: if bad.is_empty() {
                format!("{} 键全部对应", self.keys.len())
            } else {
                bad.join("；")
            },
        }
    }
}

fn build(line: usize, fields: BTreeMap<String, String>) -> Result<KeyRoots, RootsError> {
    let take = |name: &str| -> Result<&String, RootsError> {
        fields
            .get(name)
            .ok_or_else(|| err(line, format!("`[[key]]` 缺少 `{name}`")))
    };

    let key = single_char(take("key")?, line, "key")?;
    if !ALPHABET.contains(key) {
        return Err(err(
            line,
            format!("`key = {key}` 不在 a–y 内（z 是学习键，没有字根）"),
        ));
    }
    let primary = single_char(take("primary")?, line, "primary")?;
    let mnemonic = unquote(take("mnemonic")?)
        .ok_or_else(|| err(line, "`mnemonic` 需要带引号"))?
        .to_string();
    let zone = take("zone")?
        .parse::<u8>()
        .map_err(|_| err(line, "`zone` 需要 11–55 的整数"))?;
    if !(11..=55).contains(&zone) || zone % 10 == 0 || zone % 10 > 5 {
        return Err(err(line, format!("`zone = {zone}` 不是合法的区位号")));
    }
    let roots = char_array(take("roots")?, line, "roots")?;
    let variants = match fields.get("variants") {
        Some(v) => char_array(v, line, "variants")?,
        None => Vec::new(),
    };
    if roots.is_empty() {
        return Err(err(line, "`roots` 不能为空"));
    }
    Ok(KeyRoots {
        key,
        zone,
        mnemonic,
        primary,
        roots,
        variants,
    })
}

fn char_array(raw: &str, line: usize, field: &str) -> Result<Vec<char>, RootsError> {
    let inner = raw
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| err(line, format!("`{field}` 需要单行数组 [\"…\", …]")))?;
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for item in inner.split(',') {
        let item = item.trim();
        let s = unquote(item).ok_or_else(|| err(line, format!("`{field}` 的 `{item}` 缺引号")))?;
        // 注意：这里不能复用 `single_char`——它还要再剥一次引号，而 `s` 已经剥过了
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => out.push(c),
            _ => return Err(err(line, format!("`{field}` 的 `{s}` 不是单个字符"))),
        }
    }
    Ok(out)
}

fn single_char(raw: &str, line: usize, field: &str) -> Result<char, RootsError> {
    let s = unquote(raw).ok_or_else(|| err(line, format!("`{field}` 需要带引号")))?;
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(err(line, format!("`{field}` 的 `{s}` 不是单个字符"))),
    }
}

fn unquote(v: &str) -> Option<&str> {
    if let Some(inner) = v.strip_prefix('"') {
        return inner.strip_suffix('"');
    }
    if let Some(inner) = v.strip_prefix('\'') {
        return inner.strip_suffix('\'');
    }
    None
}

fn err(line: usize, reason: impl Into<String>) -> RootsError {
    RootsError {
        line,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"
# 注释
[[key]]
key = "g"
zone = 11
mnemonic = "王旁青头戋(兼)五一"
primary = "一"
roots = ["王", "一", "五", "戋"]
variants = ["龶"]

[[key]]
key = "k"
zone = 23
mnemonic = "口与川，字根稀"
primary = "中"
roots = ["口", "川"]
"#;

    #[test]
    fn parses_keys_with_arrays_and_variants() {
        let r = Roots::parse(BODY).unwrap();
        assert_eq!(r.keys().len(), 2);
        let g = r.get('g').unwrap();
        assert_eq!(g.zone, 11);
        assert_eq!(g.primary, '一');
        assert_eq!(g.roots, vec!['王', '一', '五', '戋']);
        assert_eq!(g.variants, vec!['龶']);
        assert_eq!(g.display_roots().collect::<String>(), "王一五戋龶");
        // variants 缺省为空，不报错
        assert!(r.get('k').unwrap().variants.is_empty());
        assert_eq!(r.total_roots(), 6);
    }

    #[test]
    fn lookup_walks_every_letter() {
        let r = Roots::parse(BODY).unwrap();
        let hits = r.lookup("gk").unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].key, 'g');
        // z 是学习键，没有字根；非字母同理
        assert!(r.lookup("gz").is_none());
        assert!(r.lookup("g1").is_none());
    }

    #[test]
    fn rejects_bad_shapes() {
        let bad = |s: &str| Roots::parse(s).err().map(|e| e.line);
        // 缺字段
        assert!(bad("[[key]]\nkey = \"g\"\n").is_some());
        // 双字符 "字根"
        assert!(bad(
            "[[key]]\nkey=\"g\"\nzone=11\nmnemonic=\"x\"\nprimary=\"一\"\nroots=[\"王一\"]"
        )
        .is_some());
        // z 没有字根
        assert!(
            bad("[[key]]\nkey=\"z\"\nzone=11\nmnemonic=\"x\"\nprimary=\"一\"\nroots=[\"王\"]")
                .is_some()
        );
        // 多行数组不支持
        assert!(bad(
            "[[key]]\nkey=\"g\"\nzone=11\nmnemonic=\"x\"\nprimary=\"一\"\nroots=[\n\"王\"]"
        )
        .is_some());
        // 表头只认 [[key]]
        assert!(bad("[meta]\nid=1").is_some());
        // 空文件
        assert!(bad("# 只有注释").is_some());
    }

    #[test]
    fn validate_reports_each_invariant() {
        let r = Roots::parse(BODY).unwrap();
        let items = r.validate();
        assert_eq!(items.len(), 4);
        // 只有两条键：25 键与 130 条必然不过，助记与重复应当通过
        assert!(!items[0].pass);
        assert!(items[0].detail.contains("缺"));
        assert!(!items[1].pass);
        assert!(items[2].pass);
        assert!(items[3].pass);
    }

    #[test]
    fn duplicate_root_across_keys_is_reported() {
        let text = r#"
[[key]]
key = "g"
zone = 11
mnemonic = "a"
primary = "一"
roots = ["王", "犬"]

[[key]]
key = "q"
zone = 35
mnemonic = "b"
primary = "我"
roots = ["金", "犬"]
"#;
        let r = Roots::parse(text).unwrap();
        let items = r.validate();
        let dup = items.iter().find(|i| i.name == "字根无重复").unwrap();
        assert!(!dup.pass);
        assert!(dup.detail.contains("犬"));
    }

    #[test]
    fn duplicate_root_inside_one_key_is_reported() {
        let text = r#"
[[key]]
key = "g"
zone = 11
mnemonic = "a"
primary = "一"
roots = ["王", "王"]
"#;
        let r = Roots::parse(text).unwrap();
        let items = r.validate();
        let dup = items.iter().find(|i| i.name == "字根无重复").unwrap();
        assert!(!dup.pass);
        assert!(dup.detail.contains("键内重复"));
    }
}
