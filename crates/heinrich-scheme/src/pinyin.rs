//! 拼音词库：装载、索引、音节切分判定。
//!
//! 与 [`crate::Scheme`] 是**并列的两份数据**，不是它的特例：五笔条目是
//! 「定长编码 → 词」，拼音条目是「变长拼音串 → 词」，两者的匹配语义完全不同
//! （前者查前缀只用于兜底，后者查前缀是常态）。
//!
//! 本模块只做数据与匹配，**不做排序**——排序（学习分、首位保护）在
//! `heinrich-core` 里，由 [`crate::PinyinLexicon::match_indices`] 返回的下标喂进去。
//! 这样拼音候选和五笔候选走的是同一套打分，用户在拼音下用得多，切回五笔也一样靠前。
//!
//! ## 词条格式
//!
//! `data/pinyin/lexicon.tsv`：
//!
//! ```text
//! pinyin<TAB>word<TAB>weight
//! ```
//!
//! 拼音是**去掉音节空格的连续小写串**（`haoping`）。用户敲的是连续字母，
//! 要求他打空格不现实，所以索引里也不存空格。代价是丢掉音节边界
//! （`xian` 同时是「先」和「西安」），但那本来就是一个合法候选集，
//! 需要边界的地方（判断一段字母能不能切成音节）用 `syllables.txt` 现场算。

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::manifest::Manifest;
use crate::scheme::LoadError;

/// 一条拼音词条。
#[derive(Clone, Debug)]
pub struct PinyinEntry {
    /// 无分隔小写拼音串
    pub pinyin: String,
    pub word: String,
    /// 原始权重（保留以便溯源）
    pub weight: f64,
    /// 归一化到 [0,1] 的静态权重
    pub w_norm: f32,
    /// 这个词的**五笔全码**（§6.3 的码提示用）。空串 = 五笔词库里没有这个词。
    ///
    /// 它**不在拼音词库里**：拼音数据来自 `pinyin_simp.dict.yaml`，那里没有五笔码。
    /// 值由 [`PinyinLexicon::attach_wubi_codes`] 在装载后交叉填进来——一份数据只
    /// 描述一件事，跨数据的映射在装载时算一次，而不是生成期把两份数据焊死。
    pub wubi: String,
}

/// 拼音词库。
#[derive(Debug)]
pub struct PinyinLexicon {
    entries: Vec<PinyinEntry>,
    /// 按 `(pinyin, weight 降序, word)` 排好的下标——二分与前缀扫描的基础
    sorted: Vec<u32>,
    /// 音节表（已排序，供展示与统计）
    syllables: Vec<String>,
    /// 音节集合（切分判定用）
    syllable_set: HashSet<String>,
    /// 最长音节长度，切分的枚举上界
    max_syllable: usize,
    /// 全部词条的词集合（[`PinyinLexicon::has_word`] 用，首次调用时建）
    words: std::sync::OnceLock<HashSet<String>>,
    pub entry_count: usize,
    pub syllable_count: usize,
}

/// 一次匹配的上限保护：`a` 开头的前缀区间有几千条，全倒出来没有意义，
/// 而且排序会白算。取够用的量即可——候选窗一屏 5 条，翻十页也才 50。
const SCAN_CAP: usize = 4096;

impl PinyinLexicon {
    /// 从目录装载 `<dir>/lexicon.tsv` + `<dir>/syllables.txt` + `<dir>/scheme.toml`。
    pub fn load(dir: &Path) -> Result<Self, LoadError> {
        let manifest_text = read_text(&dir.join("scheme.toml"))?;
        let manifest = Manifest::parse(&manifest_text)?;
        let entry_count = manifest.require_usize("entry_count")?;
        let want_hash = manifest.require("lexicon_sha256")?.to_string();

        let lex_path = dir.join("lexicon.tsv");
        let text = read_text(&lex_path)?;
        let actual_hash = crate::sha256_hex(text.as_bytes());
        if actual_hash != want_hash {
            return Err(LoadError::Mismatch {
                reason: format!(
                    "拼音词库哈希不符：清单 {want_hash}，实际 {actual_hash}（重新生成用 tools/gen.ps1 -Kind pinyin）"
                ),
            });
        }

        let mut entries = Vec::with_capacity(entry_count);
        for (idx, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut cols = line.split('\t');
            let (Some(pinyin), Some(word)) = (cols.next(), cols.next()) else {
                return Err(LoadError::Lexicon {
                    line: idx + 1,
                    reason: "缺少列（应为 拼音 / 词 / 权重）".into(),
                });
            };
            let weight = cols
                .next()
                .and_then(|w| w.trim().parse::<f64>().ok())
                .ok_or_else(|| LoadError::Lexicon {
                    line: idx + 1,
                    reason: "权重不是数字".into(),
                })?;
            if pinyin.is_empty()
                || word.is_empty()
                || !pinyin.bytes().all(|b| b.is_ascii_lowercase())
            {
                return Err(LoadError::Lexicon {
                    line: idx + 1,
                    reason: "拼音须为非空小写 a–z，词不得为空".into(),
                });
            }
            entries.push(PinyinEntry {
                pinyin: pinyin.to_string(),
                word: word.to_string(),
                weight,
                w_norm: 0.0,
                wubi: String::new(),
            });
        }

        if entries.len() != entry_count {
            return Err(LoadError::Mismatch {
                reason: format!(
                    "清单 entry_count = {entry_count}，实际 {} 条",
                    entries.len()
                ),
            });
        }

        // 归一化与五笔**同一公式**（`ln(1+w)/ln(1+w_max)`），各自用自己的 w_max。
        // 两份词库的量纲因此不同，但排序各自只在本词典内部比较，互不干扰。
        let max_weight = entries.iter().map(|e| e.weight).fold(0.0_f64, f64::max);
        let denom = (1.0 + max_weight).ln();
        for e in &mut entries {
            e.w_norm = if denom > 0.0 {
                (((1.0 + e.weight).ln() / denom) as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
        }

        let mut sorted: Vec<u32> = (0..entries.len() as u32).collect();
        sorted.sort_unstable_by(|&a, &b| {
            let ea = &entries[a as usize];
            let eb = &entries[b as usize];
            ea.pinyin
                .cmp(&eb.pinyin)
                .then_with(|| eb.weight.total_cmp(&ea.weight))
                .then_with(|| ea.word.cmp(&eb.word))
        });

        // 音节表
        let syl_text = read_text(&dir.join("syllables.txt"))?;
        let mut syllables: Vec<String> = syl_text
            .lines()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect();
        syllables.sort();
        syllables.dedup();
        if syllables.is_empty() {
            return Err(LoadError::Mismatch {
                reason: "音节表为空".into(),
            });
        }
        let max_syllable = syllables.iter().map(|s| s.len()).max().unwrap_or(0);
        let syllable_set: HashSet<String> = syllables.iter().cloned().collect();
        let syllable_count = syllables.len();

        Ok(Self {
            entries,
            sorted,
            syllables,
            syllable_set,
            max_syllable,
            words: std::sync::OnceLock::new(),
            entry_count,
            syllable_count,
        })
    }

    pub fn entries(&self) -> &[PinyinEntry] {
        &self.entries
    }

    /// 给每条词条补上它的**五笔全码**，返回补上的条数。
    ///
    /// 这是 §6.3 码提示的数据通路：拼音段候选要显示「这个字用五笔怎么打」，
    /// 而拼音词库里没有五笔码——只能拿五笔词库反查。反查必须在装载时做一次：
    /// 每次按键都扫 13.6 万条是绝不可能的，而建一份常驻的「词 → 码」索引要多背
    /// 约 10 MB，纯粹为了提示不值得。
    ///
    /// **为什么取全码而不是简码**：简码是把全码的后几码省掉，先给完整的才不会让
    /// 用户学出一套「只会打短码」的手感（`好` 的简码 `vb`、全码 `vbg`）。
    ///
    /// 同一词有多条全码时（少见）取字典序最小的，保证两次装载结果一致——
    /// 提示语是给人看的，不能每次启动换一个。
    pub fn attach_wubi_codes(&mut self, scheme: &crate::Scheme) -> usize {
        let mut full: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::with_capacity(scheme.entry_count());
        for i in 0..scheme.entry_count() as u32 {
            let e = scheme.entry(i);
            if e.kind != crate::CodeKind::Full {
                continue;
            }
            let code = e.code.as_str();
            match full.get(e.word.as_str()) {
                Some(prev) if *prev <= code => {}
                _ => {
                    full.insert(e.word.as_str(), code);
                }
            }
        }
        let mut filled = 0;
        for e in &mut self.entries {
            if let Some(code) = full.get(e.word.as_str()) {
                e.wubi.clear();
                e.wubi.push_str(code);
                filled += 1;
            }
        }
        filled
    }

    pub fn entry(&self, idx: u32) -> &PinyinEntry {
        &self.entries[idx as usize]
    }

    pub fn syllables(&self) -> &[String] {
        &self.syllables
    }

    /// 整串能否切分成合法的拼音音节序列（§6.1 的「完全匹配」判据）。
    ///
    /// 用可达性 DP 而不是贪心最长匹配：`xian` 贪心会切成 `xian`，
    /// 但 `xi'an`（西安）也是合法切分，贪心会把后者直接排除掉。
    ///
    /// 非 ASCII 输入一律判否——组合串理论上只有字母，但这里是**唯一**可能被
    /// 任意字节喂到的地方，切 `&str` 时按字节下标会 panic。
    pub fn can_split(&self, s: &str) -> bool {
        if s.is_empty() || !s.is_ascii() {
            return false;
        }
        let b = s.as_bytes();
        let mut ok = vec![false; b.len() + 1];
        ok[0] = true;
        for i in 0..b.len() {
            if !ok[i] {
                continue;
            }
            let upper = self.max_syllable.min(b.len() - i);
            for len in 1..=upper {
                if self.syllable_set.contains(&s[i..i + len]) {
                    ok[i + len] = true;
                }
            }
        }
        ok[b.len()]
    }

    /// 是否是一个完整的合法音节（单音节，不含组合）。
    pub fn is_syllable(&self, s: &str) -> bool {
        self.syllable_set.contains(s)
    }

    /// 这个词在拼音词库里吗（D64 多段真词过滤）。
    ///
    /// 拼音词库来自语料（rime-pinyin-simp），对「中文里真实存在的词」的覆盖
    /// 比五笔词库宽——两份词库取并集做真词判定，缺一边都会漏。
    pub fn has_word(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.words
            .get_or_init(|| self.entries.iter().map(|e| e.word.clone()).collect())
            .contains(word)
    }

    /// 匹配：先精确（拼音串完全等于输入），再前缀（拼音串以输入开头）。
    ///
    /// 返回 `(下标, 是否来自精确区)`。这是**排序前的候选集合**，顺序尚未决定。
    ///
    /// 为什么精确优先而不是混在一起按权重排：打 `hao` 时用户要的是「好」这批
    /// 同音字，而「好吃」「好吧」虽然更常用，却属于「还该继续打」的那一类。
    /// 精确区排在前面，用户按空格拿到的就是同音字首选。
    pub fn match_indices(&self, input: &str, limit: usize) -> Vec<(u32, bool)> {
        if input.is_empty() || !input.is_ascii() {
            return Vec::new();
        }
        let entries = &self.entries;
        let mut out = Vec::with_capacity(limit.min(64));

        // ---- 精确区 ----
        let lower = self
            .sorted
            .partition_point(|&i| entries[i as usize].pinyin.as_str() < input);
        for &idx in &self.sorted[lower..] {
            let e = &entries[idx as usize];
            if e.pinyin != input {
                break;
            }
            out.push((idx, true));
            if out.len() >= limit {
                return out;
            }
        }

        // ---- 前缀区 ----
        // 从 lower 开始扫：精确区一定落在前缀区的最前面（`hao` 是 `hao...` 的前缀），
        // 所以可以顺延，不用重开一个二分。
        let mut scanned = 0usize;
        for &idx in &self.sorted[lower..] {
            let e = &entries[idx as usize];
            if !e.pinyin.starts_with(input) {
                break;
            }
            scanned += 1;
            if e.pinyin == input {
                continue; // 精确区已经收过
            }
            out.push((idx, false));
            if out.len() >= limit || scanned >= SCAN_CAP {
                break;
            }
        }
        out
    }
}

fn read_text(path: &Path) -> Result<String, LoadError> {
    let text = fs::read_to_string(path).map_err(LoadError::Io)?;
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PinyinLexicon {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pinyin-mini");
        PinyinLexicon::load(&dir).expect("拼音夹具应可装载")
    }

    #[test]
    fn loads_and_normalizes() {
        let lex = fixture();
        assert_eq!(lex.entry_count, lex.entries().len());
        assert!(lex.syllable_count >= 3);
        // 权重最大者归一化为 1
        let max = lex
            .entries()
            .iter()
            .map(|e| e.w_norm)
            .fold(0.0_f32, f32::max);
        assert!((max - 1.0).abs() < 1e-6);
    }

    #[test]
    fn exact_comes_before_prefix() {
        let lex = fixture();
        let hits = lex.match_indices("hao", 10);
        assert!(!hits.is_empty());
        // 第一条必须是精确命中
        assert!(hits[0].1, "精确区必须排在最前");
        // 精确区的下标一定对应 pinyin == "hao"
        let first = lex.entry(hits[0].0);
        assert_eq!(first.pinyin, "hao");
        // 后面允许出现前缀条目（haoping 之类）
        if let Some((idx, _)) = hits.iter().find(|(_, e)| !*e) {
            assert!(lex.entry(*idx).pinyin.starts_with("hao"));
        }
    }

    #[test]
    fn prefix_matches_unfinished_input() {
        let lex = fixture();
        let hits = lex.match_indices("haop", 10);
        assert!(!hits.is_empty(), "未打完的 haop 应能前缀命中");
        assert!(hits.iter().all(|(_, exact)| !*exact));
        assert!(hits
            .iter()
            .all(|(i, _)| lex.entry(*i).pinyin.starts_with("haop")));
    }

    #[test]
    fn can_split_accepts_valid_sequences() {
        let lex = fixture();
        assert!(lex.can_split("hao"));
        assert!(lex.can_split("haoping"));
        // xian 既可以是单音节，也可以是 xi + an
        assert!(lex.can_split("xian"));
        assert!(lex.can_split("xian")); // 幂等，纯函数
    }

    #[test]
    fn can_split_rejects_invalid() {
        let lex = fixture();
        assert!(!lex.can_split(""));
        assert!(!lex.can_split("vb"), "v 不是音节起始，五笔码不该被判成拼音");
        assert!(!lex.can_split("zzz"));
        // 非 ASCII 不得 panic
        assert!(!lex.can_split("好"));
        assert!(!lex.can_split("hao好"));
    }

    #[test]
    fn empty_and_non_ascii_input_match_nothing() {
        let lex = fixture();
        assert!(lex.match_indices("", 10).is_empty());
        assert!(lex.match_indices("好", 10).is_empty());
    }

    #[test]
    fn limit_is_respected() {
        let lex = fixture();
        let hits = lex.match_indices("hao", 2);
        assert!(hits.len() <= 2);
    }
}
