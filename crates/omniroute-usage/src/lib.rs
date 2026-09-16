//! Usage accounting, pricing and cost.
//!
//! Ports the hot path of `src/lib/usage/` (`usage_history` writer,
//! `costCalculator.ts`, the user-override pricing layer). The full 2,019-line
//! hardcoded catalog ports incrementally; the seed below holds the reference
//! models with values copied verbatim from
//! `src/shared/constants/pricing/`.

pub mod pricing;
pub mod records;

pub use pricing::{ModelPrice, PriceTable};
pub use records::{UsageRecord, record_usage};
