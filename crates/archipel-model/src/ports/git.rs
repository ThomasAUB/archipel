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
use crate::changes::{ConflictSides, FileChange};
use crate::diff::{FileDiff, NumStat};
use crate::error::GitError;
use crate::file_history::{Blame, BlameOptions, FileRevision};
use crate::forecast::{MergeForecast, MergeStrategy};
use crate::history::{CommitMeta, CommitText, SquashPreview, TopologyNode, WalkOrder};
use crate::ids::{Oid, RepoPath};
use crate::journal::RefSnapshot;
use crate::refs::{BranchInfo, RefTip, StashEntry, TagInfo};
use crate::requests::{
    CheckoutTarget, CommitRequest, DeleteMode, FetchRequest, MergeMode, PatchDirection, PullMode,
    PushRequest, ResetMode, SequenceAction, StashApplyMode, StashRequest, SubmoduleAdd,
    SubmoduleUpdate, TagRequest,
};
use crate::stats::{CommitRecord, RepoStorage, TreeFile};
use crate::submodule::SubmoduleInfo;
use crate::summary::RepoSummary;
use crate::time::Timestamp;
use crate::tree::TreeEntry;
use crate::worktree::{WorktreeAddRequest, WorktreeInfo};

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

/// Writes remote configuration (no network).
pub trait RemoteWriter: Send + Sync {
    /// Points remote `name` at `url`, adding the remote when it does not
    /// exist. Returns the separate push URL (`remote.<name>.pushurl`) left
    /// in place, if any: pushes still go there.
    fn set_remote_url(
        &self,
        repo: &RepoPath,
        name: &str,
        url: &str,
    ) -> Result<Option<String>, GitError>;
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
    /// Metadata and full message of the given commits (in the same order),
    /// for history search: one read per commit.
    fn commit_texts(&self, repo: &RepoPath, oids: &[Oid]) -> Result<Vec<CommitText>, GitError>;
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

    /// Streams the commits reachable from `rev`, newest first, with their
    /// parents, author and line changes (repository statistics). The default
    /// streams nothing.
    fn walk_records(
        &self,
        repo: &RepoPath,
        rev: &str,
        sink: &mut dyn FnMut(CommitRecord) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let _ = (repo, rev, sink, cancel);
        Ok(())
    }

    /// Number of commits reachable from `rev` and not from `exclude`
    /// (`git rev-list --count rev ^exclude`). The default does not know.
    fn count_commits(
        &self,
        repo: &RepoPath,
        rev: &str,
        exclude: Oid,
    ) -> Result<Option<u64>, GitError> {
        let _ = (repo, rev, exclude);
        Ok(None)
    }

    /// What the repository weighs on disk, with its `largest` biggest blobs.
    /// The default reports nothing.
    fn storage(
        &self,
        repo: &RepoPath,
        largest: usize,
        cancel: &CancelToken,
    ) -> Result<RepoStorage, GitError> {
        let _ = (repo, largest, cancel);
        Ok(RepoStorage::default())
    }

    /// The text files of the tree at `rev` with their line counts (binary
    /// files left out) and their `linguist-*` attributes. The default
    /// lists nothing.
    fn tree_files(
        &self,
        repo: &RepoPath,
        rev: &str,
        cancel: &CancelToken,
    ) -> Result<Vec<TreeFile>, GitError> {
        let _ = (repo, rev, cancel);
        Ok(Vec::new())
    }
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
    /// Changes of the commits from `oldest` to `newest`, both included:
    /// diff from the first parent of `oldest` (the empty tree for a root
    /// commit) to `newest`.
    fn range_diff(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
    ) -> Result<Vec<FileDiff>, GitError>;
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
    /// Creates `path` with `contents`; refused when it already exists.
    fn create_file(&self, repo: &RepoPath, path: &str, contents: &[u8]) -> Result<(), GitError>;
}

/// Lists and reads the files of a repository at a commit or in the
/// working copy (the repo view's file browser). Paths are relative to the
/// repository root and `/`-separated.
pub trait TreeReader: Send + Sync {
    /// Every file of the tree of `commit`, or, when `None`, of the working
    /// copy (tracked files and untracked ones not ignored), sorted by path.
    /// Submodules are listed as entries, not walked into.
    fn list_files(&self, repo: &RepoPath, commit: Option<Oid>) -> Result<Vec<TreeEntry>, GitError>;
    /// The bytes of `path` in the tree of `commit`.
    fn read_blob(&self, repo: &RepoPath, commit: Oid, path: &str) -> Result<Vec<u8>, GitError>;
    /// The bytes of `path` as staged in the index (stage 0).
    fn read_index_blob(&self, repo: &RepoPath, path: &str) -> Result<Vec<u8>, GitError>;
}

/// Reads the history of one file (the file browser's History and Blame).
/// Paths are relative to the repository root and `/`-separated. Both calls
/// can be long on big histories: they return [`GitError::Cancelled`] once
/// `cancel` fires.
pub trait FileHistoryReader: Send + Sync {
    /// The commits that touched `path`, newest first, following renames,
    /// walking back from `from` (`HEAD` when `None`): `limit` of them after
    /// skipping the first `skip`, for paging.
    fn file_log(
        &self,
        repo: &RepoPath,
        path: &str,
        from: Option<Oid>,
        skip: usize,
        limit: usize,
        cancel: &CancelToken,
    ) -> Result<Vec<FileRevision>, GitError>;
    /// Who last changed each line of `path` as it is at commit `at`, or,
    /// when `None`, in the working copy (uncommitted lines included).
    fn blame(
        &self,
        repo: &RepoPath,
        path: &str,
        at: Option<Oid>,
        options: BlameOptions,
        cancel: &CancelToken,
    ) -> Result<Blame, GitError>;
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
    /// Applies the stash at `index` and keeps it, restoring which changes
    /// were staged (`git stash apply --index`).
    fn stash_apply_index(&self, repo: &RepoPath, index: u32) -> Result<(), GitError>;
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

/// Creates repositories (issue #68).
pub trait RepoCreator: Send + Sync {
    /// Makes `dir` (created when missing) a new, empty repository, its
    /// first branch named `initial_branch` (Git's `init.defaultBranch` when
    /// `None`).
    fn init_repo(&self, dir: &Path, initial_branch: Option<&str>) -> Result<RepoPath, GitError>;
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

/// Merge, rebase, cherry-pick, revert, squash, reword and their continuation.
pub trait HistoryRewriter: Send + Sync {
    /// Merges `rev` into the current branch.
    fn merge(&self, repo: &RepoPath, rev: &str, mode: MergeMode) -> Result<(), GitError>;
    /// Rebases the current branch onto `onto`.
    fn rebase(&self, repo: &RepoPath, onto: &str) -> Result<(), GitError>;
    /// Cherry-picks commits in order.
    fn cherry_pick(&self, repo: &RepoPath, commits: &[Oid]) -> Result<(), GitError>;
    /// Reverts a commit.
    fn revert(&self, repo: &RepoPath, commit: Oid) -> Result<(), GitError>;
    /// The commits [`Self::squash`] would take from `oldest` to `newest`,
    /// and the message it proposes. Fails as the squash would.
    fn squash_preview(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
    ) -> Result<SquashPreview, GitError>;
    /// Squashes the commits from `oldest` to `newest` (consecutive commits
    /// of the first-parent history of `HEAD`, no merge among them) into one
    /// commit with `message`, authored as `oldest`. The commits after
    /// `newest` are recreated on top with the same content. Every tree is
    /// kept, so the index and the worktree are not touched. Returns the
    /// new `HEAD`.
    fn squash(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
        message: &str,
    ) -> Result<Oid, GitError>;
    /// Gives `commit` (of the first-parent history of `HEAD`) `message`.
    /// The commits after it are recreated on top with the same content;
    /// the index and the worktree are not touched. Returns the new `HEAD`.
    fn reword(&self, repo: &RepoPath, commit: Oid, message: &str) -> Result<Oid, GitError>;
    /// Continues, aborts or skips the operation in progress.
    fn sequence(&self, repo: &RepoPath, action: SequenceAction) -> Result<(), GitError>;
}

/// Forecasts whether bringing a commit into another would conflict, in
/// memory (`git merge-tree`): the worktree, the index and the refs are never
/// touched (objects may be written, as dangling ones).
pub trait MergeForecaster: Send + Sync {
    /// The strategy `git pull` uses on `branch`: `branch.<name>.rebase`,
    /// else `pull.rebase`; a merge when neither is set.
    fn pull_strategy(&self, repo: &RepoPath, branch: &str) -> Result<MergeStrategy, GitError>;
    /// What bringing `theirs` into `ours` with `strategy` would conflict on
    /// (for a rebase: `ours`' own commits replayed onto `theirs`). Fails on
    /// unrelated histories.
    fn forecast(
        &self,
        repo: &RepoPath,
        ours: Oid,
        theirs: Oid,
        strategy: MergeStrategy,
        cancel: &CancelToken,
    ) -> Result<MergeForecast, GitError>;
    /// The paths changed on `theirs` since it forked from `ours` (what a
    /// pull would bring), sorted.
    fn incoming_paths(
        &self,
        repo: &RepoPath,
        ours: Oid,
        theirs: Oid,
    ) -> Result<Vec<String>, GitError>;
}

/// A merge tool `git mergetool --tool` accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeToolInfo {
    /// Its name (`meld`, `kdiff3`, `vscode`…).
    pub name: String,
    /// Git's description of it, if any.
    pub detail: String,
    /// Whether Git found it installed.
    pub available: bool,
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

    /// The merge tool Git's configuration names for `repo` (`merge.tool`),
    /// `None` when unset.
    fn configured_tool(&self, repo: &RepoPath) -> Result<Option<String>, GitError>;

    /// The merge tools Git knows (`git mergetool --tool-help`), the
    /// installed ones first.
    fn tools(&self, repo: &RepoPath) -> Result<Vec<MergeToolInfo>, GitError>;
}

/// Reads and resolves the conflicted paths of the index (the merge view).
/// `path` is relative to the repository root and `/`-separated.
pub trait ConflictResolver: Send + Sync {
    /// The three versions of conflicted `path` held by the index: base
    /// (stage 1), ours (stage 2) and theirs (stage 3). A side where the
    /// file does not exist (added on one side, deleted on the other) is
    /// `None`. Refused when `path` is not conflicted.
    fn conflict_sides(&self, repo: &RepoPath, path: &str) -> Result<ConflictSides, GitError>;
    /// Marks conflicted `path` resolved: `contents` is written to the
    /// worktree and staged, or, when `None`, the file is deleted from the
    /// worktree and the index. Git keeps the conflict in the index's
    /// resolve-undo record, which [`Self::unresolve_conflicts`] uses.
    fn resolve_conflict(
        &self,
        repo: &RepoPath,
        path: &str,
        contents: Option<&[u8]>,
    ) -> Result<(), GitError>;
    /// Undo of [`Self::resolve_conflict`]: brings back the conflict of
    /// `paths` in the index, and their worktree content from `snapshot` (a
    /// [`WorktreeSnapshots::snapshot_paths`] of them taken before
    /// resolving). Refused when one of them changed since it was resolved.
    fn unresolve_conflicts(
        &self,
        repo: &RepoPath,
        paths: &[String],
        snapshot: Oid,
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

    /// `git submodule add`: clones `request.url` at `request.path` and
    /// stages the new submodule (and `.gitmodules`) for the next commit.
    fn add_submodule(
        &self,
        repo: &RepoPath,
        request: &SubmoduleAdd,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError>;
}

/// Lists the worktrees of a repository (WORKTREES.md §3).
pub trait WorktreeReader: Send + Sync {
    /// Every worktree of the repository `repo` belongs to (`repo` may be
    /// any of them): the main one first, then the linked ones in Git's
    /// order. A repository without linked worktrees returns its main one.
    fn worktrees(&self, repo: &RepoPath) -> Result<Vec<WorktreeInfo>, GitError>;
}

/// Creates, removes, locks and prunes worktrees (`git worktree …`). `repo`
/// is any worktree of the repository; worktree paths are absolute.
pub trait WorktreeWriter: Send + Sync {
    /// `git worktree add`: creates the worktree of `request`.
    fn add_worktree(&self, repo: &RepoPath, request: &WorktreeAddRequest) -> Result<(), GitError>;
    /// `git worktree remove` (`--force` with `force`: dirty or locked
    /// worktrees too; the caller has said what would be lost).
    fn remove_worktree(&self, repo: &RepoPath, path: &Path, force: bool) -> Result<(), GitError>;
    /// Locks (`Some(reason)`) or unlocks (`None`) a worktree.
    fn lock_worktree(
        &self,
        repo: &RepoPath,
        path: &Path,
        reason: Option<&str>,
    ) -> Result<(), GitError>;
    /// `git worktree prune`: forgets the worktrees whose folder is gone
    /// (locked ones are kept).
    fn prune_worktrees(&self, repo: &RepoPath) -> Result<(), GitError>;
    /// `git worktree move`.
    fn move_worktree(&self, repo: &RepoPath, from: &Path, to: &Path) -> Result<(), GitError>;
}

/// Copies a repository's local files (untracked or ignored: `.env`,
/// local configs) into another worktree of it (spec §10).
pub trait LocalFilesCopier: Send + Sync {
    /// Copies the files of worktree `from` matching `patterns` (`/`-relative
    /// globs: `*`, `?` within a component, `**` for any depth) to the same
    /// relative paths in worktree `to`, never overwriting an existing file
    /// and never following a link. Missing files are ignored. Returns the
    /// relative paths copied, sorted.
    fn copy_local_files(
        &self,
        from: &RepoPath,
        to: &RepoPath,
        patterns: &[String],
    ) -> Result<Vec<String>, GitError>;
}
