//! 混输裁决（§6.1）与候选组装（§6.2）。
//!
//! 组合串是一串不区分语言的字母，判定它「这一串到底是五笔还是拼音」是 M3 的核心。
//! 本模块实现的是**单段特化版**的 DP：
//!
//! ```text
//! ① 串里有 `'`               → 纯拼音（五笔一条都不出）
//! ② 五笔精确命中             → 五笔在主段，拼音候补在后面（去掉重复的）
//! ③ 五笔没命中、拼音有候选    → 拼音在主段
//! ④ 两边都没有               → 空（不出窗）
//! ```
//!
//! **五笔优先（D48）**：这是一个五笔输入法，拼音是「不会拆」时的退路。所以
//! `hao` 必须先把五笔的「虚」摆在第一位，而不是让 61 条同音字把「虚」埋到第 13 页。
//! 想吃拼音的候选，用户看得到（就在五笔候选之后），想**独占**拼音则打 `hao'`。
//!
//! D46 原本写的是「同分判拼音」，那个规则在 M3.2 的单段实现里被真机否掉了：
//! 五笔码绝大多数是 3–4 码，而 3–4 个字母几乎总能切成一串合法音节（`hao`、`ni`、
//! `xian` 全都切得开），于是「同分判拼音」实际等价于「只要长得像拼音就判拼音」。
//!
//! **为什么只做单段**：真正的多段输入（`haovb` = 拼音段 + 五笔段，组合出「好我」）
//! 需要各段候选做笛卡尔积，而它的实际使用频率极低——用户要么在打五笔、要么在打
//! 拼音，中途切换是少数。多段留到 §6.1 的完整 DP 落地时补。
//!
//! 判定之外的排序一律交给 [`crate::rank`]，拼音与五笔共用同一份学习数据。

use heinrich_scheme::{PinyinLexicon, Scheme};

use crate::rank::{lookup, rank_inputs, Candidate, LookupMode, RankInput, RankingConfig};
use crate::usage::UsageStore;

/// 显式拼音标记（§6.5 / D11 / D48）。
///
/// 组合串里出现它，就把**整串**钉死成拼音：五笔侧一条都不出。这是「我明知这串
/// 也是合法五笔码，但现在要的是拼音」的唯一表达方式——没有它，`hao` 永远先出
/// 「虚」，想打「好」就得翻过一页同音字。
///
/// 标记本身不参与查询：`hao'` 与 `hao` 查的是同一份拼音数据，候选必须一模一样，
/// 否则用户会觉得「加了个标记反而查不到了」。
pub const MARK: char = '\'';

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
    /// **主段**候选来自哪种语言（候选窗据此画分界：主段正常色，折叠区淡一档）
    pub source: Source,
    /// 主段候选：当前判定语言的全部命中，排在候选列表最前面
    pub primary: Vec<Candidate>,
    /// 折叠区候选——另一种语言的，排在主段之后（D48）
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

/// 拼音候选的默认上限。拼音前缀区间可能有几千条，取够翻几页的量就够，
/// 剩下的按需再查没有意义——候选窗一次也只显示 5 条。
///
/// 协议一次最多带 32 条（`heinrich-proto` 的 `MAX_ENTRIES`），所以调用方通常会传更小的值；
/// 这个默认值是给 CLI 这类没有协议上限的场景用的。
const PINYIN_LIMIT: usize = 60;

/// 五笔侧查询：走既有 `lookup`，并且**只认精确码长**（D45）。
///
/// 前缀兜底必须在这里丢掉：`abc` 没有精确词条，`lookup` 会回退到前缀，给出
/// 「abce 节能」「abci 藂」——它们只是码前缀撞了，与用户打的三个键毫无关系。
/// 五笔是定长编码，「还没打完」不等于「空码」。想查某个前缀下有哪些词，走 `z`
/// 学习键（M3.5）。
fn wubi_candidates<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    input: &str,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> Vec<Candidate> {
    let (cands, mode) = lookup(scheme, input, store, now, cfg);
    if mode != LookupMode::Exact {
        return Vec::new();
    }
    cands
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
    limit: usize,
) -> Vec<Candidate> {
    let hits = lex.match_indices(input, limit);
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
            // 码提示（§6.3）读这一列：拼音候选的五笔全码在装载时由
            // `PinyinLexicon::attach_wubi_codes` 填好；没填上（五笔词库没有这个词）
            // 就是空串，chip 不画。
            wubi: &e.wubi,
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
    query_with_limit(scheme, pinyin, input, store, now, cfg, PINYIN_LIMIT)
}

/// 带条数上限的版本：server 用它把结果压进协议的 `MAX_ENTRIES`。
pub fn query_with_limit<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    pinyin: Option<&PinyinLexicon>,
    input: &str,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
    limit: usize,
) -> Mixed {
    let nothing = Mixed {
        source: Source::Wubi,
        primary: Vec::new(),
        folded: Vec::new(),
    };
    if input.is_empty() {
        return nothing;
    }

    // `'` 是显式拼音标记（D48）。标记本身不参与查询，两种写法查同一份数据。
    let marked = input.contains(MARK);
    let code: String = if marked {
        input.replace(MARK, "")
    } else {
        input.to_string()
    };
    if code.is_empty() {
        return nothing;
    }

    // 带标记时五笔侧**一条都不查**：这是用户手写的「跳过五笔」，比任何启发式都准。
    let wubi = if marked {
        Vec::new()
    } else {
        wubi_candidates(scheme, &code, store, now, cfg)
    };

    let Some(lex) = pinyin else {
        return Mixed {
            source: Source::Wubi,
            primary: wubi,
            folded: Vec::new(),
        };
    };

    let py = pinyin_candidates(lex, &code, store, now, cfg, limit);

    // ---- 主段选择（D48：五笔优先） ----
    if marked {
        return Mixed {
            source: Source::Pinyin,
            primary: py,
            folded: Vec::new(),
        };
    }
    if !wubi.is_empty() {
        // 拼音里那些五笔已经给出的词不再重复列一遍：同一个词在同一串码下出现两次，
        // 用户会以为列表坏了，而且点第 2 条与点第 1 条得到同一个字。
        let folded = without_seen(py, &wubi);
        return Mixed {
            source: Source::Wubi,
            primary: wubi,
            folded,
        };
    }
    if !py.is_empty() {
        return Mixed {
            source: Source::Pinyin,
            primary: py,
            folded: Vec::new(),
        };
    }
    nothing
}

/// 从 `cands` 里剔掉 `seen` 已经有的词，保持原顺序。
fn without_seen(cands: Vec<Candidate>, seen: &[Candidate]) -> Vec<Candidate> {
    if cands.is_empty() || seen.is_empty() {
        return cands;
    }
    let taken: std::collections::HashSet<&str> = seen.iter().map(|c| c.word.as_str()).collect();
    cands
        .into_iter()
        .filter(|c| !taken.contains(c.word.as_str()))
        .collect()
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

    /// D48：同一个码两边都能命中时**五笔先出**。`hao` 在夹具里既是五笔三级简码
    /// （虚、好）又是合法拼音，用户要的是五笔那两个——拼音候选跟在后面。
    #[test]
    fn ambiguous_input_prefers_wubi() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "hao", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Wubi);
        assert_eq!(words(&r.primary), vec!["虚", "好"], "五笔段：语料权重序");
        // 折叠区先是同音字（精确区），再是以 `hao` 开头的词组（前缀区）
        assert_eq!(
            words(&r.folded),
            vec!["号", "浩", "好评", "好吃"],
            "拼音段紧跟其后，重复的「好」已剔掉"
        );
    }

    /// 同一个词不能既在五笔段又在拼音段出现：同一串码下点第 1 条与点第 3 条
    /// 得到同一个字，用户会以为列表坏了。
    #[test]
    fn folded_drops_words_already_in_primary() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "hao", &store, 1_000, &cfg);
        assert!(r.primary.iter().any(|c| c.word == "好"), "五笔段有「好」");
        assert!(
            !r.folded.iter().any(|c| c.word == "好"),
            "拼音段不该再排一次「好」：{:?}",
            words(&r.folded)
        );
        // 总条数 = 五笔 2 + 拼音精确 2（号、浩）+ 前缀 2（好评、好吃），「好」只出现一次
        assert_eq!(r.into_all().len(), 6);
    }

    /// D48：带 `'` 就是「跳过五笔」，同一串码的候选必须**只剩拼音**。
    #[test]
    fn apostrophe_forces_pure_pinyin() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "hao'", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Pinyin);
        assert_eq!(r.primary.first().map(|c| c.word.as_str()), Some("好"));
        assert!(
            !words(&r.primary).contains(&"虚".to_string()),
            "带标记时五笔侧一条都不该出"
        );
        assert!(r.folded.is_empty());
    }

    /// 标记只是「切换语言」，不改变拼音侧查到的东西：`hao'` 与 `hao` 的拼音候选相同。
    #[test]
    fn mark_does_not_change_what_pinyin_finds() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let plain = query(&s, Some(&lex), "hao", &store, 1_000, &cfg);
        let marked = query(&s, Some(&lex), "hao'", &store, 1_000, &cfg);
        // 唯一的差别是「好」：没标记时它已在五笔段出现过，拼音段把它剔掉了；
        // 带标记时五笔不参与，它照常排在拼音第一位。其余顺序必须一致。
        assert_eq!(words(&marked.primary)[1..], words(&plain.folded)[..]);
    }

    /// 五笔没命中时拼音接管主段（`xian` 在夹具里不是五笔码）。
    #[test]
    fn pinyin_takes_over_when_wubi_misses() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "xian", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Pinyin);
        assert_eq!(r.primary.first().map(|c| c.word.as_str()), Some("先"));
        assert!(r.folded.is_empty());
    }

    /// 两边不重叠时不能误删：五笔 `ni` → 悄，拼音 `ni` → 你，两条都在。
    #[test]
    fn folded_keeps_words_absent_from_primary() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "ni", &store, 1_000, &cfg);
        assert_eq!(r.source, Source::Wubi);
        assert_eq!(words(&r.primary), vec!["悄"]);
        assert_eq!(
            words(&r.folded),
            vec!["你", "你好"],
            "精确区在前、前缀区在后"
        );
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

    /// D45 在混输里同样生效：五笔不做前缀兜底。
    ///
    /// 这条曾经被真机上的一段误改放活过（`abc` 出「abce 节能」一屏），
    /// server 自测的 `abc → 无命中` 抓住了它。
    #[test]
    fn wubi_prefix_fallback_stays_off() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        // `ntc` 不是任何词条的码，但它是 `ntcy`（发）的前缀
        let r = query(&s, Some(&lex), "ntc", &store, 1_000, &cfg);
        assert!(
            r.is_empty(),
            "五笔不做前缀兜底（D45）：{:?}",
            words(&r.into_all())
        );
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

    /// 学习数据是**一份**：在拼音下打惯了「浩」，它在拼音段里就上浮。
    /// 这里看 `folded`——`hao` 的五笔段占主段，拼音排在后面（D48）。
    #[test]
    fn pinyin_candidates_share_learning_with_wubi() {
        let s = scheme();
        let lex = pinyin();
        let mut store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let base = words(&query(&s, Some(&lex), "hao", &store, 1_000, &cfg).folded);
        assert_eq!(&base[..2], ["号", "浩"], "基线：语料权重序");

        // 连打三次「浩」，它应当超过同音的「号」
        for _ in 0..3 {
            store.record("浩", 1_000);
        }
        let r = query(&s, Some(&lex), "hao", &store, 1_000, &cfg);
        let order = words(&r.folded);
        let hao_pos = order.iter().position(|w| w == "浩").expect("浩 应在候选里");
        let hao2_pos = order.iter().position(|w| w == "号").expect("号 应在候选里");
        assert!(
            hao_pos < hao2_pos || r.folded[hao_pos].throne_held,
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
