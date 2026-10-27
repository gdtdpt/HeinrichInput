//! 候选排序（§5）：静态权重 + 学习分，纯分数降序。
//!
//! ```text
//! 有效次数 e     = max(0, n − 1)
//! 学习分 learning = ln(1 + e) × 0.5 ^ (Δt / H)
//! 综合分 total   = w + g × learning
//!   w : 码表权重，已归一化到 [0,1]；n ≤ 1 时 total = w，即码表序
//!   g : 学习增益，默认 0.35（可配）
//!   H : 半衰期，默认 45 天（可配 7–365）
//! ```
//!
//! 为什么需要 `w` 这一项：只有 `ln(1+e)` 的话，任何词的首次使用都会得到正分，
//! 而从未用过的候选恒为 0 分——「选一次就置顶」正是这么来的。加上静态项后，
//! 未使用过的候选仍按码表序排列，学习只在原有次序上做增量扰动。
//!
//! 为什么 `g = 0.35` 而不是 1.0：码表权重经对数归一化后，相邻候选的差距通常在
//! 0.1–0.2 量级；而 `ln 2 ≈ 0.69`，`g = 1` 时一次误选就能跨过 4–6 个位置。
//! `g = 0.35` 让单次使用只挪动约一位，连续使用（n ≥ 5）才明显上浮。
//!
//! **为什么是 `n − 1` 而不是 `n`（D58）**：`n = 1` 表示「点过一次」，它既可能是
//! 真的要用，也可能是按错了位置。让第一次上屏得 0 分，等于把「偶用一次不该改
//! 顺序」直接写成次数约束——比用分数门槛表达安全得多。
//!
//! 这一段是两次试错的终点，留个记录，免得再绕回去：
//!
//! - **D44 的王座**：门槛 = `1.5 × 第一名的动态总分`。第一名每被上屏一次门槛就涨
//!   一次，形成正反馈：实测连打 12 次才上位、误选一次后要再打 11 次才复位。
//! - **D54 的绝对分差 δ = 0.35**：去掉了身份与正反馈，但「0.35 分」折合几次按键
//!   会随 `n` 急剧膨胀（n≈1 时两三次，n≈26 时五十次）。结果是用得多的词反而被
//!   用得少的词压住，且两个分差小于 δ 的词会随每次上屏来回跨线（实测 `fcu` 的
//!   「去 / 云」每按一次就交换一次位置）。
//!
//! 两者的共同错误是**拿一个固定分数去表达「按几次」**。在对数模型下这两件事不成
//! 比例：`g × ln((n+2)/(n+1))` 在 n = 0 时是 0.2426，n = 26 时只剩 0.0127。
//! 所以规则的严格度必须绑在 `n` 上，不能绑在分数上。

use heinrich_scheme::{CodeKind, Scheme};

use crate::usage::{Tier, Usage, UsageStore};

/// 排序参数（可写入 TOML 供手改）。
#[derive(Clone, Copy, Debug)]
pub struct RankingConfig {
    /// 半衰期（天）
    pub half_life_days: f64,
    /// 学习增益 g
    pub learning_gain: f64,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            half_life_days: 45.0,
            learning_gain: 0.35,
        }
    }
}

impl RankingConfig {
    /// 参数越界时的兜底，避免配置文件写错导致排序行为诡异。
    pub fn sanitized(self) -> Self {
        Self {
            half_life_days: self.half_life_days.clamp(7.0, 365.0),
            learning_gain: self.learning_gain.clamp(0.0, 10.0),
        }
    }
}

/// 一个候选词及其排序依据（供 `:explain` 与设置界面展示）。
#[derive(Clone, Debug)]
pub struct Candidate {
    pub word: String,
    pub code: String,
    /// 这个词的**五笔全码**（拼音候选才有；码提示 chip 画它，§6.3）。
    ///
    /// 与 `code` 分开而不是复用，是因为两者语义不同：`code` 是「这条候选是怎么
    /// 被查出来的」（拼音下是拼音串，还参与同分次序），`wubi` 是
    /// 「这个词用五笔该怎么打」。用拼音查 `hao` 得到「好」时，前者是 `hao`、
    /// 后者是 `vbg`——合并成一个字段等于把两个问题混成一个。
    pub wubi: String,
    pub kind_label: &'static str,
    /// 多段候选的**词中码提示**（D64）。`None` = 无（单段候选、两段都是五笔、
    /// 或拼音段字不在五笔词库里）。
    ///
    /// 单段候选不它：chip 挂词尾、内容由 server 按 §6.3 裁决，不需要这里带位置。
    pub chip: Option<ChipHint>,
    /// 归一化静态权重
    pub w_norm: f32,
    /// 学习分部分
    pub learning: f64,
    /// 综合分
    pub total: f64,
    /// 累计上屏次数
    pub n: u32,
    /// 距上次使用天数（从未使用为 `f64::INFINITY`）
    pub days_since: f64,
    pub tier: Tier,
}

/// 词中码提示（D64）：chip 挂在候选词的**中间**，紧跟它解释的那个字。
///
/// 多段组合（`hao'ktn` → 好吃）里 chip 必须贴着拼音段的字（`好[vbg]吃`），
/// 挂词尾会被误读成整个词的五笔码（「好吃」的全码明明是 `vbkt`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChipHint {
    /// 拼音段字的五笔全码（空码的段不会生成 chip）。
    pub code: String,
    /// chip 前面的字符数：`0` = 挂词头（不会出现），词长 = 挂词尾。
    pub chars_before: usize,
}

/// 学习分：`ln(1 + max(0, n−1)) × 0.5^(Δt/H)`。
///
/// **第一次上屏不加分**（D58）：`n = 1` 只说明「你点过它一次」，那既可能是要用
/// 它，也可能是按错了位置。让这一次得 0 分，「偶用一次不改顺序」就成了次数约束
/// 而不是分数门槛——不会随 `n` 变严，也不需要记「谁在位」。
pub fn learning_score(n: u32, days_since: f64, cfg: &RankingConfig) -> f64 {
    let effective = n.saturating_sub(1);
    if effective == 0 {
        return 0.0;
    }
    let decay = 0.5_f64.powf(days_since / cfg.half_life_days.max(f64::MIN_POSITIVE));
    (1.0 + effective as f64).ln() * decay
}

fn days_between(last_used: i64, now: i64) -> f64 {
    if last_used <= 0 {
        return f64::INFINITY;
    }
    let secs = (now - last_used).max(0) as f64;
    secs / 86_400.0
}

/// 候选集的来源，决定候选窗是否要提示「空码回退」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LookupMode {
    /// 精确码长命中（五笔的正常行为）
    Exact,
    /// 该码长无候选，回退到前缀展开（空码兜底，避免用户卡在死码上）
    PrefixFallback,
}

/// 查询并排序：**精确码长优先**，该长度无候选时才回退到前缀展开。
///
/// 五笔是定长编码体系（一个字/词有确定的码），所以输 `v` 只应命中一级简码，
/// 不能把 `vbgd`、`vbn` 等 3–4 码候选全部倒出来——那会一次给出几千个候选，
/// 排序与翻页都失去意义。前缀展开在拼音输入法里是必需的（变长、要预测），
/// 在五笔里只适合当「空码兜底」。
pub fn lookup<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    input: &str,
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> (Vec<Candidate>, LookupMode) {
    let Some(code) = heinrich_scheme::Code::parse(input) else {
        return (Vec::new(), LookupMode::Exact);
    };
    let exact = scheme.exact_indices(&code);
    if !exact.is_empty() {
        return (
            rank_indices(scheme, exact, store, now, cfg),
            LookupMode::Exact,
        );
    }
    let prefix = scheme.match_indices(input);
    if prefix.is_empty() {
        return (Vec::new(), LookupMode::Exact);
    }
    (
        rank_indices(scheme, prefix, store, now, cfg),
        LookupMode::PrefixFallback,
    )
}

/// 排序的输入：一条候选的身份与静态权重。
///
/// 抽出来是为了让**五笔词库与拼音词库共用同一套排序**——学习分与抑制都是按
/// 「词」而不是按「哪个词典查出来的」生效的。用户在拼音下
/// 多打了几次某个字，切回五笔时它同样更靠前，这才是符合直觉的行为。
pub struct RankInput<'a> {
    pub word: &'a str,
    /// 五笔是编码，拼音是拼音串——它只用于同分时的稳定次序
    pub code: &'a str,
    /// 这个词的五笔全码。空串 = 不知道（五笔侧候选不需要它：能拆出来的人不用提示）
    pub wubi: &'a str,
    /// 归一化静态权重
    pub w_norm: f32,
    pub kind_label: &'static str,
}

/// 对给定的词条下标集合排序（排除抑制，按综合分降序）。
pub fn rank_indices<S: UsageStore + ?Sized>(
    scheme: &Scheme,
    indices: &[u32],
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> Vec<Candidate> {
    let inputs: Vec<RankInput<'_>> = indices
        .iter()
        .map(|&idx| {
            let entry = scheme.entry(idx);
            RankInput {
                word: &entry.word,
                code: entry.code.as_str(),
                // 五笔侧不给提示（§6.3：能拆出来就说明已经会了），所以这里没有
                // 「词 → 全码」反查——那正是 `heinrich-scheme` 里装载期才做的事。
                wubi: "",
                w_norm: entry.w_norm,
                kind_label: entry.kind.label(),
            }
        })
        .collect();
    rank_inputs(&inputs, store, now, cfg)
}

/// 排序核心：与词库来源无关。
pub fn rank_inputs<S: UsageStore + ?Sized>(
    inputs: &[RankInput<'_>],
    store: &S,
    now: i64,
    cfg: &RankingConfig,
) -> Vec<Candidate> {
    let cfg = cfg.sanitized();
    // D95：`Tier::Pinned` 已随「手动置顶」功能 descope 一并退役——五笔重码不足以
    // 撑起这个功能，且从未有用户入口。现在只剩 Learning / Suppressed 两档，
    // 非 Suppressed 的候选全部进入同一条学习队列。
    let mut learn: Vec<Candidate> = Vec::new();

    for item in inputs {
        let tier = store.tier(item.word);
        if tier == Tier::Suppressed {
            continue;
        }
        let usage: Usage = store.usage(item.word);
        let days_since = days_between(usage.last_used, now);
        let learning = learning_score(usage.n, days_since, &cfg);
        // n = 0 与 n = 1 时 `learning` 都是 0，所以两种「还没学到东西」的状态
        // 自然退化成码表序，不需要分支。
        let total = item.w_norm as f64 + cfg.learning_gain * learning;
        let cand = Candidate {
            word: item.word.to_string(),
            code: item.code.to_string(),
            wubi: item.wubi.to_string(),
            kind_label: item.kind_label,
            chip: None,
            w_norm: item.w_norm,
            learning,
            total,
            n: usage.n,
            days_since,
            tier,
        };
        learn.push(cand);
    }

    // 学习区：综合分降序。同分按静态权重、再按词与码定序，保证顺序可预测
    // ——`sort_by` 本身稳定，但显式写全次序，换个词库也不会漂。
    //
    // 这里**没有门槛**：一次使用不改顺序已由 `learning_score` 的 `n − 1` 保证
    // （见文件头 D58）。正因为门槛没了，排序关系重新满足严格弱序，可以交给
    // `sort_by` —— D54 那个手写的插入式前移连同它的不传递性一并删掉了。
    learn.sort_by(|a, b| {
        b.total
            .total_cmp(&a.total)
            .then_with(|| b.w_norm.total_cmp(&a.w_norm))
            .then_with(|| a.word.cmp(&b.word))
            .then_with(|| a.code.cmp(&b.code))
    });

    learn
}

/// 便于调用方展示：`CodeKind` → 中文标签。
pub fn kind_label(kind: CodeKind) -> &'static str {
    kind.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learning_score_matches_formula() {
        let cfg = RankingConfig::default();
        assert_eq!(learning_score(0, 0.0, &cfg), 0.0);
        // 第一次上屏不加分（D58）——它只说明「点过」，不说明「要用」
        assert_eq!(learning_score(1, 0.0, &cfg), 0.0);
        // 第二次起才算：n=2, 刚用过 → ln 2
        assert!((learning_score(2, 0.0, &cfg) - std::f64::consts::LN_2).abs() < 1e-12);
        // 恰好一个半衰期 → 学习分减半
        let half = learning_score(2, 45.0, &cfg);
        assert!((half - std::f64::consts::LN_2 / 2.0).abs() < 1e-12);
        // 单调不减：次数越多分越高（n=1 与 n=0 同为 0，是刻意的）
        assert_eq!(learning_score(1, 0.0, &cfg), learning_score(0, 0.0, &cfg));
        assert!(learning_score(3, 0.0, &cfg) > learning_score(2, 0.0, &cfg));
        assert!(learning_score(10, 0.0, &cfg) > learning_score(9, 0.0, &cfg));
    }

    #[test]
    fn config_is_clamped() {
        let cfg = RankingConfig {
            half_life_days: 0.0,
            learning_gain: 99.0,
        }
        .sanitized();
        assert_eq!(cfg.half_life_days, 7.0);
        assert_eq!(cfg.learning_gain, 10.0);
    }
}
