//! Persistent stores, one trait per kind of data (ARCHITECTURE §3.1).
//!
//! Implementations are `Send + Sync`; every call is short and blocking.

use crate::error::StoreError;
use crate::ids::{Oid, ProjectId, RepoId};
use crate::journal::{BatchId, EntryState, JournalEntry, JournalId};
use crate::project::RegistryEntry;
use crate::stats::{ActivityBucket, Hotspot, StatsDelta};
use crate::summary::RepoSummary;
use crate::time::Timestamp;
use crate::ui_state::UiState;

/// Cached dashboard summaries, so a project paints instantly.
pub trait SummaryStore: Send + Sync {
    /// Last saved summaries of a project.
    fn load_summaries(&self, project: &ProjectId)
    -> Result<Vec<(RepoId, RepoSummary)>, StoreError>;
    /// Replaces the saved summaries of a project.
    fn save_summaries(
        &self,
        project: &ProjectId,
        summaries: &[(RepoId, RepoSummary)],
    ) -> Result<(), StoreError>;
}

/// Per-project UI state.
pub trait UiStateStore: Send + Sync {
    /// Saved UI state, if any.
    fn load_ui_state(&self, project: &ProjectId) -> Result<Option<UiState>, StoreError>;
    /// Saves the UI state.
    fn save_ui_state(&self, project: &ProjectId, state: &UiState) -> Result<(), StoreError>;
}

/// The user registry of known projects and the recent list.
pub trait ProjectRegistry: Send + Sync {
    /// Every known project.
    fn list_projects(&self) -> Result<Vec<RegistryEntry>, StoreError>;
    /// Adds or updates a project.
    fn upsert_project(&self, entry: &RegistryEntry) -> Result<(), StoreError>;
    /// Removes a project from the registry (never deletes files).
    fn remove_project(&self, id: &ProjectId) -> Result<(), StoreError>;
    /// Records that a project was opened (recent list).
    fn mark_opened(&self, id: &ProjectId, when: Timestamp) -> Result<(), StoreError>;
}

/// The bounded operation journal used by undo/redo.
pub trait JournalStore: Send + Sync {
    /// Appends an entry (its `id` is ignored) and returns the assigned id.
    /// Implementations prune each repo to its most recent `limit` entries.
    fn append(&self, entry: &JournalEntry, limit: usize) -> Result<JournalId, StoreError>;
    /// Most recent entries of a repo, newest first.
    fn recent(&self, repo: RepoId, limit: usize) -> Result<Vec<JournalEntry>, StoreError>;
    /// Entries of a batch.
    fn batch(&self, batch: BatchId) -> Result<Vec<JournalEntry>, StoreError>;
    /// Marks an entry done or undone.
    fn set_state(&self, id: JournalId, state: EntryState) -> Result<(), StoreError>;
}

/// Incrementally computed statistics.
pub trait StatsStore: Send + Sync {
    /// Last processed commit of a repo.
    fn cursor(&self, repo: RepoId) -> Result<Option<Oid>, StoreError>;
    /// Adds a delta and moves the cursor, atomically.
    fn apply_delta(&self, repo: RepoId, delta: &StatsDelta, cursor: Oid) -> Result<(), StoreError>;
    /// Activity buckets of a repo.
    fn activity(&self, repo: RepoId) -> Result<Vec<ActivityBucket>, StoreError>;
    /// Paths with the highest churn.
    fn hotspots(&self, repo: RepoId, limit: usize) -> Result<Vec<Hotspot>, StoreError>;
    /// Forgets all statistics of a repo (history rewritten).
    fn reset_stats(&self, repo: RepoId) -> Result<(), StoreError>;
}
