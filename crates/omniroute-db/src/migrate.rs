//! Migration runner keyed on `_omniroute_migrations`.
//!
//! Mirrors `src/lib/db/migrationRunner.ts` and the `core.ts` boot order: the
//! ledger is created, each pending migration runs inside its own transaction,
//! and applied versions are recorded. Later migrations are added by appending
//! to [`MIGRATIONS`]; the runner only ever runs what the ledger lacks.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::error::{DbError, Result};

/// Base schema, extracted verbatim from `src/lib/db/core.ts` `SCHEMA_SQL`
/// (17 tables + indexes).
const BASE_SCHEMA_SQL: &str = include_str!("../schema_base.sql");

/// A single versioned migration.
pub struct Migration {
    /// Ledger version, e.g. `"001"`. The JS server seeds `"001"` for the
    /// inline schema, so this runner and the JS runner agree on history.
    pub version: &'static str,
    /// Human-readable name recorded in the ledger.
    pub name: &'static str,
    /// SQL body. Must be idempotent (`IF NOT EXISTS` / `OR IGNORE`) so a
    /// database owned by the Node server can be attached safely.
    pub sql: &'static str,
}

/// Ordered migration history. Append only — never reorder or edit applied
/// entries, the ledger has no downgrade path.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: "001",
    name: "initial_schema",
    sql: BASE_SCHEMA_SQL,
}];

fn ensure_ledger(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _omniroute_migrations (
            version TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    Ok(())
}

fn applied_versions(conn: &Connection) -> Result<HashSet<String>> {
    let mut statement = conn.prepare("SELECT version FROM _omniroute_migrations")?;
    let versions = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<HashSet<_>, _>>()?;
    Ok(versions)
}

/// Apply every migration the ledger lacks, oldest first.
///
/// Returns the versions applied by this call (empty when up to date).
pub fn migrate(conn: &mut Connection) -> Result<Vec<String>> {
    ensure_ledger(conn)?;
    let applied = applied_versions(conn)?;

    let mut newly_applied = Vec::new();
    for migration in MIGRATIONS {
        if applied.contains(migration.version) {
            continue;
        }

        let transaction = conn.transaction()?;
        if let Err(source) = transaction.execute_batch(migration.sql) {
            return Err(DbError::Migration(
                migration.version.to_string(),
                source.to_string(),
            ));
        }
        transaction.execute(
            "INSERT OR IGNORE INTO _omniroute_migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![migration.version, migration.name],
        )?;
        transaction.commit()?;
        newly_applied.push(migration.version.to_string());
    }
    Ok(newly_applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn fresh_db_applies_001_and_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        let mut guard = db.connection();

        let first = migrate(&mut guard).unwrap();
        assert_eq!(first, vec!["001".to_string()]);

        let second = migrate(&mut guard).unwrap();
        assert!(second.is_empty());

        let tables: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // 17 base tables + the ledger.
        assert_eq!(tables, 18);
    }

    #[test]
    fn preseeded_ledger_skips_base_schema() {
        let db = Db::open_in_memory().unwrap();
        let mut guard = db.connection();
        ensure_ledger(&guard).unwrap();
        guard
            .execute(
                "INSERT INTO _omniroute_migrations (version, name) VALUES ('001', 'initial_schema')",
                [],
            )
            .unwrap();

        // A database the Node server already migrated must not be touched.
        let applied = migrate(&mut guard).unwrap();
        assert!(applied.is_empty());
    }
}
