//! First-contour repositories.
//!
//! Only the tables the gateway needs first: `key_value` (settings),
//! `provider_connections` (credential selection), `api_keys` (auth) and
//! `combos` (routing). Every query names explicit columns so a database that
//! the Node server extended via later migrations keeps working.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;

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

/// A dashboard-created API key, mirroring what the JS server stores for an
/// operator key: plaintext `sk-…` secret (the Rust gateway matches it
/// verbatim), `key_prefix` for masked display, and `key_hash` (sha256 hex)
/// for JS-side lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApiKey {
    pub id: String,
    pub name: String,
    pub key: String,
    pub key_prefix: String,
    pub key_hash: String,
    pub allowed_models: String,
    pub scopes: String,
}

/// Insert a full operator key. Unspecified columns take the same defaults the
/// JS dashboard writes, so the key works on both sides.
pub fn insert_full_api_key(conn: &Connection, row: &NewApiKey) -> Result<()> {
    conn.execute(
        "INSERT INTO api_keys (
            id, name, key, machine_id, allowed_models, no_log, created_at,
            revoked_at, expires_at, last_used_at, key_prefix, scopes,
            allowed_combos, stream_default_mode, allowed_quotas,
            disable_non_public_models, usage_limit_enabled, is_active,
            max_sessions, is_banned, key_hash, allow_usage_command,
            chaos_mode_enabled, cache_default_mode, model_access_mode,
            compression_enabled
        ) VALUES (
            ?1, ?2, ?3, NULL, ?4, 0, datetime('now'),
            NULL, NULL, NULL, ?5, ?6,
            '[\"combo/*\"]', 'legacy', '[]',
            0, 0, 1,
            0, 0, ?7, 1,
            0, 'legacy', 'all',
            1
        )",
        params![
            row.id,
            row.name,
            row.key,
            row.allowed_models,
            row.key_prefix,
            row.scopes,
            row.key_hash,
        ],
    )?;
    Ok(())
}

/// An API key as the dashboard lists it: metadata only, never the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeySummary {
    pub id: String,
    pub name: String,
    pub key_prefix: Option<String>,
    /// Whether the stored secret is `enc:v1:`-encrypted (unusable by the
    /// Rust gateway, which matches plaintext secrets verbatim).
    pub is_encrypted: bool,
    pub created_at: Option<String>,
    pub revoked_at: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub is_active: bool,
}

fn map_key_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiKeySummary> {
    Ok(ApiKeySummary {
        id: row.get("id")?,
        name: row.get("name")?,
        key_prefix: row.get("key_prefix")?,
        is_encrypted: row.get::<_, i64>("is_encrypted")? != 0,
        created_at: row.get("created_at")?,
        revoked_at: row.get("revoked_at")?,
        expires_at: row.get("expires_at")?,
        last_used_at: row.get("last_used_at")?,
        is_active: row.get::<_, Option<i64>>("is_active")?.unwrap_or(1) != 0,
    })
}

const KEY_SUMMARY_COLUMNS: &str = "id, name, key_prefix, \
    CASE WHEN key LIKE 'enc:v1:%' THEN 1 ELSE 0 END AS is_encrypted, \
    created_at, revoked_at, expires_at, last_used_at, is_active";

/// List every API key, newest first. Secrets are never selected — only a
/// derived encrypted flag.
pub fn list_api_keys(conn: &Connection) -> Result<Vec<ApiKeySummary>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {KEY_SUMMARY_COLUMNS} FROM api_keys ORDER BY created_at DESC, id ASC"
    ))?;
    let rows = statement.query_map([], map_key_summary)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Fetch one key's summary by id. Secrets are never selected.
pub fn get_api_key_summary(conn: &Connection, id: &str) -> Result<Option<ApiKeySummary>> {
    let row = conn
        .query_row(
            &format!("SELECT {KEY_SUMMARY_COLUMNS} FROM api_keys WHERE id = ?1"),
            params![id],
            map_key_summary,
        )
        .optional()?;
    Ok(row)
}

/// Revoke (`true`) or restore (`false`) an API key. Returns whether the row
/// exists. Revocation takes effect immediately: authentication reads the
/// table on every request.
pub fn set_api_key_revoked(conn: &Connection, id: &str, revoked: bool) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE api_keys SET revoked_at = CASE WHEN ?1 THEN datetime('now') ELSE NULL END \
         WHERE id = ?2",
        params![revoked, id],
    )?;
    Ok(changed > 0)
}

/// An API key that passed every usability gate (the auth hot path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsableKey {
    pub id: String,
    pub name: String,
}

/// Fetch an API key by its secret value, honouring the kill switches.
///
/// Unlike [`get_api_key_by_key`], a row only matches when it is not revoked,
/// not banned, active, and unexpired. Expiry is checked in Rust because the
/// table mixes SQLite (`"2026-09-17 13:00:00"`) and ISO-8601 spellings.
pub fn find_usable_api_key(conn: &Connection, key: &str) -> Result<Option<UsableKey>> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT id, name, expires_at FROM api_keys \
             WHERE key = ?1 AND revoked_at IS NULL \
               AND COALESCE(is_banned, 0) = 0 AND COALESCE(is_active, 1) = 1",
            params![key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((id, name, expires_at)) = row else {
        return Ok(None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    if timestamp_expired(expires_at.as_deref(), now) {
        return Ok(None);
    }
    Ok(Some(UsableKey { id, name }))
}

/// Whether an `expires_at` value is already past.
///
/// `None` (no expiry) and unparseable values count as valid: a weird string
/// must never lock the operator out, and real rows come from the picker the
/// dashboard writes.
fn timestamp_expired(expires_at: Option<&str>, now_unix: u64) -> bool {
    let Some(text) = expires_at else {
        return false;
    };
    parse_db_timestamp(text).is_some_and(|expiry| expiry <= now_unix)
}

/// Parse the two timestamp spellings this database holds — SQLite
/// `"YYYY-MM-DD HH:MM:SS"` and ISO-8601 (`"...)T...Z"`, offsets, fractions)
/// — into unix seconds. Returns `None` when the text does not match either.
fn parse_db_timestamp(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, time) = text.split_once(['T', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Offset suffix: `Z`, `+hh:mm`, `+hhmm`, `+hh`, or nothing (assume UTC —
    // both writers store UTC).
    let (clock, offset_secs) = if let Some(stripped) = time.strip_suffix(['Z', 'z']) {
        (stripped, 0)
    } else if let Some(at) = time.rfind(['+', '-']) {
        let (clock, zone) = time.split_at(at);
        (clock, parse_zone_offset(zone)?)
    } else {
        (time, 0)
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts
        .next()?
        .split('.')
        .next()?
        .parse()
        .ok()?;
    if clock_parts.next().is_some()
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let adjusted = if month <= 2 { year - 1 } else { year };
    let era = adjusted.div_euclid(400);
    let year_of_era = adjusted.rem_euclid(400);
    let month_prime = (month + 9).rem_euclid(12);
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    let stamp = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    u64::try_from(stamp).ok()
}

/// Parse a `Z`/`±hh[:mm]` zone suffix into seconds east of UTC.
fn parse_zone_offset(zone: &str) -> Option<i64> {
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = zone[1..].chars().filter(|ch| *ch != ':').collect();
    let (hours, minutes) = match digits.len() {
        2 => (digits.parse::<i64>().ok()?, 0),
        4 => (
            digits[..2].parse::<i64>().ok()?,
            digits[2..].parse::<i64>().ok()?,
        ),
        _ => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
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

/// Fields that `PATCH /api/combos/:id` may change.
///
/// `name` and `sort_order` live both as columns and inside the `data` JSON;
/// this patch keeps the two copies in sync. `models` replaces the `models`
/// array wholesale (see [`render_model_items`]); callers must validate first
/// (non-empty, known providers are the admin layer's job).
#[derive(Debug, Clone, Default)]
pub struct ComboMetaPatch {
    pub name: Option<String>,
    pub sort_order: Option<i64>,
    pub is_hidden: Option<bool>,
    pub strategy: Option<String>,
    pub models: Option<Vec<ComboModelSpec>>,
}

/// One model entry for a combo rewrite: provider plus bare model id.
///
/// `model` is the bare id exactly as the router derives it (the stored ref
/// minus its first `provider/` segment), and may itself contain slashes
/// (`"openai/gpt-oss-20b"`, `"nvidia/nemotron-3-super-120b-a12b"`). It is
/// joined verbatim: the server never strips prefixes, so a GET → PATCH
/// round-trip is byte-stable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboModelSpec {
    /// Keep this entry id when set and non-blank; otherwise one is derived.
    pub id: Option<String>,
    pub provider: String,
    pub model: String,
    pub weight: i64,
}

/// Build the `models` JSON array for a combo rewrite.
///
/// New model items are emitted in order with generated ids where missing.
/// When a spec id matches a previous model item, unknown extra keys (e.g. a
/// JS-side `label`) are carried over so a Rust-side edit never drops data it
/// does not model. Array items of any other `kind` are preserved verbatim at
/// the end so a future JS-side kind survives as well.
fn render_model_items(
    combo_name: &str,
    specs: &[ComboModelSpec],
    previous: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    fn slug(text: &str) -> String {
        let mut out = String::new();
        let mut last_dash = true;
        for ch in text.to_lowercase().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
                last_dash = false;
            } else if !last_dash {
                out.push('-');
                last_dash = true;
            }
            if out.len() >= 40 {
                break;
            }
        }
        out.trim_matches('-').to_string()
    }

    let mut taken: HashSet<String> = HashSet::new();
    let mut consumed: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        // Verbatim join: `model` is already the bare id, slashes included.
        let bare = spec.model.clone();
        let base = match spec.id.clone().filter(|id| !id.trim().is_empty()) {
            Some(id) => id.trim().to_string(),
            None => format!(
                "{}-model-{}-{}-{}",
                slug(combo_name),
                index + 1,
                slug(&spec.provider),
                slug(&bare)
            ),
        };
        let mut id = base.clone();
        let mut counter = 2;
        while !taken.insert(id.clone()) {
            id = format!("{base}-{counter}");
            counter += 1;
        }
        // Carry over unknown keys from the previous item with the same id.
        let mut item = previous
            .iter()
            .find(|item| item.get("id").and_then(|id| id.as_str()) == Some(id.as_str()))
            .filter(|item| {
                item.get("kind")
                    .and_then(|kind| kind.as_str())
                    .is_none_or(|kind| kind == "model")
            })
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        consumed.insert(id.clone());
        item["id"] = serde_json::Value::String(id);
        item["kind"] = serde_json::Value::String("model".to_string());
        item["model"] = serde_json::Value::String(format!("{}/{}", spec.provider, bare));
        item["providerId"] = serde_json::Value::String(spec.provider.clone());
        item["weight"] = serde_json::Value::from(spec.weight);
        out.push(item);
    }
    for item in previous {
        let id = item
            .get("id")
            .and_then(|id| id.as_str())
            .unwrap_or("");
        if consumed.contains(id) {
            continue;
        }
        let is_model = item
            .get("kind")
            .and_then(|kind| kind.as_str())
            .is_none_or(|kind| kind == "model");
        if !is_model {
            out.push(item.clone());
        }
    }
    out
}

/// Update combo metadata, keeping the `data` JSON (`name`, `sortOrder`,
/// `isHidden`) in sync with the columns.
///
/// Returns the updated row, or `None` when `id` does not exist. A duplicate
/// `name` surfaces as a SQLite unique-constraint error for the caller to map.
pub fn update_combo_meta(
    conn: &Connection,
    id: &str,
    patch: &ComboMetaPatch,
) -> Result<Option<ComboRow>> {
    let Some(current) = get_combo(conn, id)? else {
        return Ok(None);
    };
    let mut data: serde_json::Value =
        serde_json::from_str(&current.data).unwrap_or(serde_json::json!({}));
    if !data.is_object() {
        data = serde_json::json!({});
    }
    let name = patch.name.clone().unwrap_or(current.name);
    let sort_order = patch.sort_order.unwrap_or(current.sort_order);
    if let Some(hidden) = patch.is_hidden {
        data["isHidden"] = serde_json::Value::Bool(hidden);
    }
    if let Some(strategy) = patch.strategy.clone() {
        data["strategy"] = serde_json::Value::String(strategy);
    }
    if let Some(specs) = patch.models.as_deref() {
        let previous: Vec<serde_json::Value> = data
            .get("models")
            .and_then(|models| models.as_array())
            .cloned()
            .unwrap_or_default();
        data["models"] = serde_json::Value::Array(render_model_items(&name, specs, &previous));
    }
    data["name"] = serde_json::Value::String(name.clone());
    data["sortOrder"] = serde_json::Value::from(sort_order);
    let data_text = serde_json::to_string(&data)?;
    conn.execute(
        "UPDATE combos SET name = ?1, data = ?2, sort_order = ?3, \
         updated_at = datetime('now') WHERE id = ?4",
        params![name, data_text, sort_order, id],
    )?;
    get_combo(conn, id)
}

/// Fields that `PATCH /api/connections/:id` may change.
///
/// Secrets are deliberately not patchable here; credential rotation stays a
/// JS-dashboard/CLI operation until the Rust side owns decryption.
#[derive(Debug, Clone, Default)]
pub struct ConnectionMetaPatch {
    pub name: Option<String>,
    pub is_active: Option<bool>,
    pub priority: Option<i64>,
}

/// Update connection metadata. Returns the updated row, or `None` when `id`
/// does not exist.
///
/// Note: flipping `is_active` only takes effect on the live gateway after a
/// restart, because backends are built once at startup. Callers should tell
/// the user that.
pub fn update_connection_meta(
    conn: &Connection,
    id: &str,
    patch: &ConnectionMetaPatch,
) -> Result<Option<ConnectionRow>> {
    let Some(current) = get_connection(conn, id)? else {
        return Ok(None);
    };
    let name = patch.name.clone().or(current.name);
    let is_active = patch.is_active.unwrap_or(current.is_active);
    let priority = patch.priority.unwrap_or(current.priority);
    conn.execute(
        "UPDATE provider_connections SET name = ?1, is_active = ?2, priority = ?3, \
         updated_at = datetime('now') WHERE id = ?4",
        params![name, i64::from(is_active), priority, id],
    )?;
    get_connection(conn, id)
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

/// A `provider_connections` row shaped for the admin UI (no secrets).
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionSummary {
    pub id: String,
    pub provider: String,
    pub name: Option<String>,
    pub auth_type: Option<String>,
    pub is_active: bool,
    pub priority: i64,
    pub expires_at: Option<String>,
    pub test_status: Option<String>,
    pub last_error: Option<String>,
    /// Whether the row carries a usable secret (never the secret itself).
    pub has_credential: bool,
}

/// List every connection, active first, for the admin UI.
pub fn list_connections(conn: &Connection) -> Result<Vec<ConnectionSummary>> {
    let mut statement = conn.prepare(
        "SELECT id, provider, name, auth_type, is_active, priority, expires_at, \
                test_status, last_error, access_token, api_key \
         FROM provider_connections ORDER BY is_active DESC, provider ASC, priority DESC",
    )?;
    let rows = statement.query_map([], |row| {
        let access_token: Option<String> = row.get("access_token")?;
        let api_key: Option<String> = row.get("api_key")?;
        let has_credential = [access_token, api_key]
            .iter()
            .flatten()
            .any(|secret| !secret.is_empty());
        Ok(ConnectionSummary {
            id: row.get("id")?,
            provider: row.get("provider")?,
            name: row.get("name")?,
            auth_type: row.get("auth_type")?,
            is_active: row.get::<_, Option<i64>>("is_active")?.unwrap_or(0) != 0,
            priority: row.get::<_, Option<i64>>("priority")?.unwrap_or(0),
            expires_at: row.get("expires_at")?,
            test_status: row.get("test_status")?,
            last_error: row.get("last_error")?,
            has_credential,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// One `(key, requests, tokens_in, tokens_out)` aggregate row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageBucket {
    pub key: String,
    pub requests: i64,
    pub tokens_input: i64,
    pub tokens_output: i64,
}

/// Aggregate usage since `since` (compared as text, the stored format).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsageSummary {
    pub requests: i64,
    pub successes: i64,
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_cache_read: i64,
    pub by_provider: Vec<UsageBucket>,
    pub by_model: Vec<UsageBucket>,
}

/// Summarise `usage_history` for the dashboard.
///
/// `since` is a SQLite datetime modifier (e.g. `-7 days`), evaluated by
/// SQLite itself so both timestamp spellings in the table are handled the
/// same way the Node server handled them.
pub fn usage_summary(conn: &Connection, since: &str, top: usize) -> Result<UsageSummary> {
    let mut summary = UsageSummary::default();

    let totals = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(success), 0), \
                COALESCE(SUM(tokens_input), 0), COALESCE(SUM(tokens_output), 0), \
                COALESCE(SUM(tokens_cache_read), 0) \
         FROM usage_history WHERE timestamp >= datetime('now', ?1)",
        params![since],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        },
    )?;
    summary.requests = totals.0;
    summary.successes = totals.1;
    summary.tokens_input = totals.2;
    summary.tokens_output = totals.3;
    summary.tokens_cache_read = totals.4;

    summary.by_provider = buckets(conn, "provider", since, top)?;
    summary.by_model = buckets(conn, "model", since, top)?;
    Ok(summary)
}

fn buckets(conn: &Connection, column: &str, since: &str, top: usize) -> Result<Vec<UsageBucket>> {
    // `column` is a fixed identifier chosen by the caller, never user input.
    let sql = format!(
        "SELECT COALESCE({column}, 'unknown') AS key, COUNT(*), \
                COALESCE(SUM(tokens_input), 0), COALESCE(SUM(tokens_output), 0) \
         FROM usage_history WHERE timestamp >= datetime('now', ?1) \
         GROUP BY key ORDER BY COUNT(*) DESC LIMIT ?2"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params![since, top as i64], |row| {
        Ok(UsageBucket {
            key: row.get(0)?,
            requests: row.get(1)?,
            tokens_input: row.get(2)?,
            tokens_output: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// A recent request as the dashboard shows it (no bodies, no secrets).
#[derive(Debug, Clone, PartialEq)]
pub struct CallLogSummary {
    pub timestamp: String,
    pub model: Option<String>,
    pub requested_model: Option<String>,
    pub provider: Option<String>,
    pub status: Option<i64>,
    pub duration_ms: Option<i64>,
    pub tokens_input: Option<i64>,
    pub tokens_output: Option<i64>,
    pub combo_name: Option<String>,
    pub error_summary: Option<String>,
}

/// Most recent `call_logs` rows, newest first.
pub fn recent_calls(conn: &Connection, limit: usize) -> Result<Vec<CallLogSummary>> {
    let mut statement = conn.prepare(
        "SELECT timestamp, model, requested_model, provider, status, duration, \
                tokens_in, tokens_out, combo_name, error_summary \
         FROM call_logs ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit as i64], |row| {
        Ok(CallLogSummary {
            timestamp: row.get(0)?,
            model: row.get(1)?,
            requested_model: row.get(2)?,
            provider: row.get(3)?,
            status: row.get(4)?,
            duration_ms: row.get(5)?,
            tokens_input: row.get(6)?,
            tokens_output: row.get(7)?,
            combo_name: row.get(8)?,
            error_summary: row.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(crate::error::DbError::from)
}

/// Number of API keys that can still authenticate.
pub fn count_active_api_keys(conn: &Connection) -> Result<usize> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM api_keys WHERE revoked_at IS NULL",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(count.max(0) as usize)
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
    fn timestamps_parse_both_spellings() {
        assert_eq!(parse_db_timestamp("1970-01-01 00:00:00"), Some(0));
        assert_eq!(parse_db_timestamp("2026-09-17 13:00:00"), Some(1789650000));
        assert_eq!(
            parse_db_timestamp("2000-02-29T12:30:15.123Z"),
            Some(951827415)
        );
        assert_eq!(
            parse_db_timestamp("2000-02-29T14:30:15+02:00"),
            Some(951827415)
        );
        assert_eq!(parse_db_timestamp("not a date"), None);
        assert_eq!(parse_db_timestamp("2026-13-01 00:00:00"), None);
        assert!(!timestamp_expired(None, 1789650000));
        assert!(!timestamp_expired(Some("garbage"), 1789650000));
        assert!(!timestamp_expired(
            Some("2026-09-17 13:00:01"),
            1789650000
        ));
        assert!(timestamp_expired(Some("2026-09-17 13:00:00"), 1789650000));
    }

    #[test]
    fn usable_keys_honour_kill_switches() {
        let db = migrated();
        let guard = db.connection();

        for (id, key) in [
            ("good", "sk-good"),
            ("revoked", "sk-revoked"),
            ("banned", "sk-banned"),
            ("inactive", "sk-inactive"),
            ("expired", "sk-expired"),
        ] {
            guard
                .execute(
                    "INSERT INTO api_keys (id, name, key, created_at) VALUES (?1, ?2, ?3, datetime('now'))",
                    rusqlite::params![id, id, key],
                )
                .unwrap();
        }
        guard
            .execute(
                "UPDATE api_keys SET revoked_at = datetime('now') WHERE id = 'revoked'",
                [],
            )
            .unwrap();
        guard
            .execute("UPDATE api_keys SET is_banned = 1 WHERE id = 'banned'", [])
            .unwrap();
        guard
            .execute(
                "UPDATE api_keys SET is_active = 0 WHERE id = 'inactive'",
                [],
            )
            .unwrap();
        guard
            .execute(
                "UPDATE api_keys SET expires_at = '2001-01-01 00:00:00' WHERE id = 'expired'",
                [],
            )
            .unwrap();

        let usable = find_usable_api_key(&guard, "sk-good").unwrap().unwrap();
        assert_eq!(usable.id, "good");
        for dead in ["sk-revoked", "sk-banned", "sk-inactive", "sk-expired", "sk-nope"] {
            assert!(
                find_usable_api_key(&guard, dead).unwrap().is_none(),
                "{dead} must not authenticate"
            );
        }
    }

    #[test]
    fn full_keys_insert_list_revoke_restore() {
        let db = migrated();
        let guard = db.connection();

        insert_full_api_key(
            &guard,
            &NewApiKey {
                id: "full-1".to_string(),
                name: "ops".to_string(),
                key: "sk-abc123".to_string(),
                key_prefix: "sk-abc123".to_string(),
                key_hash: "deadbeef".to_string(),
                allowed_models: "[]".to_string(),
                scopes: "[\"manage\"]".to_string(),
            },
        )
        .unwrap();

        let listed = list_api_keys(&guard).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "ops");
        assert!(!listed[0].is_encrypted);
        assert!(find_usable_api_key(&guard, "sk-abc123").unwrap().is_some());

        assert!(set_api_key_revoked(&guard, "full-1", true).unwrap());
        assert!(
            find_usable_api_key(&guard, "sk-abc123")
                .unwrap()
                .is_none()
        );
        assert!(
            get_api_key_summary(&guard, "full-1")
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );

        assert!(set_api_key_revoked(&guard, "full-1", false).unwrap());
        assert!(find_usable_api_key(&guard, "sk-abc123").unwrap().is_some());
        assert!(!set_api_key_revoked(&guard, "missing", true).unwrap());
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
    fn combo_meta_patch_keeps_json_in_sync() {
        let db = migrated();
        let guard = db.connection();

        upsert_combo(
            &guard,
            &ComboRow {
                id: "c1".to_string(),
                name: "alpha".to_string(),
                data: r#"{"name":"alpha","sortOrder":1,"isHidden":false,"models":[]}"#
                    .to_string(),
                sort_order: 1,
            },
        )
        .unwrap();

        let updated = update_combo_meta(
            &guard,
            "c1",
            &ComboMetaPatch {
                sort_order: Some(9),
                is_hidden: Some(true),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert_eq!(updated.sort_order, 9);
        assert_eq!(updated.name, "alpha");
        let data: serde_json::Value = serde_json::from_str(&updated.data).unwrap();
        assert_eq!(data["sortOrder"], serde_json::json!(9));
        assert_eq!(data["isHidden"], serde_json::json!(true));
        assert_eq!(data["name"], serde_json::json!("alpha"));

        let renamed = update_combo_meta(
            &guard,
            "c1",
            &ComboMetaPatch {
                name: Some("beta".to_string()),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert_eq!(renamed.name, "beta");
        let data: serde_json::Value = serde_json::from_str(&renamed.data).unwrap();
        assert_eq!(data["name"], serde_json::json!("beta"));
        // Untouched fields survive.
        assert_eq!(data["sortOrder"], serde_json::json!(9));

        assert!(get_combo_by_name(&guard, "beta").unwrap().is_some());
        assert!(update_combo_meta(&guard, "missing", &ComboMetaPatch::default())
            .unwrap()
            .is_none());
    }

    #[test]
    fn combo_models_rewrite_round_trips() {
        let db = migrated();
        let guard = db.connection();

        upsert_combo(
            &guard,
            &ComboRow {
                id: "c1".to_string(),
                name: "mix".to_string(),
                data: r#"{"name":"mix","strategy":"priority","sortOrder":0,
                    "models":[
                        {"id":"keep-me","kind":"model","model":"groq/openai/gpt-oss-20b","providerId":"groq","weight":95,"label":"Fast"},
                        {"id":"legacy","kind":"widget","model":"x/y"}
                    ]}"#
                    .to_string(),
                sort_order: 0,
            },
        )
        .unwrap();

        let updated = update_combo_meta(
            &guard,
            "c1",
            &ComboMetaPatch {
                strategy: Some("auto".to_string()),
                models: Some(vec![
                    ComboModelSpec {
                        id: Some("keep-me".to_string()),
                        provider: "groq".to_string(),
                        // Joined verbatim: provider + bare id, slashes kept.
                        model: "openai/gpt-oss-20b".to_string(),
                        weight: 80,
                    },
                    ComboModelSpec {
                        id: None,
                        provider: "gemini".to_string(),
                        model: "gemini-3-flash-preview".to_string(),
                        weight: 70,
                    },
                ]),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        let data: serde_json::Value = serde_json::from_str(&updated.data).unwrap();
        assert_eq!(data["strategy"], serde_json::json!("auto"));
        let models = data["models"].as_array().unwrap();
        // Two rewritten models plus the preserved non-model item.
        assert_eq!(models.len(), 3);
        assert_eq!(models[0]["id"], serde_json::json!("keep-me"));
        assert_eq!(
            models[0]["model"],
            serde_json::json!("groq/openai/gpt-oss-20b")
        );
        assert_eq!(models[0]["weight"], serde_json::json!(80));
        // Unknown extra keys survive the rewrite via the matched id.
        assert_eq!(models[0]["label"], serde_json::json!("Fast"));
        assert_eq!(
            models[1]["model"],
            serde_json::json!("gemini/gemini-3-flash-preview")
        );
        assert!(
            models[1]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("mix-model-2-gemini-"))
        );
        assert_eq!(models[2]["id"], serde_json::json!("legacy"));
    }

    #[test]
    fn connection_meta_patch_toggles_active() {
        let db = migrated();
        let guard = db.connection();

        upsert_connection(&guard, &sample_connection("c1")).unwrap();

        let updated = update_connection_meta(
            &guard,
            "c1",
            &ConnectionMetaPatch {
                is_active: Some(false),
                priority: Some(3),
                ..Default::default()
            },
        )
        .unwrap()
        .expect("row exists");
        assert!(!updated.is_active);
        assert_eq!(updated.priority, 3);
        // Untouched columns survive.
        assert_eq!(updated.provider, "grok-cli");

        assert!(update_connection_meta(&guard, "missing", &ConnectionMetaPatch::default())
            .unwrap()
            .is_none());
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
