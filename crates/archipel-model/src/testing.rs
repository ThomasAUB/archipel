//! In-memory fakes of every port trait, for consumers' tests.
//!
//! The fakes are deliberately simple: they keep plain data in maps, record
//! every write call, and can be told to fail a named operation.

use std::collections::{BTreeMap, HashMap};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::cancel::{CancelToken, ProgressSink};
use crate::changes::FileChange;
use crate::diff::FileDiff;
use crate::error::{GitError, ProjectError, StoreError, WatchError};
use crate::history::{CommitMeta, TopologyNode, WalkOrder};
use crate::ids::{Oid, ProjectId, RepoId, RepoPath};
use crate::journal::{BatchId, EntryState, HeadRef, JournalEntry, JournalId, RefSnapshot};
use crate::ports::{
    BranchWriter, ChangeSource, ChurnReader, CommitChurn, CommitWriter, Coverage, DiffReader,
    DiffSide, HistoryReader, HistoryRewriter, IndexWriter, InvalidationSink, JournalStore,
    MergeToolLauncher, ProjectRegistry, ProjectSource, RefReader, RefWriter, RemoteInfo, RemoteOps,
    RemoteReader, RepoDiscovery, StashReader, StashWriter, StatsStore, StatusReader,
    SubmoduleReader, SubmoduleWriter, SummaryStore, TagWriter, TreeReader, UiStateStore,
    WatchHandle, WorktreeFiles, WorktreeSnapshots,
};
use crate::project::{DiscoveredRepo, ProjectDef, ProjectLocation, RegistryEntry, ScanOptions};
use crate::refs::{BranchInfo, RefTip, StashEntry, TagInfo};
use crate::requests::{
    CheckoutTarget, CommitRequest, DeleteMode, FetchRequest, MergeMode, PatchDirection, PullMode,
    PushRequest, ResetMode, SequenceAction, StashApplyMode, StashRequest, SubmoduleUpdate,
    TagRequest,
};
use crate::stats::{ActivityBucket, Hotspot, StatsDelta};
use crate::submodule::SubmoduleInfo;
use crate::summary::{HeadState, RepoSummary};
use crate::time::Timestamp;
use crate::tree::TreeEntry;
use crate::ui_state::UiState;

mod forge;
pub use forge::{
    CreatedPullRequest, FakeForge, FakeReleaseFeed, InMemoryCredentials, MergedPullRequest,
};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Builds a deterministic fake object id from a number.
#[must_use]
pub fn oid(n: u64) -> Oid {
    let mut raw = [0u8; 20];
    raw[12..].copy_from_slice(&n.to_be_bytes());
    Oid::from_bytes(&raw).unwrap_or_else(|| unreachable!("20 bytes is a valid length"))
}

/// State of one fake repository.
#[derive(Clone, Debug)]
pub struct FakeRepo {
    /// Summary returned by `StatusReader::summary`.
    pub summary: RepoSummary,
    /// Changes returned by `StatusReader::changes`.
    pub changes: Vec<FileChange>,
    /// Branches.
    pub branches: Vec<BranchInfo>,
    /// Tags.
    pub tags: Vec<TagInfo>,
    /// Stashes.
    pub stashes: Vec<StashEntry>,
    /// Remotes.
    pub remotes: Vec<RemoteInfo>,
    /// History in display order.
    pub history: Vec<(TopologyNode, CommitMeta)>,
    /// Churn, newest first.
    pub churn: Vec<CommitChurn>,
    /// Commit diffs.
    pub diffs: HashMap<Oid, Vec<FileDiff>>,
    /// Full ref names → targets.
    pub refs: BTreeMap<String, Oid>,
    /// Where `HEAD` points.
    pub head: HeadRef,
    /// Submodules (one level).
    pub submodules: Vec<SubmoduleInfo>,
    /// Worktree files (relative path → bytes), for `WorktreeFiles`.
    pub files: BTreeMap<String, Vec<u8>>,
    /// Files of commit trees (commit → path → bytes), for `TreeReader`.
    pub trees: HashMap<Oid, BTreeMap<String, Vec<u8>>>,
}

impl Default for FakeRepo {
    fn default() -> Self {
        Self {
            summary: RepoSummary::empty("main"),
            changes: Vec::new(),
            branches: Vec::new(),
            tags: Vec::new(),
            stashes: Vec::new(),
            remotes: Vec::new(),
            history: Vec::new(),
            churn: Vec::new(),
            diffs: HashMap::new(),
            refs: BTreeMap::new(),
            head: HeadRef::Symbolic("refs/heads/main".into()),
            submodules: Vec::new(),
            files: BTreeMap::new(),
            trees: HashMap::new(),
        }
    }
}

impl FakeRepo {
    fn head_oid(&self) -> Option<Oid> {
        match &self.head {
            HeadRef::Symbolic(name) => self.refs.get(name).copied(),
            HeadRef::Detached(oid) => Some(*oid),
        }
    }
}

/// A recorded call to a fake writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeCall {
    /// Operation name, e.g. `"commit"`.
    pub op: &'static str,
    /// Repository path (or destination for clone).
    pub repo: String,
    /// Free-form details (message, ref name…).
    pub detail: String,
}

/// Fake implementation of every Git trait.
#[derive(Default)]
pub struct FakeGit {
    repos: Mutex<HashMap<RepoPath, FakeRepo>>,
    failures: Mutex<HashMap<(RepoPath, &'static str), GitError>>,
    calls: Mutex<Vec<FakeCall>>,
    next_oid: AtomicU64,
}

impl FakeGit {
    /// An empty fake.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_oid: AtomicU64::new(1_000_000),
            ..Self::default()
        }
    }

    /// Adds or replaces a repository.
    pub fn insert(&self, path: &RepoPath, repo: FakeRepo) {
        lock(&self.repos).insert(path.clone(), repo);
    }

    /// Mutates a repository in place (creating it if needed).
    pub fn with_repo<R>(&self, path: &RepoPath, f: impl FnOnce(&mut FakeRepo) -> R) -> R {
        f(lock(&self.repos).entry(path.clone()).or_default())
    }

    /// Makes operation `op` fail on `path` with `error` until cleared.
    pub fn fail(&self, path: &RepoPath, op: &'static str, error: GitError) {
        lock(&self.failures).insert((path.clone(), op), error);
    }

    /// Clears every configured failure.
    pub fn clear_failures(&self) {
        lock(&self.failures).clear();
    }

    /// Every recorded write call, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeCall> {
        lock(&self.calls).clone()
    }

    fn record(
        &self,
        op: &'static str,
        repo: &RepoPath,
        detail: impl Into<String>,
    ) -> Result<(), GitError> {
        if let Some(err) = lock(&self.failures).get(&(repo.clone(), op)) {
            return Err(err.clone());
        }
        lock(&self.calls).push(FakeCall {
            op,
            repo: repo.as_path().display().to_string(),
            detail: detail.into(),
        });
        Ok(())
    }

    fn read<R>(
        &self,
        repo: &RepoPath,
        op: &'static str,
        f: impl FnOnce(&FakeRepo) -> R,
    ) -> Result<R, GitError> {
        if let Some(err) = lock(&self.failures).get(&(repo.clone(), op)) {
            return Err(err.clone());
        }
        let repos = lock(&self.repos);
        let state = repos.get(repo).ok_or_else(|| GitError::NotARepository {
            path: repo.as_path().to_path_buf(),
        })?;
        Ok(f(state))
    }

    fn fresh_oid(&self) -> Oid {
        oid(self.next_oid.fetch_add(1, Ordering::Relaxed))
    }
}

impl StatusReader for FakeGit {
    fn summary(&self, repo: &RepoPath) -> Result<RepoSummary, GitError> {
        self.read(repo, "summary", |r| {
            let mut s = r.summary.clone();
            s.head_oid = r.head_oid().or(s.head_oid);
            s
        })
    }
    fn changes(&self, repo: &RepoPath) -> Result<Vec<FileChange>, GitError> {
        self.read(repo, "changes", |r| r.changes.clone())
    }
}

impl RefReader for FakeGit {
    fn branches(&self, repo: &RepoPath) -> Result<Vec<BranchInfo>, GitError> {
        self.read(repo, "branches", |r| r.branches.clone())
    }
    fn tags(&self, repo: &RepoPath) -> Result<Vec<TagInfo>, GitError> {
        self.read(repo, "tags", |r| r.tags.clone())
    }
    fn ref_tips(&self, repo: &RepoPath) -> Result<Vec<RefTip>, GitError> {
        self.read(repo, "ref_tips", |r| {
            r.refs
                .iter()
                .map(|(name, target)| RefTip {
                    name: name.clone(),
                    kind: crate::refs::RefKind::LocalBranch,
                    target: *target,
                })
                .collect()
        })
    }
    fn resolve(&self, repo: &RepoPath, rev: &str) -> Result<Option<Oid>, GitError> {
        self.read(repo, "resolve", |r| {
            if rev == "HEAD" {
                return r.head_oid();
            }
            r.refs
                .get(rev)
                .or_else(|| r.refs.get(&format!("refs/heads/{rev}")))
                .or_else(|| r.refs.get(&format!("refs/tags/{rev}")))
                .copied()
                .or_else(|| Oid::from_hex(rev).ok())
        })
    }
    fn snapshot(&self, repo: &RepoPath, refs: &[String]) -> Result<RefSnapshot, GitError> {
        self.read(repo, "snapshot", |r| RefSnapshot {
            head: r.head.clone(),
            refs: refs
                .iter()
                .map(|n| (n.clone(), r.refs.get(n).copied()))
                .collect(),
        })
    }
}

impl RemoteReader for FakeGit {
    fn remotes(&self, repo: &RepoPath) -> Result<Vec<RemoteInfo>, GitError> {
        self.read(repo, "remotes", |r| r.remotes.clone())
    }
}

impl HistoryReader for FakeGit {
    fn walk_topology(
        &self,
        repo: &RepoPath,
        _order: WalkOrder,
        sink: &mut dyn FnMut(TopologyNode) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let nodes = self.read(repo, "walk_topology", |r| {
            r.history.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()
        })?;
        for node in nodes {
            cancel.check()?;
            if sink(node).is_break() {
                break;
            }
        }
        Ok(())
    }
    fn commit_meta(&self, repo: &RepoPath, oids: &[Oid]) -> Result<Vec<CommitMeta>, GitError> {
        self.read(repo, "commit_meta", |r| {
            oids.iter()
                .filter_map(|o| {
                    r.history
                        .iter()
                        .find(|(n, _)| n.oid == *o)
                        .map(|(_, m)| m.clone())
                })
                .collect()
        })
    }
    fn commit_message(&self, repo: &RepoPath, commit: Oid) -> Result<String, GitError> {
        self.read(repo, "commit_message", |r| {
            r.history
                .iter()
                .find(|(n, _)| n.oid == commit)
                .map(|(_, m)| m.summary.clone())
                .unwrap_or_default()
        })
    }
    fn is_ancestor(
        &self,
        repo: &RepoPath,
        ancestor: Oid,
        descendant: Oid,
    ) -> Result<bool, GitError> {
        self.read(repo, "is_ancestor", |r| {
            let parents: HashMap<Oid, &Vec<Oid>> =
                r.history.iter().map(|(n, _)| (n.oid, &n.parents)).collect();
            let mut stack = vec![descendant];
            let mut seen = std::collections::HashSet::new();
            while let Some(cur) = stack.pop() {
                if cur == ancestor {
                    return true;
                }
                if seen.insert(cur)
                    && let Some(ps) = parents.get(&cur)
                {
                    stack.extend(ps.iter().copied());
                }
            }
            false
        })
    }
}

impl ChurnReader for FakeGit {
    fn walk_churn(
        &self,
        repo: &RepoPath,
        since: Option<Oid>,
        sink: &mut dyn FnMut(CommitChurn) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let commits = self.read(repo, "walk_churn", |r| r.churn.clone())?;
        for c in commits {
            cancel.check()?;
            if Some(c.oid) == since || sink(c).is_break() {
                break;
            }
        }
        Ok(())
    }
}

impl DiffReader for FakeGit {
    fn commit_diff(&self, repo: &RepoPath, commit: Oid) -> Result<Vec<FileDiff>, GitError> {
        self.read(repo, "commit_diff", |r| {
            r.diffs.get(&commit).cloned().unwrap_or_default()
        })
    }
    fn worktree_diff(
        &self,
        repo: &RepoPath,
        _side: DiffSide,
        _path: Option<&str>,
    ) -> Result<Vec<FileDiff>, GitError> {
        self.read(repo, "worktree_diff", |_| Vec::new())
    }
}

impl StashReader for FakeGit {
    fn stashes(&self, repo: &RepoPath) -> Result<Vec<StashEntry>, GitError> {
        self.read(repo, "stashes", |r| r.stashes.clone())
    }
}

impl CommitWriter for FakeGit {
    fn commit(&self, repo: &RepoPath, request: &CommitRequest) -> Result<Oid, GitError> {
        self.record("commit", repo, request.message.clone())?;
        let new = self.fresh_oid();
        self.with_repo(repo, |r| {
            if let HeadRef::Symbolic(name) = &r.head {
                r.refs.insert(name.clone(), new);
            } else {
                r.head = HeadRef::Detached(new);
            }
            r.summary.changes.staged = 0;
        });
        Ok(new)
    }
}

impl TreeReader for FakeGit {
    fn list_files(&self, repo: &RepoPath, commit: Option<Oid>) -> Result<Vec<TreeEntry>, GitError> {
        if let Some(err) = lock(&self.failures).get(&(repo.clone(), "list_files")) {
            return Err(err.clone());
        }
        Ok(self.with_repo(repo, |r| {
            let files = match commit {
                Some(c) => r.trees.get(&c).cloned().unwrap_or_default(),
                None => r.files.clone(),
            };
            files.into_keys().map(TreeEntry::file).collect()
        }))
    }
    fn read_blob(&self, repo: &RepoPath, commit: Oid, path: &str) -> Result<Vec<u8>, GitError> {
        self.with_repo(repo, |r| r.trees.get(&commit)?.get(path).cloned())
            .ok_or_else(|| GitError::InvalidRequest(format!("{path} is not in {commit}")))
    }
}

impl WorktreeFiles for FakeGit {
    fn read_file(&self, repo: &RepoPath, path: &str) -> Result<Vec<u8>, GitError> {
        if let Some(err) = lock(&self.failures).get(&(repo.clone(), "read_file")) {
            return Err(err.clone());
        }
        self.with_repo(repo, |r| r.files.get(path).cloned())
            .ok_or_else(|| GitError::Io {
                path: repo.as_path().join(path),
                message: "no such file".into(),
            })
    }
    fn write_file(&self, repo: &RepoPath, path: &str, contents: &[u8]) -> Result<(), GitError> {
        self.record("write_file", repo, path)?;
        self.with_repo(repo, |r| r.files.insert(path.to_owned(), contents.to_vec()));
        Ok(())
    }
    fn create_file(&self, repo: &RepoPath, path: &str, contents: &[u8]) -> Result<(), GitError> {
        self.record("create_file", repo, path)?;
        let created = self.with_repo(repo, |r| {
            if r.files.contains_key(path) {
                return false;
            }
            r.files.insert(path.to_owned(), contents.to_vec());
            true
        });
        if created {
            Ok(())
        } else {
            Err(GitError::Io {
                path: repo.as_path().join(path),
                message: "file exists".into(),
            })
        }
    }
}

impl IndexWriter for FakeGit {
    fn stage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError> {
        self.record("stage", repo, paths.join(","))
    }
    fn unstage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError> {
        self.record("unstage", repo, paths.join(","))
    }
    fn apply_patch(
        &self,
        repo: &RepoPath,
        patch: &str,
        direction: PatchDirection,
    ) -> Result<(), GitError> {
        self.record("apply_patch", repo, format!("{direction:?}:{patch}"))
    }
    fn discard_patch(&self, repo: &RepoPath, patch: &str) -> Result<(), GitError> {
        self.record("discard_patch", repo, patch.to_owned())
    }
    fn discard(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError> {
        self.record("discard", repo, paths.join(","))
    }
}

impl BranchWriter for FakeGit {
    fn checkout(&self, repo: &RepoPath, target: &CheckoutTarget) -> Result<(), GitError> {
        self.record("checkout", repo, format!("{target:?}"))?;
        self.with_repo(repo, |r| match target {
            CheckoutTarget::Branch(b) | CheckoutTarget::TrackRemote(b) => {
                let short = b.rsplit('/').next().unwrap_or(b);
                r.head = HeadRef::Symbolic(format!("refs/heads/{short}"));
                r.summary.head = HeadState::Branch(short.to_owned());
            }
            CheckoutTarget::Detached(oid) => {
                r.head = HeadRef::Detached(*oid);
                r.summary.head = HeadState::Detached(*oid);
            }
        });
        Ok(())
    }
    fn create_branch(
        &self,
        repo: &RepoPath,
        name: &str,
        start: Option<&str>,
    ) -> Result<(), GitError> {
        self.record("create_branch", repo, name)?;
        let start = start.unwrap_or("HEAD");
        let target = self
            .resolve(repo, start)?
            .ok_or_else(|| GitError::InvalidRequest(start.into()))?;
        self.with_repo(repo, |r| {
            r.refs.insert(format!("refs/heads/{name}"), target)
        });
        Ok(())
    }
    fn rename_branch(&self, repo: &RepoPath, old: &str, new: &str) -> Result<(), GitError> {
        self.record("rename_branch", repo, format!("{old}->{new}"))?;
        self.with_repo(repo, |r| {
            if let Some(t) = r.refs.remove(&format!("refs/heads/{old}")) {
                r.refs.insert(format!("refs/heads/{new}"), t);
            }
        });
        Ok(())
    }
    fn delete_branch(
        &self,
        repo: &RepoPath,
        name: &str,
        _mode: DeleteMode,
    ) -> Result<(), GitError> {
        self.record("delete_branch", repo, name)?;
        self.with_repo(repo, |r| r.refs.remove(&format!("refs/heads/{name}")));
        Ok(())
    }
    fn set_upstream(
        &self,
        repo: &RepoPath,
        branch: &str,
        upstream: Option<&str>,
    ) -> Result<(), GitError> {
        self.record("set_upstream", repo, format!("{branch}->{upstream:?}"))
    }
}

impl TagWriter for FakeGit {
    fn create_tag(&self, repo: &RepoPath, request: &TagRequest) -> Result<(), GitError> {
        self.record("create_tag", repo, request.name.clone())?;
        let target = self.resolve(repo, request.target.as_deref().unwrap_or("HEAD"))?;
        if let Some(t) = target {
            self.with_repo(repo, |r| {
                r.refs.insert(format!("refs/tags/{}", request.name), t)
            });
        }
        Ok(())
    }
    fn delete_tag(&self, repo: &RepoPath, name: &str) -> Result<(), GitError> {
        self.record("delete_tag", repo, name)?;
        self.with_repo(repo, |r| r.refs.remove(&format!("refs/tags/{name}")));
        Ok(())
    }
}

impl RefWriter for FakeGit {
    fn update_ref(
        &self,
        repo: &RepoPath,
        name: &str,
        new: Option<Oid>,
        _expected_old: Option<Oid>,
    ) -> Result<(), GitError> {
        self.record("update_ref", repo, format!("{name}={new:?}"))?;
        self.with_repo(repo, |r| match new {
            Some(o) => {
                r.refs.insert(name.to_owned(), o);
            }
            None => {
                r.refs.remove(name);
            }
        });
        Ok(())
    }
    fn set_symbolic_head(&self, repo: &RepoPath, branch_ref: &str) -> Result<(), GitError> {
        self.record("set_symbolic_head", repo, branch_ref)?;
        self.with_repo(repo, |r| r.head = HeadRef::Symbolic(branch_ref.to_owned()));
        Ok(())
    }
    fn reset(&self, repo: &RepoPath, target: &str, mode: ResetMode) -> Result<(), GitError> {
        self.record("reset", repo, format!("{mode:?}:{target}"))?;
        let oid = self
            .resolve(repo, target)?
            .ok_or_else(|| GitError::InvalidRequest(target.into()))?;
        self.with_repo(repo, |r| match r.head.clone() {
            HeadRef::Symbolic(name) => {
                r.refs.insert(name, oid);
            }
            HeadRef::Detached(_) => r.head = HeadRef::Detached(oid),
        });
        Ok(())
    }
}

impl StashWriter for FakeGit {
    fn stash_push(&self, repo: &RepoPath, request: &StashRequest) -> Result<bool, GitError> {
        self.record(
            "stash_push",
            repo,
            request.message.clone().unwrap_or_default(),
        )?;
        let new = self.fresh_oid();
        self.with_repo(repo, |r| {
            for s in &mut r.stashes {
                s.index += 1;
            }
            r.stashes.insert(
                0,
                StashEntry {
                    index: 0,
                    oid: new,
                    base: r.summary.head_oid.unwrap_or_else(|| oid(0)),
                    time: Timestamp::now(),
                    message: request.message.clone().unwrap_or_default(),
                },
            );
        });
        Ok(true)
    }
    fn stash_apply(
        &self,
        repo: &RepoPath,
        index: u32,
        mode: StashApplyMode,
    ) -> Result<(), GitError> {
        self.record("stash_apply", repo, format!("{index}:{mode:?}"))?;
        if mode == StashApplyMode::Pop {
            self.with_repo(repo, |r| remove_stash(r, index));
        }
        Ok(())
    }
    fn stash_apply_index(&self, repo: &RepoPath, index: u32) -> Result<(), GitError> {
        self.record("stash_apply_index", repo, index.to_string())
    }
    fn drop_stash(&self, repo: &RepoPath, index: u32) -> Result<(), GitError> {
        self.record("stash_drop", repo, index.to_string())?;
        self.with_repo(repo, |r| remove_stash(r, index));
        Ok(())
    }
    fn stash_store(&self, repo: &RepoPath, oid: Oid, message: &str) -> Result<(), GitError> {
        self.record("stash_store", repo, message)?;
        self.with_repo(repo, |r| {
            for s in &mut r.stashes {
                s.index += 1;
            }
            r.stashes.insert(
                0,
                StashEntry {
                    index: 0,
                    oid,
                    base: r.summary.head_oid.unwrap_or_else(|| self::oid(0)),
                    time: Timestamp::now(),
                    message: message.to_owned(),
                },
            );
        });
        Ok(())
    }
}

impl WorktreeSnapshots for FakeGit {
    fn snapshot_worktree(&self, repo: &RepoPath) -> Result<Option<Oid>, GitError> {
        self.record("stash_snapshot", repo, "")?;
        Ok(Some(self.fresh_oid()))
    }
    fn snapshot_paths(&self, repo: &RepoPath, paths: &[String]) -> Result<Option<Oid>, GitError> {
        self.record("snapshot_paths", repo, paths.join(","))?;
        Ok((!paths.is_empty()).then(|| self.fresh_oid()))
    }
    fn restore_paths(&self, repo: &RepoPath, snapshot: Oid) -> Result<(), GitError> {
        self.record("restore_paths", repo, snapshot.to_hex())
    }
}

fn remove_stash(r: &mut FakeRepo, index: u32) {
    r.stashes.retain(|s| s.index != index);
    for s in &mut r.stashes {
        if s.index > index {
            s.index -= 1;
        }
    }
}

impl RemoteOps for FakeGit {
    fn fetch(
        &self,
        repo: &RepoPath,
        request: &FetchRequest,
        _p: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        cancel.check()?;
        let mut detail = request.remote.clone().unwrap_or_default();
        if request.prune {
            detail.push_str(" --prune");
        }
        self.record("fetch", repo, detail)?;
        self.with_repo(repo, |r| r.summary.last_fetch = Some(Timestamp::now()));
        Ok(())
    }
    fn pull(
        &self,
        repo: &RepoPath,
        mode: PullMode,
        _p: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        cancel.check()?;
        self.record("pull", repo, format!("{mode:?}"))
    }
    fn push(
        &self,
        repo: &RepoPath,
        request: &PushRequest,
        _p: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        cancel.check()?;
        self.record("push", repo, request.refspec.clone().unwrap_or_default())
    }
    fn clone_repo(
        &self,
        url: &str,
        dest: &Path,
        _p: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RepoPath, GitError> {
        cancel.check()?;
        let path = RepoPath::new(dest);
        self.record("clone", &path, url)?;
        self.insert(&path, FakeRepo::default());
        Ok(path)
    }
}

impl HistoryRewriter for FakeGit {
    fn merge(&self, repo: &RepoPath, rev: &str, mode: MergeMode) -> Result<(), GitError> {
        self.record("merge", repo, format!("{rev}:{mode:?}"))
    }
    fn rebase(&self, repo: &RepoPath, onto: &str) -> Result<(), GitError> {
        self.record("rebase", repo, onto)
    }
    fn cherry_pick(&self, repo: &RepoPath, commits: &[Oid]) -> Result<(), GitError> {
        self.record(
            "cherry_pick",
            repo,
            commits
                .iter()
                .map(Oid::to_hex)
                .collect::<Vec<_>>()
                .join(","),
        )
    }
    fn revert(&self, repo: &RepoPath, commit: Oid) -> Result<(), GitError> {
        self.record("revert", repo, commit.to_hex())
    }
    fn sequence(&self, repo: &RepoPath, action: SequenceAction) -> Result<(), GitError> {
        self.record("sequence", repo, format!("{action:?}"))
    }
}

impl SubmoduleReader for FakeGit {
    fn submodules(&self, repo: &RepoPath) -> Result<Vec<SubmoduleInfo>, GitError> {
        self.read(repo, "submodules", |r| r.submodules.clone())
    }
}

impl SubmoduleWriter for FakeGit {
    fn update_submodules(
        &self,
        repo: &RepoPath,
        request: &SubmoduleUpdate,
        _p: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        cancel.check()?;
        self.record(
            "submodule_update",
            repo,
            format!(
                "init={} recursive={} paths={}",
                request.init,
                request.recursive,
                request.paths.join(",")
            ),
        )?;
        self.with_repo(repo, |r| {
            for s in &mut r.submodules {
                let wanted = request.paths.is_empty() || request.paths.contains(&s.name);
                if wanted && (s.initialized || request.init) {
                    s.initialized = true;
                    s.checked_out = s.pinned;
                }
            }
        });
        Ok(())
    }
}

impl MergeToolLauncher for FakeGit {
    fn launch(
        &self,
        repo: &RepoPath,
        path: Option<&str>,
        _tool: Option<&str>,
    ) -> Result<(), GitError> {
        self.record("mergetool", repo, path.unwrap_or_default())
    }
}

type RepoStats = (Option<Oid>, Vec<ActivityBucket>, Vec<Hotspot>);

/// Fake of every store trait, kept in memory.
#[derive(Default)]
pub struct InMemoryStore {
    summaries: Mutex<HashMap<ProjectId, Vec<(RepoId, RepoSummary)>>>,
    ui: Mutex<HashMap<ProjectId, UiState>>,
    registry: Mutex<BTreeMap<ProjectId, RegistryEntry>>,
    journal: Mutex<Vec<JournalEntry>>,
    stats: Mutex<HashMap<RepoId, RepoStats>>,
}

impl InMemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SummaryStore for InMemoryStore {
    fn load_summaries(
        &self,
        project: &ProjectId,
    ) -> Result<Vec<(RepoId, RepoSummary)>, StoreError> {
        Ok(lock(&self.summaries)
            .get(project)
            .cloned()
            .unwrap_or_default())
    }
    fn save_summaries(
        &self,
        project: &ProjectId,
        summaries: &[(RepoId, RepoSummary)],
    ) -> Result<(), StoreError> {
        lock(&self.summaries).insert(project.clone(), summaries.to_vec());
        Ok(())
    }
}

impl UiStateStore for InMemoryStore {
    fn load_ui_state(&self, project: &ProjectId) -> Result<Option<UiState>, StoreError> {
        Ok(lock(&self.ui).get(project).cloned())
    }
    fn save_ui_state(&self, project: &ProjectId, state: &UiState) -> Result<(), StoreError> {
        lock(&self.ui).insert(project.clone(), state.clone());
        Ok(())
    }
}

impl ProjectRegistry for InMemoryStore {
    fn list_projects(&self) -> Result<Vec<RegistryEntry>, StoreError> {
        Ok(lock(&self.registry).values().cloned().collect())
    }
    fn upsert_project(&self, entry: &RegistryEntry) -> Result<(), StoreError> {
        lock(&self.registry).insert(entry.id.clone(), entry.clone());
        Ok(())
    }
    fn remove_project(&self, id: &ProjectId) -> Result<(), StoreError> {
        lock(&self.registry).remove(id);
        Ok(())
    }
    fn mark_opened(&self, id: &ProjectId, when: Timestamp) -> Result<(), StoreError> {
        let mut reg = lock(&self.registry);
        let entry = reg
            .get_mut(id)
            .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        entry.last_opened = Some(when);
        Ok(())
    }
}

impl JournalStore for InMemoryStore {
    fn append(&self, entry: &JournalEntry, limit: usize) -> Result<JournalId, StoreError> {
        let mut journal = lock(&self.journal);
        let id = JournalId(journal.last().map_or(1, |e| e.id.0 + 1));
        let mut entry = entry.clone();
        entry.id = id;
        let repo = entry.repo;
        journal.push(entry);
        let count = journal.iter().filter(|e| e.repo == repo).count();
        if count > limit {
            let mut excess = count - limit;
            journal.retain(|e| {
                if e.repo == repo && excess > 0 {
                    excess -= 1;
                    false
                } else {
                    true
                }
            });
        }
        Ok(id)
    }
    fn recent(&self, repo: RepoId, limit: usize) -> Result<Vec<JournalEntry>, StoreError> {
        Ok(lock(&self.journal)
            .iter()
            .rev()
            .filter(|e| e.repo == repo)
            .take(limit)
            .cloned()
            .collect())
    }
    fn batch(&self, batch: BatchId) -> Result<Vec<JournalEntry>, StoreError> {
        Ok(lock(&self.journal)
            .iter()
            .filter(|e| e.batch == Some(batch))
            .cloned()
            .collect())
    }
    fn set_state(&self, id: JournalId, state: EntryState) -> Result<(), StoreError> {
        let mut journal = lock(&self.journal);
        let entry = journal
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("journal entry {}", id.0)))?;
        entry.state = state;
        Ok(())
    }
}

impl StatsStore for InMemoryStore {
    fn cursor(&self, repo: RepoId) -> Result<Option<Oid>, StoreError> {
        Ok(lock(&self.stats).get(&repo).and_then(|s| s.0))
    }
    fn apply_delta(&self, repo: RepoId, delta: &StatsDelta, cursor: Oid) -> Result<(), StoreError> {
        let mut stats = lock(&self.stats);
        let entry = stats.entry(repo).or_default();
        entry.0 = Some(cursor);
        for b in &delta.activity {
            match entry
                .1
                .iter_mut()
                .find(|x| x.week == b.week && x.author == b.author)
            {
                Some(x) => x.commits += b.commits,
                None => entry.1.push(b.clone()),
            }
        }
        for h in &delta.churn {
            match entry.2.iter_mut().find(|x| x.path == h.path) {
                Some(x) => {
                    x.added += h.added;
                    x.removed += h.removed;
                    x.commits += h.commits;
                }
                None => entry.2.push(h.clone()),
            }
        }
        Ok(())
    }
    fn activity(&self, repo: RepoId) -> Result<Vec<ActivityBucket>, StoreError> {
        Ok(lock(&self.stats)
            .get(&repo)
            .map(|s| s.1.clone())
            .unwrap_or_default())
    }
    fn hotspots(&self, repo: RepoId, limit: usize) -> Result<Vec<Hotspot>, StoreError> {
        let mut all = lock(&self.stats)
            .get(&repo)
            .map(|s| s.2.clone())
            .unwrap_or_default();
        all.sort_by(|a, b| b.churn().cmp(&a.churn()).then_with(|| a.path.cmp(&b.path)));
        all.truncate(limit);
        Ok(all)
    }
    fn reset_stats(&self, repo: RepoId) -> Result<(), StoreError> {
        lock(&self.stats).remove(&repo);
        Ok(())
    }
}

/// Fake project source keeping definitions in memory.
#[derive(Default)]
pub struct FakeProjectSource {
    projects: Mutex<HashMap<ProjectLocation, ProjectDef>>,
}

impl ProjectSource for FakeProjectSource {
    fn load(&self, location: &ProjectLocation) -> Result<ProjectDef, ProjectError> {
        lock(&self.projects).get(location).cloned().ok_or_else(|| {
            let ProjectLocation::SharedFile(path) = location;
            ProjectError::Io {
                path: path.clone(),
                message: "not found".into(),
            }
        })
    }
    fn save(&self, location: &ProjectLocation, project: &ProjectDef) -> Result<(), ProjectError> {
        lock(&self.projects).insert(location.clone(), project.clone());
        Ok(())
    }
}

/// Fake discovery returning a fixed list.
#[derive(Default)]
pub struct FakeDiscovery {
    /// Repositories returned by every scan.
    pub repos: Vec<DiscoveredRepo>,
}

impl RepoDiscovery for FakeDiscovery {
    fn scan(
        &self,
        _root: &Path,
        _options: &ScanOptions,
        cancel: &CancelToken,
    ) -> Result<Vec<DiscoveredRepo>, ProjectError> {
        if cancel.is_cancelled() {
            return Ok(Vec::new());
        }
        Ok(self.repos.clone())
    }
}

/// Fake change source: tests trigger invalidations by hand.
#[derive(Default)]
pub struct FakeChangeSource {
    sinks: Mutex<Vec<(RepoId, Arc<dyn InvalidationSink>)>>,
    /// Coverage reported by new watches.
    pub coverage: Option<Coverage>,
}

impl FakeChangeSource {
    /// Sends an invalidation to every sink watching `repo`.
    pub fn trigger(&self, repo: RepoId, scope: crate::ports::Scope) {
        for (r, sink) in lock(&self.sinks).iter() {
            if *r == repo {
                sink.invalidate(crate::ports::Invalidation { repo, scope });
            }
        }
    }

    /// Number of active watches.
    #[must_use]
    pub fn watch_count(&self) -> usize {
        lock(&self.sinks).len()
    }
}

impl ChangeSource for FakeChangeSource {
    fn watch(
        &self,
        repo: RepoId,
        _path: &RepoPath,
        sink: Arc<dyn InvalidationSink>,
    ) -> Result<WatchHandle, WatchError> {
        lock(&self.sinks).push((repo, sink));
        Ok(WatchHandle {
            coverage: self.coverage.unwrap_or(Coverage::Full),
            guard: Box::new(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_commit_moves_branch_and_records_call() {
        let git = FakeGit::new();
        let repo = RepoPath::new("/r");
        git.with_repo(&repo, |r| {
            r.refs.insert("refs/heads/main".into(), oid(1));
        });
        let new = git
            .commit(
                &repo,
                &CommitRequest {
                    message: "m".into(),
                    mode: crate::requests::CommitMode::default(),
                },
            )
            .unwrap();
        assert_eq!(git.resolve(&repo, "HEAD").unwrap(), Some(new));
        assert_eq!(git.calls()[0].op, "commit");
    }

    #[test]
    fn fake_failure_is_returned() {
        let git = FakeGit::new();
        let repo = RepoPath::new("/r");
        git.insert(&repo, FakeRepo::default());
        git.fail(&repo, "commit", GitError::Unsupported("x"));
        assert!(
            git.commit(
                &repo,
                &CommitRequest {
                    message: String::new(),
                    mode: crate::requests::CommitMode::default()
                }
            )
            .is_err()
        );
        assert_eq!(git.calls().len(), 0);
    }

    #[test]
    fn journal_is_pruned_per_repo() {
        let store = InMemoryStore::new();
        let entry = JournalEntry {
            id: JournalId(0),
            repo: RepoId(1),
            batch: None,
            kind: crate::journal::OperationKind::Commit,
            label: String::new(),
            before: RefSnapshot {
                head: HeadRef::Detached(oid(1)),
                refs: vec![],
            },
            after: RefSnapshot {
                head: HeadRef::Detached(oid(2)),
                refs: vec![],
            },
            worktree_snapshot: None,
            stash: None,
            time: Timestamp(0),
            state: EntryState::Done,
        };
        for _ in 0..5 {
            store.append(&entry, 3).unwrap();
        }
        assert_eq!(store.recent(RepoId(1), 10).unwrap().len(), 3);
    }
}
