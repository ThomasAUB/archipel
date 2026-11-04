//! Typed requests for write operations (enums instead of boolean flags).

use serde::{Deserialize, Serialize};

use crate::ids::Oid;

/// Whether a commit creates a new commit or amends `HEAD`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitMode {
    /// Create a new commit.
    #[default]
    New,
    /// Replace the `HEAD` commit.
    Amend,
}

/// A commit request. The message goes through stdin so hooks and signing run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRequest {
    /// Full commit message.
    pub message: String,
    /// New commit or amend.
    pub mode: CommitMode,
}

/// Direction in which a patch is applied to the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchDirection {
    /// Apply the patch (stage).
    Forward,
    /// Apply the patch in reverse (unstage).
    Reverse,
}

/// What to check out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckoutTarget {
    /// An existing local branch.
    Branch(String),
    /// Create a local branch tracking the given remote branch (`origin/x` → `x`).
    TrackRemote(String),
    /// Detach `HEAD` at a commit.
    Detached(Oid),
}

/// How to delete a branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteMode {
    /// Refuse when the branch is not merged (`-d`).
    Safe,
    /// Delete anyway (`-D`).
    Force,
}

/// A tag creation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagRequest {
    /// Tag name.
    pub name: String,
    /// Commit-ish to tag (`HEAD` when `None`).
    pub target: Option<String>,
    /// Message for an annotated tag; lightweight when `None`.
    pub message: Option<String>,
}

/// Mode of `git reset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetMode {
    /// Move `HEAD` only.
    Soft,
    /// Move `HEAD` and reset the index.
    Mixed,
    /// Move `HEAD`, reset index and worktree (destructive).
    Hard,
    /// Like hard but refuses to lose local changes.
    Keep,
}

/// Whether untracked files are included in a stash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UntrackedMode {
    /// Tracked files only.
    #[default]
    Exclude,
    /// Include untracked files.
    Include,
}

/// A stash push request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashRequest {
    /// Optional message.
    pub message: Option<String>,
    /// Whether to include untracked files.
    pub untracked: UntrackedMode,
}

/// Whether applying a stash also drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StashApplyMode {
    /// `git stash apply`.
    Keep,
    /// `git stash pop`.
    Pop,
}

/// How `pull` integrates remote changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PullMode {
    /// `--ff-only` (default: never creates merge commits silently).
    #[default]
    FastForwardOnly,
    /// `--rebase`.
    Rebase,
    /// `--no-rebase` (merge).
    Merge,
}

/// A fetch request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchRequest {
    /// Remote name; all remotes when `None`.
    pub remote: Option<String>,
    /// Prune deleted remote branches.
    pub prune: bool,
}

/// Whether a push may overwrite the remote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForceMode {
    /// Regular push.
    #[default]
    None,
    /// `--force-with-lease`, optionally with the expected remote OID.
    WithLease(Option<Oid>),
}

/// A push request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRequest {
    /// Remote name (`origin` when `None`).
    pub remote: Option<String>,
    /// Refspec or branch name (current branch when `None`).
    pub refspec: Option<String>,
    /// Set upstream (`-u`).
    pub set_upstream: bool,
    /// Force mode.
    pub force: ForceMode,
    /// Push tags too.
    pub tags: bool,
}

/// How a merge is performed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeMode {
    /// Default: fast-forward when possible, else merge commit.
    #[default]
    Auto,
    /// Fast-forward only.
    FastForwardOnly,
    /// Always create a merge commit.
    NoFastForward,
}

/// Action on a stopped merge, rebase, cherry-pick or revert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceAction {
    /// `--continue`.
    Continue,
    /// `--abort`.
    Abort,
    /// `--skip` (rebase / cherry-pick).
    Skip,
}
