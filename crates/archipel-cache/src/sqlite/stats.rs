//! Incrementally computed statistics (PLAN Phase 7).

use archipel_model::ports::store::StatsStore;
use archipel_model::stats::{ActivityBucket, Hotspot, StatsDelta};
use archipel_model::{Oid, RepoId, StoreError};
use rusqlite::{OptionalExtension, params};

use super::codec::{repo_to_sql, u32_from_sql, u64_from_sql, u64_to_sql, usize_to_sql};
use super::{SqliteStore, db, write_tx};

/// Name of an activity record in error messages.
const ACTIVITY: &str = "activity bucket";
/// Name of a churn record in error messages.
const CHURN: &str = "hotspot";

impl StatsStore for SqliteStore {
    fn cursor(&self, repo: RepoId) -> Result<Option<Oid>, StoreError> {
        let hex: Option<String> = self
            .lock()
            .prepare_cached("SELECT oid FROM stats_cursor WHERE repo = ?1")
            .and_then(|mut stmt| {
                stmt.query_row(params![repo_to_sql(repo)], |row| row.get(0))
                    .optional()
            })
            .map_err(db)?;
        hex.map(|hex| {
            Oid::from_hex(&hex).map_err(|e| StoreError::Corrupt(format!("stats cursor: {e}")))
        })
        .transpose()
    }

    fn apply_delta(&self, repo: RepoId, delta: &StatsDelta, cursor: Oid) -> Result<(), StoreError> {
        let repo = repo_to_sql(repo);
        let mut conn = self.lock();
        let tx = write_tx(&mut conn)?;
        {
            let mut upsert = tx
                .prepare_cached(
                    "INSERT INTO stats_activity (repo, week, author, commits) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (repo, week, author)
                     DO UPDATE SET commits = commits + excluded.commits",
                )
                .map_err(db)?;
            for bucket in &delta.activity {
                upsert
                    .execute(params![
                        repo,
                        bucket.week,
                        bucket.author,
                        i64::from(bucket.commits)
                    ])
                    .map_err(db)?;
            }
        }
        {
            let mut upsert = tx
                .prepare_cached(
                    "INSERT INTO stats_churn (repo, path, added, removed, commits)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (repo, path) DO UPDATE SET
                         added = added + excluded.added,
                         removed = removed + excluded.removed,
                         commits = commits + excluded.commits",
                )
                .map_err(db)?;
            for hotspot in &delta.churn {
                upsert
                    .execute(params![
                        repo,
                        hotspot.path,
                        u64_to_sql(hotspot.added)?,
                        u64_to_sql(hotspot.removed)?,
                        i64::from(hotspot.commits)
                    ])
                    .map_err(db)?;
            }
        }
        tx.prepare_cached(
            "INSERT INTO stats_cursor (repo, oid) VALUES (?1, ?2)
             ON CONFLICT (repo) DO UPDATE SET oid = excluded.oid",
        )
        .and_then(|mut stmt| stmt.execute(params![repo, cursor.to_hex()]))
        .map_err(db)?;
        tx.commit().map_err(db)
    }

    /// Activity buckets of a repo, ordered by week then author.
    fn activity(&self, repo: RepoId) -> Result<Vec<ActivityBucket>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT week, author, commits FROM stats_activity WHERE repo = ?1
                 ORDER BY week, author",
            )
            .map_err(db)?;
        let rows: Vec<(i64, String, i64)> = stmt
            .query_map(params![repo_to_sql(repo)], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .and_then(Iterator::collect)
            .map_err(db)?;
        rows.into_iter()
            .map(|(week, author, commits)| {
                Ok(ActivityBucket {
                    week,
                    author,
                    commits: u32_from_sql(commits, ACTIVITY)?,
                })
            })
            .collect()
    }

    /// Paths ordered by churn (added + removed) descending, then path.
    fn hotspots(&self, repo: RepoId, limit: usize) -> Result<Vec<Hotspot>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare_cached(
                "SELECT path, added, removed, commits FROM stats_churn WHERE repo = ?1
                 ORDER BY added + removed DESC, path LIMIT ?2",
            )
            .map_err(db)?;
        let rows: Vec<(String, i64, i64, i64)> = stmt
            .query_map(params![repo_to_sql(repo), usize_to_sql(limit)], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .and_then(Iterator::collect)
            .map_err(db)?;
        rows.into_iter()
            .map(|(path, added, removed, commits)| {
                Ok(Hotspot {
                    path,
                    added: u64_from_sql(added, CHURN)?,
                    removed: u64_from_sql(removed, CHURN)?,
                    commits: u32_from_sql(commits, CHURN)?,
                })
            })
            .collect()
    }

    fn reset_stats(&self, repo: RepoId) -> Result<(), StoreError> {
        let repo = repo_to_sql(repo);
        let mut conn = self.lock();
        let tx = write_tx(&mut conn)?;
        for sql in [
            "DELETE FROM stats_cursor WHERE repo = ?1",
            "DELETE FROM stats_activity WHERE repo = ?1",
            "DELETE FROM stats_churn WHERE repo = ?1",
        ] {
            tx.execute(sql, params![repo]).map_err(db)?;
        }
        tx.commit().map_err(db)
    }
}
