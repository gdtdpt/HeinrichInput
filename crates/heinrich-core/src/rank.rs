//! 候选排序（§5）：静态权重 + 学习分，带「第一名保护」阈值。
//!
//! ```text
//! 学习分 learning = ln(1 + n) × 0.5 ^ (Δt / H)
//! 综合分 total    = w + g × learning
//!   w : 码表权重，已归一化到 [0,1]；n = 0 时 total = w，即码表序
//!   g : 学习增益，默认 0.35（可配）
//!   H : 半衰期，默认 45 天（可配 7–365）
//! ```
//!
//! 为什么需要 `w` 这一项：只有 `ln(1+n)` 的话，任何词的首次使用都会得到正分，
//! 而从未用过的候选恒为 0 分——「选一次就置顶」正是这么来的。加上静态项后，
//! 未使用过的候选仍按码表序排列，学习只在原有次序上做增量扰动。
//!
//! 为什么 `g = 0.35` 而不是 1.0：码表权重经对数归一化后，相邻候选的差距通常在
//! 0.1–0.2 量级；而 `ln 2 ≈ 0.69`，`g = 1` 时一次误选就能跨过 4–6 个位置。
//! `g = 0.35` 让单次使用只挪动约一位，连续使用（n ≥ 5）才明显上浮——这正是
//! 「低频词偶用一次不该改顺序」的量化表达。参数可配，写在 `config.toml` 里。
//!
//! 第一名保护（§5.2）：候选要越过当前第一名，需 `total ≥ K × total(第一名)`，
//! `K` 默认 1.5。低频词偶用一次够不着门槛，只能上浮到第二名。

use heinrich_scheme::{CodeKind, Scheme};

use crate::usage::{Tier, Usage, UsageStore};

/// 排序参数（可写入 TOML 供手改）。
#[derive(Clone, Copy, Debug)]
pub struct RankingConfig {
    /// 半衰期（天）
    pub half_life_days: f64,
    /// 晋升阈值系数 K
    pub promote_k: f64,
    /// 学习增益 g
    pub learning_gain: f64,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            half_life_days: 45.0,
            promote_k: 1.5,
            learning_gain: 0.35,
        }
    }
}

impl RankingConfig {
    /// 参数越界时的兜底，避免配置文件写错导致排序行为诡异。
    pub fn sanitized(self) -> Self {
        Self {
            half_life_days: self.half_life_days.clamp(7.0, 365.0),
            promote_k: self.promote_k.clamp(1.0, 10.0),
            learning_gain: self.learning_gain.clamp(0.0, 10.0),
        }
    }
}

/// 一个候选词及其排序依据（供 `:explain` 与设置界面展示）。
#[derive(Clone, Debug)]
pub struct Candidate {
    pub word: String,
    pub code: String,
    pub kind_label: &'static str,
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
    /// 是否因第一名保护被压回原位
    pub throne_held: bool,
}

/// 学习分：`ln(1+n) × 0.5^(Δt/H)`。
pub fn learning_score(n: u32, days_since: f64, cfg: &RankingConfig) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let decay = 0.5_f64.powf(days_since / cfg.half_life_days.max(f64::MIN_POSITIVE));
    (1.0 + n as f64).ln() * decay
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
/// 抽出来是为了让**五笔词库与拼音词库共用同一套排序**——学习分、首位保护、
/// 抑制与固顶都是按「词」而不是按「哪个词典查出来的」生效的。用户在拼音下
/// 多打了几次某个字，切回五笔时它同样更靠前，这才是符合直觉的行为。
pub struct RankInput<'a> {
    pub word: &'a str,
    /// 五笔是编码，拼音是拼音串——它只用于同分时稳定次序与 `throne` 的身份判定
    pub code: &'a str,
    /// 归一化静态权重
    pub w_norm: f32,
    pub kind_label: &'static str,
}

/// 对给定的词条下标集合排序（固顶 → 学习区，首位受阈值保护）。
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
    let mut pinned: Vec<Candidate> = Vec::new();
    let mut learn: Vec<Candidate> = Vec::new();

    for item in inputs {
        let tier = store.tier(item.word);
        if tier == Tier::Suppressed {
            continue;
        }
        let usage: Usage = store.usage(item.word);
        let days_since = days_between(usage.last_used, now);
        let learning = learning_score(usage.n, days_since, &cfg);
        let total = if usage.n == 0 {
            item.w_norm as f64
        } else {
            item.w_norm as f64 + cfg.learning_gain * learning
        };
        let cand = Candidate {
            word: item.word.to_string(),
            code: item.code.to_string(),
            kind_label: item.kind_label,
            w_norm: item.w_norm,
            learning,
            total,
            n: usage.n,
            days_since,
            tier,
            throne_held: false,
        };
        match tier {
            Tier::Pinned => pinned.push(cand),
            _ => learn.push(cand),
        }
    }

    // 固顶：静态权重降序（稳定）
    pinned.sort_by(|a, b| b.w_norm.total_cmp(&a.w_norm));

    // 基线序 = 码表序：静态权重降序，同权重保持码表先后。
    // 基线第一名即「王座」持有者——它是用户肌肉记忆里的首候选。
    learn.sort_by(|a, b| b.w_norm.total_cmp(&a.w_norm));
    let throne: Option<(String, String)> = learn.first().map(|c| (c.code.clone(), c.word.clone()));

    // 学习区：综合分降序
    learn.sort_by(|a, b| {
        b.total
            .total_cmp(&a.total)
            .then_with(|| b.w_norm.total_cmp(&a.w_norm))
            .then_with(|| a.word.cmp(&b.word))
            .then_with(|| a.code.cmp(&b.code))
    });

    // 第一名保护：挑战者必须够 K 倍才能夺位
    if let Some((t_code, t_word)) = throne {
        let first_is_throne = learn[0].code == t_code && learn[0].word == t_word;
        if !first_is_throne {
            let leader_total = learn
                .iter()
                .find(|c| c.code == t_code && c.word == t_word)
                .map(|c| c.total)
                .unwrap_or(0.0)
                .max(f64::EPSILON);
            if learn[0].total < cfg.promote_k * leader_total {
                if let Some(pos) = learn
                    .iter()
                    .position(|c| c.code == t_code && c.word == t_word)
                {
                    let mut held = learn.remove(pos);
                    held.throne_held = true;
                    learn.insert(0, held);
                }
            }
        }
    }

    pinned.extend(learn);
    pinned
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
        // n=1, 刚用过 → ln 2
        assert!((learning_score(1, 0.0, &cfg) - std::f64::consts::LN_2).abs() < 1e-12);
        // 恰好一个半衰期 → 学习分减半
        let half = learning_score(1, 45.0, &cfg);
        assert!((half - std::f64::consts::LN_2 / 2.0).abs() < 1e-12);
        // 单调：次数越多分越高
        assert!(learning_score(10, 0.0, &cfg) > learning_score(9, 0.0, &cfg));
    }

    #[test]
    fn config_is_clamped() {
        let cfg = RankingConfig {
            half_life_days: 0.0,
            promote_k: 0.1,
            learning_gain: 99.0,
        }
        .sanitized();
        assert_eq!(cfg.half_life_days, 7.0);
        assert_eq!(cfg.promote_k, 1.0);
        assert_eq!(cfg.learning_gain, 10.0);
    }
}
