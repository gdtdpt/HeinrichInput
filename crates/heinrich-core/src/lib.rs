//! 亨利输入法 · 核心：解码与排序的纯逻辑层。
//!
//! 约束（PROJECT.md §4.2）：本 crate **不得** 出现 `cfg(windows)`、文件 I/O、
//! 时钟读取。时间一律由调用方以 Unix 秒传入，这样排序行为在测试里完全可复现。

mod engine;
mod pinyin;
mod rank;
mod usage;

pub use engine::Engine;
pub use pinyin::{query, Mixed, Source};
pub use rank::{
    kind_label, learning_score, lookup, rank_indices, rank_inputs, Candidate, LookupMode,
    RankInput, RankingConfig,
};
pub use usage::{MemoryStore, Tier, Usage, UsageStore};

/// 方案数据层，便于调用方只依赖 `heinrich-core`。
pub use heinrich_scheme as scheme;
