//! Store layer: SQLite persistence for projects, DAG nodes and edges, runs, events, and active
//! context.
//!
//! All persistence concerns live here. Work happens inside [`Store::transaction`]: the closure
//! receives a [`Tx`] whose repository methods all run in one SQLite transaction, committed when the
//! closure returns `Ok` and rolled back otherwise. The store owns the injected [`Clock`] and
//! [`IdGenerator`], so every record it creates gets its ID and timestamps from one place.
//!
//! Active-context rows are stored independently of DAG nodes: removing a context row never deletes
//! a node. The schema itself enforces the durable-state invariants (see
//! `migrations/0001_initial.sql`), so they hold even for code that bypasses this API.

mod dag;
mod migrations;
mod projects;
mod sql;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

use crate::domain::{
    Clock, EdgeType, IdGenerator, NodeId, ProjectId, RandomIds, SystemClock, Timestamp,
};

pub use migrations::SCHEMA_VERSION;

/// Errors raised by the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "database schema version {found} is newer than this DAGOS build supports ({supported})"
    )]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("{kind} `{id}` not found")]
    NotFound { kind: &'static str, id: String },
    #[error(transparent)]
    Dag(#[from] DagViolation),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// A mutation the durable DAG refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DagViolation {
    #[error("node `{0}` does not exist in this project")]
    UnknownNode(NodeId),
    #[error("an edge cannot connect node `{0}` to itself")]
    SelfLoop(NodeId),
    #[error("edge `{from} {edge_type} {to}` already exists")]
    DuplicateEdge { from: NodeId, to: NodeId, edge_type: EdgeType },
    #[error("edge `{from} {edge_type} {to}` would create a cycle")]
    Cycle { from: NodeId, to: NodeId, edge_type: EdgeType },
}

/// Handle to one DAGOS SQLite database.
///
/// v0.1 serves one local project with one active run at a time, so a single connection behind a
/// mutex is enough; each transaction holds the lock only while it runs.
pub struct Store {
    conn: Mutex<Connection>,
    clock: Box<dyn Clock>,
    ids: Box<dyn IdGenerator>,
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
        Ok(Self { conn: Mutex::new(conn), clock: Box::new(SystemClock), ids: Box::new(RandomIds) })
    }

    /// Replaces the clock used for new records (e.g. a stepping clock in tests).
    pub fn with_clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// Replaces the ID generator used for new records (e.g. sequential IDs in tests).
    pub fn with_ids(mut self, ids: impl IdGenerator + 'static) -> Self {
        self.ids = Box::new(ids);
        self
    }

    /// The schema version recorded in the database.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        Ok(migrations::schema_version(&self.lock())?)
    }

    /// Runs `work` in one transaction: committed if it returns `Ok`, rolled back otherwise.
    ///
    /// Transactions start `IMMEDIATE`, taking the write lock up front, so concurrent DAGOS
    /// processes wait on each other (up to the busy timeout) instead of failing mid-transaction.
    pub fn transaction<T, E>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let mut conn = self.lock();
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let tx = Tx { conn: transaction, clock: self.clock.as_ref(), ids: self.ids.as_ref() };
        let value = work(&tx)?;
        tx.conn.commit().map_err(StoreError::from)?;
        Ok(value)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock drops any open transaction, which rolls it back, so the
        // connection is still consistent and safe to reuse.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An open transaction. Repository methods are grouped by record kind in the store's submodules.
pub struct Tx<'a> {
    conn: rusqlite::Transaction<'a>,
    clock: &'a dyn Clock,
    ids: &'a dyn IdGenerator,
}

impl Tx<'_> {
    fn now(&self) -> Timestamp {
        self.clock.now()
    }

    fn not_found(kind: &'static str, id: impl ToString) -> StoreError {
        StoreError::NotFound { kind, id: id.to_string() }
    }

    fn require_project(&self, project_id: &ProjectId) -> Result<(), StoreError> {
        if self.project(project_id)?.is_some() {
            Ok(())
        } else {
            Err(Self::not_found("project", project_id))
        }
    }
}

#[cfg(test)]
mod schema_tests;
