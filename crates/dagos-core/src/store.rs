//! Store layer: SQLite persistence for projects, DAG nodes and edges, runs, events, and active
//! context.
//!
//! All persistence concerns live here. Active-context rows are stored independently of DAG nodes:
//! removing a context row never deletes a node. The schema itself enforces the durable-state
//! invariants (see `migrations/0001_initial.sql`), so they hold even for code that bypasses the
//! repository API.

mod migrations;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::Connection;

pub use migrations::SCHEMA_VERSION;

/// Errors raised by the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "database schema version {found} is newer than this DAGOS build supports ({supported})"
    )]
    SchemaTooNew { found: u32, supported: u32 },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Handle to one DAGOS SQLite database.
///
/// v0.1 serves one local project with one active run at a time, so a single connection behind a
/// mutex is enough; each operation holds the lock only for its own transaction.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Opens (creating if needed) the database at `path` and migrates it to [`SCHEMA_VERSION`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::from_connection(Connection::open(path)?)
    }

    /// Opens a private in-memory database, migrated to [`SCHEMA_VERSION`].
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        migrations::migrate(&mut conn, migrations::MIGRATIONS)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// The schema version recorded in the database.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(migrations::schema_version(&self.lock())?)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock drops any open transaction, which rolls it back, so the
        // connection is still consistent and safe to reuse.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod schema_tests;
