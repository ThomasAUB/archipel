//! Single-repository actions (PLAN Phase 4 step 6), each journaled so it can
//! be undone. These are blocking calls meant to run inside scheduler jobs.

use std::sync::Arc;

use archipel_model::journal::{BatchId, OperationKind, StashRecord};
use archipel_model::ports::{
    BranchWriter, CommitWriter, HistoryReader, HistoryRewriter, IndexWriter, MergeToolLauncher,
    RefReader, RefWriter, RemoteOps, RemoteReader, StashReader, StashWriter, StatusReader,
    SubmoduleReader, SubmoduleWriter, TagWriter, WorktreeFiles,
};
use archipel_model::requests::{
    CheckoutTarget, CommitMode, CommitRequest, DeleteMode, FetchRequest, MergeMode, PatchDirection,
    PullMode, PushRequest, ResetMode, SequenceAction, StashApplyMode, StashRequest, TagRequest,
};
use archipel_model::{CancelToken, GitError, HeadState, Oid, ProgressSink, RepoPath};

use crate::error::OpError;
use crate::journal::{Journal, Recording};

/// The Git ports used by actions, each a narrow trait object.
///
/// One adapter usually implements all of them; [`GitPorts::from_backend`]
/// wires such an adapter in one line.
#[derive(Clone)]
pub struct GitPorts {
    /// Status reads.
    pub status: Arc<dyn StatusReader>,
    /// Ref reads.
    pub refs: Arc<dyn RefReader>,
    /// History reads.
    pub history: Arc<dyn HistoryReader>,
    /// Stash list.
    pub stashes: Arc<dyn StashReader>,
    /// Commits.
    pub commits: Arc<dyn CommitWriter>,
    /// Index and worktree.
    pub index: Arc<dyn IndexWriter>,
    /// Branches.
    pub branches: Arc<dyn BranchWriter>,
    /// Tags.
    pub tags: Arc<dyn TagWriter>,
    /// Raw ref moves.
    pub ref_writer: Arc<dyn RefWriter>,
    /// Stash writes and worktree snapshots.
    pub stash_writer: Arc<dyn StashWriter>,
    /// Network.
    pub remote: Arc<dyn RemoteOps>,
    /// Merge, rebase, cherry-pick, revert.
    pub rewriter: Arc<dyn HistoryRewriter>,
    /// External merge tool.
    pub merge_tool: Arc<dyn MergeToolLauncher>,
    /// Submodule list (dependency tree).
    pub submodules: Arc<dyn SubmoduleReader>,
    /// Submodule init / update.
    pub submodule_writer: Arc<dyn SubmoduleWriter>,
    /// Remote configuration (forge detection from the remote URL).
    pub remotes: Arc<dyn RemoteReader>,
    /// Worktree file contents (the diff view's file editor).
    pub files: Arc<dyn WorktreeFiles>,
}

impl GitPorts {
    /// Uses one backend for every port.
    pub fn from_backend<B>(backend: Arc<B>) -> Self
    where
        B: StatusReader
            + RefReader
            + HistoryReader
            + StashReader
            + CommitWriter
            + IndexWriter
            + BranchWriter
            + TagWriter
            + RefWriter
            + StashWriter
            + RemoteOps
            + HistoryRewriter
            + MergeToolLauncher
            + SubmoduleReader
            + SubmoduleWriter
            + RemoteReader
            + WorktreeFiles
            + 'static,
    {
        Self {
            status: backend.clone(),
            refs: backend.clone(),
            history: backend.clone(),
            stashes: backend.clone(),
            commits: backend.clone(),
            index: backend.clone(),
            branches: backend.clone(),
            tags: backend.clone(),
            ref_writer: backend.clone(),
            stash_writer: backend.clone(),
            remote: backend.clone(),
            rewriter: backend.clone(),
            merge_tool: backend.clone(),
            submodules: backend.clone(),
            submodule_writer: backend.clone(),
            remotes: backend.clone(),
            files: backend,
        }
    }
}

fn branch_ref(name: &str) -> String {
    format!("refs/heads/{name}")
}

fn tag_ref(name: &str) -> String {
    format!("refs/tags/{name}")
}

/// Single-repository actions with journaling.
#[derive(Clone)]
pub struct RepoActions {
    git: GitPorts,
    journal: Journal,
}

impl RepoActions {
    /// Creates the actions.
    #[must_use]
    pub fn new(git: GitPorts, journal: Journal) -> Self {
        Self { git, journal }
    }

    /// The ports.
    #[must_use]
    pub fn ports(&self) -> &GitPorts {
        &self.git
    }

    /// The journal.
    #[must_use]
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Commits the index or amends `HEAD`.
    pub fn commit(
        &self,
        repo: &RepoPath,
        request: &CommitRequest,
        batch: Option<BatchId>,
    ) -> Result<Oid, OpError> {
        let kind = if request.mode == CommitMode::Amend {
            OperationKind::Amend
        } else {
            OperationKind::Commit
        };
        let summary = request.message.lines().next().unwrap_or_default();
        let rec = Recording::new(kind, format!("Commit “{summary}”")).in_batch(batch);
        self.journal
            .record(repo, rec, || self.git.commits.commit(repo, request))
    }

    /// Stages whole files.
    pub fn stage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), OpError> {
        Ok(self.git.index.stage(repo, paths)?)
    }

    /// Unstages whole files.
    pub fn unstage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), OpError> {
        Ok(self.git.index.unstage(repo, paths)?)
    }

    /// Stages or unstages part of a file (hunk / lines), see [`crate::patch`].
    pub fn apply_patch(
        &self,
        repo: &RepoPath,
        patch: &str,
        direction: PatchDirection,
    ) -> Result<(), OpError> {
        Ok(self.git.index.apply_patch(repo, patch, direction)?)
    }

    /// Discards part of the worktree changes of `file` (a hunk / line
    /// patch of its unstaged diff, see [`crate::patch`]) after snapshotting
    /// the file (undoable).
    pub fn discard_patch(&self, repo: &RepoPath, file: &str, patch: &str) -> Result<(), OpError> {
        let paths = [file.to_owned()];
        let rec = Recording::new(OperationKind::Discard, format!("Discard lines of {file}"))
            .snapshot_paths(&paths);
        self.journal
            .record(repo, rec, || self.git.index.discard_patch(repo, patch))
    }

    /// The bytes of a worktree file (`path` relative to the repo root).
    pub fn read_file(&self, repo: &RepoPath, path: &str) -> Result<Vec<u8>, OpError> {
        Ok(self.git.files.read_file(repo, path)?)
    }

    /// Saves an edit of a worktree file: writes `contents` over `path`,
    /// unless the file no longer holds `expected` (what the editor was
    /// opened on), so a change made meanwhile elsewhere is never lost.
    pub fn save_file(
        &self,
        repo: &RepoPath,
        path: &str,
        expected: &[u8],
        contents: &[u8],
    ) -> Result<(), OpError> {
        if self.git.files.read_file(repo, path)? != expected {
            return Err(OpError::Refused(format!(
                "{path} changed on disk since it was opened: reopen it to edit"
            )));
        }
        Ok(self.git.files.write_file(repo, path, contents)?)
    }

    /// Discards worktree changes after snapshotting them (undoable).
    pub fn discard(&self, repo: &RepoPath, paths: &[String]) -> Result<(), OpError> {
        let rec = Recording::new(
            OperationKind::Discard,
            format!("Discard {} file(s)", paths.len()),
        )
        .snapshot_paths(paths);
        self.journal
            .record(repo, rec, || self.git.index.discard(repo, paths))
    }

    /// Checks out a branch or commit.
    pub fn checkout(
        &self,
        repo: &RepoPath,
        target: &CheckoutTarget,
        batch: Option<BatchId>,
    ) -> Result<(), OpError> {
        let (label, refs) = match target {
            CheckoutTarget::Branch(b) => (format!("Checkout {b}"), vec![branch_ref(b)]),
            CheckoutTarget::TrackRemote(r) => {
                let local = r.split_once('/').map_or(r.as_str(), |(_, b)| b);
                (
                    format!("Checkout {local} tracking {r}"),
                    vec![branch_ref(local)],
                )
            }
            CheckoutTarget::Detached(oid) => (format!("Checkout {}", oid.short(8)), Vec::new()),
        };
        let rec = Recording::new(OperationKind::Checkout, label)
            .with_refs(refs)
            .in_batch(batch);
        self.journal
            .record(repo, rec, || self.git.branches.checkout(repo, target))
    }

    /// Creates a branch.
    pub fn create_branch(
        &self,
        repo: &RepoPath,
        name: &str,
        start: Option<&str>,
        batch: Option<BatchId>,
    ) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::BranchCreate, format!("Create branch {name}"))
            .with_refs([branch_ref(name)])
            .in_batch(batch);
        self.journal.record(repo, rec, || {
            self.git.branches.create_branch(repo, name, start)
        })
    }

    /// Renames a branch.
    pub fn rename_branch(&self, repo: &RepoPath, old: &str, new: &str) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::BranchRename, format!("Rename {old} → {new}"))
            .with_refs([branch_ref(old), branch_ref(new)]);
        self.journal.record(repo, rec, || {
            self.git.branches.rename_branch(repo, old, new)
        })
    }

    /// Deletes a branch.
    pub fn delete_branch(
        &self,
        repo: &RepoPath,
        name: &str,
        mode: DeleteMode,
    ) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::BranchDelete, format!("Delete branch {name}"))
            .with_refs([branch_ref(name)]);
        self.journal.record(repo, rec, || {
            self.git.branches.delete_branch(repo, name, mode)
        })
    }

    /// Sets or unsets a branch upstream (configuration only, not journaled).
    pub fn set_upstream(
        &self,
        repo: &RepoPath,
        branch: &str,
        upstream: Option<&str>,
    ) -> Result<(), OpError> {
        Ok(self.git.branches.set_upstream(repo, branch, upstream)?)
    }

    /// Creates a tag.
    pub fn create_tag(
        &self,
        repo: &RepoPath,
        request: &TagRequest,
        batch: Option<BatchId>,
    ) -> Result<(), OpError> {
        let rec = Recording::new(
            OperationKind::TagCreate,
            format!("Create tag {}", request.name),
        )
        .with_refs([tag_ref(&request.name)])
        .in_batch(batch);
        self.journal
            .record(repo, rec, || self.git.tags.create_tag(repo, request))
    }

    /// Deletes a tag.
    pub fn delete_tag(&self, repo: &RepoPath, name: &str) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::TagDelete, format!("Delete tag {name}"))
            .with_refs([tag_ref(name)]);
        self.journal
            .record(repo, rec, || self.git.tags.delete_tag(repo, name))
    }

    /// Pushes a single tag to a remote.
    pub fn push_tag(
        &self,
        repo: &RepoPath,
        remote: &str,
        name: &str,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), OpError> {
        let request = PushRequest {
            remote: Some(remote.into()),
            refspec: Some(tag_ref(name)),
            ..PushRequest::default()
        };
        Ok(self.git.remote.push(repo, &request, progress, cancel)?)
    }

    /// Pushes a stash entry; returns `false` when there was nothing to stash.
    pub fn stash_push(&self, repo: &RepoPath, request: &StashRequest) -> Result<bool, OpError> {
        let pushed = self.git.stash_writer.stash_push(repo, request)?;
        if pushed {
            // Journal after the fact: a push only adds a stash entry.
            let top = self.git.stashes.stashes(repo)?.into_iter().next();
            let record = top.map(|s| StashRecord {
                oid: s.oid,
                message: s.message,
            });
            let rec = Recording::new(OperationKind::StashPush, "Stash changes").with_stash(record);
            self.journal.record(repo, rec, || Ok(()))?;
        }
        Ok(pushed)
    }

    fn stash_at(&self, repo: &RepoPath, index: u32) -> Result<StashRecord, OpError> {
        self.git
            .stashes
            .stashes(repo)?
            .into_iter()
            .find(|s| s.index == index)
            .map(|s| StashRecord {
                oid: s.oid,
                message: s.message,
            })
            .ok_or_else(|| OpError::Refused(format!("no stash at index {index}")))
    }

    /// Applies or pops a stash.
    pub fn stash_apply(
        &self,
        repo: &RepoPath,
        index: u32,
        mode: StashApplyMode,
    ) -> Result<(), OpError> {
        if mode == StashApplyMode::Keep {
            return Ok(self.git.stash_writer.stash_apply(repo, index, mode)?);
        }
        let record = self.stash_at(repo, index)?;
        let rec = Recording::new(OperationKind::StashPop, format!("Pop “{}”", record.message))
            .with_stash(Some(record));
        self.journal.record(repo, rec, || {
            self.git.stash_writer.stash_apply(repo, index, mode)
        })
    }

    /// Drops a stash (undoable: the stash commit is recorded).
    pub fn stash_drop(&self, repo: &RepoPath, index: u32) -> Result<(), OpError> {
        let record = self.stash_at(repo, index)?;
        let rec = Recording::new(
            OperationKind::StashDrop,
            format!("Drop “{}”", record.message),
        )
        .with_stash(Some(record));
        self.journal
            .record(repo, rec, || self.git.stash_writer.drop_stash(repo, index))
    }

    /// Fetches (nothing to undo, not journaled).
    pub fn fetch(
        &self,
        repo: &RepoPath,
        request: &FetchRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), OpError> {
        Ok(self.git.remote.fetch(repo, request, progress, cancel)?)
    }

    /// Fetches every remote unattended; returns whether any ref moved.
    pub fn fetch_background(&self, repo: &RepoPath, cancel: &CancelToken) -> Result<bool, OpError> {
        Ok(self.git.remote.fetch_background(repo, cancel)?)
    }

    /// Pulls the current branch (the local part is undoable).
    pub fn pull(
        &self,
        repo: &RepoPath,
        mode: PullMode,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
        batch: Option<BatchId>,
    ) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::Pull, "Pull").in_batch(batch);
        self.journal.record(repo, rec, || {
            self.git.remote.pull(repo, mode, progress, cancel)
        })
    }

    /// Pushes; the remote-tracking ref is recorded for compensating actions.
    pub fn push(
        &self,
        repo: &RepoPath,
        request: &PushRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
        batch: Option<BatchId>,
    ) -> Result<(), OpError> {
        let remote = request.remote.clone().unwrap_or_else(|| "origin".into());
        let branch = match &request.refspec {
            Some(spec) => spec
                .rsplit(':')
                .next()
                .unwrap_or(spec)
                .trim_start_matches("refs/heads/")
                .to_owned(),
            None => match self.git.status.summary(repo)?.head {
                HeadState::Branch(b) => b,
                _ => {
                    return Err(OpError::Refused(
                        "cannot push a detached HEAD without a refspec".into(),
                    ));
                }
            },
        };
        let rec = Recording::new(OperationKind::Push, format!("Push {branch} to {remote}"))
            .with_refs([format!("refs/remotes/{remote}/{branch}")])
            .in_batch(batch);
        self.journal.record(repo, rec, || {
            self.git.remote.push(repo, request, progress, cancel)
        })
    }

    /// Resets `HEAD` (hard resets snapshot the worktree first).
    pub fn reset(&self, repo: &RepoPath, target: &str, mode: ResetMode) -> Result<(), OpError> {
        let mut rec = Recording::new(
            OperationKind::Reset,
            format!("Reset ({mode:?}) to {target}"),
        );
        if mode == ResetMode::Hard {
            rec = rec.snapshot_worktree();
        }
        self.journal
            .record(repo, rec, || self.git.ref_writer.reset(repo, target, mode))
    }

    /// Reverts a commit.
    pub fn revert(&self, repo: &RepoPath, commit: Oid) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::Revert, format!("Revert {}", commit.short(8)));
        self.journal
            .record(repo, rec, || self.git.rewriter.revert(repo, commit))
    }

    /// Merges `rev` into the current branch.
    pub fn merge(&self, repo: &RepoPath, rev: &str, mode: MergeMode) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::Merge, format!("Merge {rev}"));
        self.journal
            .record(repo, rec, || self.git.rewriter.merge(repo, rev, mode))
    }

    /// Rebases the current branch onto `onto`.
    pub fn rebase(&self, repo: &RepoPath, onto: &str) -> Result<(), OpError> {
        let rec = Recording::new(OperationKind::Rebase, format!("Rebase onto {onto}"));
        self.journal
            .record(repo, rec, || self.git.rewriter.rebase(repo, onto))
    }

    /// Cherry-picks commits onto the current branch.
    pub fn cherry_pick(&self, repo: &RepoPath, commits: &[Oid]) -> Result<(), OpError> {
        let rec = Recording::new(
            OperationKind::CherryPick,
            format!("Cherry-pick {} commit(s)", commits.len()),
        );
        self.journal
            .record(repo, rec, || self.git.rewriter.cherry_pick(repo, commits))
    }

    /// Fast-forwards a branch that is not checked out to `target`.
    pub fn fast_forward(&self, repo: &RepoPath, branch: &str, target: Oid) -> Result<(), OpError> {
        let name = branch_ref(branch);
        let current = self
            .git
            .refs
            .resolve(repo, &name)?
            .ok_or_else(|| OpError::Refused(format!("no branch {branch}")))?;
        if !self.git.history.is_ancestor(repo, current, target)? {
            return Err(OpError::Refused(format!(
                "{branch} cannot be fast-forwarded: histories diverged"
            )));
        }
        if self.git.status.summary(repo)?.head.branch() == Some(branch) {
            return self.merge(repo, &target.to_hex(), MergeMode::FastForwardOnly);
        }
        let rec = Recording::new(OperationKind::FastForward, format!("Fast-forward {branch}"))
            .with_refs([name.clone()]);
        self.journal.record(repo, rec, || {
            self.git
                .ref_writer
                .update_ref(repo, &name, Some(target), Some(current))
        })
    }

    /// Continues, aborts or skips the operation in progress.
    pub fn sequence(&self, repo: &RepoPath, action: SequenceAction) -> Result<(), OpError> {
        Ok(self.git.rewriter.sequence(repo, action)?)
    }

    /// Opens the external merge tool.
    pub fn merge_tool(
        &self,
        repo: &RepoPath,
        path: Option<&str>,
        tool: Option<&str>,
    ) -> Result<(), OpError> {
        self.git
            .merge_tool
            .launch(repo, path, tool)
            .map_err(|e| match e {
                GitError::CommandFailed { .. } => OpError::Git(e),
                other => other.into(),
            })
    }
}
