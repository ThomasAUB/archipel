//! Dashboard summaries and per-project UI state.

use archipel_model::ports::store::{SummaryStore, UiStateStore};
use archipel_model::summary::RepoSummary;
use archipel_model::ui_state::UiState;
use archipel_model::{ProjectId, RepoId, StoreError};
use rusqlite::params;

use super::codec::{
    from_json, load_json, repo_from_sql, repo_to_sql, save_json, to_json, usize_to_sql,
};
use super::{SqliteStore, db, write_tx};

/// Name of a summary record in error messages.
const SUMMARY: &str = "repository summary";
/// Name of a UI state record in error messages.
const UI_STATE: &str = "UI state";

impl SummaryStore for SqliteStore {
    fn load_summaries(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<(RepoId, RepoSummary)>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare_cached("SELECT repo, data FROM summaries WHERE project = ?1 ORDER BY position")
            .map_err(db)?;
        let rows: Vec<(i64, String)> = stmt
            .query_map(params![project.0], |row| Ok((row.get(0)?, row.get(1)?)))
            .and_then(Iterator::collect)
            .map_err(db)?;
        rows.into_iter()
            .map(|(repo, data)| Ok((repo_from_sql(repo), from_json(&data, SUMMARY)?)))
            .collect()
    }

    fn save_summaries(
        &self,
        project: &ProjectId,
        summaries: &[(RepoId, RepoSummary)],
    ) -> Result<(), StoreError> {
        let mut conn = self.lock();
        let tx = write_tx(&mut conn)?;
        tx.execute(
            "DELETE FROM summaries WHERE project = ?1",
            params![project.0],
        )
        .map_err(db)?;
        {
            let mut insert = tx
                .prepare_cached(
                    "INSERT INTO summaries (project, position, repo, data) VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(db)?;
            for (position, (repo, summary)) in summaries.iter().enumerate() {
                insert
                    .execute(params![
                        project.0,
                        usize_to_sql(position),
                        repo_to_sql(*repo),
                        to_json(summary, SUMMARY)?
                    ])
                    .map_err(db)?;
            }
        }
        tx.commit().map_err(db)
    }
}

impl UiStateStore for SqliteStore {
    fn load_ui_state(&self, project: &ProjectId) -> Result<Option<UiState>, StoreError> {
        load_json(
            &self.lock(),
            "SELECT data FROM ui_state WHERE project = ?1",
            &project.0,
            UI_STATE,
        )
    }

    fn save_ui_state(&self, project: &ProjectId, state: &UiState) -> Result<(), StoreError> {
        save_json(
            &self.lock(),
            "INSERT OR REPLACE INTO ui_state (project, data) VALUES (?1, ?2)",
            &project.0,
            state,
            UI_STATE,
        )
    }
}
