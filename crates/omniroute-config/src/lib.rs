//! Configuration and typed settings.
//!
//! Ports the curated subset of `src/lib/env/` and `src/lib/db/settings.ts`
//! the Rust gateway needs: process environment (a handful of variables, not
//! the 819-var JS surface) and typed access to the `key_value` settings
//! namespaces.

pub mod config;
pub mod settings;

pub use config::GatewayConfig;
pub use settings::Settings;
