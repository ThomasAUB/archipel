//! Worktree actions (spec §10, WORKTREES.md §6): create, remove (saying what
//! would be lost, undoable), lock, prune. Journaled on the repository's
//! main worktree, so `Ctrl+Z` there brings a removed worktree back.

use archipel_model::journal::{BatchId, HeadRef, OperationKind, WorktreeRecord};
use archipel_model::requests::{DeleteMode, SubmoduleUpdate};
use archipel_model::worktree::{WorktreeAddRequest, WorktreeTarget};
use archipel_model::{CancelToken, Oid, ProgressSink, RepoOp, RepoPath};

use crate::actions::RepoActions;
use crate::error::OpError;
use crate::journal::Recording;

/// What removing a worktree would lose (the Remove and Finish dialogs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeLoss {
    /// Uncommitted changes (paths).
    pub changes: Vec<String>,
    /// Commits of its `HEAD` on no remote-tracking branch (at least 1 when
    /// any; the count is the upstream's "ahead" when known).
    pub unpushed: u32,
    /// `unpushed` is a count (the branch has an upstream), not just "some".
    pub unpushed_counted: bool,
    /// Operation in progress (must be aborted first).
    pub in_progress: Option<RepoOp>,
    /// Its branch, if on one.
    pub branch: Option<String>,
    /// Its branch is merged into the default branch or fully pushed:
    /// deleting it loses nothing.
    pub branch_safe_to_delete: bool,
}

impl WorktreeLoss {
    /// Removing it loses nothing.
    #[must_use]
    pub fn is_safe(&self) -> bool {
        self.changes.is_empty() && self.unpushed == 0 && self.in_progress.is_none()
    }

    /// One line per thing lost, for the dialogs.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(op) = self.in_progress {
            out.push(format!("{} in progress", op_name(op)));
        }
        match self.changes.len() {
            0 => {}
            1 => out.push("1 file changed".to_owned()),
            n => out.push(format!("{n} files changed")),
        }
        match (self.unpushed, self.unpushed_counted) {
            (0, _) => {}
            (_, false) => out.push("commits not pushed anywhere".to_owned()),
            (1, true) => out.push("1 unpushed commit".to_owned()),
            (n, true) => out.push(format!("{n} unpushed commits")),
        }
        out
    }
}

fn op_name(op: RepoOp) -> &'static str {
    match op {
        RepoOp::Merge => "merge",
        RepoOp::Rebase => "rebase",
        RepoOp::CherryPick => "cherry-pick",
        RepoOp::Revert => "revert",
        RepoOp::Bisect => "bisect",
        RepoOp::ApplyMailbox => "am",
    }
}

/// What to do after `git worktree add` (WORKTREES.md §6.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeSetup {
    /// Patterns of local files copied from the main worktree.
    pub copy: Vec<String>,
    /// `git submodule update --init --recursive` in the new worktree.
    pub init_submodules: bool,
    /// Batch the creation belongs to (a Worktree Set: undone as one).
    pub batch: Option<BatchId>,
}

/// How to remove a worktree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoveOptions {
    /// Remove it although it has changes (they are snapshotted for undo).
    pub force: bool,
    /// Also delete its branch.
    pub delete_branch: bool,
    /// Batch the removal belongs to (finishing a Worktree Set).
    pub batch: Option<BatchId>,
}

/// Branches a default branch may be called, nearest first.
const DEFAULT_BRANCHES: &[&str] = &[
    "refs/remotes/origin/HEAD",
    "refs/heads/main",
    "refs/heads/master",
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
];

fn branch_ref(name: &str) -> String {
    format!("refs/heads/{name}")
}

impl RepoActions {
    /// Creates a worktree of `repo` (any worktree of it), then copies the
    /// local files and initializes submodules in it. Journaled on `repo`:
    /// undo removes it again while it is clean. Returns the files copied.
    pub fn add_worktree(
        &self,
        repo: &RepoPath,
        request: &WorktreeAddRequest,
        setup: &WorktreeSetup,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<Vec<String>, OpError> {
        let g = self.ports();
        let branch = request.target.branch().map(str::to_owned);
        let refs: Vec<String> = branch.iter().map(|b| branch_ref(b)).collect();
        let created = match &branch {
            Some(b) => g.refs.resolve(repo, &branch_ref(b))?.is_none(),
            None => false,
        };
        let head = match (&branch, &request.target) {
            (Some(b), _) => HeadRef::Symbolic(branch_ref(b)),
            (None, WorktreeTarget::Detached(rev)) => HeadRef::Detached(
                g.refs
                    .resolve(repo, rev)?
                    .ok_or_else(|| OpError::Refused(format!("{rev} is not a commit")))?,
            ),
            (None, _) => return Err(OpError::Refused("nothing to check out".into())),
        };
        let label = format!(
            "New worktree {}",
            request.target.branch().unwrap_or("(detached)")
        );
        let rec = Recording::new(OperationKind::WorktreeAdd, label)
            .with_refs(refs)
            .in_batch(setup.batch)
            .with_worktree(
                WorktreeRecord {
                    path: request.path.clone(),
                    head,
                    branch_changed: created,
                },
                None,
            );
        self.journal()
            .record(repo, rec, || g.worktree_writer.add_worktree(repo, request))?;
        let new = RepoPath::new(request.path.clone());
        let copied = if setup.copy.is_empty() {
            Vec::new()
        } else {
            g.local_files.copy_local_files(repo, &new, &setup.copy)?
        };
        if setup.init_submodules {
            let update = SubmoduleUpdate {
                paths: Vec::new(),
                init: true,
                recursive: true,
            };
            g.submodule_writer
                .update_submodules(&new, &update, progress, cancel)?;
        }
        Ok(copied)
    }

    /// What removing worktree `wt` would lose.
    pub fn worktree_loss(&self, wt: &RepoPath) -> Result<WorktreeLoss, OpError> {
        let g = self.ports();
        let summary = g.status.summary(wt)?;
        let changes = g.status.changes(wt)?.into_iter().map(|c| c.path).collect();
        let branch = summary.head.branch().map(str::to_owned);
        let remotes: Vec<Oid> = g
            .refs
            .branches(wt)?
            .into_iter()
            .filter(|b| b.is_remote)
            .map(|b| b.target)
            .collect();
        let contained = |oid: Oid, tips: &[Oid]| -> Result<bool, OpError> {
            for tip in tips {
                if g.history.is_ancestor(wt, oid, *tip)? {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        let unpushed_counted = summary.ahead() > 0;
        let (unpushed, safe) = match summary.head_oid {
            None => (0, true),
            Some(head) => {
                let pushed = contained(head, &remotes)?;
                let unpushed = if pushed { 0 } else { summary.ahead().max(1) };
                let mut defaults = Vec::new();
                for name in DEFAULT_BRANCHES {
                    let own = branch.as_deref().is_some_and(|b| *name == branch_ref(b));
                    if !own && let Some(oid) = g.refs.resolve(wt, name)? {
                        defaults.push(oid);
                    }
                }
                (unpushed, pushed || contained(head, &defaults)?)
            }
        };
        Ok(WorktreeLoss {
            changes,
            unpushed,
            unpushed_counted,
            in_progress: summary.in_progress,
            branch,
            branch_safe_to_delete: safe,
        })
    }

    /// Removes linked worktree `wt` of `repo` (its main worktree), after
    /// snapshotting its changes: journaled on `repo`, undo recreates it at
    /// the same path on the same branch and restores the files. Refused
    /// while an operation is in progress in it, and when it has changes
    /// unless `options.force`.
    pub fn remove_worktree(
        &self,
        repo: &RepoPath,
        wt: &RepoPath,
        options: RemoveOptions,
    ) -> Result<(), OpError> {
        let g = self.ports();
        let summary = g.status.summary(wt)?;
        if let Some(op) = summary.in_progress {
            return Err(OpError::Refused(format!(
                "a {} is in progress in {}: abort it first",
                op_name(op),
                wt.as_path().display()
            )));
        }
        let paths: Vec<String> = g.status.changes(wt)?.into_iter().map(|c| c.path).collect();
        if !paths.is_empty() && !options.force {
            return Err(OpError::Refused(format!(
                "{} has uncommitted changes",
                wt.as_path().display()
            )));
        }
        let head = g.refs.snapshot(wt, &[])?.head;
        let branch = match &head {
            HeadRef::Symbolic(r) => Some(r.trim_start_matches("refs/heads/").to_owned()),
            HeadRef::Detached(_) => None,
        };
        let snapshot = g.snapshots.snapshot_paths(wt, &paths)?;
        let delete = options.delete_branch && branch.is_some();
        let label = format!(
            "Remove worktree {}",
            branch.as_deref().unwrap_or("(detached)")
        );
        let rec = Recording::new(OperationKind::WorktreeRemove, label)
            .with_refs(branch.iter().map(|b| branch_ref(b)))
            .in_batch(options.batch)
            .with_worktree(
                WorktreeRecord {
                    path: wt.as_path().to_path_buf(),
                    head,
                    branch_changed: delete,
                },
                snapshot,
            );
        // Untracked files make Git refuse without `--force`: they are in
        // the snapshot.
        let force = options.force || !paths.is_empty();
        self.journal().record(repo, rec, || {
            g.worktree_writer
                .remove_worktree(repo, wt.as_path(), force)?;
            if delete && let Some(b) = &branch {
                g.branches.delete_branch(repo, b, DeleteMode::Force)?;
            }
            Ok(())
        })
    }

    /// Locks (`Some(reason)`) or unlocks linked worktree `wt` of `repo`.
    pub fn lock_worktree(
        &self,
        repo: &RepoPath,
        wt: &RepoPath,
        reason: Option<&str>,
    ) -> Result<(), OpError> {
        Ok(self
            .ports()
            .worktree_writer
            .lock_worktree(repo, wt.as_path(), reason)?)
    }

    /// Forgets the worktrees of `repo` whose folder is gone (locked ones
    /// are kept, as Git does).
    pub fn prune_worktrees(&self, repo: &RepoPath) -> Result<(), OpError> {
        Ok(self.ports().worktree_writer.prune_worktrees(repo)?)
    }
}
