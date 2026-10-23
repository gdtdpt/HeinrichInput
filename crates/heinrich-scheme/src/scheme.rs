//! 方案装载：`scheme.toml` + `lexicon.tsv` → 内存索引。
//!
//! 数据格式选择（D15）：
//!
//! - 大表用**行式 TSV**（`code \t word \t weight \t short`），而不是 TOML。
//!   TOML 适合几十行的配置，不适合 13 万行词库：解析慢一个数量级，且体积膨胀。
//!   行式格式对 `diff` 友好，且手写解析器足够（本 crate 零依赖）。
//! - 小配置（`scheme.toml` / 未来的 `emoji.toml`）继续用 TOML。
//! - 文件内**不写生成时间**，只写来源与计数，否则哈希不可复现。
//!
//! 索引策略：编码打包成 `u32` 后整体排序，前缀查询退化为一次二分（见 [`crate::code`]）。

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use crate::code::{prefix_bounds, Code, MAX_CODE_LEN};
use crate::hash::sha256_hex;
use crate::manifest::{Manifest, ManifestError};

/// 词条在方案里的角色。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodeKind {
    /// 全码（单字全码或词组码）
    Full,
    /// 简码，附带级别（1/2/3 级）
    Short(u8),
}

impl CodeKind {
    pub fn short_level(self) -> u8 {
        match self {
            Self::Full => 0,
            Self::Short(n) => n,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "全码",
            Self::Short(1) => "一级简码",
            Self::Short(2) => "二级简码",
            Self::Short(3) => "三级简码",
            Self::Short(_) => "简码",
        }
    }
}

/// 一条码 → 词。
#[derive(Clone, Debug)]
pub struct Entry {
    pub code: Code,
    pub word: String,
    /// 码表原始权重（越大越常用），保留原始量纲以便溯源
    pub weight: f64,
    /// 归一化到 [0,1] 的静态权重，供排序使用
    pub w_norm: f32,
    pub kind: CodeKind,
}

impl Entry {
    pub fn is_single_char(&self) -> bool {
        self.word.chars().count() == 1
    }
}

/// 一条**自定义词组**（用户词库里的词，码与权重都已解析好）。
///
/// 与 [`Entry`] 分开：`Entry` 是装载后带 `w_norm` 的成品词条，而这里的 `weight`
/// 还是原始量纲，归一化要由 [`Scheme::apply_user_words`] 用基础词库的分母算。
#[derive(Clone, Debug)]
pub struct UserEntry {
    pub code: String,
    pub word: String,
    pub weight: f64,
}

#[derive(Clone, Debug)]
pub struct SchemeMeta {
    pub id: String,
    pub name: String,
    pub version: String,
    pub alphabet: String,
    pub max_code_len: usize,
    pub source: String,
    pub license: String,
    pub entry_count: usize,
    pub lexicon_sha256: String,
}

/// 装载时顺带算好的统计量（供 `heinrich-cli check` 与报告复用）。
#[derive(Clone, Debug, Default)]
pub struct SchemeStats {
    pub total: usize,
    pub single_char: usize,
    pub multi_char: usize,
    /// 下标 = 码长（0 位不用）
    pub by_code_len: [usize; MAX_CODE_LEN + 1],
    pub short_codes: [usize; 4],
    /// 存在多个词的编码数量
    pub shared_codes: usize,
    /// 单编码最大重码数
    pub max_shared: usize,
    /// 一级简码的键位（用于校验「每键至多一条」）
    pub level1_keys: usize,
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Manifest(ManifestError),
    /// 词库某行格式非法
    Lexicon {
        line: usize,
        reason: String,
    },
    /// 清单与词库不一致
    Mismatch {
        reason: String,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "读取失败：{e}"),
            Self::Manifest(e) => write!(f, "{e}"),
            Self::Lexicon { line, reason } => write!(f, "词库第 {line} 行：{reason}"),
            Self::Mismatch { reason } => write!(f, "数据不一致：{reason}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ManifestError> for LoadError {
    fn from(e: ManifestError) -> Self {
        Self::Manifest(e)
    }
}

/// 已装载的方案。
pub struct Scheme {
    pub meta: SchemeMeta,
    pub entries: Vec<Entry>,
    pub stats: SchemeStats,
    /// 按 (编码, 权重降序, 词) 排好的词条下标，前缀查询的二分基础
    sorted: Vec<u32>,
    /// 全部词条的词集合（[`Scheme::has_word`] 用，首次调用时建）。
    ///
    /// 惰性而不是装载时建：纯五笔用户永远用不到它（多段真词过滤是 D64 的
    /// 拼音混输特性），不该为用不到的东西付十几 MB 内存。
    words: std::sync::OnceLock<std::collections::HashSet<String>>,
    /// 基础词库的条数（不含用户词）。用户词恒追加在末尾，重载时截断到这里，
    /// 所以「重载」= 先退回基础库、再合并新的一份，不需要额外的撤销逻辑。
    base_len: usize,
    /// 权重归一化分母（装载期的最大权重）。用户词要按同一个分母算 `w_norm`，
    /// 否则它们与基础词的静态权重不可比——这会让「权重与正常词库一致」落空。
    weight_denom: f64,
    /// 单字 → 全码（[`Scheme::word_code`] 用，首次调用时建）。
    ///
    /// 与 `words` 同样惰性：纯五笔用户不会调取码规则。
    chars: std::sync::OnceLock<std::collections::HashMap<char, String>>,
    /// 按词长分档的基础词库权重中位数（下标 = 词长，≥4 记 4）。
    ///
    /// 自定义词的默认权重取这里——「与正常词库一致」不是一句口号，要落到一个
    /// 具体的数上：新词应该落在同类词的中间，既不置顶也不沉底（D65）。
    len_weights: std::sync::OnceLock<[f64; 5]>,
}

impl fmt::Debug for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scheme")
            .field("id", &self.meta.id)
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl Scheme {
    /// 从目录装载 `<dir>/scheme.toml` 与 `<dir>/lexicon.tsv`。
    pub fn load(dir: &Path) -> Result<Self, LoadError> {
        let manifest_path = dir.join("scheme.toml");
        let lexicon_path = dir.join("lexicon.tsv");

        let manifest_text = read_text(&manifest_path)?;
        let manifest = Manifest::parse(&manifest_text)?;
        let meta = SchemeMeta {
            id: manifest.require("id")?.to_string(),
            name: manifest.require("name")?.to_string(),
            version: manifest.require("version")?.to_string(),
            alphabet: manifest.require("alphabet")?.to_string(),
            max_code_len: manifest.require_usize("max_code_len")?,
            source: manifest.require("source")?.to_string(),
            license: manifest.require("license")?.to_string(),
            entry_count: manifest.require_usize("entry_count")?,
            lexicon_sha256: manifest.require("lexicon_sha256")?.to_string(),
        };
        if meta.max_code_len != MAX_CODE_LEN {
            return Err(LoadError::Mismatch {
                reason: format!(
                    "max_code_len = {}，当前实现只支持 {MAX_CODE_LEN}",
                    meta.max_code_len
                ),
            });
        }

        let raw = read_text(&lexicon_path)?;
        let meta = SchemeMeta {
            lexicon_sha256: if meta.lexicon_sha256 == "skip" {
                // 只在开发期允许跳过校验（例如手改 fixture）
                meta.lexicon_sha256
            } else {
                let actual = sha256_hex(raw.as_bytes());
                if actual != meta.lexicon_sha256 {
                    return Err(LoadError::Mismatch {
                        reason: format!(
                            "lexicon.tsv 哈希不符（清单 {} / 实际 {actual}）",
                            meta.lexicon_sha256
                        ),
                    });
                }
                actual
            },
            ..meta
        };

        let mut entries = parse_lexicon(&raw)?;
        if entries.len() != meta.entry_count {
            return Err(LoadError::Mismatch {
                reason: format!(
                    "清单 entry_count = {}，实际 {} 条",
                    meta.entry_count,
                    entries.len()
                ),
            });
        }

        let max_weight = entries.iter().map(|e| e.weight).fold(0.0_f64, f64::max);
        let denom = (1.0 + max_weight).ln();
        for e in &mut entries {
            e.w_norm = if denom > 0.0 {
                (((1.0 + e.weight).ln() / denom) as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }

        let stats = compute_stats(&entries);
        let mut sorted: Vec<u32> = (0..entries.len() as u32).collect();
        sorted.sort_unstable_by(|&a, &b| cmp_entries(&entries[a as usize], &entries[b as usize]));

        Ok(Self {
            base_len: entries.len(),
            weight_denom: denom,
            meta,
            entries,
            stats,
            sorted,
            words: std::sync::OnceLock::new(),
            chars: std::sync::OnceLock::new(),
            len_weights: std::sync::OnceLock::new(),
        })
    }

    /// 取 `<prefix>` 开头的全部词条下标（编码短于或等于前缀者天然包含在内）。
    ///
    /// 顺序即「码表序」：权重降序，权重相同按词排序。
    pub fn match_indices(&self, prefix: &str) -> &[u32] {
        let Some((lo_key, hi_key)) = prefix_bounds(prefix) else {
            return &[];
        };
        let lo = self
            .sorted
            .partition_point(|&i| self.entries[i as usize].code.packed() < lo_key);
        let hi = self
            .sorted
            .partition_point(|&i| self.entries[i as usize].code.packed() < hi_key);
        &self.sorted[lo..hi]
    }

    pub fn entry(&self, idx: u32) -> &Entry {
        &self.entries[idx as usize]
    }

    /// 精确编码命中（不做前缀展开）。
    ///
    /// 前缀区间内「同长度的编码」必然排在最前（短码尾部补零，打包值最小），
    /// 因此只需在区间内再切一刀。
    pub fn exact_indices(&self, code: &Code) -> &[u32] {
        let slice = self.match_indices(code.as_str());
        let cutoff = slice.partition_point(|&i| self.entries[i as usize].code.len() == code.len());
        &slice[..cutoff]
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// 这个词在五笔词库里吗（D64 多段真词过滤）。
    ///
    /// 不区分词条类型（全码 / 简码 / 词组）：过滤问的是「中文里有没有这个串」，
    /// 「它是用什么码打出来的」是另一回事。单字条目也在集合里，但两段各取
    /// 至少一字拼出的串长度 ≥ 2，永远撞不上单字条目。
    pub fn has_word(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.words
            .get_or_init(|| self.entries.iter().map(|e| e.word.clone()).collect())
            .contains(word)
    }

    // -----------------------------------------------------------------------
    // 自定义词组（D65）
    // -----------------------------------------------------------------------

    /// 单字 → 全码（取字典序最小，与 `attach_wubi_codes` 同一约定）。
    fn char_code(&self, ch: char) -> Option<&str> {
        let map = self.chars.get_or_init(|| {
            let mut map: std::collections::HashMap<char, String> = std::collections::HashMap::new();
            for e in &self.entries {
                if e.kind != CodeKind::Full {
                    continue;
                }
                let mut it = e.word.chars();
                let (Some(c), None) = (it.next(), it.next()) else {
                    continue;
                };
                let code = e.code.as_str();
                match map.get(&c) {
                    Some(prev) if prev.as_str() <= code => {}
                    _ => {
                        map.insert(c, code.to_string());
                    }
                }
            }
            map
        });
        map.get(&ch).map(String::as_str)
    }

    /// 按五笔 86 的词组取码规则，从一个词推出它的码（D65）。
    ///
    /// - 单字：就是它的全码；
    /// - 2 字：每字取前 2 码；
    /// - 3 字：前两字各取首码 + 末字前 2 码；
    /// - ≥4 字：前三字各取首码 + 末字首码。
    ///
    /// 这条规则不是猜的：拿词库里 6.1 万条现成词组回推对照，一致率 99.96%
    /// （例外多是足字旁同码这类人工特例）。它服务两件事——自定义词的码、
    /// 以及日后若要给拼音侧独有的词补五笔码。
    ///
    /// 任一字没有全码就返回 `None`：宁可让这个词只走拼音，也不给一个打不出来的码。
    pub fn word_code(&self, word: &str) -> Option<String> {
        let chars: Vec<char> = word.chars().collect();
        let first = *chars.first()?;
        if chars.len() == 1 {
            return self.char_code(first).map(str::to_string);
        }
        let mut codes = Vec::with_capacity(chars.len());
        for &c in &chars {
            codes.push(self.char_code(c)?);
        }
        // 单字全码本身可能短于要取的位数（.len() 用字节，码表里全是 ASCII）
        let derived = match chars.len() {
            2 => format!("{}{}", head(codes[0], 2), head(codes[1], 2)),
            3 => format!(
                "{}{}{}",
                head(codes[0], 1),
                head(codes[1], 1),
                head(codes[2], 2)
            ),
            n => format!(
                "{}{}{}{}",
                head(codes[0], 1),
                head(codes[1], 1),
                head(codes[2], 1),
                head(codes[n - 1], 1)
            ),
        };
        Some(derived)
    }

    /// 自定义词的默认权重：同词长的中位数（只统计非零权重）。
    ///
    /// 「与正常词库一致」要落成一个具体的数：新词该落在同类词的中间，既不置顶
    /// 也不沉底。排除零权重是因为单字里有 3 万条权重 0 的生僻字，把它们算进
    /// 中位数会把单字词条的默认权重压成 0（等于永远排最后）。
    pub fn default_weight(&self, word: &str) -> f64 {
        let len = word.chars().count().clamp(1, 4);
        self.len_weights
            .get_or_init(|| median_weights(&self.entries))[len]
    }

    /// 用一份用户词**全量替换**（不是叠加）已合并的自定义词，返回生效条数。
    ///
    /// 全量替换是热重载的关键：删掉一行、改一个码，下次重载后必须真的消失。
    /// 实现上就是把词条表截回 [`Self::base_len`] 再追加，天然不需要撤销逻辑。
    ///
    /// 同一 `(码, 词)` 只留一条（保留基础词库那条）：用户把词库里已有的词再加一遍
    /// 是常见操作，不该让候选窗里出现两个一模一样的词。
    pub fn apply_user_words(&mut self, items: &[UserEntry]) -> usize {
        self.entries.truncate(self.base_len);
        let mut added = 0;
        for it in items {
            if it.word.is_empty() {
                continue;
            }
            let Some(code) = Code::parse(&it.code) else {
                continue;
            };
            let weight = if it.weight.is_finite() {
                it.weight.max(0.0)
            } else {
                0.0
            };
            let w_norm = if self.weight_denom > 0.0 {
                (((1.0 + weight).ln() / self.weight_denom) as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            self.entries.push(Entry {
                code,
                word: it.word.clone(),
                weight,
                w_norm,
                kind: CodeKind::Full,
            });
            added += 1;
        }
        self.resort();
        added
    }

    /// 基础词库条数（不含已合并的用户词）。
    pub fn base_entry_count(&self) -> usize {
        self.base_len
    }

    /// 重排索引 + 去重 + 刷新统计与惰性缓存。
    fn resort(&mut self) {
        let mut order: Vec<u32> = (0..self.entries.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| {
            cmp_entries(&self.entries[a as usize], &self.entries[b as usize])
        });

        // 同 (码, 词) 只留一条。比较相邻即可——排序把相同的码放在一起。
        // 冲突时删**下标更大**的那条：基础词条在前，用户词追加在后，于是留下
        // 的总是基础词库那条，用户的那份被静默忽略（它的码本来就一样）。
        let mut drop = vec![false; self.entries.len()];
        for w in order.windows(2) {
            let (i, j) = (w[0] as usize, w[1] as usize);
            let (a, b) = (&self.entries[i], &self.entries[j]);
            if a.code.packed() == b.code.packed() && a.word == b.word {
                drop[i.max(j)] = true;
            }
        }
        if drop.iter().any(|&d| d) {
            let old = std::mem::take(&mut self.entries);
            let mut map = vec![u32::MAX; old.len()];
            let mut kept = Vec::with_capacity(old.len());
            for (idx, e) in old.into_iter().enumerate() {
                if drop[idx] {
                    continue;
                }
                map[idx] = kept.len() as u32;
                kept.push(e);
            }
            self.entries = kept;
            self.sorted = order
                .iter()
                .filter(|&&i| !drop[i as usize])
                .map(|&i| map[i as usize])
                .collect();
        } else {
            self.sorted = order;
        }

        self.stats = compute_stats(&self.entries);
        // 词集合与单字码表都吃全量词条，合并后必须重建（`OnceLock` 就地换新）。
        self.words = std::sync::OnceLock::new();
        self.chars = std::sync::OnceLock::new();
    }
}

/// 读文本并去掉 UTF-8 BOM。
///
/// Windows 上的编辑器（记事本、VS、部分 PowerShell 重定向）给带中文的文本加
/// BOM 是常态；若按字节比对哈希，同一个文件会因为「有没有 BOM」算出两个值。
/// 统一在入口处剥掉，哈希也只覆盖剥离后的内容。
fn read_text(path: &Path) -> Result<String, LoadError> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text))
}

fn parse_lexicon(raw: &str) -> Result<Vec<Entry>, LoadError> {
    let mut out = Vec::with_capacity(140_000);
    for (idx, line) in raw.lines().enumerate() {
        let line_no = idx + 1;
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(code_raw), Some(word), Some(weight_raw), Some(level_raw)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            return Err(LoadError::Lexicon {
                line: line_no,
                reason: "需要 4 列：代码 / 词 / 权重 / 简码级别".into(),
            });
        };
        if cols.next().is_some() {
            return Err(LoadError::Lexicon {
                line: line_no,
                reason: "列数多于 4".into(),
            });
        }
        let code = Code::parse(code_raw).ok_or_else(|| LoadError::Lexicon {
            line: line_no,
            reason: format!("非法编码 `{code_raw}`"),
        })?;
        let weight: f64 = weight_raw.parse().map_err(|_| LoadError::Lexicon {
            line: line_no,
            reason: format!("非法权重 `{weight_raw}`"),
        })?;
        let level: u8 = level_raw.parse().map_err(|_| LoadError::Lexicon {
            line: line_no,
            reason: format!("非法简码级别 `{level_raw}`"),
        })?;
        let kind = if level == 0 {
            CodeKind::Full
        } else {
            if level as usize != code.len() {
                return Err(LoadError::Lexicon {
                    line: line_no,
                    reason: format!("{level} 级简码 `{code_raw}` 长度不是 {level}"),
                });
            }
            if word.chars().count() != 1 {
                return Err(LoadError::Lexicon {
                    line: line_no,
                    reason: format!("简码只能挂单字，`{word}` 是多字词"),
                });
            }
            CodeKind::Short(level)
        };
        out.push(Entry {
            code,
            word: word.to_string(),
            weight,
            w_norm: 0.0,
            kind,
        });
    }
    Ok(out)
}

/// 取码时「前 n 位」的工具：码比 n 短就整条给它（个别字的全码只有 2 位）。
fn head(s: &str, n: usize) -> &str {
    &s[..n.min(s.len())]
}

/// 码表序：码升序 → 权重降序 → 词升序（§5）。装载与用户词重排共用一份，/// 两处一旦分叉，「加词前后同一个字的位次」就会变，用户以为排序坏了。
fn cmp_entries(a: &Entry, b: &Entry) -> std::cmp::Ordering {
    a.code
        .packed()
        .cmp(&b.code.packed())
        .then_with(|| b.weight.total_cmp(&a.weight))
        .then_with(|| a.word.cmp(&b.word))
}

/// 按词长（1–4，≥4 归 4）算非零权重的中位数。下标 0 不用。
fn median_weights(entries: &[Entry]) -> [f64; 5] {
    let mut buckets: [Vec<f64>; 5] = Default::default();
    for e in entries {
        if e.weight <= 0.0 {
            continue;
        }
        buckets[e.word.chars().count().clamp(1, 4)].push(e.weight);
    }
    let mut out = [1.0; 5];
    for (len, mut v) in buckets.into_iter().enumerate() {
        if len == 0 || v.is_empty() {
            continue;
        }
        v.sort_by(f64::total_cmp);
        out[len] = v[v.len() / 2];
    }
    out
}

fn compute_stats(entries: &[Entry]) -> SchemeStats {
    let mut stats = SchemeStats {
        total: entries.len(),
        ..Default::default()
    };
    for e in entries {
        if e.is_single_char() {
            stats.single_char += 1;
        } else {
            stats.multi_char += 1;
        }
        stats.by_code_len[e.code.len()] += 1;
        if let CodeKind::Short(n) = e.kind {
            stats.short_codes[n as usize] += 1;
        }
    }

    // entries 未排序，重码统计单独按编码聚合
    let mut counts: Vec<(u32, usize)> = Vec::new();
    let mut packed: Vec<u32> = entries.iter().map(|e| e.code.packed()).collect();
    packed.sort_unstable();
    for key in packed {
        match counts.last_mut() {
            Some((k, c)) if *k == key => *c += 1,
            _ => counts.push((key, 1)),
        }
    }
    stats.shared_codes = counts.iter().filter(|(_, c)| *c > 1).count();
    stats.max_shared = counts.iter().map(|(_, c)| *c).max().unwrap_or(0);
    stats.level1_keys = entries
        .iter()
        .filter(|e| matches!(e.kind, CodeKind::Short(1)))
        .map(|e| e.code.as_str().to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    stats
}
