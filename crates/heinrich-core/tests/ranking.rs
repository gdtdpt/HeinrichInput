//! 排序与查询语义验收（PROJECT.md §5、M0b 验收项①②）。
//!
//! 全部用固定时间戳，不读系统时钟，因此断言完全可复现。

use heinrich_core::{lookup, LookupMode, MemoryStore, RankingConfig, Tier, UsageStore};
use heinrich_scheme::Scheme;

const NOW: i64 = 1_700_000_000;

fn scheme() -> Scheme {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini");
    Scheme::load(&dir).expect("fixture 应可装载")
}

fn words(cands: &[heinrich_core::Candidate]) -> Vec<String> {
    cands.iter().map(|c| c.word.clone()).collect()
}

fn ranked(s: &Scheme, input: &str, store: &MemoryStore) -> Vec<String> {
    words(&lookup(s, input, store, NOW, &RankingConfig::default()).0)
}

fn record(store: &mut MemoryStore, word: &str, times: u32) {
    for _ in 0..times {
        store.record(word, NOW);
    }
}

#[test]
fn lookup_matches_exact_code_length_only() {
    let s = scheme();
    let store = MemoryStore::new();

    // 1 码只命中一级简码，不展开 vbg/vbgd 等更长码
    let (cands, mode) = lookup(&s, "v", &store, NOW, &RankingConfig::default());
    assert_eq!(mode, LookupMode::Exact);
    assert_eq!(words(&cands), ["发"]);

    // 2 码命中二级简码
    assert_eq!(ranked(&s, "vb", &store), ["好"]);

    // 3 码命中 3 码全码
    assert_eq!(ranked(&s, "vbg", &store), ["好"]);

    // 4 码命中全码重码组
    assert_eq!(ranked(&s, "vbgd", &store), ["号", "豪", "呵", "蒿"]);
}

#[test]
fn empty_code_falls_back_to_prefix() {
    let s = scheme();
    let store = MemoryStore::new();
    // `ntc` 本身没有词条，但 `ntcy`（发）以它为前缀
    let (cands, mode) = lookup(&s, "ntc", &store, NOW, &RankingConfig::default());
    assert_eq!(mode, LookupMode::PrefixFallback, "空码才回退到前缀");
    assert_eq!(words(&cands), ["发"]);

    // 完全没有命中的输入
    let (cands, _) = lookup(&s, "zz", &store, NOW, &RankingConfig::default());
    assert!(cands.is_empty(), "非法编码应无候选");
}

#[test]
fn unused_candidates_follow_static_weight() {
    let s = scheme();
    let store = MemoryStore::new();
    let cands = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    assert_eq!(words(&cands), ["号", "豪", "呵", "蒿"]);
    assert!(cands.iter().all(|c| c.n == 0 && c.learning == 0.0));
}

/// 第一次上屏**完全不计分**（D58）：它只说明「点过」，不说明「要用」。
///
/// 这条替代了 D54 的 `single_use_does_not_reorder`——那个版本靠「一次最多加
/// 0.2426 分、够不到 δ=0.35」实现，机制是对的但代价太大：δ 折合几次按键会随 n
/// 膨胀（n≈26 时要五十次），结果用得多的词反而被用得少的词压住。
#[test]
fn single_use_does_not_reorder() {
    let s = scheme();
    let mut store = MemoryStore::new();
    let before = ranked(&s, "vbgd", &store);

    record(&mut store, "蒿", 1);
    let after = ranked(&s, "vbgd", &store);
    assert_eq!(after, before, "偶用一次不该改顺序");

    // 换一个权重最低的词也一样——不看分差，只看次数
    record(&mut store, "呵", 1);
    assert_eq!(ranked(&s, "vbgd", &store), before);
}

/// 第二次上屏就开始起作用：从这一次起，加分规则与以前完全一样。
#[test]
fn second_use_reorders() {
    let s = scheme();
    let mut store = MemoryStore::new();
    record(&mut store, "蒿", 2);
    assert_eq!(
        ranked(&s, "vbgd", &store),
        ["号", "豪", "蒿", "呵"],
        "蒿（0.690）越过呵（0.578），还够不到豪（0.941）"
    );
}

/// 用得少就停在少数——不会因为「按了三次」就跳到前面去。
#[test]
fn three_uses_stop_where_the_scores_say() {
    let s = scheme();
    let mut store = MemoryStore::new();
    record(&mut store, "蒿", 3);
    assert_eq!(
        ranked(&s, "vbgd", &store),
        ["号", "豪", "蒿", "呵"],
        "蒿（0.832）仍低于豪（0.941）"
    );
}

/// **用户核心诉求的回归**：用得多的词必须赢过用得少的。
///
/// 蒿的静态权重只有号的 45%，但用十次就该排第一（1.253 > 1.000）。
/// D54 的 δ=0.35 下它够不到门槛（1.253 < 1.000+0.35），打十次也上不去——
/// 这正是「输入法在手上越用越舒服」失灵的地方。
#[test]
fn heavy_use_beats_a_heavier_static_weight() {
    let s = scheme();
    let mut store = MemoryStore::new();
    record(&mut store, "蒿", 10);

    let cands = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    assert_eq!(cands[0].word, "蒿", "十次使用应当越过码表首位");
    assert_eq!(cands[1].word, "号");
}

/// 刚上位的词不该被原首位「打一次就夺回」。
///
/// 这是 D44 王座设计的核心缺陷的回归测试：那时门槛是「原首位动态总分的 1.5 倍」，
/// 它每被打一次门槛就抬一次，挑战者刚上位就会被踢回去（实测要再打 11 次）。
/// 现在原首位打一次得 0 分（n=1），连门槛都不需要。
#[test]
fn the_challenger_is_not_pushed_back() {
    let s = scheme();
    let mut store = MemoryStore::new();

    record(&mut store, "豪", 6);
    assert_eq!(ranked(&s, "vbgd", &store)[0], "豪", "六次后已上位");

    record(&mut store, "号", 1);
    let after = ranked(&s, "vbgd", &store);
    assert_eq!(after[0], "豪", "原首位打一次不该夺回");
    assert_eq!(after[1], "号");
}

#[test]
fn decay_lets_old_usage_fade() {
    let s = scheme();
    let mut store = MemoryStore::new();
    record(&mut store, "豪", 6);

    let fresh = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    // 半年之后同样的使用次数：学习分按 45 天半衰期衰减到约 1/16
    let stale_now = NOW + 180 * 86_400;
    let stale = lookup(&s, "vbgd", &store, stale_now, &RankingConfig::default()).0;
    assert_eq!(fresh[0].word, "豪");
    assert_eq!(stale[0].word, "号", "半年不用应退回码表序");
    let decayed = stale.iter().find(|c| c.word == "豪").unwrap();
    assert!(decayed.learning < 0.15, "衰减后学习分应接近零");
}

#[test]
fn suppressed_disappears() {
    let s = scheme();
    let mut store = MemoryStore::new();
    store.set_tier("号", Tier::Suppressed);

    let cands = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    assert!(!cands.iter().any(|c| c.word == "号"), "抑制词不得出现");
    assert!(
        cands.iter().any(|c| c.word == "蒿"),
        "未处置的词仍按码表序出现"
    );
}

#[test]
fn exact_code_lookup_does_not_leak_longer_codes() {
    let s = scheme();
    let vb = heinrich_scheme::Code::parse("vb").unwrap();
    let hits = s.exact_indices(&vb);
    assert_eq!(hits.len(), 1);
    assert_eq!(s.entry(hits[0]).word, "好");
    assert_eq!(s.entry(hits[0]).kind.short_level(), 2);

    let v = heinrich_scheme::Code::parse("v").unwrap();
    let hits = s.exact_indices(&v);
    assert_eq!(hits.len(), 1);
    assert_eq!(s.entry(hits[0]).word, "发");
    assert_eq!(s.entry(hits[0]).kind.short_level(), 1);
}
