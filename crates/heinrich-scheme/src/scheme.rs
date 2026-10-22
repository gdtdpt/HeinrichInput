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
        sorted.sort_unstable_by(|&a, &b| {
            let ea = &entries[a as usize];
            let eb = &entries[b as usize];
            ea.code
                .packed()
                .cmp(&eb.code.packed())
                .then_with(|| eb.weight.total_cmp(&ea.weight))
                .then_with(|| ea.word.cmp(&eb.word))
        });

        Ok(Self {
            meta,
            entries,
            stats,
            sorted,
            words: std::sync::OnceLock::new(),
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
