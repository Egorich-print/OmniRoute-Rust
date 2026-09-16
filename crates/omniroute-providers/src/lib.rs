//! Provider registry as data.
//!
//! The JavaScript `REGISTRY` (277 providers) is dumped to `registry.json`
//! (see `rust-rewrite/PLAN.md` §5 Phase 0) and embedded here. Logic that
//! interprets entries — URL/header builders, auth flows — is ported
//! separately per executor; this crate only owns the static metadata and the
//! structural contracts that keep it honest.

pub mod registry;

pub use registry::{ProviderEntry, ProviderRegistry, RegistryModel, parse_wire_format};
