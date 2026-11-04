//! Operation journal entries: the "before" state of every mutating action,
//! recorded so it can be undone (PLAN §2.4).

use serde::{Deserialize, Serialize};

use crate::ids::{Oid, RepoId};
use crate::time::Timestamp;

/// Identifier of a journal entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JournalId(pub i64);

/// Identifier grouping the entries of one batch operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BatchId(pub u64);

/// Where `HEAD` pointed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadRef {
    /// Symbolic ref to a branch (full ref name, e.g. `refs/heads/main`).
    Symbolic(String),
    /// Detached at a commit.
    Detached(Oid),
}

/// Positions of `HEAD` and a set of refs at one point in time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefSnapshot {
    /// Where `HEAD` pointed.
    pub head: HeadRef,
    /// Full ref names and their targets (`None`: the ref did not exist).
    pub refs: Vec<(String, Option<Oid>)>,
}

impl RefSnapshot {
    /// Target of a ref in this snapshot (`None` when absent or recorded as missing).
    #[must_use]
    pub fn target(&self, name: &str) -> Option<Oid> {
        self.refs
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, oid)| *oid)
    }
}

/// Kind of the recorded operation; drives the undo strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OperationKind {
    /// Commit.
    Commit,
    /// Amend.
    Amend,
    /// Merge.
    Merge,
    /// Rebase.
    Rebase,
    /// Cherry-pick.
    CherryPick,
    /// Revert.
    Revert,
    /// Reset.
    Reset,
    /// Pull (local part).
    Pull,
    /// Fast-forward of a branch.
    FastForward,
    /// Checkout / switch.
    Checkout,
    /// Branch creation.
    BranchCreate,
    /// Branch deletion.
    BranchDelete,
    /// Branch rename.
    BranchRename,
    /// Tag creation.
    TagCreate,
    /// Tag deletion.
    TagDelete,
    /// Stash push.
    StashPush,
    /// Stash pop.
    StashPop,
    /// Stash drop.
    StashDrop,
    /// Discarding worktree changes.
    Discard,
    /// Staging or unstaging.
    Stage,
    /// Push to a remote (undo = compensating action).
    Push,
    /// Fetch (nothing to undo).
    Fetch,
}

/// Whether an entry is currently applied or has been undone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryState {
    /// The operation is in effect.
    Done,
    /// The operation was undone (can be redone).
    Undone,
}

/// A journal entry: what an operation changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Entry id (assigned by the store).
    pub id: JournalId,
    /// Repository.
    pub repo: RepoId,
    /// Batch the entry belongs to.
    pub batch: Option<BatchId>,
    /// Kind of operation.
    pub kind: OperationKind,
    /// Human readable label ("Commit 'fix typo'").
    pub label: String,
    /// State before the operation.
    pub before: RefSnapshot,
    /// State after the operation.
    pub after: RefSnapshot,
    /// Worktree snapshot (`git stash create`) taken before destructive ops.
    pub worktree_snapshot: Option<Oid>,
    /// Stash commit affected by stash operations.
    pub stash: Option<StashRecord>,
    /// When the operation ran.
    pub time: Timestamp,
    /// Whether the entry is applied or undone.
    pub state: EntryState,
}

/// A stash commit recorded so a dropped stash can be restored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashRecord {
    /// Stash commit id.
    pub oid: Oid,
    /// Stash message.
    pub message: String,
}
