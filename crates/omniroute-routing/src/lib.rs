//! Request routing: model-to-backend selection, failover and breakers.
//!
//! Ports the Phase 4 core (`open-sse/services/combo.ts`, account selection,
//! circuit breakers) in sliceable form. This crate owns the *decision*
//! (which backends to try, in which order, and which are currently
//! tripped) — never the backends themselves, so there is no dependency cycle
//! with the gateway.

pub mod breaker;
pub mod router;

pub use breaker::{BreakerSet, CircuitBreaker};
pub use router::{RouteRule, Router};
