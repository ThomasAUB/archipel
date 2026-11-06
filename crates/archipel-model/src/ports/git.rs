//! Git access, split by role (ARCHITECTURE §3.1).
//!
//! Contract shared by every trait in this module:
//! * implementations are `Send + Sync` and may be called concurrently for
//!   **different** repositories; writes to the same repository must be
//!   serialized by the caller (the `archipel-ops` scheduler does it);
//! * calls are blocking: callers run them off the UI thread;
//! * long calls take a [`CancelToken`] and return [`GitError::Cancelled`] when
//!   it fires; errors carry the exact command and stderr when a CLI is involved.

use std::ops::ControlFlow;
use std::path::Path;

use crate::cancel::{CancelToken, NoProgress, ProgressSink};
use crate::changes::FileChange;
use crate::diff::{FileDiff, NumStat};
use crate::error::GitError;
use crate::history::{CommitMeta, TopologyNode, WalkOrder};
use crate::ids::{Oid, RepoPath};
use crate::journal::RefSnapshot;
use crate::refs::{BranchInfo, RefTip, StashEntry, TagInfo};
use crate::requests::{
    CheckoutTarget, CommitRequest, DeleteMode, FetchRequest, MergeMode, PatchDirection, PullMode,
    PushRequest, ResetMode, SequenceAction, StashApplyMode, StashRequest, SubmoduleUpdate,
    TagRequest,
};
use crate::submodule::SubmoduleInfo;
use crate::summary::RepoSummary;
use crate::time::Timestamp;

/// Reads the dashboard summary and the working copy changes.
pub trait StatusReader: Send + Sync {
    /// Computes the full dashboard row of a repository.
    fn summary(&self, repo: &RepoPath) -> Result<RepoSummary, GitError>;
    /// Lists changed paths of the working copy (staged, unstaged, untracked, conflicted).
    fn changes(&self, repo: &RepoPath) -> Result<Vec<FileChange>, GitError>;
}

/// Reads branches, tags and ref positions.
pub trait RefReader: Send + Sync {
    /// Local and remote-tracking branches.
    fn branches(&self, repo: &RepoPath) -> Result<Vec<BranchInfo>, GitError>;
    /// Tags, peeled to commits.
    fn tags(&self, repo: &RepoPath) -> Result<Vec<TagInfo>, GitError>;
    /// Every ref pointing into the history (graph badges), including `HEAD`.
    fn ref_tips(&self, repo: &RepoPath) -> Result<Vec<RefTip>, GitError>;
    /// Resolves a revision (`HEAD`, a branch, a short id…) to a commit.
    fn resolve(&self, repo: &RepoPath, rev: &str) -> Result<Option<Oid>, GitError>;
    /// Records `HEAD` and the given full ref names (for the undo journal).
    fn snapshot(&self, repo: &RepoPath, refs: &[String]) -> Result<RefSnapshot, GitError>;
}

/// A configured remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInfo {
    /// Remote name (`origin`).
    pub name: String,
    /// Fetch URL.
    pub url: String,
}

/// Reads remote configuration.
pub trait RemoteReader: Send + Sync {
    /// Configured remotes.
    fn remotes(&self, repo: &RepoPath) -> Result<Vec<RemoteInfo>, GitError>;
}

/// Reads the commit history in a streamed, bounded-memory way.
pub trait HistoryReader: Send + Sync {
    /// Streams the topology reachable from every ref and `HEAD`, children
    /// before parents. The sink returns `Break` to stop early.
    fn walk_topology(
        &self,
        repo: &RepoPath,
        order: WalkOrder,
        sink: &mut dyn FnMut(TopologyNode) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError>;
    /// Loads display metadata for the given commits (in the same order).
    fn commit_meta(&self, repo: &RepoPath, oids: &[Oid]) -> Result<Vec<CommitMeta>, GitError>;
    /// Full message of a commit, trailing newlines trimmed.
    fn commit_message(&self, repo: &RepoPath, commit: Oid) -> Result<String, GitError>;
    /// Whether `ancestor` is reachable from `descendant`.
    fn is_ancestor(
        &self,
        repo: &RepoPath,
        ancestor: Oid,
        descendant: Oid,
    ) -> Result<bool, GitError>;
}

/// Per-commit line statistics, for churn statistics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitChurn {
    /// Commit id.
    pub oid: Oid,
    /// Author email.
    pub author_email: String,
    /// Author date.
    pub author_time: Timestamp,
    /// Lines changed per path (empty for merges).
    pub files: Vec<NumStat>,
}

/// Streams per-commit churn (`git log --numstat`).
pub trait ChurnReader: Send + Sync {
    /// Streams commits reachable from `HEAD` and not from `since`, newest first.
    fn walk_churn(
        &self,
        repo: &RepoPath,
        since: Option<Oid>,
        sink: &mut dyn FnMut(CommitChurn) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError>;
}

/// Which side of the working copy to diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffSide {
    /// `HEAD` ↔ index (`git diff --cached`).
    Staged,
    /// Index ↔ worktree (`git diff`).
    Unstaged,
}

/// Reads diffs.
pub trait DiffReader: Send + Sync {
    /// Diff of a commit against its first parent (or the empty tree).
    fn commit_diff(&self, repo: &RepoPath, commit: Oid) -> Result<Vec<FileDiff>, GitError>;
    /// Working copy diff, optionally limited to one path.
    fn worktree_diff(
        &self,
        repo: &RepoPath,
        side: DiffSide,
        path: Option<&str>,
    ) -> Result<Vec<FileDiff>, GitError>;
}

/// Reads the stash list.
pub trait StashReader: Send + Sync {
    /// Stash entries, newest first.
    fn stashes(&self, repo: &RepoPath) -> Result<Vec<StashEntry>, GitError>;
}

/// Creates commits. Hooks and signing run as in the user's terminal.
pub trait CommitWriter: Send + Sync {
    /// Commits the index (or amends `HEAD`) and returns the new commit id.
    fn commit(&self, repo: &RepoPath, request: &CommitRequest) -> Result<Oid, GitError>;
}

/// Reads and writes files of the worktree as raw bytes (the diff view's
/// file editor). `path` is relative to the repository root, `/`-separated,
/// as in [`FileChange::path`]; a path leaving the worktree is refused.
pub trait WorktreeFiles: Send + Sync {
    /// The bytes of `path`.
    fn read_file(&self, repo: &RepoPath, path: &str) -> Result<Vec<u8>, GitError>;
    /// Replaces the bytes of `path` (an existing file).
    fn write_file(&self, repo: &RepoPath, path: &str, contents: &[u8]) -> Result<(), GitError>;
}

/// Modifies the index and the worktree.
pub trait IndexWriter: Send + Sync {
    /// Stages whole paths (`git add`).
    fn stage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError>;
    /// Unstages whole paths (`git restore --staged`).
    fn unstage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError>;
    /// Applies a unified-diff patch to the index (hunk / line staging).
    fn apply_patch(
        &self,
        repo: &RepoPath,
        patch: &str,
        direction: PatchDirection,
    ) -> Result<(), GitError>;
    /// Discards part of the worktree changes: applies a patch of the
    /// unstaged diff in reverse to the worktree (hunk / line discard).
    fn discard_patch(&self, repo: &RepoPath, patch: &str) -> Result<(), GitError>;
    /// Discards worktree changes of paths (tracked: restore; untracked: delete).
    fn discard(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError>;
}

/// Branch operations.
pub trait BranchWriter: Send + Sync {
    /// Checks out a branch or commit.
    fn checkout(&self, repo: &RepoPath, target: &CheckoutTarget) -> Result<(), GitError>;
    /// Creates a branch at `start` (`HEAD` when `None`).
    fn create_branch(
        &self,
        repo: &RepoPath,
        name: &str,
        start: Option<&str>,
    ) -> Result<(), GitError>;
    /// Renames a branch.
    fn rename_branch(&self, repo: &RepoPath, old: &str, new: &str) -> Result<(), GitError>;
    /// Deletes a branch.
    fn delete_branch(&self, repo: &RepoPath, name: &str, mode: DeleteMode) -> Result<(), GitError>;
    /// Sets (or unsets with `None`) the upstream of a branch.
    fn set_upstream(
        &self,
        repo: &RepoPath,
        branch: &str,
        upstream: Option<&str>,
    ) -> Result<(), GitError>;
}

/// Tag operations.
pub trait TagWriter: Send + Sync {
    /// Creates a lightweight or annotated tag.
    fn create_tag(&self, repo: &RepoPath, request: &TagRequest) -> Result<(), GitError>;
    /// Deletes a tag.
    fn delete_tag(&self, repo: &RepoPath, name: &str) -> Result<(), GitError>;
}

/// Low-level ref moves, used by reset and undo.
pub trait RefWriter: Send + Sync {
    /// Sets (or deletes with `None`) a full ref, optionally checking its old value.
    fn update_ref(
        &self,
        repo: &RepoPath,
        name: &str,
        new: Option<Oid>,
        expected_old: Option<Oid>,
    ) -> Result<(), GitError>;
    /// Points `HEAD` symbolically at a full branch ref without touching the worktree.
    fn set_symbolic_head(&self, repo: &RepoPath, branch_ref: &str) -> Result<(), GitError>;
    /// `git reset` to a commit-ish.
    fn reset(&self, repo: &RepoPath, target: &str, mode: ResetMode) -> Result<(), GitError>;
}

/// Stash operations.
pub trait StashWriter: Send + Sync {
    /// Pushes a new stash; returns `false` when there was nothing to stash.
    fn stash_push(&self, repo: &RepoPath, request: &StashRequest) -> Result<bool, GitError>;
    /// Applies (or pops) the stash at `index`.
    fn stash_apply(
        &self,
        repo: &RepoPath,
        index: u32,
        mode: StashApplyMode,
    ) -> Result<(), GitError>;
    /// Drops the stash at `index`.
    fn drop_stash(&self, repo: &RepoPath, index: u32) -> Result<(), GitError>;
    /// Stores a stash commit back into the stash list (undo of drop).
    fn stash_store(&self, repo: &RepoPath, oid: Oid, message: &str) -> Result<(), GitError>;
}

/// Worktree snapshots taken before destructive operations, so undo can
/// bring the files back. Snapshots are dangling commits: they never show in
/// the stash list or any ref.
pub trait WorktreeSnapshots: Send + Sync {
    /// Snapshots the worktree into a dangling stash commit without touching
    /// the stash list (`git stash create`); `None` when there is nothing to save.
    fn snapshot_worktree(&self, repo: &RepoPath) -> Result<Option<Oid>, GitError>;
    /// Snapshots the worktree content of `paths` (untracked files included,
    /// missing files recorded as deleted) into a dangling commit whose
    /// parent holds `HEAD`'s version: the diff between the two names the
    /// snapshotted paths. `None` when `paths` is empty.
    fn snapshot_paths(&self, repo: &RepoPath, paths: &[String]) -> Result<Option<Oid>, GitError>;
    /// Writes the paths of a [`Self::snapshot_paths`] snapshot back into the
    /// worktree, leaving the index and every other file alone. Refused when
    /// one of them has unstaged changes (it would overwrite newer work).
    fn restore_paths(&self, repo: &RepoPath, snapshot: Oid) -> Result<(), GitError>;
}

/// Network operations.
pub trait RemoteOps: Send + Sync {
    /// Fetches.
    fn fetch(
        &self,
        repo: &RepoPath,
        request: &FetchRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError>;
    /// Fetches every remote unattended (periodic background fetch): no
    /// progress, no automatic maintenance. Returns whether any ref was
    /// updated, so callers can skip work when nothing changed.
    fn fetch_background(&self, repo: &RepoPath, cancel: &CancelToken) -> Result<bool, GitError> {
        self.fetch(repo, &FetchRequest::default(), &NoProgress, cancel)
            .map(|()| true)
    }
    /// Pulls the current branch.
    fn pull(
        &self,
        repo: &RepoPath,
        mode: PullMode,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError>;
    /// Pushes.
    fn push(
        &self,
        repo: &RepoPath,
        request: &PushRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError>;
    /// Clones `url` into `dest`.
    fn clone_repo(
        &self,
        url: &str,
        dest: &Path,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RepoPath, GitError>;
}

/// Merge, rebase, cherry-pick, revert and their continuation.
pub trait HistoryRewriter: Send + Sync {
    /// Merges `rev` into the current branch.
    fn merge(&self, repo: &RepoPath, rev: &str, mode: MergeMode) -> Result<(), GitError>;
    /// Rebases the current branch onto `onto`.
    fn rebase(&self, repo: &RepoPath, onto: &str) -> Result<(), GitError>;
    /// Cherry-picks commits in order.
    fn cherry_pick(&self, repo: &RepoPath, commits: &[Oid]) -> Result<(), GitError>;
    /// Reverts a commit.
    fn revert(&self, repo: &RepoPath, commit: Oid) -> Result<(), GitError>;
    /// Continues, aborts or skips the operation in progress.
    fn sequence(&self, repo: &RepoPath, action: SequenceAction) -> Result<(), GitError>;
}

/// Launches the user's external merge tool.
pub trait MergeToolLauncher: Send + Sync {
    /// Runs `git mergetool` for one path (all conflicts when `None`).
    fn launch(
        &self,
        repo: &RepoPath,
        path: Option<&str>,
        tool: Option<&str>,
    ) -> Result<(), GitError>;
}

/// Lists the submodules of a repository (spec §9).
pub trait SubmoduleReader: Send + Sync {
    /// Submodules declared by `repo` (one level), in path order. A repository
    /// without submodules returns an empty list, not an error.
    fn submodules(&self, repo: &RepoPath) -> Result<Vec<SubmoduleInfo>, GitError>;
}

/// Initializes and updates submodules (may clone: a network operation).
pub trait SubmoduleWriter: Send + Sync {
    /// `git submodule update` with the options of `request`.
    fn update_submodules(
        &self,
        repo: &RepoPath,
        request: &SubmoduleUpdate,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError>;
}
