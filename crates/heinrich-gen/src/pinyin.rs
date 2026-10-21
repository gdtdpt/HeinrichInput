//! 拼音方案装配：`data/raw/pinyin_simp.dict.yaml` → `data/pinyin/`。
//!
//! 输入（Rime 格式，`...` 之后是 `词 \t 音节串 \t 权重`）：
//!
//! ```text
//! 好    hao       56789
//! 好评  hao ping  1234
//! ```
//!
//! 输出：
//! - `lexicon.tsv`：`拼音<TAB>词<TAB>权重`——拼音是**去掉空格**的连续串
//! - `syllables.txt`：音节表，每行一个，供切分与合法性校验
//! - `scheme.toml`：清单 + 词库哈希
//! - 报告 `out/m3-pinyin-report.md`
//!
//! **为什么去掉空格**：用户敲的是连续字母，输入 `haoping` 时不该要求他打空格。
//! 去掉空格后查询退化成一次普通前缀匹配——`hao` 命中「好」，`haop` 命中「好评」，
//! 与 §6.1 的「前缀匹配质量 0.5」在语义上正好对齐。
//!
//! 代价是**丢掉音节边界**：`xian` 同时是「先」（xiān）和「西安」（xi'an）。
//! 这两条会一起出现在候选里——但那本来就是用户想要的，主流拼音输入法也这么做。
//! 真正需要边界的地方（切分一段连续字母是否合法）用 `syllables.txt` 现场算，
//! 不需要词条自带边界。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use heinrich_scheme::sha256_hex;

use crate::{is_han, parse_common, read_text};

/// 一条装配后的拼音词条。
struct Entry {
    /// 无分隔小写拼音串
    pinyin: String,
    word: String,
    weight: f64,
}

/// 装配结果统计，供报告使用。
struct Stats {
    entries: usize,
    single: usize,
    multi: usize,
    syllables: usize,
    /// 单向映射：常用字里有拼音的字数
    common_covered: usize,
    common_total: usize,
    /// 拼音串最长的词条
    longest: Option<(String, String)>,
}

pub fn run(raw: &Path, out: &Path, report: &Path) -> Result<bool, String> {
    let rows = parse_pinyin_dict(&raw.join("pinyin_simp.dict.yaml"))?;
    if rows.is_empty() {
        return Err("拼音词库解析结果为空，请检查原始文件格式".into());
    }
    let common = parse_common(&raw.join("common3500.csv"))?;

    // ---- 合并与去重 ----
    // 原始数据里有极少数同一个词带同一个拼音却给两个权重（权重不同），
    // 取较大的那个——同 key 只该有一条，否则候选区会出现两个一模一样的词。
    let mut merged: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut syllables = BTreeSet::new();
    let mut malformed = 0usize;
    let mut bad_alpha = Vec::new();

    for (word, py, weight) in rows {
        let parts: Vec<&str> = py.split_whitespace().collect();
        if parts.is_empty() {
            malformed += 1;
            continue;
        }
        // 音节必须是纯小写字母。出现数字/声调符号/大写一律视为脏数据丢掉，
        // 否则它会以「永远匹配不上」的形式静默留在词库里。
        if !parts
            .iter()
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
        {
            bad_alpha.push(format!("{word}\t{py}"));
            continue;
        }
        for s in &parts {
            syllables.insert((*s).to_string());
        }
        let code = parts.concat();
        let w = if weight.is_finite() && weight > 0.0 {
            weight
        } else {
            0.0
        };
        let key = (code, word);
        let slot = merged.entry(key).or_insert(w);
        if w > *slot {
            *slot = w;
        }
    }

    if merged.is_empty() {
        return Err("去重后没有任何拼音词条".into());
    }

    // ---- 排序：拼音升序 → 权重降序 → 词 ----
    // 与五笔词库同一套可复现约束：不依赖哈希表遍历顺序。
    let mut entries: Vec<Entry> = merged
        .into_iter()
        .map(|((pinyin, word), weight)| Entry {
            pinyin,
            word,
            weight,
        })
        .collect();
    entries.sort_by(|a, b| {
        a.pinyin
            .cmp(&b.pinyin)
            .then(b.weight.total_cmp(&a.weight))
            .then(a.word.cmp(&b.word))
    });

    // ---- 写 lexicon.tsv ----
    let mut text = String::with_capacity(entries.len() * 24);
    text.push_str("# 亨利 / Heinrich —— 拼音词库（生成物，请勿手改；重新生成用 tools/gen.ps1 -Kind pinyin）\n");
    text.push_str("# source: rime/rime-pinyin-simp pinyin_simp.dict.yaml (Apache-2.0)\n");
    text.push_str("# format: pinyin<TAB>word<TAB>weight   (pinyin 为去掉空格的连续小写串)\n");
    for e in &entries {
        text.push_str(&e.pinyin);
        text.push('\t');
        text.push_str(&e.word);
        text.push('\t');
        text.push_str(&format!("{:.0}", e.weight));
        text.push('\n');
    }
    fs::create_dir_all(out).map_err(|e| format!("创建输出目录失败：{e}"))?;
    let lexicon_path = out.join("lexicon.tsv");
    fs::write(&lexicon_path, &text).map_err(|e| format!("写入拼音词库失败：{e}"))?;
    let digest = sha256_hex(text.as_bytes());

    // ---- 写音节表 ----
    let mut syl_text = String::with_capacity(syllables.len() * 8);
    syl_text.push_str("# 拼音音节表（生成物，请勿手改）——供切分与合法性校验\n");
    syl_text.push_str("# source: rime/rime-pinyin-simp (Apache-2.0)\n");
    for s in &syllables {
        syl_text.push_str(s);
        syl_text.push('\n');
    }
    fs::write(out.join("syllables.txt"), &syl_text).map_err(|e| format!("写入音节表失败：{e}"))?;

    // ---- 清单 ----
    let single = entries
        .iter()
        .filter(|e| e.word.chars().count() == 1)
        .count();
    let multi = entries.len() - single;
    let longest = entries
        .iter()
        .max_by_key(|e| e.pinyin.len())
        .map(|e| (e.word.clone(), e.pinyin.clone()));
    let longest_syl = syllables
        .iter()
        .max_by_key(|s| s.len())
        .cloned()
        .unwrap_or_default();

    let manifest = format!(
        "# 亨利拼音方案清单（由 heinrich-gen 生成，请勿手改）\n\
         id = \"pinyin\"\n\
         name = \"拼音\"\n\
         version = \"1.0\"\n\
         alphabet = \"abcdefghijklmnopqrstuvwxyz\"\n\
         # 0 = 不限。拼音词条长短完全取决于词组，没有「码长」这个概念；\n\
         # 单音节最长 6（shuang/zhuang/chuang），实测最长音节 {syl_len}\n\
         max_code_len = 0\n\
         source = \"rime/rime-pinyin-simp pinyin_simp.dict.yaml\"\n\
         license = \"Apache-2.0\"\n\
         entry_count = {count}\n\
         syllable_count = {syl_count}\n\
         lexicon_sha256 = \"{digest}\"\n",
        syl_len = longest_syl.len(),
        count = entries.len(),
        syl_count = syllables.len(),
    );
    fs::write(out.join("scheme.toml"), manifest).map_err(|e| format!("写入清单失败：{e}"))?;

    // ---- 覆盖率 ----
    let covered: BTreeSet<char> = entries
        .iter()
        .filter(|e| e.word.chars().count() == 1)
        .filter_map(|e| e.word.chars().next())
        .filter(|c| is_han(*c))
        .collect();
    let common_covered = common.iter().filter(|c| covered.contains(c)).count();

    let stats = Stats {
        entries: entries.len(),
        single,
        multi,
        syllables: syllables.len(),
        common_covered,
        common_total: common.len(),
        longest,
    };

    let verdicts = verdicts(&stats, &bad_alpha, &syllables);
    let report_text = render_report(&stats, &verdicts, &bad_alpha, malformed, &digest, out);
    if let Some(parent) = report.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败：{e}"))?;
    }
    fs::write(report, report_text).map_err(|e| format!("写入报告失败：{e}"))?;

    println!(
        "拼音词库：{} 条 → {}",
        entries.len(),
        lexicon_path.display()
    );
    println!("音节表：{} 个", syllables.len());
    println!("哈希：{digest}");
    println!("报告：{}", report.display());
    for (name, ok, detail) in &verdicts {
        println!("[{}] {name}：{detail}", if *ok { "通过" } else { "未通过" });
    }
    Ok(verdicts.iter().all(|(_, ok, _)| *ok))
}

/// 解析 Rime 拼音词典，返回 `(词, 音节串, 权重)`。
fn parse_pinyin_dict(path: &Path) -> Result<Vec<(String, String, f64)>, String> {
    let text = read_text(path)?;
    let mut rows = Vec::with_capacity(70_000);
    let mut in_body = false;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if !in_body {
            if line.trim() == "..." {
                in_body = true;
            }
            continue;
        }
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(word), Some(py)) = (cols.next(), cols.next()) else {
            continue;
        };
        let weight = cols
            .next()
            .and_then(|w| w.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        if word.is_empty() || py.is_empty() {
            continue;
        }
        rows.push((word.to_string(), py.to_string(), weight));
    }
    Ok(rows)
}

/// 验收项。返回 `(名称, 是否通过, 说明)`。
fn verdicts(
    stats: &Stats,
    bad_alpha: &[String],
    syllables: &BTreeSet<String>,
) -> Vec<(String, bool, String)> {
    let cover_pct = if stats.common_total == 0 {
        0.0
    } else {
        stats.common_covered as f64 / stats.common_total as f64 * 100.0
    };

    // 常用字必须全覆盖。「打拼音打不出常用字」是拼音方案的底线缺陷，
    // 而且这批字几乎必然同时是拼音使用频率最高的那批。
    let cover_ok = stats.common_covered == stats.common_total;

    // 脏音节：数字、声调、大写。它们不会报错，只会永远匹配不上。
    let alpha_ok = bad_alpha.is_empty();

    // 音节表必须非空，且每个音节长度 ≤ 6（§6.1：zhuang/chuang/shuang 是上限）。
    // 更长的「音节」说明原始数据把两个音节粘成了一个，会让切分算法解不出来。
    let too_long: Vec<&String> = syllables.iter().filter(|s| s.len() > 6).collect();
    let syl_ok = !syllables.is_empty() && too_long.is_empty();

    vec![
        (
            "常用字覆盖".to_string(),
            cover_ok,
            format!(
                "{}/{} = {cover_pct:.2}%{}",
                stats.common_covered,
                stats.common_total,
                if cover_ok { "" } else { "（应为 100%）" }
            ),
        ),
        (
            "拼音字符集".to_string(),
            alpha_ok,
            if alpha_ok {
                "全部为小写 a–z".to_string()
            } else {
                format!("发现 {} 条含非法字符", bad_alpha.len())
            },
        ),
        (
            "音节表".to_string(),
            syl_ok,
            if !too_long.is_empty() {
                format!("发现 {} 个超长音节：{too_long:?}", too_long.len())
            } else {
                format!("{} 个，最长 {} 字母", syllables.len(), {
                    syllables.iter().map(|s| s.len()).max().unwrap_or(0)
                })
            },
        ),
        (
            "单字占比".to_string(),
            stats.single > 0 && stats.multi > 0,
            format!("单字 {} / 词组 {}", stats.single, stats.multi),
        ),
    ]
}

fn render_report(
    stats: &Stats,
    verdicts: &[(String, bool, String)],
    bad_alpha: &[String],
    malformed: usize,
    digest: &str,
    out: &Path,
) -> String {
    let passed = verdicts.iter().filter(|(_, ok, _)| *ok).count();
    let mut s = String::new();
    s.push_str("# M3 拼音数据装配报告\n\n");
    s.push_str(&format!(
        "**结论**：{} / {} 项通过\n\n",
        passed,
        verdicts.len()
    ));
    s.push_str("| 项 | 结果 | 说明 |\n|---|---|---|\n");
    for (name, ok, detail) in verdicts {
        s.push_str(&format!(
            "| {name} | {} | {detail} |\n",
            if *ok { "✅" } else { "❌" }
        ));
    }
    s.push_str(&format!(
        "\n**产物**：`{}`\n\n**词库 SHA-256**：`{digest}`\n\n",
        out.join("lexicon.tsv").display()
    ));
    s.push_str(&format!(
        "**规模**：{} 条（单字 {} / 词组 {}），音节 {} 个\n\n",
        stats.entries, stats.single, stats.multi, stats.syllables
    ));
    if let Some((word, py)) = &stats.longest {
        s.push_str(&format!(
            "**最长词条**：`{word}` = `{py}`（{} 字母）\n\n",
            py.len()
        ));
    }
    s.push_str("## 数据处理说明\n\n");
    s.push_str(&format!(
        "- 跳过格式非法的行：{malformed} 条\n\
         - 丢弃含非法字符的行：{} 条{}\n\
         - 同一（拼音, 词）出现多次时取较大权重（源数据的极少数重复）\n\
         - 拼音去掉音节间空格后建索引：用户敲的是连续字母，查询即前缀匹配\n",
        bad_alpha.len(),
        if bad_alpha.is_empty() {
            String::new()
        } else {
            format!("，例如 `{}`", bad_alpha.first().unwrap())
        }
    ));
    s
}
