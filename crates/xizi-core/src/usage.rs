//! 学习数据：使用计数、时间戳与三档归属。
//!
//! 存储是可替换的：M0 用内存实现（CLI/单测），server 阶段换成 SQLite 实现，
//! 排序算法只依赖 [`UsageStore`] 这个抽象。

use std::collections::HashMap;

/// 用户对某个词的处置档位（§5.3）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tier {
    /// 固顶：永远排在候选首位，不参与浮动
    Pinned,
    /// 学习（默认）：参与打分浮动
    #[default]
    Learning,
    /// 抑制：永不出现
    Suppressed,
}

/// 一个词的使用统计。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// 累计上屏次数
    pub n: u32,
    /// 最近一次上屏的 Unix 秒；0 表示从未使用
    pub last_used: i64,
}

/// 学习数据的读写接口。
pub trait UsageStore {
    fn usage(&self, word: &str) -> Usage;
    fn tier(&self, word: &str) -> Tier;
    /// 记一次上屏。
    fn record(&mut self, word: &str, at_unix: i64);
    fn set_tier(&mut self, word: &str, tier: Tier);
}

#[derive(Clone, Debug, Default)]
struct Record {
    usage: Usage,
    tier: Tier,
}

/// 内存实现：供 CLI、单测与「临时会话」使用。
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: HashMap<String, Record>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn known_words(&self) -> usize {
        self.map.len()
    }

    /// 按次数降序列出（供 `:stats` 展示）。
    pub fn top(&self, limit: usize) -> Vec<(&str, Usage, Tier)> {
        let mut items: Vec<(&str, Usage, Tier)> = self
            .map
            .iter()
            .filter(|(_, r)| r.usage.n > 0)
            .map(|(w, r)| (w.as_str(), r.usage, r.tier))
            .collect();
        items.sort_unstable_by(|a, b| b.1.n.cmp(&a.1.n).then_with(|| a.0.cmp(b.0)));
        items.truncate(limit);
        items
    }
}

impl UsageStore for MemoryStore {
    fn usage(&self, word: &str) -> Usage {
        self.map.get(word).map(|r| r.usage).unwrap_or_default()
    }

    fn tier(&self, word: &str) -> Tier {
        self.map.get(word).map(|r| r.tier).unwrap_or_default()
    }

    fn record(&mut self, word: &str, at_unix: i64) {
        let rec = self.map.entry(word.to_string()).or_default();
        rec.usage.n = rec.usage.n.saturating_add(1);
        rec.usage.last_used = at_unix;
    }

    fn set_tier(&mut self, word: &str, tier: Tier) {
        self.map.entry(word.to_string()).or_default().tier = tier;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_accumulates_and_keeps_last_time() {
        let mut s = MemoryStore::new();
        assert_eq!(s.usage("好"), Usage::default());
        s.record("好", 100);
        s.record("好", 250);
        assert_eq!(
            s.usage("好"),
            Usage {
                n: 2,
                last_used: 250
            }
        );
        assert_eq!(s.tier("好"), Tier::Learning);
    }

    #[test]
    fn tier_is_independent_of_usage() {
        let mut s = MemoryStore::new();
        s.set_tier("号", Tier::Suppressed);
        s.record("号", 10);
        assert_eq!(s.tier("号"), Tier::Suppressed);
        assert_eq!(s.usage("号").n, 1);
    }
}
