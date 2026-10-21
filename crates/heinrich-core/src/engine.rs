//! 组合期状态机：一次「开始打字 → 上屏」期间的候选顺序必须锁定（§5.4）。
//!
//! 为什么需要锁定：候选是按综合分实时算的，若某次选择立刻改变分数，同一串
//! 按键在翻页过程中会「跳位」，用户的手指会落到错的候选上。因此同一缓冲区
//! 的排序结果被缓存，直到缓冲区变化或组合结束才重算。
//!
//! M3 起这里还承担**混输判定**：同一个缓冲区要么判五笔、要么判拼音，判定结果
//! 与候选一起被锁定。否则用户看到的候选会在「拼音区」和「五笔区」之间跳，
//! 比不锁定更糟。

use heinrich_scheme::{PinyinLexicon, Scheme};

use crate::pinyin::{self, Mixed, Source};
use crate::rank::{Candidate, LookupMode, RankingConfig};
use crate::usage::{Tier, UsageStore};

/// 一个缓冲区的查询结果快照。
struct Cached {
    buffer: String,
    mixed: Mixed,
    /// `primary` + `folded` 的扁平副本。留一份是为了让既有调用方
    /// （server / TSF 的「按序号取候选」）不必关心折叠区的存在——
    /// 它们拿到的仍是「第 n 条候选」这个一维序列。
    flat: Vec<Candidate>,
}

/// 输入法引擎：持有一份方案（可选一份拼音词库）、一份学习数据与排序参数。
pub struct Engine<S: UsageStore> {
    scheme: Scheme,
    pinyin: Option<PinyinLexicon>,
    store: S,
    cfg: RankingConfig,
    cache: Option<Cached>,
    /// 本次组合是否已经产生过候选（用于统计「首屏」等场景）
    composed: bool,
}

impl<S: UsageStore> Engine<S> {
    pub fn new(scheme: Scheme, store: S, cfg: RankingConfig) -> Self {
        Self {
            scheme,
            pinyin: None,
            store,
            cfg,
            cache: None,
            composed: false,
        }
    }

    /// 挂上拼音词库，此后查询走混输判定（§6.1）。
    ///
    /// 做成装配式而不是 `new` 的参数：没有拼音数据的场合（既有单测、只装五笔的
    /// 环境）应当原样工作，不该被迫构造一个空词库。
    pub fn with_pinyin(mut self, lex: PinyinLexicon) -> Self {
        self.pinyin = Some(lex);
        self.cache = None;
        self
    }

    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    pub fn pinyin(&self) -> Option<&PinyinLexicon> {
        self.pinyin.as_ref()
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    fn ensure(&mut self, buffer: &str, now: i64) {
        let hit = matches!(&self.cache, Some(c) if c.buffer == buffer);
        if hit {
            return;
        }
        let mixed = pinyin::query(
            &self.scheme,
            self.pinyin.as_ref(),
            buffer,
            &self.store,
            now,
            &self.cfg,
        );
        let mut flat = Vec::with_capacity(mixed.primary.len() + mixed.folded.len());
        flat.extend(mixed.primary.iter().cloned());
        flat.extend(mixed.folded.iter().cloned());
        self.cache = Some(Cached {
            buffer: buffer.to_string(),
            mixed,
            flat,
        });
        self.composed |= !buffer.is_empty();
    }

    /// 取当前缓冲区的候选；同一缓冲区重复调用返回同一顺序（顺序锁定）。
    ///
    /// 返回值是「首屏 + 折叠区」的一维序列——需要区分两种语言的调用方用
    /// [`Self::mixed`]。
    pub fn candidates(&mut self, buffer: &str, now: i64) -> &[Candidate] {
        self.ensure(buffer, now);
        self.cache
            .as_ref()
            .map(|c| c.flat.as_slice())
            .unwrap_or(&[])
    }

    /// 取完整的混输结果（含判定来源与折叠区）。
    pub fn mixed(&mut self, buffer: &str, now: i64) -> &Mixed {
        self.ensure(buffer, now);
        // `ensure` 之后 cache 必然存在
        &self.cache.as_ref().expect("ensure 已填充缓存").mixed
    }

    /// 当前候选集的判定来源。
    pub fn source(&self) -> Option<Source> {
        self.cache.as_ref().map(|c| c.mixed.source)
    }

    /// 当前候选集的来源（精确命中 / 空码回退前缀）。
    ///
    /// 混输下这个值不再有决策作用（D45 已取消五笔前缀兜底），保留是为了让
    /// 既有调用方与新调用方共用同一个入口。
    pub fn lookup_mode(&self) -> Option<LookupMode> {
        self.cache.as_ref().map(|_| LookupMode::Exact)
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

    fn pinyin_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../heinrich-scheme/tests/fixtures/pinyin-mini")
    }

    fn engine() -> Engine<MemoryStore> {
        let scheme = Scheme::load(&fixture()).expect("fixture 应可装载");
        Engine::new(scheme, MemoryStore::new(), RankingConfig::default())
    }

    fn mixed_engine() -> Engine<MemoryStore> {
        let scheme = Scheme::load(&fixture()).expect("fixture 应可装载");
        let lex = PinyinLexicon::load(&pinyin_fixture()).expect("拼音夹具应可装载");
        Engine::new(scheme, MemoryStore::new(), RankingConfig::default()).with_pinyin(lex)
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

    /// D48 之后 `hao` 的判定变回五笔：它是合法五笔三级简码（虚、好），
    /// 拼音候选排在五笔之后而不是顶上去。
    #[test]
    fn mixed_engine_routes_pinyin_and_keeps_decision_stable() {
        let mut eng = mixed_engine();
        let m = eng.mixed("hao", 1_000);
        assert_eq!(m.source, Source::Wubi);
        assert_eq!(m.primary.first().map(|c| c.word.as_str()), Some("虚"));
        assert_eq!(
            m.folded.first().map(|c| c.word.as_str()),
            Some("号"),
            "拼音候选接在五笔之后（「好」已在五笔段，不再重复）"
        );
        // 判定结果在缓冲区内锁定：换个时刻问，来源不变
        assert_eq!(eng.source(), Some(Source::Wubi));
        // 五笔码仍走五笔
        assert_eq!(eng.mixed("vbgd", 1_000).source, Source::Wubi);
        // 纯拼音串（五笔没有这个词条）走拼音
        assert_eq!(eng.mixed("xian", 1_000).source, Source::Pinyin);
    }

    #[test]
    fn mixed_engine_candidates_are_flat_primary_then_folded() {
        let mut eng = mixed_engine();
        let m = eng.mixed("hao", 1_000);
        let primary: Vec<String> = m.primary.iter().map(|c| c.word.clone()).collect();
        let folded: Vec<String> = m.folded.iter().map(|c| c.word.clone()).collect();
        let flat = eng.candidates("hao", 1_000);
        let mut want = primary;
        want.extend(folded);
        let got: Vec<String> = flat.iter().map(|c| c.word.clone()).collect();
        assert_eq!(got, want, "扁平序列必须是首屏在前、折叠区在后");
    }

    #[test]
    fn empty_buffer_has_no_source() {
        let mut eng = mixed_engine();
        assert!(eng.candidates("", 1_000).is_empty());
        // 空缓冲区仍会建立缓存条目，来源退化到五笔，但候选为空
        assert_eq!(eng.mixed("", 1_000).source, Source::Wubi);
    }
}
