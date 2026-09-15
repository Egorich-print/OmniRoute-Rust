//! Identifier helpers.
//!
//! Kept dependency-free on purpose: the ids only need to be unique within a
//! process and sortable by time, not cryptographically strong.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Seconds since the Unix epoch (saturating to 0 before 1970).
pub fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// A unique `chatcmpl-*` identifier.
pub fn completion_id() -> String {
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("chatcmpl-rust-{:x}-{:x}", unix_seconds(), seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_ids_are_unique_and_prefixed() {
        let a = completion_id();
        let b = completion_id();
        assert!(a.starts_with("chatcmpl-rust-"));
        assert_ne!(a, b);
    }
}
