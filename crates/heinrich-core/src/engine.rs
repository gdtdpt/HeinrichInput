//! 组合期状态机：一次「开始打字 → 上屏」期间的候选顺序必须锁定（§5.4）。
//!
//! 为什么需要锁定：候选是按综合分实时算的，若某次选择立刻改变分数，同一串
//! 按键在翻页过程中会「跳位」，用户的手指会落到错的候选上。因此同一缓冲区
//! 的排序结果被缓存，直到缓冲区变化或组合结束才重算。

use heinrich_scheme::Scheme;

use crate::rank::{lookup, Candidate, LookupMode, RankingConfig};
use crate::usage::{Tier, UsageStore};

/// 输入法引擎：持有一份方案、一份学习数据与排序参数。
pub struct Engine<S: UsageStore> {
    scheme: Scheme,
    store: S,
    cfg: RankingConfig,
    /// (缓冲区, 上次排序结果, 来源)：组合期顺序锁定
    cache: Option<(String, Vec<Candidate>, LookupMode)>,
    /// 本次组合是否已经产生过候选（用于统计「首屏」等场景）
    composed: bool,
}

impl<S: UsageStore> Engine<S> {
    pub fn new(scheme: Scheme, store: S, cfg: RankingConfig) -> Self {
        Self {
            scheme,
            store,
            cfg,
            cache: None,
            composed: false,
        }
    }

    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// 取当前缓冲区的候选；同一缓冲区重复调用返回同一顺序（顺序锁定）。
    pub fn candidates(&mut self, buffer: &str, now: i64) -> &[Candidate] {
        let hit = matches!(&self.cache, Some((cached, _, _)) if cached == buffer);
        if !hit {
            let (list, mode) = if buffer.is_empty() {
                (Vec::new(), LookupMode::Exact)
            } else {
                lookup(&self.scheme, buffer, &self.store, now, &self.cfg)
            };
            self.cache = Some((buffer.to_string(), list, mode));
        }
        self.composed |= !buffer.is_empty();
        self.cache
            .as_ref()
            .map(|(_, l, _)| l.as_slice())
            .unwrap_or(&[])
    }

    /// 当前候选集的来源（精确命中 / 空码回退前缀）。
    pub fn lookup_mode(&self) -> Option<LookupMode> {
        self.cache.as_ref().map(|(_, _, m)| *m)
    }

    /// 选中第 `idx` 个候选并记一次上屏；返回被选中的候选。
    pub fn select(&mut self, buffer: &str, idx: usize, now: i64) -> Option<Candidate> {
        let picked = self.candidates(buffer, now).get(idx)?.clone();
        self.store.record(&picked.word, now);
        // 上屏即组合结束：缓存作废，下一次组合用新的学习数据重算
        self.cache = None;
        self.composed = false;
        Some(picked)
    }

    /// 调整候选档位（供 CLI `:pin` / `:suppress` 与设置界面使用）。
    pub fn set_tier(&mut self, word: &str, tier: Tier) {
        self.store.set_tier(word, tier);
        self.cache = None;
    }

    /// 结束当前组合（Esc / 回车 / 焦点丢失）。
    pub fn reset(&mut self) {
        self.cache = None;
        self.composed = false;
    }

    /// 本次编辑会话里是否打出过东西。
    pub fn has_composed(&self) -> bool {
        self.composed
    }

    pub fn config(&self) -> RankingConfig {
        self.cfg
    }

    pub fn set_config(&mut self, cfg: RankingConfig) {
        self.cfg = cfg.sanitized();
        self.cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::MemoryStore;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mini")
    }

    fn engine() -> Engine<MemoryStore> {
        let scheme = Scheme::load(&fixture()).expect("fixture 应可装载");
        Engine::new(scheme, MemoryStore::new(), RankingConfig::default())
    }

    #[test]
    fn order_is_locked_within_one_composition() {
        let mut eng = engine();
        let first: Vec<String> = eng
            .candidates("vbgd", 1_000)
            .iter()
            .map(|c| c.word.clone())
            .collect();
        // 模拟在别处记录了使用（另一窗口、另一应用），但当前缓冲区未变
        eng.store.record("蒿", 1_000);
        let again: Vec<String> = eng
            .candidates("vbgd", 1_000)
            .iter()
            .map(|c| c.word.clone())
            .collect();
        assert_eq!(first, again, "组合期内顺序不得变动");
    }

    #[test]
    fn buffer_change_recomputes() {
        let mut eng = engine();
        assert_eq!(eng.candidates("vbgd", 1_000).len(), 4);
        assert_eq!(
            eng.candidates("vbg", 1_000).len(),
            1,
            "3 码全码只有一个候选"
        );
        assert_eq!(eng.candidates("v", 1_000).len(), 1, "1 码只命中一级简码");
    }

    #[test]
    fn select_records_and_ends_composition() {
        let mut eng = engine();
        let picked = eng.select("vbgd", 0, 1_000).expect("应有候选");
        assert_eq!(picked.word, "号");
        assert_eq!(eng.store().usage(&picked.word).n, 1);
        assert!(!eng.has_composed());
        // 组合结束后重新计算：被选中的词在下一轮拿到学习分
        let after = eng.candidates("vbgd", 1_000);
        assert!(after.iter().any(|c| c.word == picked.word && c.n == 1));
    }

    #[test]
    fn suppressed_is_filtered_and_pinned_leads() {
        let mut eng = engine();
        let all: Vec<String> = eng
            .candidates("vbgd", 1_000)
            .iter()
            .map(|c| c.word.clone())
            .collect();
        let victim = all[1].clone();
        eng.set_tier(&victim, Tier::Suppressed);
        let after = eng.candidates("vbgd", 1_000);
        assert!(
            !after.iter().any(|c| c.word == victim),
            "被抑制的词不得出现"
        );

        let target = after.last().unwrap().word.clone();
        eng.set_tier(&target, Tier::Pinned);
        let ranked = eng.candidates("vbgd", 1_000);
        assert_eq!(ranked[0].word, target, "固顶词必须排首位");
    }
}
