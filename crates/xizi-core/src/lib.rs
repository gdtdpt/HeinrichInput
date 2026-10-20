//! 析字核心：解码与排序的纯逻辑层。
//!
//! 约束（PROJECT.md §4.2）：本 crate **不得** 出现 `cfg(windows)`、文件 I/O、
//! 时钟读取。时间一律由调用方以 Unix 秒传入，这样排序行为在测试里完全可复现。

mod engine;
mod rank;
mod usage;

pub use engine::Engine;
pub use rank::{
    kind_label, learning_score, lookup, rank_indices, Candidate, LookupMode, RankingConfig,
};
pub use usage::{MemoryStore, Tier, Usage, UsageStore};

/// 方案数据层，便于调用方只依赖 `xizi-core`。
pub use xizi_scheme as scheme;
