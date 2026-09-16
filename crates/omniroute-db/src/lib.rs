//! SQLite data layer.
//!
//! Ports `src/lib/db/` (better-sqlite3 singleton, 176 migrations, ~127 tables).
//! Phase 1 covers the foundation: connection handling, PRAGMAs, the migration
//! runner keyed on `_omniroute_migrations`, the base schema (001), and the
//! repositories the gateway needs first (`key_value`, `provider_connections`,
//! `api_keys`, `combos`).
//!
//! Dual-run note: the runner only ever executes `IF NOT EXISTS` DDL and
//! `INSERT OR IGNORE` ledger rows, so attaching to a database owned by the
//! Node server is safe. All reads name explicit columns.

pub mod db;
pub mod error;
pub mod migrate;
pub mod repos;

pub use db::Db;
pub use error::{DbError, Result};
