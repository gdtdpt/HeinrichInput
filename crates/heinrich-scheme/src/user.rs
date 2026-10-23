//! 自定义词组（D65）：`%APPDATA%\Heinrich\words.tsv` 的读写与解析。
//!
//! 一个文件、四列、后三列可空：
//!
//! ```text
//! # 词  拼音  码  权重
//! 微信  weixin
//! 扫码  saoma    rrtd
//! ```
//!
//! **宽容读取**是本模块的第一原则，与 `config.toml` 同一套思路：这是人改的文件，
//! 一个拼写错误不该让整份词库失效。坏行跳过并记一条警告，其余照常生效。
//!
//! 三列的缺省含义分别是：拼音从拼音词库反查（再不行就逐字拼）、码按 86 词组
//! 取码规则从单字全码推、权重取同词长的中位数。推导都在 [`resolve`] 里做，
//! 文件本身只记「用户写了什么」——手填的值永远压过推导值。

use std::fs;
use std::io;
use std::path::Path;

use crate::pinyin::PinyinUserEntry;
use crate::scheme::{Scheme, UserEntry};
use crate::PinyinLexicon;

/// 用户词库里的一条（原始列，未解析）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserWord {
    pub word: String,
    /// 不带分隔的小写拼音串（`weixin`）
    pub pinyin: Option<String>,
    /// 五笔码（a–y，1–4 位）
    pub code: Option<String>,
    /// 原始权重（与码表同量纲）
    pub weight: Option<f64>,
}

/// 一次读取的结果：词条 + 警告（都不致命）。
#[derive(Clone, Debug, Default)]
pub struct UserFile {
    pub words: Vec<UserWord>,
    pub warnings: Vec<String>,
}

/// 解析文件内容。不做 IO，便于单测。
pub fn parse(text: &str) -> UserFile {
    let mut out = UserFile::default();
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        // 第一行可能带 BOM（记事本另存为 UTF-8 时常见）
        let line = raw.trim_start_matches('\u{feff}').trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let word = cols.next().unwrap_or("").trim();
        if word.is_empty() {
            out.warnings
                .push(format!("第 {line_no} 行：词为空，已跳过"));
            continue;
        }
        let pinyin = cols.next().map(str::trim).unwrap_or("");
        let code = cols.next().map(str::trim).unwrap_or("");
        let weight = cols.next().map(str::trim).unwrap_or("");
        if cols.next().is_some() {
            out.warnings
                .push(format!("第 {line_no} 行：列数多于 4，只取前 4 列"));
        }

        let pinyin = if pinyin.is_empty() {
            None
        } else if pinyin.bytes().all(|b| b.is_ascii_lowercase()) {
            Some(pinyin.to_string())
        } else {
            out.warnings.push(format!(
                "第 {line_no} 行：拼音 `{pinyin}` 须为小写字母，已忽略这一列"
            ));
            None
        };

        let code = if code.is_empty() {
            None
        } else if crate::Code::parse(code).is_some() {
            Some(code.to_string())
        } else {
            out.warnings.push(format!(
                "第 {line_no} 行：码 `{code}` 非法（须为 1–4 位 a–y），已忽略这一列"
            ));
            None
        };

        let weight = if weight.is_empty() {
            None
        } else {
            match weight.parse::<f64>() {
                Ok(w) if w.is_finite() && w >= 0.0 => Some(w),
                _ => {
                    out.warnings.push(format!(
                        "第 {line_no} 行：权重 `{weight}` 不是非负数，已忽略这一列"
                    ));
                    None
                }
            }
        };

        // 同词重复：后写的覆盖先写的（用户改码改拼音时只改一行即可）
        let entry = UserWord {
            word: word.to_string(),
            pinyin,
            code,
            weight,
        };
        match out.words.iter_mut().find(|w| w.word == entry.word) {
            Some(slot) => *slot = entry,
            None => out.words.push(entry),
        }
    }
    out
}

/// 读文件。文件不存在时返回空（这是「第一次用」的正常状态，不是错误）。
pub fn load(path: &Path) -> io::Result<UserFile> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(UserFile::default()),
        Err(e) => Err(e),
    }
}

/// 渲染成文件内容（列顺序稳定，便于 `diff` 与手改）。
pub fn render(words: &[UserWord]) -> String {
    let mut out = String::from(
        "# 亨利 / Heinrich —— 自定义词组（手改也行，改完 1 秒内生效）\n\
         # format: 词<TAB>拼音<TAB>码<TAB>权重   （后三列可空：拼音自动反查、码按规则推、权重取中位数）\n",
    );
    for w in words {
        out.push_str(&w.word);
        out.push('\t');
        out.push_str(w.pinyin.as_deref().unwrap_or(""));
        out.push('\t');
        out.push_str(w.code.as_deref().unwrap_or(""));
        out.push('\t');
        if let Some(x) = w.weight {
            out.push_str(&format!("{x}"));
        }
        out.push('\n');
    }
    out
}

/// 写回文件：**先写临时文件再改名**。
///
/// server 每 1 秒 stat 一次这个文件，直接覆写会让它读到半行——改名是原子的，
/// 读者要么看到旧文件、要么看到新文件，不会看到中间态。
pub fn save(path: &Path, words: &[UserWord]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tsv.tmp");
    fs::write(&tmp, render(words))?;
    fs::rename(&tmp, path)
}

/// 新增或按词覆盖，返回 `true` 表示是新加的词。
pub fn upsert(list: &mut Vec<UserWord>, word: UserWord) -> bool {
    match list.iter_mut().find(|w| w.word == word.word) {
        Some(slot) => {
            *slot = word;
            false
        }
        None => {
            list.push(word);
            true
        }
    }
}

/// 解析后的一条：码、拼音、权重都已定下来。
#[derive(Clone, Debug)]
pub struct Resolved {
    pub word: String,
    /// `None` = 只走五笔（拼音查不到，也没手填）
    pub pinyin: Option<String>,
    /// `None` = 只走拼音（字的全码缺失，推不出码）
    pub code: Option<String>,
    pub weight: f64,
}

/// 把原始列补全成可合并的条目。
///
/// 顺序是「手填 > 反查 > 推导」，每缺一项才往下走一步；只有**拼音和码都拿不到**
/// 的词才会被丢弃（它哪条路都打不出来）。
pub fn resolve(
    raw: &[UserWord],
    scheme: &Scheme,
    pinyin: Option<&PinyinLexicon>,
) -> (Vec<Resolved>, Vec<String>) {
    let mut out = Vec::with_capacity(raw.len());
    let mut warnings = Vec::new();
    for w in raw {
        let code = match (&w.code, scheme.word_code(&w.word)) {
            (Some(c), _) => Some(c.clone()),
            (None, Some(c)) => Some(c),
            (None, None) => None,
        };
        let pinyin_text = match (&w.pinyin, pinyin) {
            (Some(p), _) => Some(p.clone()),
            (None, Some(lex)) => lex
                .pinyin_of(&w.word)
                .map(str::to_string)
                .or_else(|| lex.pinyin_of_text(&w.word)),
            (None, None) => None,
        };
        if code.is_none() && pinyin_text.is_none() {
            warnings.push(format!(
                "「{}」既推不出五笔码、也查不到拼音，已跳过（可手填码或拼音）",
                w.word
            ));
            continue;
        }
        if code.is_none() {
            warnings.push(format!(
                "「{}」的字不在五笔码表里，码推不出来：这个词只能在拼音里打出来",
                w.word
            ));
        }
        if pinyin_text.is_none() {
            warnings.push(format!(
                "「{}」查不到拼音：这个词只能在五笔里打出来",
                w.word
            ));
        }
        let weight = w.weight.unwrap_or_else(|| scheme.default_weight(&w.word));
        out.push(Resolved {
            word: w.word.clone(),
            pinyin: pinyin_text,
            code,
            weight,
        });
    }
    (out, warnings)
}

/// 把解析结果拆成两侧各自要合并的条目。
pub fn split_for_merge(items: &[Resolved]) -> (Vec<UserEntry>, Vec<PinyinUserEntry>) {
    let mut wubi = Vec::with_capacity(items.len());
    let mut pin = Vec::with_capacity(items.len());
    for it in items {
        if let Some(code) = &it.code {
            wubi.push(UserEntry {
                code: code.clone(),
                word: it.word.clone(),
                weight: it.weight,
            });
        }
        if let Some(py) = &it.pinyin {
            pin.push(PinyinUserEntry {
                pinyin: py.clone(),
                word: it.word.clone(),
                weight: it.weight,
                wubi: it.code.clone().unwrap_or_default(),
            });
        }
    }
    (wubi, pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme() -> Scheme {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wubi-mini");
        Scheme::load(&dir).expect("wubi-mini 夹具装载失败")
    }

    #[test]
    fn parses_four_columns_and_defaults() {
        let f = parse("# 注释\n微信\tweixin\n扫码\t\twrhq\n\n打包\n");
        assert_eq!(f.words.len(), 3, "{:?}", f.words);
        assert_eq!(f.words[0].word, "微信");
        assert_eq!(f.words[0].pinyin.as_deref(), Some("weixin"));
        assert_eq!(f.words[1].code.as_deref(), Some("wrhq"));
        assert_eq!(f.words[2].pinyin, None);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
    }

    #[test]
    fn bad_columns_downgrade_to_warnings() {
        let f = parse("坏拼音\tWeiXin\n坏码\t\tVZ\n坏权重\t\t\t-3\n\t空白词\n");
        // 坏拼音 / 坏码 / 坏权重各丢一列，词本身还在；空白词整行丢掉
        assert_eq!(f.words.len(), 3);
        assert!(f
            .words
            .iter()
            .all(|w| w.pinyin.is_none() && w.code.is_none()));
        assert_eq!(f.warnings.len(), 4, "{:?}", f.warnings);
    }

    #[test]
    fn duplicate_words_take_the_last_line() {
        let f = parse("微信\tweixin\n微信\t\twx\n");
        assert_eq!(f.words.len(), 1);
        assert_eq!(f.words[0].pinyin, None);
        assert_eq!(f.words[0].code.as_deref(), Some("wx"));
    }

    #[test]
    fn derive_code_from_single_char_codes() {
        let s = scheme();
        // 夹具里「好」全码 vbg、「吃」ktn → 2 字词各取前 2 码
        assert_eq!(s.word_code("好吃").as_deref(), Some("vbkt"));
        // 单字就是全码
        assert_eq!(s.word_code("好").as_deref(), Some("vbg"));
        // 有字不在码表里 → 推不出来
        assert!(s.word_code("好龘").is_none());
    }

    #[test]
    fn merges_user_words_into_both_indexes() {
        let mut s = scheme();
        let before = s.entry_count();
        let raw = vec![UserWord {
            word: "好吃".into(),
            code: Some("vbkt".into()),
            ..Default::default()
        }];
        // 不给拼音词库时，拼音这一列查不到 → 只警告、不丢弃
        let (resolved, warns) = resolve(&raw, &s, None);
        assert_eq!(resolved.len(), 1);
        assert!(warns.iter().any(|w| w.contains("拼音")), "{warns:?}");
        let (wubi, _) = split_for_merge(&resolved);
        let added = s.apply_user_words(&wubi);
        assert_eq!(added, 1);
        assert_eq!(s.entry_count(), before + 1);
        assert!(s.has_word("好吃"));
        let code = crate::Code::parse("vbkt").unwrap();
        assert!(s
            .exact_indices(&code)
            .iter()
            .any(|&i| s.entry(i).word == "好吃"));
        // 重载：同一份再合一次不会翻倍
        s.apply_user_words(&wubi);
        assert_eq!(s.entry_count(), before + 1);
        // 清空用户词后回到基础库
        s.apply_user_words(&[]);
        assert_eq!(s.entry_count(), before);
    }

    #[test]
    fn pinyin_side_gets_the_word_and_its_wubi_code() {
        let mut s = scheme();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pinyin-mini");
        let mut lex = PinyinLexicon::load(&dir).expect("拼音夹具装载失败");
        let before = lex.entry_count;
        // 拼音与码都不写：拼音从词库反查（haochi），码按规则推（vbkt）；
        // 「号好」不在拼音词库里，拼音走逐字兜底（haohao），而它的字不全在
        // 五笔码表里 → 码推不出来，只警告不丢弃。
        let raw = vec![
            UserWord {
                word: "好吃".into(),
                ..Default::default()
            },
            UserWord {
                word: "号好".into(),
                ..Default::default()
            },
        ];
        let (resolved, warns) = resolve(&raw, &s, Some(&lex));
        assert_eq!(resolved[0].pinyin.as_deref(), Some("haochi"));
        assert_eq!(resolved[0].code.as_deref(), Some("vbkt"));
        assert_eq!(resolved[1].pinyin.as_deref(), Some("haohao"));
        assert_eq!(resolved[1].code, None);
        assert!(warns.iter().any(|w| w.contains("五笔")), "{warns:?}");

        let (wubi, pin) = split_for_merge(&resolved);
        assert_eq!(s.apply_user_words(&wubi), 1, "只有「好吃」有码");
        // 「好吃」已在拼音词库里 → 不重复加，但要把派生出来的码补进去（chip 才有内容）
        assert_eq!(lex.apply_user_words(&pin), 1, "只有「号好」是新的");
        assert_eq!(lex.entry_count, before + 1);
        let idx = lex.entries().iter().position(|e| e.word == "好吃").unwrap() as u32;
        assert_eq!(lex.entry(idx).wubi, "vbkt");
        let idx = lex.entries().iter().position(|e| e.word == "号好").unwrap() as u32;
        assert_eq!(lex.entry(idx).pinyin, "haohao");
        assert_eq!(lex.entry(idx).wubi, "");
        // 混打过滤要能看到新词
        assert!(lex.has_word("号好"));
        // 重载（同一份再来一次）不翻倍
        lex.apply_user_words(&pin);
        assert_eq!(lex.entry_count, before + 1);
        lex.apply_user_words(&[]);
        assert_eq!(lex.entry_count, before);
    }

    #[test]
    fn user_weight_defaults_to_length_median() {
        let s = scheme();
        let w = s.default_weight("好吃");
        assert!(w > 0.0, "默认权重应大于 0，实际 {w}");
        // 单字词的默认权重也不能是 0（大量生僻字权重为 0 会把它拉平）
        assert!(s.default_weight("好") > 0.0);
    }

    #[test]
    fn render_roundtrips_through_parse() {
        let words = vec![
            UserWord {
                word: "微信".into(),
                pinyin: Some("weixin".into()),
                code: None,
                weight: None,
            },
            UserWord {
                word: "扫码".into(),
                pinyin: None,
                code: Some("wrhq".into()),
                weight: Some(1234.0),
            },
        ];
        let text = render(&words);
        let back = parse(&text);
        assert_eq!(back.words, words);
        assert!(back.warnings.is_empty());
    }
}
