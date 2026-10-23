//! M3 总验收：混输判定语料（150 例 + 3 个负例）。
//!
//! 语料在 `tests/corpus/mixed.tsv`，由 `tools/gen-corpus.py` 从**真实词库分层取样**
//! 生成：一半取高频词、一半在全量里均匀分散，避免「只挑容易过的样例」。所以这个
//! 测试盯的是判定规则在真实数据上的表现，不是手写夹具上的正确性（那些在单测里）。
//!
//! 断言对应 §6.1 的规则与 §6.5 的多段规则：
//!
//! | kind | 断言 |
//! |---|---|
//! | `wubi` | 精确命中 → 主段五笔，词在首屏 |
//! | `wubi-like` | 同上；这批的码**恰好也能切成一串拼音音节**，专门验 D48「五笔优先」 |
//! | `pinyin` | 五笔没命中、拼音有 → 主段拼音，词在首屏 |
//! | `mixed` | 多段 → 主段拼音，拼出的词在首屏，chip 挂在第 1 个字之后（D64） |
//! | `empty` | 拼不出词典真词 → 整体不出窗（D64 严格模式） |
//!
//! 另有一条横切断言（§6.1 验收第 ④ 项「首屏只放当前判定语言」）：主段每一条候选
//! 都必须能在**对应语言的词库**里查到。这条比逐个比对词表更彻底——连「混进了另一
//! 语言的候选」这种错误也能抓到。
//!
//! 数据缺失时跳过而不是失败：`data/` 是生成物（`tools/fetch-data.ps1` + `gen.ps1`），
//! 刚 clone 下来不存在是正常状态。

use heinrich_core::{Engine, MemoryStore, RankingConfig, Source};
use heinrich_scheme::{PinyinLexicon, Scheme};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const CORPUS: &str = include_str!("corpus/mixed.tsv");

fn data_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

struct Row {
    code: String,
    source: String,
    word: String,
    kind: String,
}

fn rows() -> Vec<Row> {
    let mut out = Vec::new();
    for line in CORPUS.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        assert!(f.len() >= 4, "语料行字段不足：{line}");
        out.push(Row {
            code: f[0].into(),
            source: f[1].into(),
            word: f[2].into(),
            kind: f[3].into(),
        });
    }
    out
}

/// 词库 `lexicon.tsv` 里出现过的词（第 2 列）。两侧格式相同，只是第 1 列含义不同。
fn words_of(path: &Path) -> HashSet<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split('\t').nth(1))
        .map(str::to_string)
        .collect()
}

#[test]
fn mixed_judgement_corpus() {
    let root = data_root();
    if !root.join("wubi86").is_dir() || !root.join("pinyin").is_dir() {
        eprintln!(
            "跳过：{} 缺词库（先跑 tools/fetch-data.ps1 与 gen.ps1）",
            root.display()
        );
        return;
    }
    let wubi_words = words_of(&root.join("wubi86/lexicon.tsv"));
    let pinyin_words = words_of(&root.join("pinyin/lexicon.tsv"));
    let both_words: HashSet<String> = wubi_words.union(&pinyin_words).cloned().collect();

    let scheme = Scheme::load(&root.join("wubi86")).expect("五笔词库装载失败");
    let mut lex = PinyinLexicon::load(&root.join("pinyin")).expect("拼音词库装载失败");
    // 与 server 装载期一致：拼音候选的 chip 靠这一列
    lex.attach_wubi_codes(&scheme);
    let mut eng = Engine::new(scheme, MemoryStore::new(), RankingConfig::default());
    eng = eng.with_pinyin(lex);

    let mut failures: Vec<String> = Vec::new();
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    let started = std::time::Instant::now();
    for r in rows() {
        *kinds.entry(r.kind.clone()).or_default() += 1;
        let m = eng.mixed(&r.code, 0);
        let head: Vec<&str> = m.primary.iter().take(6).map(|c| c.word.as_str()).collect();

        if r.source == "empty" {
            if !m.is_empty() {
                failures.push(format!(
                    "[{}] `{}` 应不出窗，实际 {} 条（首屏 {head:?}）",
                    r.kind,
                    r.code,
                    m.primary.len() + m.folded.len()
                ));
            }
            continue;
        }

        let want = match r.source.as_str() {
            "wubi" => Source::Wubi,
            "pinyin" => Source::Pinyin,
            other => panic!("语料里出现未知的 expect_source：{other}"),
        };
        if m.source != want {
            failures.push(format!(
                "[{}] `{}` 该判{}，实际判{}（首屏 {head:?}）",
                r.kind,
                r.code,
                want.label(),
                m.source.label()
            ));
            continue;
        }

        if !r.word.is_empty() && !head.contains(&r.word.as_str()) {
            failures.push(format!(
                "[{}] `{}` 首屏没有「{}」，只有 {head:?}",
                r.kind, r.code, r.word
            ));
            continue;
        }

        // 首屏只放当前判定语言。多段是例外：它的候选是「拼音段字 + 五笔段字」拼出来的，
        // 真词过滤取的是**两份词库的并集**（D64），所以「彩色」只进五笔库也要能出。
        let dict = match (m.source, r.kind.as_str()) {
            (_, "mixed") => &both_words,
            (Source::Wubi, _) => &wubi_words,
            (Source::Pinyin, _) => &pinyin_words,
        };
        if let Some(stray) = m.primary.iter().find(|c| !dict.contains(&c.word)) {
            failures.push(format!(
                "[{}] `{}` 首屏混进了{}词库里没有的「{}」",
                r.kind,
                r.code,
                m.source.label(),
                stray.word
            ));
            continue;
        }

        // 多段：chip 必须挂在拼音段的字后面（D64）
        if r.kind == "mixed" {
            if let Some(c) = m.primary.iter().find(|c| c.word == r.word) {
                match &c.chip {
                    Some(chip) if chip.chars_before == 1 => {}
                    Some(chip) => failures.push(format!(
                        "[mixed] `{}` 的 chip 位置应为 1，实际 {}（码 {}）",
                        r.code, chip.chars_before, chip.code
                    )),
                    None => failures.push(format!("[mixed] `{}` 的多段候选没有 chip", r.code)),
                }
            }
        }
    }

    assert_eq!(
        kinds.values().sum::<usize>(),
        rows().len(),
        "语料行数与统计不符"
    );
    // 逐键查询发生在**每个字母键**上（D37），所以这里顺手记一下单次查询的量级：
    // 段池 16→32 之后组合上限翻四倍，这个数字就是「加池子有没有伤到延迟」的证据。
    let n = rows().len();
    eprintln!(
        "{n} 例查询耗时 {:?}（均 {:.0} µs/例；装载不计入）",
        started.elapsed(),
        started.elapsed().as_secs_f64() * 1e6 / n as f64
    );
    assert!(
        failures.is_empty(),
        "{}/{} 例不符：\n{}",
        failures.len(),
        rows().len(),
        failures.join("\n")
    );
}
