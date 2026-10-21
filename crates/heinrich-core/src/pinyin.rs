//! 混输裁决（§6.1）与候选组装（§6.2）。
//!
//! 组合串是一串不区分语言的字母，判定它「这一串到底是五笔还是拼音」是 M3 的核心。
//! 本模块实现的是**单段特化版**的 DP：
//!
//! ```text
//! 五笔质量 = 精确命中 1.0（D45：不做前缀兜底，所以没有 0.5 那一档）
//! 拼音质量 = 整串可切分为合法音节 1.0 / 未打完的前缀 0.5
//! 同分 → 判拼音（§6.1：拼音是「我不会拆」时的退路，错判代价低）
//! ```
//!
//! **为什么先做单段**：真正的多段输入（`haovb` = 拼音段 + 五笔段，组合出「好我」）
//! 需要各段候选做笛卡尔积，而它的实际使用频率极低——用户要么在打五笔、要么在打
//! 拼音，中途切换是少数。单段判定已经覆盖「纯拼音」「纯五笔」「`hao` 这类两种都
//! 成立的歧义」，也就是绝大多数击键。多段留到 §6.1 的完整 DP 落地时补。
//!
//! 判定之外的排序一律交给 [`crate::rank`]，拼音与五笔共用同一份学习数据。

use heinrich_scheme::{PinyinLexicon, Scheme};

use crate::rank::{lookup, rank_inputs, Candidate, LookupMode, RankInput, RankingConfig};
use crate::usage::UsageStore;

/// 当前首屏候选来自哪种语言。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Wubi,
    Pinyin,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Wubi => "五笔",
            Source::Pinyin => "拼音",
        }
    }
}

/// 一次混输查询的结果。
#[derive(Debug)]
pub struct Mixed {
    /// 判定为首屏的语言
    pub source: Source,
    /// 首屏候选（§6.2：只放当前判定语言的）
    pub primary: Vec<Candidate>,
    /// 折叠区候选——另一种语言的，排在首屏之后
    pub folded: Vec<Candidate>,
}

impl Mixed {
    pub fn is_empty(&self) -> bool {
        self.primary.is_empty() && self.folded.is_empty()
    }

    /// 首屏 + 折叠区的完整序列（候选窗按这个顺序显示）。
    pub fn into_all(self) -> Vec<Candidate> {
        let mut out = self.primary;
        out.extend(self.folded);
        out
    }
}

/// 拼音候选的上限。拼音前缀区间可能有几千条，取够翻几页的量就够，
/// 剩下的按需再查没有意义——候选窗一次也只显示 5 条。
const PINYIN_LIMIT: usize = 60;

/// 五笔侧查询：走既有 `lookup`（精确优先，D45 不做前缀兜底）。
fn wubi_candidates<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    input: &str,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> (Vec<Candidate>, bool) {
    let (cands, mode) = lookup(scheme, input, store, now, cfg);
    let exact = mode == LookupMode::Exact && !cands.is_empty();
    (cands, exact)
}

/// 拼音侧查询：匹配下标 → 复用同一套排序。
///
/// **精确区与前缀区必须分开排序，不能混在一起按权重排。** 混排的后果是
/// `hao` 的候选变成「好、号、好吧、好不好、好啊、好不容易、浩、豪…」——
/// 「好吧」「好不好」的语料权重高于「浩」「豪」，于是把同音字挤到了后面。
/// 但打 `hao` 的人要的是同音字，词组是「还得继续打」的另一类东西。
fn pinyin_candidates<S: UsageStore + ?Sized>(
    lex: &PinyinLexicon,
    input: &str,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> Vec<Candidate> {
    let hits = lex.match_indices(input, PINYIN_LIMIT);
    if hits.is_empty() {
        return Vec::new();
    }
    let mut exact: Vec<RankInput<'_>> = Vec::new();
    let mut prefix: Vec<RankInput<'_>> = Vec::new();
    for &(idx, is_exact) in &hits {
        let e = lex.entry(idx);
        let item = RankInput {
            word: &e.word,
            code: &e.pinyin,
            w_norm: e.w_norm,
            kind_label: "拼音",
        };
        if is_exact {
            exact.push(item);
        } else {
            prefix.push(item);
        }
    }
    let mut out = rank_inputs(&exact, store, now, cfg);
    out.extend(rank_inputs(&prefix, store, now, cfg));
    out
}

/// 判定 + 查候选。
///
/// `pinyin` 为 `None` 时退化为纯五笔（M1/M2 的行为），方便没有拼音数据的环境
/// 与既有单测继续工作。
pub fn query<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    pinyin: Option<&PinyinLexicon>,
    input: &str,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> Mixed {
    if input.is_empty() {
        return Mixed {
            source: Source::Wubi,
            primary: Vec::new(),
            folded: Vec::new(),
        };
    }

    let (wubi, wubi_exact) = wubi_candidates(scheme, input, store, now, cfg);

    let Some(lex) = pinyin else {
        return Mixed {
            source: Source::Wubi,
            primary: wubi,
            folded: Vec::new(),
        };
    };

    // 拼音的「可切分」与「有候选」是两件事：
    //   `zhuang` 可切分且一定有候选 → 质量 1.0
    //   `zhuangx` 不可切分（末尾多一个字母），但可能有以它开头的词组前缀 → 0.5
    // 这一档不能省：否则「打了一半的拼音」会和「完整的五笔码」同分，
    // 而 §6.1 的同分规则会把五笔码判成拼音。
    let splittable = lex.can_split(input);
    let py = pinyin_candidates(lex, input, store, now, cfg);
    let pinyin_q = if py.is_empty() {
        0.0_f32
    } else if splittable {
        1.0
    } else {
        0.5
    };
    let wubi_q = if wubi_exact { 1.0_f32 } else { 0.0 };

    // ---- 判定（§6.1 的段质量比较） ----
    // 同分判拼音：`hao` 既是合法五笔码（h/a/o 都是字根键）也是合法拼音，
    // 判成拼音能照顾「不会拆就打拼音」这个更高频的场景；而五笔候选折叠后
    // 仍然可点选，不会真的丢掉。
    let source = if pinyin_q >= wubi_q && pinyin_q > 0.0 {
        Source::Pinyin
    } else if wubi_q > 0.0 {
        Source::Wubi
    } else {
        return Mixed {
            source: Source::Wubi,
            primary: Vec::new(),
            folded: Vec::new(),
        };
    };

    match source {
        Source::Pinyin => Mixed {
            source,
            primary: py,
            folded: wubi,
        },
        Source::Wubi => Mixed {
            source,
            primary: wubi,
            folded: py,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::MemoryStore;
    use std::path::PathBuf;

    /// 五笔夹具在本 crate 下，拼音夹具在 `heinrich-scheme` 下（`PinyinLexicon` 在那里，
    /// 夹具跟着它走，避免同一个夹具维护两份）。
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name)
    }

    fn scheme() -> Scheme {
        Scheme::load(&fixture("tests/fixtures/mini")).expect("五笔夹具应可装载")
    }

    fn pinyin() -> PinyinLexicon {
        PinyinLexicon::load(&fixture("../heinrich-scheme/tests/fixtures/pinyin-mini"))
            .expect("拼音夹具应可装载")
    }

    fn words(c: &[Candidate]) -> Vec<String> {
        c.iter().map(|x| x.word.clone()).collect()
    }

    #[test]
    fn pure_wubi_when_not_splittable_as_pinyin() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        // vbgd 里的 v 不是合法音节起始 ⇒ 只能是五笔
        let r = query(&s, Some(&lex), "vbgd", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Wubi);
        assert!(!r.primary.is_empty());
        assert!(r.folded.is_empty());
    }

    #[test]
    fn ambiguous_input_goes_to_pinyin() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        // hao：拼音合法。它是否同时是合法五笔码取决于夹具，但无论如何
        // 「拼音有候选」时应当判拼音（同分原则的落点）。
        let r = query(&s, Some(&lex), "hao", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Pinyin);
        assert_eq!(r.primary.first().map(|c| c.word.as_str()), Some("好"));
    }

    #[test]
    fn unfinished_pinyin_still_shows_candidates() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "haop", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Pinyin);
        assert!(words(&r.primary).contains(&"好评".to_string()));
    }

    #[test]
    fn nothing_matches_returns_empty() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "zzz", &store, 1_000, &cfg);
        assert!(r.is_empty(), "zzz 既不是五笔码也不是拼音音节");
    }

    #[test]
    fn without_pinyin_lexicon_it_degrades_to_wubi() {
        let s = scheme();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, None, "vbgd", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Wubi);
        assert!(!r.primary.is_empty());
    }

    #[test]
    fn pinyin_candidates_share_learning_with_wubi() {
        let s = scheme();
        let lex = pinyin();
        let mut store = MemoryStore::new();
        let cfg = RankingConfig::default();
        // 用拼音连打三次「浩」，它应当超过同音的「号」
        for _ in 0..3 {
            store.record("浩", 1_000);
        }
        let r = query(&s, Some(&lex), "hao", &store, 1_000, &cfg);
        let order = words(&r.primary);
        let hao_pos = order.iter().position(|w| w == "浩").expect("浩 应在候选里");
        let hao2_pos = order.iter().position(|w| w == "号").expect("号 应在候选里");
        assert!(
            hao_pos < hao2_pos || r.primary[hao_pos].throne_held,
            "学习分应当让「浩」上浮（被首位保护压住也是一种合法结果）：{order:?}"
        );
    }

    #[test]
    fn empty_input_is_empty() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "", &store, 1_000, &cfg);
        assert!(r.is_empty());
    }
}
