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
//! **多段（§6.5，M3.5）**：`hao'vb` = 拼音段「好」+ 五笔段。分段的边界是用户用
//! `'` **写出来**的，所以这里不需要 §6.1 的 DP——DP 是用来猜边界的（`haovb` 这种
//! 没标记的串），而显式标记已经把答案给了。真正需要的完整 DP（自动分段）仍然不做：
//! 用户想切换语言时本来就要按一个 `'`，为一个已经表达清楚的意图去猜，只会误判。
//!
//! **通配（M3.5）**：`vbz` = 「`vb` 开头、末码待定」，由 [`wildcard_stem`] 识别。
//! 只认**末位**，理由见那个函数的文档。
//!
//! 判定之外的排序一律交给 [`crate::rank`]，拼音与五笔共用同一份学习数据。

use heinrich_scheme::{PinyinLexicon, Scheme};

use crate::rank::{
    lookup, rank_indices, rank_inputs, Candidate, ChipHint, LookupMode, RankInput, RankingConfig,
};
use crate::usage::{Tier, UsageStore};

/// 显式拼音标记（§6.5 / D11 / D48）。
///
/// 组合串里出现它，就把**整串**钉死成拼音：五笔侧一条都不出。这是「我明知这串
/// 也是合法五笔码，但现在要的是拼音」的唯一表达方式——没有它，`hao` 永远先出
/// 「虚」，想打「好」就得翻过一页同音字。
///
/// 标记本身不参与查询：`hao'` 与 `hao` 查的是同一份拼音数据，候选必须一模一样，
/// 否则用户会觉得「加了个标记反而查不到了」。
pub const MARK: char = '\'';

/// 万能学习键（§6.5 / M3.5）：出现在**组合串末位**时表示「末码待定」。
///
/// 只认末位，是因为它同时是拼音的声母。拼音里 `z` 只做声母、只出现在音节开头，
/// 但连续拼音（`niza` = 你咋）的第二个音节正以它开头——若允许出现在任意位置，
/// 打到 `niz` 那一刻就会被当成通配符，候选窗整片跳成五笔前缀。只认末位把冲突
/// 压到「用户还在往下打的那一瞬间」：`niz` 会闪一下五笔前缀，按下一个字母就好了。
pub const WILDCARD: char = 'z';

/// 多段标记（§6.5）最多支持几段。
///
/// 2 段覆盖绝大多数真实场景（先拼音确认一个字、再切五笔打下一个）。
/// 3 段以上按 5³ 涨到 125 条组合，翻页翻不过来，且用户真正想要的是「分两次打」。
const MAX_SEGMENTS: usize = 2;

/// 每一段最多取几条候选参与组合。
///
/// D64 之后组合结果要过**真词过滤**，大部分组合会被滤掉，池子必须比一页大——
/// 取 16 是给过滤器留足料：拼音段的精确区（同音单字）通常有一二十条，
/// 16 条够覆盖到权重较低的常用字，过滤后仍能凑满一页真词。
///
/// **为什么从 16 提到 32**（M3 总验收实测）：`wu` 的同音候选里「悟」排在**第 17**，
/// 于是 `wu'ntgg`（悟性）明明是真词却出不了窗——差一条就是全盘皆输。32 让池子留出
/// 一倍余量（对齐到 32 而不是 17，是给「再来一个比 17 更靠后的字」留的缓冲）。
/// 组合上限从 256 涨到 1024 次 `has_word`，微秒级，按键感知不到。
const SEGMENT_LIMIT: usize = 32;

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
    /// **强制显示码提示**，内容取 `Candidate.code`（不是 `wubi`）。
    ///
    /// 目前只有**通配**（`vbz`）必须显示：这一段本来就是「我要找 `vb` 开头的字」，
    /// 看不清每条字自己的码，就等于看了一屏无关的字。
    ///
    /// 多段（`hao'vb`）**不再走这一位**（D64）：它的提示是**词中 chip**（挂在
    /// 拼音段字后面、内容是该字的五笔全码），由 `Candidate.chip` 携带——挂词尾
    /// 的 `hao'vb` 会被误读成整个词的五笔码（「好吃」的全码明明是 `vbkt`）。
    ///
    /// 其余情况这一位是 `false`，码提示仍按 §6.3 的「拼音段给码、五笔段不给」走。
    pub show_codes: bool,
}

impl Mixed {
    /// 空结果（不出窗）。
    fn none(source: Source) -> Self {
        Self {
            source,
            primary: Vec::new(),
            folded: Vec::new(),
            show_codes: false,
        }
    }

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

/// 通配模式：`vbz` → `vb`。不是通配就返回 `None`。
///
/// 三个约束，每个都对应一种「不该通配」的情形：
///
/// - **末位是 `z`**：见 [`WILDCARD`]。
/// - **前缀非空且 ≤ 3 位**：空前缀（单独一个 `z`）没有「待定」可言；4 位前缀加 `z`
///   等于问「第 5 码是什么」，而五笔没有第 5 码（单字全码 3 或 4 码，词组一律 4 码）。
/// - **前缀全在 `a`–`y`**：`z` 不在五笔字根键里，`vzz` 这种串不是通配，是打错了。
fn wildcard_stem(input: &str) -> Option<&str> {
    let stem = input.strip_suffix(WILDCARD)?;
    if stem.is_empty() || stem.len() > 3 {
        return None;
    }
    if !stem.bytes().all(|b| (b'a'..=b'y').contains(&b)) {
        return None;
    }
    Some(stem)
}

/// 按 `'` 切分多段（§6.5）。返回每段与「这一段是不是拼音」。
///
/// `'` 标记它**前面**那一段是拼音；末尾的 `'` 标记最后一段也是拼音。于是：
///
/// | 输入 | 段 | 含义 |
/// |---|---|---|
/// | `hao'vb` | `hao` 拼音 + `vb` 自动 | 拼音打「好」，再切五笔打 |
/// | `hao'ni'` | 两段都拼音 | 两段都要拼音 |
/// | `hao'` | 单段 | 老写法：整串纯拼音 |
///
/// 返回 `None` 表示「不是多段语法」，交给单段逻辑：没有 `'`、只有一段（`hao'`）、
/// 有空段（`'hao` / `hao''vb`）、超过 [`MAX_SEGMENTS`] 段。
fn split_segments(input: &str) -> Option<Vec<(&str, bool)>> {
    if !input.contains(MARK) {
        return None;
    }
    let mut parts: Vec<&str> = input.split(MARK).collect();
    let trailing = input.ends_with(MARK);
    if trailing {
        parts.pop();
    }
    if parts.len() < 2 || parts.len() > MAX_SEGMENTS || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let n = parts.len();
    Some(
        parts
            .into_iter()
            .enumerate()
            .map(|(i, p)| (p, i + 1 < n || trailing))
            .collect(),
    )
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
    let nothing = Mixed::none(Source::Wubi);
    if input.is_empty() {
        return nothing;
    }

    // ---- 多段（§6.5 / M3.5）----
    // 边界是用户用 `'` 写出来的，不需要猜，所以这里没有 DP。
    if let Some(segs) = split_segments(input) {
        return segment_query(scheme, pinyin, &segs, store, now, cfg, limit);
    }

    // ---- 通配（M3.5）----
    // 必须排在单段判定之前：`vbz` 里的 `z` 不属于任何拼音音节序列，
    // 交给拼音去切只会得到一堆以 `vb` 开头的无关词。
    if let Some(stem) = wildcard_stem(input) {
        return wildcard_query(scheme, pinyin, stem, store, now, cfg, limit);
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
            show_codes: false,
        };
    };

    let py = pinyin_candidates(lex, &code, store, now, cfg, limit);

    // ---- 主段选择（D48：五笔优先） ----
    if marked {
        return Mixed {
            source: Source::Pinyin,
            primary: py,
            ..Mixed::none(Source::Pinyin)
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
            show_codes: false,
        };
    }
    if !py.is_empty() {
        return Mixed {
            source: Source::Pinyin,
            primary: py,
            ..Mixed::none(Source::Pinyin)
        };
    }
    nothing
}

/// 通配查询：`stem` 开头的全部词，带各自的五笔码。
///
/// 主段永远是五笔——会按 `z` 的人都在打五笔；拼音前缀放在折叠区，是为了照顾
/// 「连续拼音刚好停在 `z` 上」（`niza` 打到 `niz`）时不至于让候选窗整片变成五笔。
fn wildcard_query<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    pinyin: Option<&PinyinLexicon>,
    stem: &str,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
    limit: usize,
) -> Mixed {
    let indices = scheme.match_indices(stem);
    if indices.is_empty() {
        return Mixed::none(Source::Wubi);
    }
    let ranked = rank_indices(scheme, indices, store, now, cfg);

    // 同一个字常常挂在多条词条上（`vb` 二级简码与 `vbg` 三级简码都是「好」）。
    // 通配问的是「有哪些**字**」，不是「有哪些词条」——不去重的话首屏会被同一个字占满，
    // 而用户想看的正是「`vb` 下面都有谁」。
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let wubi: Vec<Candidate> = ranked
        .into_iter()
        .filter(|c| seen.insert(c.word.clone()))
        .take(limit)
        .collect();

    let folded = match pinyin {
        Some(lex) => without_seen(pinyin_candidates(lex, stem, store, now, cfg, limit), &wubi),
        None => Vec::new(),
    };
    Mixed {
        source: Source::Wubi,
        primary: wubi,
        folded,
        show_codes: true,
    }
}

/// 多段查询（§6.5 / D64）：每段独立取候选，组合后**只保留词典真词**。
///
/// `'` 的语法决定了**段 1 恒为拼音段**（`'` 跟在它后面），段 2 是五笔（默认）
/// 或拼音（尾 `'`）。D64 之前这里做的是裸笛卡尔积，「号吃」「浩吃」这类不存在
/// 词的字符拼接会铺满候选窗——用户要的是词，不是「每段各选一个字」的排列。
/// 现在的规则：
///
/// - 组合必须过**真词过滤**（两份词库取并集判定）；
/// - 一个真词都凑不出来就整体不出窗（用户选的严格模式：列表永远干净，
///   非词典组合用「分两次单打」表达）。
/// - chip（[`ChipHint`]）挂段 1 的字后面，内容是它的五笔全码——挂词尾会被
///   误读成整个词的五笔码（「好吃」的全码是 `vbkt`，不是 `vbg`）。
fn segment_query<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    pinyin: Option<&PinyinLexicon>,
    segs: &[(&str, bool)],
    store: &S,
    now: i64,
    cfg: &RankingConfig,
    limit: usize,
) -> Mixed {
    let mut pools: Vec<Vec<Candidate>> = Vec::with_capacity(segs.len());
    for (text, is_pinyin) in segs {
        let pool = segment_pool(scheme, pinyin, text, *is_pinyin, store, now, cfg);
        if pool.is_empty() {
            // 任何一段没候选就整体不出窗。只弹「好」再让用户猜第二段，
            // 用户会以为第二段打错了；不如什么都不弹，编码原样留在缓冲里。
            return Mixed::none(Source::Wubi);
        }
        pools.push(pool);
    }
    // `split_segments` 保证段数是 2，这里用模式匹配而不是下标，是为了万一
    // 以后放宽 `MAX_SEGMENTS` 时编译器能提醒，而不是运行时 panic。
    let [first, second] = pools.as_slice() else {
        return Mixed::none(Source::Wubi);
    };

    let mut primary = Vec::new();
    'outer: for a in first {
        for b in second {
            let joined = join_word(a, b);
            if !is_known_word(scheme, pinyin, &joined) {
                continue;
            }
            primary.push(join_parts(a, b));
            if primary.len() >= limit {
                break 'outer;
            }
        }
    }
    // 严格模式：真词一个都没有就不出窗，绝不用字符拼接凑数。
    if primary.is_empty() {
        return Mixed::none(Source::Wubi);
    }
    Mixed {
        source: Source::Pinyin,
        primary,
        folded: Vec::new(),
        show_codes: false,
    }
}

/// 组合串是不是词典真词（两份词库取并集）。
///
/// 拼音词库来自语料，覆盖宽（「好吃」只进拼音库、不进五笔库的情况不少）；
/// 五笔词库收的是打字常用词。缺任何一边都会把真词误杀成「不出窗」。
fn is_known_word(scheme: &Scheme, pinyin: Option<&PinyinLexicon>, word: &str) -> bool {
    if scheme.has_word(word) {
        return true;
    }
    pinyin.is_some_and(|lex| lex.has_word(word))
}

/// 两段候选拼出的完整词（真词判定用，chip 信息由 [`join_parts`] 单独算）。
fn join_word(a: &Candidate, b: &Candidate) -> String {
    format!("{}{}", a.word, b.word)
}

/// 单段候选池。`is_pinyin` 为真时只查拼音；为假时**五笔优先、没命中再试拼音**
/// （与单段判定的第 ③ 条一致：没标记的段仍然按「这是五笔输入法」来判）。
fn segment_pool<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    pinyin: Option<&PinyinLexicon>,
    text: &str,
    is_pinyin: bool,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> Vec<Candidate> {
    let by_pinyin = || match pinyin {
        Some(lex) => pinyin_candidates(lex, text, store, now, cfg, SEGMENT_LIMIT),
        None => Vec::new(),
    };
    if is_pinyin {
        return by_pinyin();
    }
    let wubi = wubi_candidates(scheme, text, store, now, cfg);
    if wubi.is_empty() {
        by_pinyin()
    } else {
        wubi.into_iter().take(SEGMENT_LIMIT).collect()
    }
}

/// 把两段的候选拼成一条候选；chip 挂段 1 的字后面（D64）。
///
/// 段 1 恒为拼音段（`'` 语法决定，见 [`segment_query`]），chip 内容取段 1
/// 候选的五笔全码；「好」不在五笔词库里时无码可教，不挂。
fn join_parts(a: &Candidate, b: &Candidate) -> Candidate {
    let chip = (!a.wubi.is_empty()).then(|| ChipHint {
        code: a.wubi.clone(),
        chars_before: a.word.chars().count(),
    });
    Candidate {
        word: format!("{}{}", a.word, b.word),
        code: format!("{}{}{}", a.code, MARK, b.code),
        // 多段没有「词 → 五笔全码」可言：chip 取的是 `chip`（`wubi` 留空，
        // 免得以后有人误用它去反查——多段的五笔码不是词级概念）。
        wubi: String::new(),
        kind_label: "多段",
        chip,
        // 排序字段只影响「候选区怎么排」，而多段的顺序已经由双循环定死；
        // 取两段里较小的那个，纯粹是给日志与调试一个说得过去的数。
        w_norm: a.w_norm.min(b.w_norm),
        learning: 0.0,
        total: a.total.min(b.total),
        n: 0,
        days_since: f64::INFINITY,
        tier: Tier::Learning,
    }
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
        // 码提示的数据前提与 server 一致：装载后立刻反查五笔全码（§6.3）。
        // 不 attach 的话所有 `wubi` 都是空串，chip 永远挂不上。
        let scheme = scheme();
        let mut lex =
            PinyinLexicon::load(&fixture("../heinrich-scheme/tests/fixtures/pinyin-mini"))
                .expect("拼音夹具应可装载");
        lex.attach_wubi_codes(&scheme);
        lex
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
            hao_pos < hao2_pos,
            "连打三次应当让「浩」越过「号」（D58 之后没有分数门槛，谁分高谁在前）：{order:?}"
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

    // ---------- 通配（M3.5 / D62） ----------

    /// 三个「不该通配」的边界：单个 `z`、4 位前缀、前缀里又出现 `z`。
    #[test]
    fn wildcard_stem_only_accepts_short_lower_letters() {
        assert_eq!(wildcard_stem("vbz"), Some("vb"));
        assert_eq!(wildcard_stem("vz"), Some("v"));
        assert_eq!(wildcard_stem("vbgz"), Some("vbg"));
        // 单独一个 z：没有「待定」可言，空缓冲时 DLL 直接放行
        assert_eq!(wildcard_stem("z"), None);
        // 五位前缀 + z：等于问「第 5 码是什么」，五笔没有第 5 码
        assert_eq!(wildcard_stem("vbguz"), None);
        // z 不在 a–y：`vzz` 是打错了，不是通配
        assert_eq!(wildcard_stem("vzz"), None);
        // 不以 z 结尾就不是通配
        assert_eq!(wildcard_stem("vbg"), None);
    }

    /// `vbz` = 「`vb` 开头的字」。同一字挂在多条词条上（二级简码 + 三级简码）
    /// 只出现一次，并且**每条都带自己的五笔码**（chip 靠它）。
    #[test]
    fn wildcard_lists_words_once_with_codes() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "vbz", &store, 1_000, &cfg);

        assert_eq!(r.source, Source::Wubi);
        assert!(r.show_codes, "通配必须显示码：不显示就等于看了一屏无关的字");
        let ws = words(&r.primary);
        assert!(ws.contains(&"好".to_string()), "vb 下必须有「好」：{ws:?}");
        let mut uniq = ws.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), ws.len(), "同一个字不该出现两次：{ws:?}");
        assert!(
            r.primary.iter().all(|c| c.code.starts_with("vb")),
            "每条候选的码都必须以 vb 开头：{:?}",
            r.primary.iter().map(|c| &c.code).collect::<Vec<_>>()
        );
    }

    /// `z` 只认末位：`vzgu` 不是通配，按普通串走单段判定（夹具里查不到东西）。
    #[test]
    fn wildcard_z_must_be_last() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "vzgu", &store, 1_000, &cfg);
        assert!(!r.show_codes, "中间位置的 z 不该走通配");
    }

    // ---------- 多段（M3.5 / §6.5 / D64） ----------

    /// `hao'vb` = 拼音段「hao」× 五笔段「vb」。D64：组合必须过真词过滤，
    /// 「号好」「浩好」这类字符拼接不再出现——「好好」是词典词，它是唯一幸存者。
    /// chip 挂在拼音段字（好）后面，内容是它的五笔全码 `vbg`。
    #[test]
    fn segments_keep_only_dictionary_words() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "hao'vb", &store, 1_000, &cfg);

        assert_eq!(r.source, Source::Pinyin, "段 1 是拼音");
        assert!(!r.show_codes, "多段的提示走 chip（D64），不再整串铺码");
        assert!(r.folded.is_empty(), "多段没有折叠区");
        assert_eq!(words(&r.primary), ["好好"], "只有词典真词幸存");
        let chip = r.primary[0]
            .chip
            .as_ref()
            .expect("拼音段有五笔码就该挂 chip");
        // 夹具里「好」有两条全码（hao、vbg——为 D48 的重叠测试故意造的），
        // `attach_wubi_codes` 取字典序最小的，提示语不能每次启动换一个。
        assert_eq!(chip.code, "hao");
        assert_eq!(chip.chars_before, 1, "chip 挂在「好」后面");
    }

    /// 两段拼不出任何词典真词就**整体不出窗**（用户选的严格模式）：
    /// 绝不用字符拼接凑数。「好你」不是词，所以 `hao'ni'` 什么都不给。
    #[test]
    fn segments_without_any_real_word_show_nothing() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "hao'ni'", &store, 1_000, &cfg);
        assert!(
            r.is_empty(),
            "组合里没有真词时不能铺拼接串：{:?}",
            words(&r.into_all())
        );
    }

    /// 任何一段没候选就整体不出窗：只弹半截会让用户以为第二段打错了。
    #[test]
    fn segments_need_every_part() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        // 第二段 `qqq` 五笔拼音都没有
        let r = query(&s, Some(&lex), "hao'qqq", &store, 1_000, &cfg);
        assert!(r.is_empty(), "第二段没候选时不能只出第一段");
    }

    /// 两段都显式拼音：`ni'hao'` → 「你好」是真词，幸存；chip 挂段 1 字后，
    /// 但「你」不在五笔词库里（夹具），无码可教 → 不挂。
    #[test]
    fn two_pinyin_segments_keep_real_words_and_chip_on_first() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "ni'hao'", &store, 1_000, &cfg);
        assert_eq!(words(&r.primary), ["你好"]);
        assert!(r.primary[0].chip.is_none(), "「你」没有五笔码，不挂 chip");
    }

    /// 没有拼音词库时，显式拼音段无从查起——整串退回不出窗，而不是把 `hao`
    /// 当五笔硬查（段 1 恒为拼音段，`'` 语法决定的）。
    #[test]
    fn segments_degrade_without_pinyin_lexicon() {
        let s = scheme();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, None, "hao'vb", &store, 1_000, &cfg);
        assert!(r.is_empty());
    }

    /// 超过两段不处理：`a'b'c` 落回单段逻辑（去掉 `'` 当纯拼音），查不到就不出窗。
    #[test]
    fn more_than_two_segments_fall_back() {
        let s = scheme();
        let lex = pinyin();
        let store = MemoryStore::new();
        let cfg = RankingConfig::default();
        let r = query(&s, Some(&lex), "hao'ni'xian", &store, 1_000, &cfg);
        assert!(r.is_empty(), "三段以上不参与组合，也不该给出假的组合");
    }
}
