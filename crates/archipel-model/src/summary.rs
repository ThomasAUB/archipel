//! The dashboard summary of one repository (spec §3).

use serde::{Deserialize, Serialize};

use crate::changes::FileChange;
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

/// The changed files of the working copy, capped to [`ChangedFiles::MAX`]
/// entries so the summary stays small (the dashboard lists them on hover).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFiles {
    /// The first changed paths, in status order.
    pub files: Vec<FileChange>,
    /// Number of changed paths, including those not kept in `files`.
    pub total: u32,
}

impl ChangedFiles {
    /// Most paths kept in a summary.
    pub const MAX: usize = 50;

    /// Keeps the first [`Self::MAX`] of `changes`.
    #[must_use]
    pub fn capped(mut changes: Vec<FileChange>) -> Self {
        let total = u32::try_from(changes.len()).unwrap_or(u32::MAX);
        changes.truncate(Self::MAX);
        Self {
            files: changes,
            total,
        }
    }

    /// Number of changed paths not kept in `files`.
    #[must_use]
    pub fn omitted(&self) -> u32 {
        self.total
            .saturating_sub(u32::try_from(self.files.len()).unwrap_or(u32::MAX))
    }
}

/// A branch whose history contains a detached `HEAD`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainingBranch {
    /// Short name, e.g. `main` or `origin/release-2.1`.
    pub name: String,
    /// Whether it is a remote-tracking branch.
    pub is_remote: bool,
    /// Commits on the branch past `HEAD` (0 when `HEAD` is its tip).
    pub ahead: u32,
}

impl ContainingBranch {
    /// Most branches kept in a summary.
    pub const MAX: usize = 10;

    /// The branches nearest to `HEAD` first (fewest commits past it), local
    /// before remote-tracking on a tie, then by name; keeps [`Self::MAX`].
    #[must_use]
    pub fn nearest(mut branches: Vec<Self>) -> Vec<Self> {
        branches
            .sort_by(|a, b| (a.ahead, a.is_remote, &a.name).cmp(&(b.ahead, b.is_remote, &b.name)));
        branches.truncate(Self::MAX);
        branches
    }

    /// `main` at its tip, `main~3` three commits below it (git's notation).
    #[must_use]
    pub fn label(&self) -> String {
        match self.ahead {
            0 => self.name.clone(),
            n => format!("{}~{n}", self.name),
        }
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
    /// The changed paths behind `changes` (absent from older cached summaries).
    #[serde(default)]
    pub changed_files: ChangedFiles,
    /// When `HEAD` is detached (e.g. a tag checked out): the branches whose
    /// history contains it, nearest first ([`ContainingBranch::nearest`]).
    /// Empty when on a branch.
    #[serde(default)]
    pub containing_branches: Vec<ContainingBranch>,
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
            changed_files: ChangedFiles::default(),
            containing_branches: Vec::new(),
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
