//! Credential loading from `provider_connections`.
//!
//! Ports the read side of `src/lib/db/providers.ts` +
//! `src/lib/db/encryption.ts`: active connections for a provider, ordered by
//! priority, with `access_token`/`api_key` decrypted via [`FieldCrypto`].
//! Rows whose secret cannot be decrypted are skipped rather than served
//! with a broken credential.

use omniroute_crypto::FieldCrypto;
use omniroute_db::Result;
use rusqlite::Connection;

/// One usable credential for a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    /// Connection row id.
    pub connection_id: String,
    /// Provider id.
    pub provider: String,
    /// Decrypted bearer token (OAuth access token or API key).
    pub token: String,
}

/// Pick the token column: OAuth access tokens first, then API keys.
fn token_source<'a>(access_token: Option<&'a str>, api_key: Option<&'a str>) -> Option<&'a str> {
    match (access_token, api_key) {
        (Some(token), _) if !token.is_empty() => Some(token),
        (_, Some(key)) if !key.is_empty() => Some(key),
        _ => None,
    }
}

/// Load usable credentials for `provider`, highest priority first.
///
/// `crypto` is optional: without it, values that are not encrypted (or when
/// no key is configured) pass through, while encrypted values are dropped.
pub fn load_credentials(
    conn: &Connection,
    provider: &str,
    crypto: Option<&FieldCrypto>,
) -> Result<Vec<Credential>> {
    let mut statement = conn.prepare(
        "SELECT id, provider, access_token, api_key, expires_at \
         FROM provider_connections \
         WHERE is_active = 1 AND provider = ?1 \
         ORDER BY priority DESC, id ASC",
    )?;
    let rows = statement.query_map(rusqlite::params![provider], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut credentials = Vec::new();
    for row in rows {
        let (connection_id, provider, access_token, api_key, expires_at) = row?;
        if is_expired(expires_at.as_deref()) {
            continue;
        }
        let Some(raw) = token_source(access_token.as_deref(), api_key.as_deref()) else {
            continue;
        };
        let token = match crypto {
            Some(crypto) => match crypto.decrypt(raw) {
                Some(token) => token,
                None => continue,
            },
            None => {
                if FieldCrypto::looks_encrypted(raw) {
                    continue;
                }
                raw.to_string()
            }
        };
        if token.is_empty() {
            continue;
        }
        credentials.push(Credential {
            connection_id,
            provider,
            token,
        });
    }
    Ok(credentials)
}

/// Distinct providers that have at least one active connection.
pub fn active_providers(conn: &Connection) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT DISTINCT provider FROM provider_connections WHERE is_active = 1 ORDER BY provider",
    )?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Whether a stored `expires_at` is in the past. Unparseable values are
/// treated as "not expired" so an odd timestamp never hides a credential.
fn is_expired(expires_at: Option<&str>) -> bool {
    let Some(raw) = expires_at else {
        return false;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    match parse_rfc3339_seconds(trimmed) {
        Some(expiry) => expiry <= now_seconds(),
        None => false,
    }
}

/// Minimal RFC3339 parser for `YYYY-MM-DDTHH:MM:SS(.fff)?(Z|±HH:MM)`.
/// Avoids a date-library dependency for a single comparison.
fn parse_rfc3339_seconds(value: &str) -> Option<i64> {
    let (date, rest) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    let mut time = rest.trim_end_matches('Z');
    let offset_seconds = if let Some((time_part, offset)) = time.rsplit_once(['+']) {
        time = time_part;
        parse_offset(offset)?
    } else if let Some((time_part, offset)) = time.rsplit_once('-') {
        // A '-' after the time section is a negative offset; guard against the
        // date separators already consumed.
        if offset.contains(':') {
            time = time_part;
            -parse_offset(offset)?
        } else {
            0
        }
    } else {
        0
    };

    let time = time.split('.').next()?;
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next().unwrap_or("0").parse().ok()?;

    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - offset_seconds,
    )
}

fn parse_offset(offset: &str) -> Option<i64> {
    let (hours, minutes) = offset.split_once(':')?;
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    Some(hours * 3600 + minutes * 60)
}

/// Days since 1970-01-01 (Howard Hinnant's civil-date algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_adjusted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_adjusted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omniroute_db::{Db, repos};

    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const FUTURE: &str = "2999-01-01T00:00:00.000Z";
    const PAST: &str = "2000-01-01T00:00:00.000Z";

    fn db_with_connections() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        let crypto = FieldCrypto::from_secret(SECRET).unwrap();
        repos::upsert_connection(
            &guard,
            &repos::ConnectionRow {
                id: "high".to_string(),
                provider: "grok-cli".to_string(),
                auth_type: Some("oauth".to_string()),
                name: None,
                email: None,
                priority: 10,
                is_active: true,
                access_token: Some(crypto.encrypt("tok-high")),
                refresh_token: None,
                expires_at: Some(FUTURE.to_string()),
                api_key: None,
                provider_specific_data: None,
            },
        )
        .unwrap();
        repos::upsert_connection(
            &guard,
            &repos::ConnectionRow {
                id: "low".to_string(),
                provider: "grok-cli".to_string(),
                auth_type: Some("apikey".to_string()),
                name: None,
                email: None,
                priority: 1,
                is_active: true,
                access_token: None,
                refresh_token: None,
                expires_at: None,
                api_key: Some("plain-api-key".to_string()),
                provider_specific_data: None,
            },
        )
        .unwrap();
        repos::upsert_connection(
            &guard,
            &repos::ConnectionRow {
                id: "expired".to_string(),
                provider: "grok-cli".to_string(),
                auth_type: Some("oauth".to_string()),
                name: None,
                email: None,
                priority: 99,
                is_active: true,
                access_token: Some(crypto.encrypt("tok-expired")),
                refresh_token: None,
                expires_at: Some(PAST.to_string()),
                api_key: None,
                provider_specific_data: None,
            },
        )
        .unwrap();
        drop(guard);
        db
    }

    #[test]
    fn loads_active_credentials_ordered_by_priority() {
        let db = db_with_connections();
        let guard = db.connection();
        let crypto = FieldCrypto::from_secret(SECRET).unwrap();
        let credentials = load_credentials(&guard, "grok-cli", Some(&crypto)).unwrap();

        let ids: Vec<&str> = credentials
            .iter()
            .map(|credential| credential.connection_id.as_str())
            .collect();
        assert_eq!(ids, vec!["high", "low"]);
        assert_eq!(credentials[0].token, "tok-high");
        assert_eq!(credentials[1].token, "plain-api-key");
    }

    #[test]
    fn skips_expired_and_undecryptable() {
        let db = db_with_connections();
        let guard = db.connection();
        // No crypto: encrypted rows are dropped, the plaintext key survives.
        let credentials = load_credentials(&guard, "grok-cli", None).unwrap();
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].connection_id, "low");

        // Wrong key: nothing decrypts, so only the plaintext key remains.
        let wrong = FieldCrypto::from_secret("ffffffff").unwrap();
        let credentials = load_credentials(&guard, "grok-cli", Some(&wrong)).unwrap();
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].connection_id, "low");
    }

    #[test]
    fn rfc3339_parser_handles_offsets_and_fractions() {
        assert_eq!(parse_rfc3339_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_seconds("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_rfc3339_seconds("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339_seconds("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(parse_rfc3339_seconds("garbage"), None);
    }
}
