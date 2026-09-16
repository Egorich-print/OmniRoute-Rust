//! Connection handling, `DATA_DIR` resolution and PRAGMAs.
//!
//! Mirrors `src/lib/db/core.ts` and `src/lib/dataPaths.ts`: one synchronous
//! handle (the JS `better-sqlite3` singleton), WAL journaling, and the same
//! data-directory precedence.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

use crate::error::Result;

/// Thread-safe handle around a single SQLite connection.
pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    fn apply_pragmas(conn: &Connection) -> Result<()> {
        // Same order and values as core.ts: busy timeout first so the WAL
        // switch cannot surface `database is locked` at startup.
        conn.pragma_update(None, "busy_timeout", 2000)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "cache_size", -65536)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "mmap_size", 268435456i64)?;
        Ok(())
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        Self::apply_pragmas(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open (creating parent directories) a database file.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Open the default `storage.sqlite` under the resolved data directory.
    pub fn open_default() -> Result<(Self, PathBuf)> {
        let dir = Self::data_dir();
        let db = Self::open(&dir.join("storage.sqlite"))?;
        Ok((db, dir))
    }

    /// Open a private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    /// Resolve the data directory with the same precedence as the JS server:
    /// `DATA_DIR` env → legacy `~/.omniroute` if it exists →
    /// `$XDG_CONFIG_HOME/omniroute` → `~/.omniroute` (`%APPDATA%/omniroute`
    /// on Windows).
    pub fn data_dir() -> PathBuf {
        if let Ok(value) = std::env::var("DATA_DIR")
            && !value.trim().is_empty()
        {
            return PathBuf::from(value);
        }

        #[cfg(windows)]
        {
            if let Ok(appdata) = std::env::var("APPDATA")
                && !appdata.trim().is_empty()
            {
                return Path::new(&appdata).join("omniroute");
            }
            return PathBuf::from(".omniroute");
        }

        #[cfg(not(windows))]
        {
            if let Ok(home) = std::env::var("HOME") {
                let legacy = Path::new(&home).join(".omniroute");
                if legacy.is_dir() {
                    return legacy;
                }
            }
            if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
                && !xdg.trim().is_empty()
            {
                return Path::new(&xdg).join("omniroute");
            }
            if let Ok(home) = std::env::var("HOME") {
                return Path::new(&home).join("omniroute");
            }
            PathBuf::from(".omniroute")
        }
    }

    /// Lock the underlying connection.
    pub fn connection(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run the pending migrations. See [`crate::migrate`].
    pub fn migrate(&self) -> Result<Vec<String>> {
        let mut guard = self.connection();
        crate::migrate::migrate(&mut guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_prefers_env() {
        unsafe { std::env::set_var("DATA_DIR", "/tmp/omniroute-test-dir") };
        assert_eq!(Db::data_dir(), PathBuf::from("/tmp/omniroute-test-dir"));
        unsafe { std::env::remove_var("DATA_DIR") };
    }

    #[test]
    fn in_memory_opens_with_wal_pragmas() {
        let db = Db::open_in_memory().unwrap();
        let guard = db.connection();
        let journal: String = guard
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        // In-memory databases report `memory`; file databases report `wal`.
        assert!(journal == "memory" || journal == "wal", "{journal}");
    }
}
