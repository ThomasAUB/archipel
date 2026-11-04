//! SQLite implementation of every store trait of `archipel-model`
//! (PLAN §2.1 "Persistent local data").
//!
//! One [`SqliteStore`] holds a single connection behind a mutex: every call is
//! short, so contention is not a concern. Records are stored as JSON
//! (`serde_json`) next to the indexed key columns they are looked up by.
//!
//! # Encoding choices
//!
//! - [`RepoId`](archipel_model::RepoId) and
//!   [`BatchId`](archipel_model::journal::BatchId) are `u64`, SQLite integers
//!   are signed 64-bit. They are stored as the `i64` with the **same bit
//!   pattern**: lossless, reversible, and still a compact integer key usable by
//!   indexes. Ordering on those columns is therefore meaningless; no query
//!   relies on it.
//! - Object ids are stored as lowercase hexadecimal text.
//! - Counters (`u32`/`u64`) are stored as `i64`. A value that does not fit is
//!   refused with [`StoreError::Backend`]; a negative or out-of-range value read
//!   back is reported as [`StoreError::Corrupt`].
//! - Undecodable JSON is reported as [`StoreError::Corrupt`], any SQLite
//!   failure as [`StoreError::Backend`].
//!
//! No `rusqlite` type leaves this module (ARCHITECTURE §2.3).

mod codec;
mod journal;
mod registry;
mod schema;
mod stats;
mod summary;

#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use archipel_model::StoreError;
use rusqlite::{Connection, Transaction, TransactionBehavior};

/// How long a statement waits for a lock held by another connection (another
/// process using the same database file) before failing with "database is busy".
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The SQLite store: summaries, UI state, project registry, the operation
/// journal and statistics, in one database. Project definitions are not in
/// it: they live in `archipel.toml` files.
#[derive(Debug)]
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Schema version this build creates and understands (`PRAGMA user_version`).
    pub const SCHEMA_VERSION: u32 = schema::LATEST_VERSION;

    /// Opens the database at `path` (creating the file and its parent
    /// directories if needed), then migrates it to [`Self::SCHEMA_VERSION`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| {
                StoreError::Backend(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        let conn = Connection::open(path).map_err(db)?;
        Self::init(conn)
    }

    /// Opens a private in-memory database (lost when dropped), already migrated.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory().map_err(db)?)
    }

    /// Current schema version of the database.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        schema::user_version(&self.lock())
    }

    fn init(mut conn: Connection) -> Result<Self, StoreError> {
        configure(&conn)?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// The connection. A poisoned lock is recovered: an interrupted
    /// transaction was rolled back when it was dropped, so the connection is
    /// in a consistent state.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Connection settings: WAL journal (readers never block the writer),
/// `synchronous = NORMAL` (safe with WAL, no fsync per commit), busy timeout
/// and foreign keys.
fn configure(conn: &Connection) -> Result<(), StoreError> {
    conn.busy_timeout(BUSY_TIMEOUT).map_err(db)?;
    // `journal_mode` answers with the resulting mode ("memory" for in-memory DBs).
    let _mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .map_err(db)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(db)?;
    conn.pragma_update(None, "foreign_keys", true).map_err(db)?;
    Ok(())
}

/// Starts a write transaction, taking the write lock up front so concurrent
/// writers wait (busy timeout) instead of failing on lock upgrade.
fn write_tx(conn: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db)
}

/// Converts a SQLite error into a store error.
#[allow(clippy::needless_pass_by_value)] // shaped for `map_err(db)`
fn db(error: rusqlite::Error) -> StoreError {
    StoreError::Backend(error.to_string())
}
