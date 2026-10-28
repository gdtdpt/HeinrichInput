//! `heinrich-gen`：把原始码表装配成 `data/wubi86/`，并产出 M0a 校验报告。
//!
//! 输入（`data/raw/`，不进产物、不进仓库）：
//! - `wubi86.dict.yaml`：Rime 格式，`...` 之后是 `词 \t 码 \t 权重`
//! - `common3500.csv`：`ID,汉字,常用`
//!
//! 输出：
//! - `data/wubi86/lexicon.tsv`：`码 \t 词 \t 权重 \t 简码级别`
//! - `data/wubi86/scheme.toml`：清单 + 词库哈希
//! - `out/m0a-report.md`：校验报告
//!
//! **可复现约束**：输出文件里不写时间戳、不依赖哈希表遍历顺序（全程排序），
//! 因此同输入必然同字节、同哈希。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use heinrich_scheme::{sha256_hex, Code, MAX_CODE_LEN};

mod emoji;
mod pinyin;

/// 装配哪一套词库。三套的输入格式、校验项、产物都不同，共用入口只是为了
/// 让 `tools/gen.ps1` 只有一个 `cargo run`。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Wubi,
    Pinyin,
    Emoji,
}

struct Args {
    kind: Kind,
    raw: PathBuf,
    /// `None` 表示按 `kind` 取默认值（`--kind` 可能出现在任何位置，
    /// 所以默认值不能在参数解析前就定下来）。
    out: Option<PathBuf>,
    report: Option<PathBuf>,
    /// emoji 装配的词来源（拼音词库）。默认 `data/pinyin/lexicon.tsv`。
    lexicon: Option<PathBuf>,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut kind = Kind::Wubi;
        let mut raw = PathBuf::from("data/raw");
        let mut out = None;
        let mut report = None;
        let mut lexicon = None;
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let value = it.next().ok_or_else(|| format!("`{flag}` 缺少取值"))?;
            match flag.as_str() {
                "--kind" => {
                    kind = match value.as_str() {
                        "wubi" => Kind::Wubi,
                        "pinyin" => Kind::Pinyin,
                        "emoji" => Kind::Emoji,
                        other => {
                            return Err(format!("未知的 --kind `{other}`（wubi | pinyin | emoji）"))
                        }
                    }
                }
                "--raw" => raw = PathBuf::from(value),
                "--out" => out = Some(PathBuf::from(value)),
                "--report" => report = Some(PathBuf::from(value)),
                "--lexicon" => lexicon = Some(PathBuf::from(value)),
                other => return Err(format!("未知参数 `{other}`")),
            }
        }
        Ok(Args {
            kind,
            raw,
            out,
            report,
            lexicon,
        })
    }

    fn out(&self) -> PathBuf {
        self.out.clone().unwrap_or_else(|| match self.kind {
            Kind::Wubi | Kind::Emoji => PathBuf::from("data/wubi86"),
            Kind::Pinyin => PathBuf::from("data/pinyin"),
        })
    }

    fn report(&self) -> PathBuf {
        self.report.clone().unwrap_or_else(|| match self.kind {
            Kind::Wubi => PathBuf::from("out/m0a-report.md"),
            Kind::Pinyin => PathBuf::from("out/m3-pinyin-report.md"),
            Kind::Emoji => PathBuf::from("out/m4c-emoji-report.md"),
        })
    }

    fn lexicon(&self) -> PathBuf {
        self.lexicon
            .clone()
            .unwrap_or_else(|| PathBuf::from("data/pinyin/lexicon.tsv"))
    }
}

/// 码表里的一条原始记录。
struct RawRow {
    word: String,
    code: String,
    weight: f64,
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("部分验收项未通过，详见报告");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("错误：{e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool, String> {
    let args = Args::parse()?;
    let out = args.out();
    let report = args.report();
    match args.kind {
        Kind::Pinyin => pinyin::run(&args.raw, &out, &report),
        Kind::Wubi => run_wubi(&args.raw, &out, &report),
        Kind::Emoji => emoji::run(&args.raw, &args.lexicon(), &out, &report),
    }
}

fn run_wubi(raw_dir: &Path, out_dir: &Path, report_path: &Path) -> Result<bool, String> {
    let rows = parse_dict(&raw_dir.join("wubi86.dict.yaml"))?;
    let common = parse_common(&raw_dir.join("common3500.csv"))?;
    if rows.is_empty() {
        return Err("码表解析结果为空，请检查原始文件格式".into());
    }

    // ---- 分类与校验 ----
    // 五笔的单字全码不一定是 4 码：字根不足四个时补末笔识别码，只补到 3 码也是
    // 合法全码（好 = VBG、了 = BNH）。因此「码短于 4」不能作为「是简码」的判据，
    // 正确判据是「同一字的多条编码里，最长的那条是全码，其余是简码」。
    let mut full_len_of_word: HashMap<&str, usize> = HashMap::new();
    for row in &rows {
        if row.word.chars().count() != 1 {
            continue;
        }
        if let Some(code) = Code::parse(&row.code) {
            let slot = full_len_of_word.entry(row.word.as_str()).or_insert(0);
            *slot = (*slot).max(code.len());
        }
    }

    let mut lexicon: Vec<(Code, String, f64, u8)> = Vec::with_capacity(rows.len());
    let mut anomalies: Vec<String> = Vec::new();
    let mut malformed: Vec<String> = Vec::new();

    for row in &rows {
        let Some(code) = Code::parse(&row.code) else {
            malformed.push(format!("{} → `{}`", row.word, row.code));
            continue;
        };
        let single = row.word.chars().count() == 1;
        let level = if single {
            let full_len = full_len_of_word
                .get(row.word.as_str())
                .copied()
                .unwrap_or(code.len());
            if code.len() < full_len {
                code.len() as u8
            } else {
                0
            }
        } else {
            if code.len() != MAX_CODE_LEN {
                anomalies.push(format!(
                    "词组 `{}` 的码 `{}` 不是 {} 码",
                    row.word, row.code, MAX_CODE_LEN
                ));
            }
            0
        };
        lexicon.push((code, row.word.clone(), row.weight, level));
    }

    // 确定性排序：码升序 → 权重降序 → 词升序
    lexicon.sort_by(|a, b| {
        a.0.packed()
            .cmp(&b.0.packed())
            .then_with(|| b.2.total_cmp(&a.2))
            .then_with(|| a.1.cmp(&b.1))
    });

    // ---- 写 lexicon.tsv ----
    let mut text = String::with_capacity(lexicon.len() * 24);
    text.push_str(
        "# 亨利 / Heinrich —— 五笔 86 词库（生成物，请勿手改；重新生成用 tools/gen.ps1）\n",
    );
    text.push_str("# source: rime/rime-wubi wubi86.dict.yaml (LGPL-3.0)\n");
    text.push_str(
        "# format: code<TAB>word<TAB>weight<TAB>short   (short: 0=全码, 1/2/3=简码级别)\n",
    );
    for (code, word, weight, level) in &lexicon {
        text.push_str(code.as_str());
        text.push('\t');
        text.push_str(word);
        text.push('\t');
        text.push_str(&format!("{weight:.0}"));
        text.push('\t');
        text.push_str(&level.to_string());
        text.push('\n');
    }
    fs::create_dir_all(out_dir).map_err(|e| format!("创建输出目录失败：{e}"))?;
    let lexicon_path = out_dir.join("lexicon.tsv");
    fs::write(&lexicon_path, &text).map_err(|e| format!("写入词库失败：{e}"))?;
    let digest = sha256_hex(text.as_bytes());

    // ---- 写 fallback.tsv（D100：server 不可用时 TSF 内嵌应急的一级/二级简码）----
    //
    // 只收「单字 + 码长 ≤ 2」的简码条目，每码取权重最高的一条（lexicon 已按
    // 「码升序 → 权重降序」排好，遇到重复码跳过即可）。这份表在构建期被 TSF
    // `include_str!` 进 DLL：server 无应答时 1–2 码仍能出字，全码字照旧原样上屏。
    let mut fallback = String::with_capacity(641 * 16);
    fallback.push_str(
        "# 亨利 / Heinrich —— 应急简码表（生成物，请勿手改；重新生成用 tools/gen.ps1）\n",
    );
    fallback.push_str(
        "# 用途：server 无应答时 TSF 内嵌兜底（一级 + 二级简码单字）；format: code<TAB>word\n",
    );
    let mut fallback_count = 0usize;
    let mut last_code = "";
    for (code, word, _, level) in &lexicon {
        if *level != 1 && *level != 2 {
            continue;
        }
        if code.as_str() == last_code {
            continue;
        }
        last_code = code.as_str();
        fallback.push_str(code.as_str());
        fallback.push('\t');
        fallback.push_str(word);
        fallback.push('\n');
        fallback_count += 1;
    }
    let fallback_path = out_dir.join("fallback.tsv");
    fs::write(&fallback_path, &fallback).map_err(|e| format!("写入应急简码表失败：{e}"))?;
    println!(
        "应急简码：{fallback_count} 条 → {}",
        fallback_path.display()
    );

    let manifest = format!(
        "# 亨利方案清单（由 heinrich-gen 生成，请勿手改）\n\
         id = \"wubi86\"\n\
         name = \"五笔 86\"\n\
         version = \"1.0\"\n\
         alphabet = \"abcdefghijklmnopqrstuvwxy\"\n\
         max_code_len = {MAX_CODE_LEN}\n\
         source = \"rime/rime-wubi wubi86.dict.yaml\"\n\
         license = \"LGPL-3.0\"\n\
         entry_count = {count}\n\
         lexicon_sha256 = \"{digest}\"\n",
        count = lexicon.len()
    );
    fs::write(out_dir.join("scheme.toml"), manifest).map_err(|e| format!("写入清单失败：{e}"))?;

    // ---- 统计与验收 ----
    let stats = Stats::compute(&lexicon, &common);
    let report = render_report(&stats, &anomalies, &malformed, &digest, out_dir);
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败：{e}"))?;
    }
    fs::write(report_path, report).map_err(|e| format!("写入报告失败：{e}"))?;

    println!("词库：{} 条 → {}", lexicon.len(), lexicon_path.display());
    println!("哈希：{digest}");
    println!("报告：{}", report_path.display());
    for (name, ok, detail) in stats.verdicts() {
        println!("[{}] {name}：{detail}", if ok { "通过" } else { "未通过" });
    }
    Ok(stats.all_passed())
}

struct Stats {
    total: usize,
    single: usize,
    multi: usize,
    by_len: [usize; MAX_CODE_LEN + 1],
    short: [usize; 4],
    /// 单字全码按码长（3 码 / 4 码；五笔没有 1、2 码全码）
    full3: usize,
    full4: usize,
    /// 一级简码 → 命中的词（应恰好 25 键、每键 1 条）
    level1: BTreeMap<String, Vec<String>>,
    /// 编码 → 词数
    shared: Vec<(String, usize)>,
    /// 简码不是其全码前缀的情况
    prefix_violations: Vec<String>,
    /// 常用 3500 字覆盖
    covered: usize,
    common_total: usize,
    missing: Vec<char>,
    /// 4 码空间的占用
    used4: usize,
    space4: usize,
}

impl Stats {
    fn compute(lexicon: &[(Code, String, f64, u8)], common: &[char]) -> Self {
        let mut by_len = [0usize; MAX_CODE_LEN + 1];
        let mut short = [0usize; 4];
        let mut full3 = 0usize;
        let mut full4 = 0usize;
        let mut level1: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut per_code: HashMap<u32, (String, usize)> = HashMap::new();
        let mut single_words: BTreeSet<char> = BTreeSet::new();
        // 词 → 全部编码（用于检查简码是否为全码前缀）
        let mut codes_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

        for (code, word, _, level) in lexicon {
            by_len[code.len()] += 1;
            if *level > 0 {
                short[*level as usize] += 1;
                if *level == 1 {
                    level1
                        .entry(code.as_str().to_string())
                        .or_default()
                        .push(word.clone());
                }
            } else if word.chars().count() == 1 {
                match code.len() {
                    3 => full3 += 1,
                    4 => full4 += 1,
                    _ => {}
                }
            }
            let slot = per_code
                .entry(code.packed())
                .or_insert_with(|| (code.as_str().to_string(), 0));
            slot.1 += 1;
            if word.chars().count() == 1 {
                if let Some(c) = word.chars().next() {
                    single_words.insert(c);
                }
            }
            codes_of
                .entry(word.as_str())
                .or_default()
                .push(code.as_str());
        }

        let mut prefix_violations = Vec::new();
        for (word, codes) in &codes_of {
            let full_len = codes.iter().map(|c| c.len()).max().unwrap_or(0);
            let full: Vec<&str> = codes
                .iter()
                .copied()
                .filter(|c| c.len() == full_len)
                .collect();
            if full.is_empty() {
                continue;
            }
            // 一级简码是「键位 → 高频字」的固定映射（我=Q、发=V…），与全码无前缀关系，
            // 不能拿前缀规则去卡；二级/三级简码则是「取全码前两/三码」，必须成前缀。
            for c in codes.iter().filter(|c| c.len() >= 2 && c.len() < full_len) {
                if !full.iter().any(|f| f.starts_with(*c)) {
                    prefix_violations.push(format!(
                        "{word}: {c} 码简码 `{c}` 不是全码 `{}` 的前缀",
                        full[0],
                        c = c.len()
                    ));
                }
            }
        }
        prefix_violations.sort();

        let missing: Vec<char> = common
            .iter()
            .copied()
            .filter(|c| !single_words.contains(c))
            .collect();

        let mut shared: Vec<(String, usize)> =
            per_code.into_values().filter(|(_, n)| *n > 1).collect();
        shared.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let single = lexicon
            .iter()
            .filter(|(_, w, _, _)| w.chars().count() == 1)
            .count();

        Self {
            total: lexicon.len(),
            single,
            multi: lexicon.len() - single,
            by_len,
            short,
            full3,
            full4,
            level1,
            shared,
            prefix_violations,
            covered: common.len() - missing.len(),
            common_total: common.len(),
            missing,
            used4: by_len[MAX_CODE_LEN],
            space4: 25usize.pow(MAX_CODE_LEN as u32),
        }
    }

    fn coverage_pct(&self) -> f64 {
        if self.common_total == 0 {
            return 0.0;
        }
        100.0 * self.covered as f64 / self.common_total as f64
    }

    fn verdicts(&self) -> Vec<(String, bool, String)> {
        vec![
            (
                "① 3,500 常用字覆盖 ≥ 99%".into(),
                self.coverage_pct() >= 99.0,
                format!(
                    "{}/{} = {:.2}%",
                    self.covered,
                    self.common_total,
                    self.coverage_pct()
                ),
            ),
            (
                "③ 二级/三级简码必须是全码前缀（例外 ≤ 1%）".into(),
                self.prefix_violation_rate() <= 1.0,
                format!(
                    "{} 处 / {} 条 2–3 码简码 = {:.2}%",
                    self.prefix_violations.len(),
                    self.short[2] + self.short[3],
                    self.prefix_violation_rate()
                ),
            ),
            (
                "④ 一级简码 25 键、每键至多 1 条".into(),
                self.level1.len() == 25 && self.level1.values().all(|v| v.len() == 1),
                format!(
                    "{} 键，最多 {} 条/键",
                    self.level1.len(),
                    self.level1.values().map(Vec::len).max().unwrap_or(0)
                ),
            ),
        ]
    }

    fn prefix_violation_rate(&self) -> f64 {
        let base = self.short[2] + self.short[3];
        if base == 0 {
            return 0.0;
        }
        100.0 * self.prefix_violations.len() as f64 / base as f64
    }

    fn all_passed(&self) -> bool {
        self.verdicts().iter().all(|(_, ok, _)| *ok)
    }
}

fn render_report(
    s: &Stats,
    anomalies: &[String],
    malformed: &[String],
    digest: &str,
    out: &Path,
) -> String {
    let mut r = String::new();
    r.push_str("# M0a 数据装配校验报告\n\n");
    r.push_str("> 由 `heinrich-gen` 自动生成，请勿手改。本文不含时间戳，同输入必得同字节。\n\n");

    r.push_str("## 1. 产物\n\n| 项 | 值 |\n|---|---|\n");
    r.push_str(&format!(
        "| 词库 | `{}` |\n",
        out.join("lexicon.tsv").display()
    ));
    r.push_str(&format!(
        "| 清单 | `{}` |\n",
        out.join("scheme.toml").display()
    ));
    r.push_str(&format!("| 词条数 | {} |\n", s.total));
    r.push_str(&format!("| 词库 SHA-256 | `{digest}` |\n\n"));

    r.push_str("## 2. 规模\n\n| 项 | 值 |\n|---|---|\n");
    r.push_str(&format!("| 单字词条 | {} |\n", s.single));
    r.push_str(&format!("| 词组词条 | {} |\n", s.multi));
    for len in 1..=MAX_CODE_LEN {
        r.push_str(&format!("| {len} 码词条 | {} |\n", s.by_len[len]));
    }
    r.push_str(&format!(
        "| 4 码空间占用 | {} / {} = {:.1}% |\n",
        s.used4,
        s.space4,
        100.0 * s.used4 as f64 / s.space4 as f64
    ));
    r.push_str(&format!(
        "| 一级/二级/三级简码 | {} / {} / {} |\n",
        s.short[1], s.short[2], s.short[3]
    ));
    r.push_str(&format!(
        "| 单字全码 3 码 / 4 码 | {} / {} |\n\n",
        s.full3, s.full4
    ));
    r.push_str(
        "> 五笔的单字全码**不一定是 4 码**：字根不足四个时补末笔识别码，补到 3 码也是合法全码\n",
    );
    r.push_str("> （如 `好 = VBG`、`了 = BNH`）。因此「码短于 4 即简码」是错的判据，本表按\n");
    r.push_str("> 「同一字多条编码中最长者为全码」分类。3 码全码的存在也是「候选集必须按精确码长匹配」的原因之一。\n\n");

    r.push_str("一级简码键位表（`wubi86.dict.yaml` 自带，随包提供）：\n\n| 键 | 字 |\n|---|---|\n");
    for (key, words) in &s.level1 {
        r.push_str(&format!("| `{key}` | {} |\n", words.join(" / ")));
    }
    r.push('\n');

    r.push_str("## 3. 重码\n\n");
    r.push_str(&format!(
        "| 项 | 值 |\n|---|---|\n| 存在多个词的编码数 | {} |\n| 单编码最大重码数 | {} |\n\n",
        s.shared.len(),
        s.shared.first().map(|(_, n)| *n).unwrap_or(0)
    ));
    r.push_str("重码最多的编码（前 10）：\n\n| 编码 | 词数 |\n|---|---|\n");
    for (code, n) in s.shared.iter().take(10) {
        r.push_str(&format!("| `{code}` | {n} |\n"));
    }
    r.push('\n');

    r.push_str("## 4. 验收项\n\n| 验收项 | 结果 | 实测 |\n|---|---|---|\n");
    for (name, ok, detail) in s.verdicts() {
        r.push_str(&format!(
            "| {name} | {} | {detail} |\n",
            if ok { "✅ 通过" } else { "❌ 未通过" }
        ));
    }
    r.push('\n');

    r.push_str("## 5. 异常明细\n\n");
    if malformed.is_empty() {
        r.push_str("- 非法编码：无\n");
    } else {
        r.push_str(&format!(
            "- 非法编码（已丢弃）{} 条：\n  - 说明：全部为含 `z` 的编码（`z` 是五笔万能键，不参与编码），\
             内容为带圈数字、注音符号等非汉字。丢弃是预期行为。\n",
            malformed.len()
        ));
        for m in malformed.iter().take(10) {
            r.push_str(&format!("  - {m}\n"));
        }
    }
    if anomalies.is_empty() {
        r.push_str("- 词组码长异常：无\n");
    } else {
        r.push_str(&format!("- 词组码长异常 {} 条：\n", anomalies.len()));
        for a in anomalies.iter().take(20) {
            r.push_str(&format!("  - {a}\n"));
        }
    }
    if s.prefix_violations.is_empty() {
        r.push_str("- 简码非全码前缀：无\n");
    } else {
        r.push_str(&format!(
            "- 简码非全码前缀 {} 条：\n",
            s.prefix_violations.len()
        ));
        for v in s.prefix_violations.iter().take(30) {
            r.push_str(&format!("  - {v}\n"));
        }
    }

    r.push_str("\n## 6. 常用字缺口\n\n");
    if s.missing.is_empty() {
        r.push_str("无缺口。\n");
    } else {
        r.push_str(&format!("缺 {} 字：\n\n```\n", s.missing.len()));
        let line: String = s.missing.iter().take(200).collect();
        r.push_str(&line);
        r.push_str("\n```\n");
    }

    r.push_str("\n## 7. 复现方式\n\n```powershell\n");
    r.push_str(
        "cargo run -p heinrich-gen -- --raw data/raw --out data/wubi86 --report out/m0a-report.md\n",
    );
    r.push_str("```\n");
    r
}

/// 读文本并剥掉 UTF-8 BOM（Windows 编辑器常给中文文本加 BOM）。
pub(crate) fn read_text(path: &Path) -> Result<String, String> {
    let text = fs::read_to_string(path).map_err(|e| {
        format!(
            "读取 {} 失败：{e}（先跑 tools/fetch-data.ps1）",
            path.display()
        )
    })?;
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text))
}

/// 解析 Rime 词典：`...` 之后是 `词 \t 码 \t 权重`。
fn parse_dict(path: &Path) -> Result<Vec<RawRow>, String> {
    let text = read_text(path)?;
    let mut rows = Vec::with_capacity(140_000);
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
        let (Some(word), Some(code)) = (cols.next(), cols.next()) else {
            continue;
        };
        let weight = cols
            .next()
            .and_then(|w| w.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        if word.is_empty() || code.is_empty() {
            continue;
        }
        rows.push(RawRow {
            word: word.to_string(),
            code: code.to_string(),
            weight,
        });
    }
    Ok(rows)
}

/// 解析常用字表：`ID,汉字,常用`，取第二列。
///
/// 表里有若干 `_` 占位行（原数据集的缺失标记），不是汉字，统计前剔除，
/// 否则覆盖率会被两个无关字符拖下水。
pub(crate) fn parse_common(path: &Path) -> Result<Vec<char>, String> {
    let text = read_text(path)?;
    let mut out = Vec::with_capacity(3500);
    for (idx, line) in text.lines().enumerate() {
        if idx == 0 {
            continue; // 表头
        }
        let mut cols = line.split(',');
        let _id = cols.next();
        if let Some(ch) = cols.next().and_then(|s| s.trim().chars().next()) {
            if is_han(ch) {
                out.push(ch);
            }
        }
    }
    Ok(out)
}

pub(crate) fn is_han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
}
