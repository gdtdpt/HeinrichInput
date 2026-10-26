//! `--kind emoji`：把 CLDR 的中文注音表装配成「词组 → emoji」映射。
//!
//! 输入：`data/raw/cldr-annotations-zh.xml`（Unicode CLDR，Unicode License）。
//! 它给每个 emoji 一组**中文关键词**（`👍` → 赞 / 顶 / 大拇指 / 手 / 好），
//! 与拼音词库求交就得到「哪些词可以挂哪个 emoji」。
//!
//! 输出：`data/wubi86/emoji.toml`（与 `roots.toml` 同住方案目录，随 `--scheme-dir` 走）。
//!
//! ## 为什么用注音表而不是手写
//!
//! 两三千条手写既不可复核也不可持续；CLDR 是 Unicode 官方的多语言注音数据，
//! 每个关键词都能追溯到「哪个 emoji 的哪条官方注解」，将来能重跑出同一份结果
//! （同输入必得同字节，与 M0a 的约束一致）。
//!
//! ## 几条硬规则（都对应验收项）
//!
//! - **只收单码位 emoji**：CLDR 这份表已经把 `U+FE0F` 去掉，剩下的多码位项是
//!   ZWJ 序列、旗帜（区域指示符成对）、肤色修饰——它们的渲染差异大，v1 一律跳过。
//! - **只挂词组**（≥2 字）：单字挂 emoji 会让候选行凭空变宽，而五笔的单字候选
//!   本来就是一行好几个。
//! - **一词一 emoji**：同一个词被多个 emoji 的关键词命中时，取「关键词在注解里
//!   排得最靠前」的那个（排第一的是官方主称），再按码位升序定胜负——必须确定，
//!   否则同一份输入两次生成会得到两份不同的表。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use heinrich_scheme::{is_emoji, sha256_hex};

use crate::{is_han, read_text};

/// 一个 emoji 及其官方中文关键词（按注解顺序）。
struct Annotation {
    emoji: char,
    keywords: Vec<String>,
}

pub fn run(
    raw_dir: &Path,
    lexicon: &Path,
    out_dir: &Path,
    report_path: &Path,
) -> Result<bool, String> {
    let xml = read_text(&raw_dir.join("cldr-annotations-zh.xml"))?;
    let annotations = parse_annotations(&xml);
    if annotations.is_empty() {
        return Err("CLDR 注解解析结果为空，请检查原始文件格式".into());
    }
    let words = read_words(lexicon)?;
    if words.is_empty() {
        return Err(format!(
            "词库 {} 里没有词组（先跑 tools/gen.ps1 -Kind pinyin）",
            lexicon.display()
        ));
    }

    // 词 → (关键词排名, emoji 码位, emoji)。排名越小越「官方」。
    let mut best: BTreeMap<&str, (usize, u32, char)> = BTreeMap::new();
    let mut hits = 0usize;
    for ann in &annotations {
        for (rank, kw) in ann.keywords.iter().enumerate() {
            if kw.chars().count() < 2 || !kw.chars().all(is_han) {
                continue;
            }
            if !words.contains(kw.as_str()) {
                continue;
            }
            hits += 1;
            let cand = (rank, ann.emoji as u32, ann.emoji);
            match best.get(kw.as_str()) {
                Some(cur) if *cur <= cand => {}
                _ => {
                    best.insert(kw.as_str(), cand);
                }
            }
        }
    }

    // ---- 写 emoji.toml ----
    // 正文（`word\temoji` 行）单独拼出来，哈希只覆盖正文：头部带上自己的哈希
    // 才可能自校验，而把头部算进去就会自指。
    let mut body = String::with_capacity(best.len() * 24);
    for (word, (_, _, emoji)) in &best {
        body.push_str(word);
        body.push('\t');
        body.push(*emoji);
        body.push('\n');
    }
    let digest = sha256_hex(body.as_bytes());

    let mut text = String::with_capacity(body.len() + 512);
    text.push_str("# 亨利 / Heinrich —— 词组 emoji 映射（生成物，请勿手改）\n");
    text.push_str(
        "# 重新生成：powershell -ExecutionPolicy Bypass -File tools\\gen.ps1 -Kind emoji\n",
    );
    text.push_str("# source: Unicode CLDR common/annotations/zh.xml（Unicode License）\n");
    text.push_str(
        "# 用户改绑 / 停用请写 %APPDATA%\\Heinrich\\emoji.user.toml，本文件升级会被覆盖\n",
    );
    text.push_str("# sha256 只覆盖下方条目正文（word<TAB>emoji），供装载期比对本文件是否被改过\n");
    text.push_str(&format!("entry_count = {}\n", best.len()));
    text.push_str(&format!("sha256 = \"{digest}\"\n"));
    for (word, (_, _, emoji)) in &best {
        text.push('\n');
        text.push_str("[[emoji]]\n");
        text.push_str(&format!("word = \"{word}\"\n"));
        text.push_str(&format!("emoji = \"{emoji}\"\n"));
    }

    fs::create_dir_all(out_dir).map_err(|e| format!("创建输出目录失败：{e}"))?;
    let path = out_dir.join("emoji.toml");
    fs::write(&path, &text).map_err(|e| format!("写入 emoji.toml 失败：{e}"))?;

    // ---- 报告 ----
    let single_char_words = best.keys().filter(|w| w.chars().count() < 2).count();
    let non_han = best.keys().filter(|w| !w.chars().all(is_han)).count();
    let report = render_report(
        &annotations,
        best.len(),
        hits,
        single_char_words,
        non_han,
        &digest,
        &path,
        lexicon,
    );
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败：{e}"))?;
    }
    fs::write(report_path, report).map_err(|e| format!("写入报告失败：{e}"))?;

    println!("emoji 映射：{} 条 → {}", best.len(), path.display());
    println!("哈希：{digest}");
    println!("报告：{}", report_path.display());
    let ok = (2000..=3000).contains(&best.len()) && single_char_words == 0 && non_han == 0;
    println!(
        "[{}] 条目 2000–3000、全为 ≥2 字汉字词组：{} 条",
        if ok { "通过" } else { "未通过" },
        best.len()
    );
    Ok(ok)
}

/// 从拼音词库里取「词组」（≥2 字、全汉字）。
fn read_words(path: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    let text = read_text(path)?;
    let mut out = std::collections::BTreeSet::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let _pinyin = cols.next();
        if let Some(word) = cols.next() {
            if word.chars().count() >= 2 && word.chars().all(is_han) {
                out.insert(word.to_string());
            }
        }
    }
    Ok(out)
}

/// 解析 `<annotation cp="…">k1 | k2</annotation>`。
///
/// 只认这一种标签：`type="tts"` 的是「这个 emoji 叫什么」（单条短名），不是关键词表，
/// 混进来会把「大拇指」这类名字当成关键词用——语义不同，不能合。
fn parse_annotations(xml: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<annotation ") {
        rest = &rest[start + "<annotation ".len()..];
        let Some(gt) = rest.find('>') else { break };
        let attrs = &rest[..gt];
        rest = &rest[gt + 1..];
        let Some(end) = rest.find("</annotation>") else {
            break;
        };
        let content = &rest[..end];
        rest = &rest[end + "</annotation>".len()..];

        if attrs.contains("type=\"tts\"") {
            continue;
        }
        let Some(cp) = attr_value(attrs, "cp") else {
            continue;
        };
        let mut chars = cp.chars();
        let (Some(emoji), None) = (chars.next(), chars.next()) else {
            continue; // 多码位（ZWJ / 旗帜 / 肤色）一律跳过
        };
        if !is_emoji(emoji) {
            continue;
        }
        let keywords = content
            .split('|')
            .map(|k| unescape(k.trim()))
            .filter(|k| !k.is_empty())
            .collect();
        out.push(Annotation { emoji, keywords });
    }
    out
}

/// 从标签属性串里取 `name="value"`。
fn attr_value<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=\"");
    let start = attrs.find(&pat)? + pat.len();
    let rest = &attrs[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// 关键词里可能带 XML 实体（`&amp;` 之类），还原成字符；数字实体一并处理。
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after.find(';') else {
            out.push_str(after);
            return out;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            other => other.strip_prefix('#').and_then(|num| {
                let cp = if let Some(hex) = num.strip_prefix('x') {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    num.parse::<u32>().ok()
                }?;
                char::from_u32(cp)
            }),
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&after[..=semi]),
        }
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    out
}

#[allow(clippy::too_many_arguments)]
fn render_report(
    annotations: &[Annotation],
    entries: usize,
    hits: usize,
    single_char_words: usize,
    non_han: usize,
    digest: &str,
    out: &Path,
    lexicon: &Path,
) -> String {
    let mut r = String::new();
    r.push_str("# M4c emoji 映射装配报告\n\n");
    r.push_str(
        "> 由 `heinrich-gen --kind emoji` 自动生成，请勿手改。不含时间戳，同输入必得同字节。\n\n",
    );
    r.push_str("## 1. 产物\n\n| 项 | 值 |\n|---|---|\n");
    r.push_str(&format!("| 映射表 | `{}` |\n", out.display()));
    r.push_str(&format!("| 词来源 | `{}` |\n", lexicon.display()));
    r.push_str(&format!("| 条目数 | {entries} |\n"));
    r.push_str(&format!("| 正文 SHA-256 | `{digest}` |\n\n"));

    r.push_str("## 2. 规模\n\n| 项 | 值 |\n|---|---|\n");
    r.push_str(&format!("| CLDR 可用 emoji | {} |\n", annotations.len()));
    r.push_str(&format!("| 关键词命中次数（含重复） | {hits} |\n"));
    r.push_str(&format!(
        "| 平均每个 emoji 命中 | {:.1} 个词 |\n\n",
        hits as f64 / annotations.len().max(1) as f64
    ));

    r.push_str("## 3. 验收项\n\n| 验收项 | 结果 | 实测 |\n|---|---|---|\n");
    let ok_range = (2000..=3000).contains(&entries);
    r.push_str(&format!(
        "| 条目数在 2000–3000 | {} | {entries} |\n",
        if ok_range {
            "✅ 通过"
        } else {
            "❌ 未通过"
        }
    ));
    r.push_str(&format!(
        "| 全部为词组（≥2 字） | {} | 单字 {single_char_words} 条 |\n",
        if single_char_words == 0 {
            "✅ 通过"
        } else {
            "❌ 未通过"
        }
    ));
    r.push_str(&format!(
        "| 全部为汉字 | {} | 非汉字 {non_han} 条 |\n",
        if non_han == 0 {
            "✅ 通过"
        } else {
            "❌ 未通过"
        }
    ));
    r.push_str("| 一词一条（BTreeMap 天然去重） | ✅ 通过 | 键唯一 |\n\n");

    r.push_str("## 4. 规则\n\n");
    r.push_str("- 只收**单码位** emoji：多码位项（ZWJ 序列、旗帜、肤色修饰）跳过。\n");
    r.push_str("- 只收码位在 emoji 主区间内的字符：CLDR 也给 `{`、《 之类写了关键词，不过滤会把「括号」挂上 `《`。\n");
    r.push_str("- 一词一 emoji：按「关键词在注解里的排名 → 码位升序」取最小者，结果确定。\n");
    r.push_str("- 只看词组（≥2 字）：单字候选一行好几个，挂 emoji 会让行宽翻倍。\n");
    r.push_str("\n## 5. 复现方式\n\n```powershell\n");
    r.push_str("powershell -ExecutionPolicy Bypass -File tools\\fetch-data.ps1\n");
    r.push_str("cargo run -p heinrich-gen -- --kind emoji --raw data/raw --out data/wubi86 --report out/m4c-emoji-report.md\n");
    r.push_str("```\n");
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析注解并跳过_tts_与多码位() {
        let xml = r#"<annotations>
            <annotation cp="👍">赞 | 顶 | 大拇指</annotation>
            <annotation cp="👍" type="tts">大拇指</annotation>
            <annotation cp="👨‍👩‍👧">家庭 | 家</annotation>
            <annotation cp="{">大括号 | 左花括号</annotation>
        </annotations>"#;
        let anns = parse_annotations(xml);
        assert_eq!(
            anns.len(),
            1,
            "只剩 👍：tts、ZWJ 序列、非 emoji 字符都该跳过"
        );
        assert_eq!(anns[0].emoji, '👍');
        assert_eq!(anns[0].keywords, ["赞", "顶", "大拇指"]);
    }

    #[test]
    fn 关键词里的实体被还原() {
        assert_eq!(unescape("A&amp;B"), "A&B");
        assert_eq!(unescape("&#x1F44D;"), "👍");
        assert_eq!(unescape("没有实体"), "没有实体");
    }

    #[test]
    fn 码位过滤挡住标点与肤色修饰() {
        assert!(is_emoji('👍'));
        assert!(is_emoji('🎉'));
        assert!(!is_emoji('{'));
        assert!(!is_emoji('《'));
        assert!(!is_emoji('好'), "汉字不是 emoji");
        assert!(!is_emoji('🏻'), "肤色修饰符不能单独当 emoji");
        assert!(!is_emoji('\u{1F1E8}'), "区域指示符是旗帜的一半");
    }
}
