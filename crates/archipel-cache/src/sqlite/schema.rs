//! Schema migrations driven by `PRAGMA user_version`.
//!
//! `MIGRATIONS[i]` upgrades a database from version `i` to version `i + 1`; a
//! fresh database has version 0. Steps are append-only: never edit a released
//! step, add a new one.

use archipel_model::StoreError;
use rusqlite::Connection;

use super::{db, write_tx};

/// v1: per-project records (summaries, UI state, registry, private projects).
const V1_PROJECTS: &str = "
CREATE TABLE summaries (
    project  TEXT    NOT NULL,
    position INTEGER NOT NULL,
    repo     INTEGER NOT NULL,
    data     TEXT    NOT NULL,
    PRIMARY KEY (project, position)
) WITHOUT ROWID;
CREATE TABLE ui_state (
    project TEXT NOT NULL PRIMARY KEY,
    data    TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE registry (
    id   TEXT NOT NULL PRIMARY KEY,
    data TEXT NOT NULL
) WITHOUT ROWID;
CREATE TABLE private_projects (
    id   TEXT NOT NULL PRIMARY KEY,
    data TEXT NOT NULL
) WITHOUT ROWID;
";

/// v2: the operation journal. `AUTOINCREMENT` guarantees ids are never
/// reused, even after pruning removed the newest rows.
const V2_JOURNAL: &str = "
CREATE TABLE journal (
    id    INTEGER PRIMARY KEY AUTOINCREMENT,
    repo  INTEGER NOT NULL,
    batch INTEGER,
    data  TEXT    NOT NULL
);
CREATE INDEX journal_by_repo ON journal (repo, id);
CREATE INDEX journal_by_batch ON journal (batch, id) WHERE batch IS NOT NULL;
";

/// v3: incremental statistics.
const V3_STATS: &str = "
CREATE TABLE stats_cursor (
    repo INTEGER NOT NULL PRIMARY KEY,
    oid  TEXT    NOT NULL
);
CREATE TABLE stats_activity (
    repo    INTEGER NOT NULL,
    week    INTEGER NOT NULL,
    author  TEXT    NOT NULL,
    commits INTEGER NOT NULL,
    PRIMARY KEY (repo, week, author)
) WITHOUT ROWID;
CREATE TABLE stats_churn (
    repo    INTEGER NOT NULL,
    path    TEXT    NOT NULL,
    added   INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    commits INTEGER NOT NULL,
    PRIMARY KEY (repo, path)
) WITHOUT ROWID;
";

/// Every migration step, in order.
const MIGRATIONS: &[&str] = &[V1_PROJECTS, V2_JOURNAL, V3_STATS];

/// Version reached after every step of [`MIGRATIONS`] (checked by a test).
pub(super) const LATEST_VERSION: u32 = 3;

/// Current `user_version` of the database.
pub(super) fn user_version(conn: &Connection) -> Result<u32, StoreError> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(db)?;
    u32::try_from(version)
        .map_err(|_| StoreError::Corrupt(format!("invalid schema version {version}")))
}

/// Migrates the database to [`LATEST_VERSION`].
pub(super) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    migrate_to(conn, LATEST_VERSION)
}

/// Applies the steps needed to reach `target`, one transaction per step.
/// A database newer than this build is refused.
pub(super) fn migrate_to(conn: &mut Connection, target: u32) -> Result<(), StoreError> {
    loop {
        let tx = write_tx(conn)?;
        // Read inside the write transaction: another process may have migrated meanwhile.
        let current = user_version(&tx)?;
        if current > LATEST_VERSION {
            return Err(StoreError::Backend(format!(
                "database schema version {current} is newer than supported version {LATEST_VERSION}"
            )));
        }
        if current >= target {
            return Ok(());
        }
        let step = usize::try_from(current)
            .ok()
            .and_then(|index| MIGRATIONS.get(index))
            .ok_or_else(|| StoreError::Backend(format!("no migration from version {current}")))?;
        tx.execute_batch(step).map_err(db)?;
        tx.pragma_update(None, "user_version", current + 1)
            .map_err(db)?;
        tx.commit().map_err(db)?;
        tracing::debug!(version = current + 1, "store schema migrated");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_version_matches_migration_count() {
        assert_eq!(usize::try_from(LATEST_VERSION).unwrap(), MIGRATIONS.len());
    }
}
