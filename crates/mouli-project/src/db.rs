//! SQLite persistence: the whole store as one JSON snapshot row.

use crate::model::Store;
use crate::ProjectError;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

fn storage(e: impl std::fmt::Display) -> ProjectError {
    ProjectError::Storage(e.to_string())
}

/// Database handle.
#[derive(Debug)]
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (or create) the database at `path`.
    pub fn open(path: &Path) -> Result<Self, ProjectError> {
        Self::init(Connection::open(path).map_err(storage)?)
    }

    /// In-memory database (tests, demos).
    pub fn in_memory() -> Result<Self, ProjectError> {
        Self::init(Connection::open_in_memory().map_err(storage)?)
    }

    fn init(conn: Connection) -> Result<Self, ProjectError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mouli_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                payload TEXT NOT NULL,
                updated_at TEXT NOT NULL)",
        )
        .map_err(storage)?;
        Ok(Self { conn })
    }

    /// Save the snapshot (single statement, so it is atomic).
    pub fn save(&self, store: &Store) -> Result<(), ProjectError> {
        let payload = serde_json::to_string(store).map_err(storage)?;
        self.conn
            .execute(
                "INSERT INTO mouli_state (id, payload, updated_at) VALUES (1, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, updated_at = excluded.updated_at",
                params![payload, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Load the snapshot, or an empty store if none was saved.
    pub fn load(&self) -> Result<Store, ProjectError> {
        let payload: Option<String> = self
            .conn
            .query_row("SELECT payload FROM mouli_state WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(storage)?;
        match payload {
            Some(p) => serde_json::from_str(&p).map_err(storage),
            None => Ok(Store::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::sample_store;

    #[test]
    fn save_load_roundtrip() {
        let db = Db::in_memory().unwrap();
        assert_eq!(db.load().unwrap(), Store::default());
        let s = sample_store();
        db.save(&s).unwrap();
        db.save(&s).unwrap();
        assert_eq!(db.load().unwrap(), s);
    }

    #[test]
    fn file_database_persists() {
        let path = std::env::temp_dir().join(format!("mouli-project-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let s = sample_store();
        Db::open(&path).unwrap().save(&s).unwrap();
        assert_eq!(Db::open(&path).unwrap().load().unwrap(), s);
        std::fs::remove_file(&path).unwrap();
    }
}
