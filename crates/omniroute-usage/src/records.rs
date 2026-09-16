//! `usage_history` writes.

use rusqlite::{Connection, params};

use omniroute_db::Result;

/// One accounted request. Mirrors the columns `saveRequestUsage` writes.
#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub connection_id: Option<String>,
    pub api_key_id: Option<String>,
    pub api_key_name: Option<String>,
    pub tokens_input: u32,
    pub tokens_output: u32,
    pub tokens_cache_read: u32,
    pub tokens_cache_creation: u32,
    pub tokens_reasoning: u32,
    pub status: Option<String>,
    pub success: bool,
    pub latency_ms: u32,
    pub error_code: Option<String>,
}

/// Append a usage row.
pub fn record_usage(conn: &Connection, record: &UsageRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_history (
            provider, model, connection_id, api_key_id, api_key_name,
            tokens_input, tokens_output, tokens_cache_read, tokens_cache_creation,
            tokens_reasoning, status, success, latency_ms, error_code,
            timestamp
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            datetime('now')
        )",
        params![
            record.provider,
            record.model,
            record.connection_id,
            record.api_key_id,
            record.api_key_name,
            record.tokens_input,
            record.tokens_output,
            record.tokens_cache_read,
            record.tokens_cache_creation,
            record.tokens_reasoning,
            record.status,
            i64::from(record.success),
            record.latency_ms,
            record.error_code,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use omniroute_db::Db;

    #[test]
    fn usage_row_round_trip() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();

        record_usage(
            &guard,
            &UsageRecord {
                provider: Some("grok-cli".to_string()),
                model: Some("grok-4.6".to_string()),
                api_key_id: Some("k1".to_string()),
                tokens_input: 100,
                tokens_output: 50,
                success: true,
                latency_ms: 1200,
                ..Default::default()
            },
        )
        .unwrap();

        let (count, tokens_in): (i64, i64) = guard
            .query_row(
                "SELECT COUNT(*), SUM(tokens_input) FROM usage_history WHERE model = 'grok-4.6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(tokens_in, 100);
    }
}
