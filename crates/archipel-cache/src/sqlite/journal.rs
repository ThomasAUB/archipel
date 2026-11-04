//! The bounded operation journal (PLAN §2.4, Phase 6).
//!
//! The `id` column is authoritative: the `id` inside the JSON record is
//! ignored and replaced by the column value when an entry is read.

use archipel_model::journal::{BatchId, EntryState, JournalEntry, JournalId};
use archipel_model::ports::store::JournalStore;
use archipel_model::{RepoId, StoreError};
use rusqlite::{Connection, OptionalExtension, params};

use super::codec::{batch_to_sql, from_json, repo_to_sql, to_json, usize_to_sql};
use super::{SqliteStore, db, write_tx};

/// Name of a journal record in error messages.
const JOURNAL_ENTRY: &str = "journal entry";

impl JournalStore for SqliteStore {
    fn append(&self, entry: &JournalEntry, limit: usize) -> Result<JournalId, StoreError> {
        let data = to_json(entry, JOURNAL_ENTRY)?;
        let repo = repo_to_sql(entry.repo);
        let mut conn = self.lock();
        let tx = write_tx(&mut conn)?;
        tx.prepare_cached("INSERT INTO journal (repo, batch, data) VALUES (?1, ?2, ?3)")
            .and_then(|mut stmt| stmt.execute(params![repo, entry.batch.map(batch_to_sql), data]))
            .map_err(db)?;
        let id = tx.last_insert_rowid();
        // Delete everything up to the newest entry beyond the `limit` most recent.
        tx.prepare_cached(
            "DELETE FROM journal WHERE repo = ?1 AND id <= (
                 SELECT id FROM journal WHERE repo = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2
             )",
        )
        .and_then(|mut stmt| stmt.execute(params![repo, usize_to_sql(limit)]))
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(JournalId(id))
    }

    fn recent(&self, repo: RepoId, limit: usize) -> Result<Vec<JournalEntry>, StoreError> {
        query_entries(
            &self.lock(),
            "SELECT id, data FROM journal WHERE repo = ?1 ORDER BY id DESC LIMIT ?2",
            &[&repo_to_sql(repo), &usize_to_sql(limit)],
        )
    }

    fn batch(&self, batch: BatchId) -> Result<Vec<JournalEntry>, StoreError> {
        query_entries(
            &self.lock(),
            "SELECT id, data FROM journal WHERE batch = ?1 ORDER BY id",
            &[&batch_to_sql(batch)],
        )
    }

    fn set_state(&self, id: JournalId, state: EntryState) -> Result<(), StoreError> {
        let mut conn = self.lock();
        let tx = write_tx(&mut conn)?;
        let data: String = tx
            .prepare_cached("SELECT data FROM journal WHERE id = ?1")
            .and_then(|mut stmt| stmt.query_row(params![id.0], |row| row.get(0)).optional())
            .map_err(db)?
            .ok_or_else(|| StoreError::NotFound(format!("journal entry {}", id.0)))?;
        let mut entry: JournalEntry = from_json(&data, JOURNAL_ENTRY)?;
        entry.id = id;
        entry.state = state;
        let data = to_json(&entry, JOURNAL_ENTRY)?;
        tx.prepare_cached("UPDATE journal SET data = ?2 WHERE id = ?1")
            .and_then(|mut stmt| stmt.execute(params![id.0, data]))
            .map_err(db)?;
        tx.commit().map_err(db)
    }
}

/// Runs a query selecting `(id, data)` journal rows and decodes them.
fn query_entries(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<JournalEntry>, StoreError> {
    let mut stmt = conn.prepare_cached(sql).map_err(db)?;
    let rows: Vec<(i64, String)> = stmt
        .query_map(params, |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(Iterator::collect)
        .map_err(db)?;
    rows.into_iter()
        .map(|(id, data)| {
            let mut entry: JournalEntry = from_json(&data, JOURNAL_ENTRY)?;
            entry.id = JournalId(id);
            Ok(entry)
        })
        .collect()
}
