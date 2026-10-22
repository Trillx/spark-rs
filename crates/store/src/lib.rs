//! Local storage for spark-rs: one SQLite database per profile.

use std::path::Path;

use rusqlite::Connection;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::configure(Connection::open(path)?)
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::configure(Connection::open_in_memory()?)
    }

    fn configure(conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(Self { conn })
    }

    /// Version of the SQLite library compiled into the binary.
    pub fn sqlite_version() -> &'static str {
        rusqlite::version()
    }

    /// Whether full-text search (FTS5) is available; message search depends on it.
    pub fn has_fts5(&self) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| row.get(0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_sqlite_has_fts5() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.has_fts5().unwrap());
    }

    #[test]
    fn opens_file_database_in_wal_mode() {
        let dir = std::env::temp_dir().join(format!("spark-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("test.sqlite")).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(store);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
