//! Recording mutating operations in the journal (PLAN Phase 4 step 7).
//!
//! Every mutating action goes through [`Journal::record`], which snapshots
//! `HEAD` and the touched refs before and after the action (and optionally the
//! worktree), so the undo service never needs a retrofit.

use std::sync::Arc;

use archipel_model::journal::{
    BatchId, EntryState, HeadRef, JournalEntry, JournalId, OperationKind, RefSnapshot, StashRecord,
};
use archipel_model::ports::{JournalStore, RefReader, StashWriter};
use archipel_model::{GitError, RepoPath, Timestamp};

use crate::error::OpError;

/// What to record about an operation.
#[derive(Clone, Debug)]
pub struct Recording {
    /// Kind of operation (drives the undo strategy).
    pub kind: OperationKind,
    /// Label shown in the operations drawer.
    pub label: String,
    /// Full ref names the operation may move, in addition to `HEAD`'s branch.
    pub refs: Vec<String>,
    /// Batch the operation belongs to.
    pub batch: Option<BatchId>,
    /// Snapshot the worktree first (destructive operations).
    pub snapshot_worktree: bool,
    /// Snapshot only these worktree paths first (discard); takes precedence
    /// over `snapshot_worktree`.
    pub snapshot_paths: Vec<String>,
    /// Stash commit affected by a stash operation.
    pub stash: Option<StashRecord>,
}

impl Recording {
    /// A recording of `kind` with a label and no extra refs.
    #[must_use]
    pub fn new(kind: OperationKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            refs: Vec::new(),
            batch: None,
            snapshot_worktree: false,
            snapshot_paths: Vec::new(),
            stash: None,
        }
    }

    /// Adds full ref names the operation may move.
    #[must_use]
    pub fn with_refs<I: IntoIterator<Item = String>>(mut self, refs: I) -> Self {
        self.refs.extend(refs);
        self
    }

    /// Sets the batch.
    #[must_use]
    pub fn in_batch(mut self, batch: Option<BatchId>) -> Self {
        self.batch = batch;
        self
    }

    /// Requests a worktree snapshot before the operation.
    #[must_use]
    pub fn snapshot_worktree(mut self) -> Self {
        self.snapshot_worktree = true;
        self
    }

    /// Requests a snapshot of the worktree content of `paths` before the operation.
    #[must_use]
    pub fn snapshot_paths(mut self, paths: &[String]) -> Self {
        self.snapshot_paths = paths.to_vec();
        self
    }

    /// Records the stash commit involved.
    #[must_use]
    pub fn with_stash(mut self, stash: Option<StashRecord>) -> Self {
        self.stash = stash;
        self
    }
}

/// Records operations into a [`JournalStore`].
#[derive(Clone)]
pub struct Journal {
    store: Arc<dyn JournalStore>,
    refs: Arc<dyn RefReader>,
    snapshots: Arc<dyn StashWriter>,
    limit: usize,
}

impl Journal {
    /// Creates a journal keeping at most `limit` entries per repository.
    #[must_use]
    pub fn new(
        store: Arc<dyn JournalStore>,
        refs: Arc<dyn RefReader>,
        snapshots: Arc<dyn StashWriter>,
        limit: usize,
    ) -> Self {
        Self {
            store,
            refs,
            snapshots,
            limit,
        }
    }

    /// The underlying store.
    #[must_use]
    pub fn store(&self) -> &Arc<dyn JournalStore> {
        &self.store
    }

    /// Runs `op`, recording the state before and after it. Nothing is
    /// recorded when `op` fails. A journal write failure after a successful
    /// operation is logged, not returned: the operation did happen.
    pub fn record<T>(
        &self,
        repo: &RepoPath,
        rec: Recording,
        op: impl FnOnce() -> Result<T, GitError>,
    ) -> Result<T, OpError> {
        let mut names = rec.refs.clone();
        // `HEAD` may be unborn: then there is nothing to snapshot yet.
        let first = self.refs.snapshot(repo, &names).ok();
        if let Some(RefSnapshot {
            head: HeadRef::Symbolic(branch),
            ..
        }) = &first
            && !names.contains(branch)
        {
            names.push(branch.clone());
        }
        let before = match first {
            Some(_) => Some(self.refs.snapshot(repo, &names)?),
            None => None,
        };
        let worktree_snapshot = if !rec.snapshot_paths.is_empty() {
            self.snapshots.snapshot_paths(repo, &rec.snapshot_paths)?
        } else if rec.snapshot_worktree {
            self.snapshots.snapshot_worktree(repo)?
        } else {
            None
        };
        let value = op()?;
        let after = self.refs.snapshot(repo, &names);
        match (before, after) {
            (Some(before), Ok(after)) => {
                let entry = JournalEntry {
                    id: JournalId(0),
                    repo: repo.id(),
                    batch: rec.batch,
                    kind: rec.kind,
                    label: rec.label,
                    before,
                    after,
                    worktree_snapshot,
                    stash: rec.stash,
                    time: Timestamp::now(),
                    state: EntryState::Done,
                };
                if let Err(e) = self.store.append(&entry, self.limit) {
                    tracing::warn!(error = %e, "could not record journal entry");
                }
            }
            (None, _) => tracing::debug!("operation on an unborn HEAD is not journaled"),
            (_, Err(e)) => tracing::warn!(error = %e, "could not snapshot refs after operation"),
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use archipel_model::ports::{CommitWriter, JournalStore};
    use archipel_model::requests::{CommitMode, CommitRequest};
    use archipel_model::testing::{FakeGit, InMemoryStore, oid};

    use super::*;

    #[test]
    fn records_before_and_after_of_head_branch() {
        let git = Arc::new(FakeGit::new());
        let store = Arc::new(InMemoryStore::new());
        let repo = RepoPath::new("/r");
        git.with_repo(&repo, |r| {
            r.refs.insert("refs/heads/main".into(), oid(1));
        });
        let journal = Journal::new(store.clone(), git.clone(), git.clone(), 10);
        let new = journal
            .record(
                &repo,
                Recording::new(OperationKind::Commit, "commit"),
                || {
                    git.commit(
                        &repo,
                        &CommitRequest {
                            message: "m".into(),
                            mode: CommitMode::New,
                        },
                    )
                },
            )
            .unwrap();
        let entries = store.recent(repo.id(), 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].before.target("refs/heads/main"), Some(oid(1)));
        assert_eq!(entries[0].after.target("refs/heads/main"), Some(new));
    }

    #[test]
    fn failed_operations_are_not_recorded() {
        let git = Arc::new(FakeGit::new());
        let store = Arc::new(InMemoryStore::new());
        let repo = RepoPath::new("/r");
        git.with_repo(&repo, |r| {
            r.refs.insert("refs/heads/main".into(), oid(1));
        });
        let journal = Journal::new(store.clone(), git.clone(), git, 10);
        let r: Result<(), _> =
            journal.record(&repo, Recording::new(OperationKind::Commit, "c"), || {
                Err(GitError::Cancelled)
            });
        assert!(r.is_err());
        assert!(store.recent(repo.id(), 10).unwrap().is_empty());
    }
}
