//! First-contour repositories.
//!
//! Only the tables the gateway needs first: `key_value` (settings),
//! `provider_connections` (credential selection), `api_keys` (auth) and
//! `combos` (routing). Every query names explicit columns so a database that
//! the Node server extended via later migrations keeps working.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;

/// A row in `provider_connections` (auth/selection-relevant columns only).
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionRow {
    pub id: String,
    pub provider: String,
    pub auth_type: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub priority: i64,
    pub is_active: bool,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<String>,
    pub api_key: Option<String>,
    pub provider_specific_data: Option<String>,
}

fn map_connection(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectionRow> {
    Ok(ConnectionRow {
        id: row.get("id")?,
        provider: row.get("provider")?,
        auth_type: row.get("auth_type")?,
        name: row.get("name")?,
        email: row.get("email")?,
        priority: row.get::<_, Option<i64>>("priority")?.unwrap_or(0),
        is_active: row.get::<_, Option<i64>>("is_active")?.unwrap_or(0) != 0,
        access_token: row.get("access_token")?,
        refresh_token: row.get("refresh_token")?,
        expires_at: row.get("expires_at")?,
        api_key: row.get("api_key")?,
        provider_specific_data: row.get("provider_specific_data")?,
    })
}

const CONNECTION_COLUMNS: &str = "id, provider, auth_type, name, email, priority, \
    is_active, access_token, refresh_token, expires_at, api_key, provider_specific_data";

/// Fetch a connection by id.
pub fn get_connection(conn: &Connection, id: &str) -> Result<Option<ConnectionRow>> {
    let row = conn
        .query_row(
            &format!("SELECT {CONNECTION_COLUMNS} FROM provider_connections WHERE id = ?1"),
            params![id],
            map_connection,
        )
        .optional()?;
    Ok(row)
}

/// List active connections, highest priority first.
pub fn list_active_connections(
    conn: &Connection,
    provider: Option<&str>,
) -> Result<Vec<ConnectionRow>> {
    let (sql, bind_provider) = match provider {
        Some(_) => (
            format!(
                "SELECT {CONNECTION_COLUMNS} FROM provider_connections \
                 WHERE is_active = 1 AND provider = ?1 \
                 ORDER BY priority DESC, id ASC"
            ),
            true,
        ),
        None => (
            format!(
                "SELECT {CONNECTION_COLUMNS} FROM provider_connections \
                 WHERE is_active = 1 ORDER BY priority DESC, id ASC"
            ),
            false,
        ),
    };

    let mut statement = conn.prepare(&sql)?;
    let rows = if bind_provider {
        statement.query_map(params![provider], map_connection)?
    } else {
        statement.query_map([], map_connection)?
    };
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Insert or replace a connection row.
pub fn upsert_connection(conn: &Connection, row: &ConnectionRow) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_connections (
            id, provider, auth_type, name, email, priority, is_active,
            access_token, refresh_token, expires_at, api_key,
            provider_specific_data, created_at, updated_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
            datetime('now'), datetime('now')
        ) ON CONFLICT(id) DO UPDATE SET
            provider = excluded.provider,
            auth_type = excluded.auth_type,
            name = excluded.name,
            email = excluded.email,
            priority = excluded.priority,
            is_active = excluded.is_active,
            access_token = excluded.access_token,
            refresh_token = excluded.refresh_token,
            expires_at = excluded.expires_at,
            api_key = excluded.api_key,
            provider_specific_data = excluded.provider_specific_data,
            updated_at = datetime('now')",
        params![
            row.id,
            row.provider,
            row.auth_type,
            row.name,
            row.email,
            row.priority,
            i64::from(row.is_active),
            row.access_token,
            row.refresh_token,
            row.expires_at,
            row.api_key,
            row.provider_specific_data,
        ],
    )?;
    Ok(())
}

/// A row in `api_keys`.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyRow {
    pub id: String,
    pub name: String,
    pub key: String,
    pub allowed_models: String,
    pub no_log: bool,
}

fn map_api_key(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKeyRow> {
    Ok(ApiKeyRow {
        id: row.get("id")?,
        name: row.get("name")?,
        key: row.get("key")?,
        allowed_models: row
            .get::<_, Option<String>>("allowed_models")?
            .unwrap_or_else(|| "[]".to_string()),
        no_log: row.get::<_, Option<i64>>("no_log")?.unwrap_or(0) != 0,
    })
}

/// Fetch an API key by its secret value (the auth hot path).
pub fn get_api_key_by_key(conn: &Connection, key: &str) -> Result<Option<ApiKeyRow>> {
    let row = conn
        .query_row(
            "SELECT id, name, key, allowed_models, no_log FROM api_keys WHERE key = ?1",
            params![key],
            map_api_key,
        )
        .optional()?;
    Ok(row)
}

/// Insert an API key.
pub fn insert_api_key(conn: &Connection, row: &ApiKeyRow) -> Result<()> {
    conn.execute(
        "INSERT INTO api_keys (id, name, key, allowed_models, no_log, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        params![
            row.id,
            row.name,
            row.key,
            row.allowed_models,
            i64::from(row.no_log)
        ],
    )?;
    Ok(())
}

/// A row in `combos`.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboRow {
    pub id: String,
    pub name: String,
    pub data: String,
    pub sort_order: i64,
}

fn map_combo(row: &rusqlite::Row<'_>) -> rusqlite::Result<ComboRow> {
    Ok(ComboRow {
        id: row.get("id")?,
        name: row.get("name")?,
        data: row.get("data")?,
        sort_order: row.get::<_, Option<i64>>("sort_order")?.unwrap_or(0),
    })
}

/// Fetch a combo by id.
pub fn get_combo(conn: &Connection, id: &str) -> Result<Option<ComboRow>> {
    let row = conn
        .query_row(
            "SELECT id, name, data, sort_order FROM combos WHERE id = ?1",
            params![id],
            map_combo,
        )
        .optional()?;
    Ok(row)
}

/// Fetch a combo by name.
pub fn get_combo_by_name(conn: &Connection, name: &str) -> Result<Option<ComboRow>> {
    let row = conn
        .query_row(
            "SELECT id, name, data, sort_order FROM combos WHERE name = ?1",
            params![name],
            map_combo,
        )
        .optional()?;
    Ok(row)
}

/// List combos in display order.
pub fn list_combos(conn: &Connection) -> Result<Vec<ComboRow>> {
    let mut statement = conn.prepare(
        "SELECT id, name, data, sort_order FROM combos ORDER BY sort_order ASC, name ASC",
    )?;
    let rows = statement.query_map([], map_combo)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Insert or replace a combo.
pub fn upsert_combo(conn: &Connection, row: &ComboRow) -> Result<()> {
    conn.execute(
        "INSERT INTO combos (id, name, data, sort_order, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'), datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            data = excluded.data,
            sort_order = excluded.sort_order,
            updated_at = datetime('now')",
        params![row.id, row.name, row.data, row.sort_order],
    )?;
    Ok(())
}

/// Read a `key_value` setting.
pub fn kv_get(conn: &Connection, namespace: &str, key: &str) -> Result<Option<String>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM key_value WHERE namespace = ?1 AND key = ?2",
            params![namespace, key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value)
}

/// Write a `key_value` setting.
pub fn kv_set(conn: &Connection, namespace: &str, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO key_value (namespace, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(namespace, key) DO UPDATE SET value = excluded.value",
        params![namespace, key, value],
    )?;
    Ok(())
}

/// Delete a `key_value` setting.
pub fn kv_delete(conn: &Connection, namespace: &str, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM key_value WHERE namespace = ?1 AND key = ?2",
        params![namespace, key],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn migrated() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        db
    }

    fn sample_connection(id: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.to_string(),
            provider: "grok-cli".to_string(),
            auth_type: Some("oauth".to_string()),
            name: None,
            email: Some("user@example.com".to_string()),
            priority: 10,
            is_active: true,
            access_token: Some("tok".to_string()),
            refresh_token: Some("ref".to_string()),
            expires_at: None,
            api_key: None,
            provider_specific_data: None,
        }
    }

    #[test]
    fn connections_round_trip_and_active_order() {
        let db = migrated();
        let guard = db.connection();

        upsert_connection(&guard, &sample_connection("c-low")).unwrap();
        let mut high = sample_connection("c-high");
        high.priority = 99;
        upsert_connection(&guard, &high).unwrap();
        let mut off = sample_connection("c-off");
        off.is_active = false;
        upsert_connection(&guard, &off).unwrap();

        let active = list_active_connections(&guard, Some("grok-cli")).unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].id, "c-high");
        assert_eq!(active[1].id, "c-low");

        let fetched = get_connection(&guard, "c-low").unwrap().unwrap();
        assert_eq!(fetched.email.as_deref(), Some("user@example.com"));

        assert!(get_connection(&guard, "missing").unwrap().is_none());
    }

    #[test]
    fn api_keys_round_trip_by_secret() {
        let db = migrated();
        let guard = db.connection();

        insert_api_key(
            &guard,
            &ApiKeyRow {
                id: "k1".to_string(),
                name: "test".to_string(),
                key: "sk-test-123".to_string(),
                allowed_models: "[\"*\"]".to_string(),
                no_log: false,
            },
        )
        .unwrap();

        let fetched = get_api_key_by_key(&guard, "sk-test-123").unwrap().unwrap();
        assert_eq!(fetched.id, "k1");
        assert!(get_api_key_by_key(&guard, "sk-nope").unwrap().is_none());
    }

    #[test]
    fn combos_round_trip_and_order() {
        let db = migrated();
        let guard = db.connection();

        upsert_combo(
            &guard,
            &ComboRow {
                id: "b".to_string(),
                name: "b-combo".to_string(),
                data: "{}".to_string(),
                sort_order: 2,
            },
        )
        .unwrap();
        upsert_combo(
            &guard,
            &ComboRow {
                id: "a".to_string(),
                name: "a-combo".to_string(),
                data: "{}".to_string(),
                sort_order: 1,
            },
        )
        .unwrap();

        let list = list_combos(&guard).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "a");

        assert_eq!(
            get_combo_by_name(&guard, "b-combo").unwrap().unwrap().id,
            "b"
        );
        assert_eq!(get_combo(&guard, "a").unwrap().unwrap().name, "a-combo");
    }

    #[test]
    fn key_value_set_get_delete() {
        let db = migrated();
        let guard = db.connection();

        assert_eq!(kv_get(&guard, "settings", "theme").unwrap(), None);
        kv_set(&guard, "settings", "theme", "dark").unwrap();
        assert_eq!(
            kv_get(&guard, "settings", "theme").unwrap().as_deref(),
            Some("dark")
        );
        kv_set(&guard, "settings", "theme", "light").unwrap();
        assert_eq!(
            kv_get(&guard, "settings", "theme").unwrap().as_deref(),
            Some("light")
        );
        kv_delete(&guard, "settings", "theme").unwrap();
        assert_eq!(kv_get(&guard, "settings", "theme").unwrap(), None);
    }
}
