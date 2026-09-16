//! Typed access to the `key_value` settings namespaces.
//!
//! Mirrors the `settings` / `pricing` / `databaseSettings` namespaces in
//! `src/lib/db/settings.ts`. Values are JSON-encoded so structured settings
//! round-trip without a schema migration.

use rusqlite::Connection;
use serde::{Serialize, de::DeserializeOwned};

use omniroute_db::{Result, repos};

/// Typed settings over a SQLite connection.
pub struct Settings<'a> {
    conn: &'a Connection,
}

impl<'a> Settings<'a> {
    /// Borrow settings backed by an open connection.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Read a raw string setting.
    pub fn get(&self, namespace: &str, key: &str) -> Result<Option<String>> {
        repos::kv_get(self.conn, namespace, key)
    }

    /// Write a raw string setting.
    pub fn set(&self, namespace: &str, key: &str, value: &str) -> Result<()> {
        repos::kv_set(self.conn, namespace, key, value)
    }

    /// Read a JSON-encoded setting.
    pub fn get_json<T: DeserializeOwned>(&self, namespace: &str, key: &str) -> Result<Option<T>> {
        match repos::kv_get(self.conn, namespace, key)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// Write a JSON-encoded setting.
    pub fn set_json<T: Serialize>(&self, namespace: &str, key: &str, value: &T) -> Result<()> {
        let raw = serde_json::to_string(value)?;
        repos::kv_set(self.conn, namespace, key, &raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omniroute_db::Db;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Theme {
        mode: String,
    }

    #[test]
    fn json_settings_round_trip() {
        let db = Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let guard = db.connection();
        let settings = Settings::new(&guard);

        assert_eq!(
            settings.get_json::<Theme>("settings", "theme").unwrap(),
            None
        );
        settings
            .set_json(
                "settings",
                "theme",
                &Theme {
                    mode: "dark".to_string(),
                },
            )
            .unwrap();
        assert_eq!(
            settings.get_json::<Theme>("settings", "theme").unwrap(),
            Some(Theme {
                mode: "dark".to_string()
            })
        );
    }
}
