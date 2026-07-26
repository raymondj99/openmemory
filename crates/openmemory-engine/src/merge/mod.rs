//! Staged materialization and crash-safe whole-root promotion.

mod materialize;
mod promotion;

pub(crate) use materialize::derived_stub_id;
pub use materialize::{materialize_snapshot, MaterializationReport};
pub use promotion::{
    complete_promotion, promote_staged_root, recover_promotion, MergePromotionIntent,
    PromotionMode, PromotionPhase, PromotionRecovery, PromotionReport,
};
