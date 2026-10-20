//! 排序与查询语义验收（PROJECT.md §5、M0b 验收项①②）。
//!
//! 全部用固定时间戳，不读系统时钟，因此断言完全可复现。

use xizi_core::{lookup, LookupMode, MemoryStore, RankingConfig, Tier, UsageStore};
use xizi_scheme::Scheme;

const NOW: i64 = 1_700_000_000;

fn scheme() -> Scheme {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini");
    Scheme::load(&dir).expect("fixture 应可装载")
}

fn words(cands: &[xizi_core::Candidate]) -> Vec<String> {
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

#[test]
fn single_use_moves_at_most_one_slot() {
    let s = scheme();
    let mut store = MemoryStore::new();
    let before = ranked(&s, "vbgd", &store);

    record(&mut store, "蒿", 1);
    let after = ranked(&s, "vbgd", &store);

    assert_eq!(after, ["号", "豪", "蒿", "呵"]);
    let pos_before = before.iter().position(|w| w == "蒿").unwrap();
    let pos_after = after.iter().position(|w| w == "蒿").unwrap();
    assert_eq!(pos_before - pos_after, 1, "一次使用只应前进一位");
}

#[test]
fn leader_holds_against_a_few_uses() {
    let s = scheme();
    let mut store = MemoryStore::new();
    record(&mut store, "呵", 3);

    let cands = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    assert_eq!(cands[0].word, "号", "第一名不得被三次使用夺走");
    assert!(cands[0].throne_held, "应标记为「因阈值保住首位」");
    assert_eq!(cands[1].word, "呵", "未过阈值的挑战者只能上浮到第二位");
}

#[test]
fn sustained_use_can_take_the_throne() {
    let s = scheme();
    let mut store = MemoryStore::new();
    record(&mut store, "豪", 6);

    let cands = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    assert_eq!(cands[0].word, "豪", "持续使用应能越过 1.5 倍阈值");
    assert!(!cands[0].throne_held);
}

#[test]
fn low_frequency_word_never_reaches_first() {
    let s = scheme();
    let mut store = MemoryStore::new();
    record(&mut store, "蒿", 10);

    let cands = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    assert_eq!(cands[0].word, "号");
    assert!(cands[0].throne_held);
    assert_eq!(cands[1].word, "蒿", "低权重词可以上浮，但到不了第一位");
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
fn pinned_leads_and_suppressed_disappears() {
    let s = scheme();
    let mut store = MemoryStore::new();
    store.set_tier("蒿", Tier::Pinned);
    store.set_tier("号", Tier::Suppressed);

    let cands = lookup(&s, "vbgd", &store, NOW, &RankingConfig::default()).0;
    assert_eq!(cands[0].word, "蒿", "固顶词无视权重与阈值");
    assert!(!cands.iter().any(|c| c.word == "号"), "抑制词不得出现");
}

#[test]
fn exact_code_lookup_does_not_leak_longer_codes() {
    let s = scheme();
    let vb = xizi_scheme::Code::parse("vb").unwrap();
    let hits = s.exact_indices(&vb);
    assert_eq!(hits.len(), 1);
    assert_eq!(s.entry(hits[0]).word, "好");
    assert_eq!(s.entry(hits[0]).kind.short_level(), 2);

    let v = xizi_scheme::Code::parse("v").unwrap();
    let hits = s.exact_indices(&v);
    assert_eq!(hits.len(), 1);
    assert_eq!(s.entry(hits[0]).word, "发");
    assert_eq!(s.entry(hits[0]).kind.short_level(), 1);
}
