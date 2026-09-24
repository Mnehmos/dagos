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

mod context;
mod conversations;
mod dag;
mod defaults;
mod events;
mod migrations;
mod projects;
mod runs;
mod sql;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

use crate::domain::{
    Clock, EdgeType, Event, IdGenerator, NodeId, ProjectId, RandomIds, RunId, RunStatus,
    SystemClock, Timestamp,
};

pub use conversations::{MAX_TITLE_CHARS, clean_title};
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
    #[error("run `{running}` is still running in this project")]
    RunInProgress { running: RunId },
    #[error("run `{id}` already finished with status `{status}`")]
    RunFinished { id: RunId, status: RunStatus },
    #[error("{0}")]
    Invalid(&'static str),
    #[error("node `{0}` appears more than once in the active context")]
    DuplicateContextMember(NodeId),
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
    listener: Option<EventListener>,
}

/// Receives the events of each committed transaction, in sequence order.
pub type EventListener = Box<dyn Fn(&[Event]) + Send + Sync>;

impl Store {
    /// Opens (creating if needed) the database at `path` and migrates it to [`SCHEMA_VERSION`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        // Every streamed delta is its own committed event, so commits must be cheap. In WAL mode
        // with `synchronous = NORMAL`, a commit appends to the log without waiting for the disk;
        // the database can never be corrupted, and at worst the last moments before a power
        // failure are lost (a run cut off that way is failed as interrupted on restart).
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::from_connection(conn)
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
            clock: Box::new(SystemClock),
            ids: Box::new(RandomIds),
            listener: None,
        })
    }

    /// Calls `listener` after every committed transaction that appended events, with those events
    /// in order. Events from rolled-back transactions are never observed. The listener runs after
    /// the store is released, so it may read the store.
    pub fn with_event_listener(
        mut self,
        listener: impl Fn(&[Event]) + Send + Sync + 'static,
    ) -> Self {
        self.listener = Some(Box::new(listener));
        self
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
        let _entered = Entered::new();
        Ok(migrations::schema_version(&self.lock())?)
    }

    /// Runs `work` in one transaction: committed if it returns `Ok`, rolled back otherwise.
    ///
    /// Transactions start `IMMEDIATE`, taking the write lock up front, so concurrent DAGOS
    /// processes wait on each other (up to the busy timeout) instead of failing mid-transaction.
    ///
    /// # Panics
    /// If called from inside another transaction on the same thread, which would otherwise
    /// deadlock: do the work with the outer transaction's [`Tx`] instead.
    pub fn transaction<T, E>(&self, work: impl FnOnce(&Tx<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let entered = Entered::new();
        let (value, appended) = {
            let mut conn = self.lock();
            let transaction = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::from)?;
            let tx = Tx {
                conn: transaction,
                clock: self.clock.as_ref(),
                ids: self.ids.as_ref(),
                appended: self.listener.as_ref().map(|_| RefCell::default()),
            };
            let value = work(&tx)?;
            let appended = tx.appended.map(RefCell::into_inner).unwrap_or_default();
            tx.conn.commit().map_err(StoreError::from)?;
            (value, appended)
        };
        drop(entered);
        if let Some(listener) = &self.listener
            && !appended.is_empty()
        {
            listener(&appended);
        }
        Ok(value)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock drops any open transaction, which rolls it back, so the
        // connection is still consistent and safe to reuse.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

thread_local! {
    static IN_STORE: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as inside the store for its lifetime. The store's mutex is not
/// reentrant, so re-entering from the same thread would deadlock; this turns that into a panic.
struct Entered;

impl Entered {
    fn new() -> Self {
        assert!(
            !IN_STORE.get(),
            "Store::transaction called while this thread is already inside the store; \
             nesting would deadlock, so use the outer transaction's Tx instead"
        );
        IN_STORE.set(true);
        Entered
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        IN_STORE.set(false);
    }
}

/// An open transaction. Repository methods are grouped by record kind in the store's submodules.
pub struct Tx<'a> {
    conn: rusqlite::Transaction<'a>,
    clock: &'a dyn Clock,
    ids: &'a dyn IdGenerator,
    /// Events appended in this transaction, collected only when a listener wants them.
    appended: Option<RefCell<Vec<Event>>>,
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
