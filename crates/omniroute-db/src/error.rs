//! Database errors.

use thiserror::Error;

/// Errors surfaced by the data layer.
#[derive(Debug, Error)]
pub enum DbError {
    /// Underlying SQLite failure.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Filesystem failure (creating the data directory, opening the file).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialization failure for JSON-encoded columns.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A migration body failed to apply.
    #[error("migration {0} failed: {1}")]
    Migration(String, String),
}

/// Convenience result alias.
pub type Result<T> = std::result::Result<T, DbError>;
