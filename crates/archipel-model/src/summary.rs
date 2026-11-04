//! The dashboard summary of one repository (spec §3).

use serde::{Deserialize, Serialize};

use crate::ids::Oid;
use crate::time::Timestamp;

/// Where `HEAD` points.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadState {
    /// On a local branch (short name, e.g. `main`).
    Branch(String),
    /// Detached at a commit.
    Detached(Oid),
    /// On a branch that has no commit yet.
    Unborn(String),
}

impl HeadState {
    /// The branch name, if `HEAD` is on a branch (born or unborn).
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Branch(name) | Self::Unborn(name) => Some(name),
            Self::Detached(_) => None,
        }
    }
}

/// Upstream tracking information of the current branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upstream {
    /// Upstream short name, e.g. `origin/main`.
    pub name: String,
    /// Commits on the local branch not on the upstream.
    pub ahead: u32,
    /// Commits on the upstream not on the local branch.
    pub behind: u32,
    /// Whether the upstream ref is missing (deleted on the remote).
    pub gone: bool,
}

/// Counts of changed files in the working copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeCounts {
    /// Files with unstaged changes.
    pub modified: u32,
    /// Files with staged changes.
    pub staged: u32,
    /// Untracked files.
    pub untracked: u32,
    /// Files with unresolved conflicts.
    pub conflicted: u32,
}

impl ChangeCounts {
    /// Whether the working copy has any change at all.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.modified + self.staged + self.untracked + self.conflicted > 0
    }
}

/// A multi-step operation in progress in the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepoOp {
    /// `git merge` stopped (conflicts or `--no-commit`).
    Merge,
    /// Interactive or non-interactive rebase in progress.
    Rebase,
    /// `git cherry-pick` stopped.
    CherryPick,
    /// `git revert` stopped.
    Revert,
    /// `git bisect` session.
    Bisect,
    /// `git am` session.
    ApplyMailbox,
}

/// One dashboard row (spec §3). Kept small: inactive projects keep only these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSummary {
    /// Where `HEAD` points.
    pub head: HeadState,
    /// Commit `HEAD` resolves to (`None` when unborn).
    pub head_oid: Option<Oid>,
    /// Tracking info of the current branch, if it has an upstream.
    pub upstream: Option<Upstream>,
    /// Working copy change counts.
    pub changes: ChangeCounts,
    /// Number of stash entries.
    pub stash_count: u32,
    /// `git describe --tags` output (latest tag + distance).
    pub describe: Option<String>,
    /// Time of the last fetch (mtime of `FETCH_HEAD`).
    pub last_fetch: Option<Timestamp>,
    /// Operation in progress, if any.
    pub in_progress: Option<RepoOp>,
    /// Whether the repository declares submodules (`.gitmodules`).
    pub has_submodules: bool,
    /// When this summary was computed.
    pub computed_at: Timestamp,
}

impl RepoSummary {
    /// A summary for a repo whose `HEAD` is unborn and everything else empty.
    #[must_use]
    pub fn empty(branch: &str) -> Self {
        Self {
            head: HeadState::Unborn(branch.to_owned()),
            head_oid: None,
            upstream: None,
            changes: ChangeCounts::default(),
            stash_count: 0,
            describe: None,
            last_fetch: None,
            in_progress: None,
            has_submodules: false,
            computed_at: Timestamp::now(),
        }
    }

    /// Commits ahead of upstream (0 without upstream).
    #[must_use]
    pub fn ahead(&self) -> u32 {
        self.upstream.as_ref().map_or(0, |u| u.ahead)
    }

    /// Commits behind upstream (0 without upstream).
    #[must_use]
    pub fn behind(&self) -> u32 {
        self.upstream.as_ref().map_or(0, |u| u.behind)
    }
}
