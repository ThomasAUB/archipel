//! Multi-repo commit (spec §4).
//!
//! 1. [`MultiCommit::prepare`] reads each repo's branch and staged files, and
//!    pre-selects repos that have staged changes; it warns when branches differ.
//! 2. [`MultiCommit::run`] commits **sequentially** (`git commit -F -`, so hooks
//!    and signing run), records a per-repo outcome (a failing hook does not
//!    stop the others), then optionally pushes the committed repos in parallel.
//!    Failed repos can be retried by running again with only them selected.

use std::sync::Arc;

use archipel_model::changes::FileChange;
use archipel_model::requests::{CommitMode, CommitRequest, PushRequest};
use archipel_model::{Oid, RepoId, RepoPath};

use crate::actions::RepoActions;
use crate::batch::{BatchOps, BatchReport, BatchTarget, Outcome};
use crate::error::OpError;
use crate::scheduler::{OpClass, OpSpec, Scheduler};

/// One repository in the commit dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitTarget {
    /// Repository id.
    pub id: RepoId,
    /// Repository path.
    pub path: RepoPath,
    /// Current branch (`None` when detached).
    pub branch: Option<String>,
    /// Staged files (preview).
    pub staged: Vec<FileChange>,
    /// Number of files with unstaged or untracked changes ("stage all" offer).
    pub unstaged: usize,
    /// Whether the repo is selected for the commit.
    pub selected: bool,
}

/// The prepared commit dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitPlan {
    /// Every repo considered.
    pub targets: Vec<CommitTarget>,
}

impl CommitPlan {
    /// Distinct branches of the selected repos.
    #[must_use]
    pub fn selected_branches(&self) -> Vec<Option<&str>> {
        let mut branches: Vec<Option<&str>> = self
            .targets
            .iter()
            .filter(|t| t.selected)
            .map(|t| t.branch.as_deref())
            .collect();
        branches.sort_unstable();
        branches.dedup();
        branches
    }

    /// Whether the selected repos are on different branches (show a warning).
    #[must_use]
    pub fn branch_mismatch(&self) -> bool {
        self.selected_branches().len() > 1
    }

    /// Selects or deselects a repo.
    pub fn select(&mut self, repo: RepoId, selected: bool) {
        if let Some(t) = self.targets.iter_mut().find(|t| t.id == repo) {
            t.selected = selected;
        }
    }
}

/// Options of a multi-repo commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitOptions {
    /// Push the committed repos afterwards.
    pub push_after: bool,
    /// Repos where every change is staged before committing.
    pub stage_all: Vec<RepoId>,
    /// Refuse to start when the selected repos are on different branches.
    pub require_same_branch: bool,
    /// Amend instead of creating new commits.
    pub mode: CommitMode,
}

/// Per-repo result of a multi-repo commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committed {
    /// The new commit.
    pub oid: Oid,
    /// Push result when `push_after` was requested.
    pub push: Option<Result<(), OpError>>,
}

/// The multi-repo commit use case.
pub struct MultiCommit {
    scheduler: Arc<Scheduler>,
    actions: RepoActions,
    batches: Arc<BatchOps>,
}

impl MultiCommit {
    /// Creates the use case.
    #[must_use]
    pub fn new(scheduler: Arc<Scheduler>, actions: RepoActions, batches: Arc<BatchOps>) -> Self {
        Self {
            scheduler,
            actions,
            batches,
        }
    }

    /// Reads branch and changes of every repo (in parallel) and pre-selects
    /// repos with staged changes. Unreadable repos are left out.
    pub async fn prepare(&self, repos: &[RepoPath]) -> CommitPlan {
        let handles: Vec<_> = repos
            .iter()
            .map(|path| {
                let (actions, path) = (self.actions.clone(), path.clone());
                let spec = OpSpec::new(Some(path.id()), OpClass::Read, "Prepare commit");
                self.scheduler.submit(spec, move |_| {
                    let summary = actions.ports().status.summary(&path)?;
                    let changes = actions.ports().status.changes(&path)?;
                    let (staged, other): (Vec<_>, Vec<_>) =
                        changes.into_iter().partition(FileChange::is_staged);
                    let unstaged = other.len() + staged.iter().filter(|c| c.is_unstaged()).count();
                    Ok(CommitTarget {
                        id: path.id(),
                        branch: summary.head.branch().map(str::to_owned),
                        selected: !staged.is_empty(),
                        path,
                        staged,
                        unstaged,
                    })
                })
            })
            .collect();
        let mut targets = Vec::new();
        for h in handles {
            match h.wait().await {
                Ok(t) => targets.push(t),
                Err(e) => tracing::warn!(error = %e, "repo left out of the commit dialog"),
            }
        }
        CommitPlan { targets }
    }

    /// Commits every selected repo sequentially with the same message.
    pub async fn run(
        &self,
        plan: &CommitPlan,
        message: &str,
        options: &CommitOptions,
    ) -> Result<BatchReport<Committed>, OpError> {
        if message.trim().is_empty() {
            return Err(OpError::Refused("the commit message is empty".into()));
        }
        if options.require_same_branch && plan.branch_mismatch() {
            return Err(OpError::Refused(
                "this project requires every repo to be on the same branch".into(),
            ));
        }
        let batch = Some(self.batches.new_batch_id());
        let mut report = BatchReport::default();
        let mut committed = Vec::new();
        for target in plan.targets.iter().filter(|t| t.selected) {
            let (actions, path) = (self.actions.clone(), target.path.clone());
            let stage_all = options.stage_all.contains(&target.id);
            let request = CommitRequest {
                message: message.to_owned(),
                mode: options.mode,
            };
            let spec = OpSpec::new(
                Some(target.id),
                OpClass::Write,
                format!("Commit — {}", target.path.name()),
            );
            let result = self
                .scheduler
                .submit(spec, move |_| {
                    if stage_all {
                        actions.stage(&path, &[".".to_owned()])?;
                    }
                    actions.commit(&path, &request, batch)
                })
                .wait()
                .await;
            match result {
                Ok(oid) => {
                    committed.push((
                        target.id,
                        oid,
                        BatchTarget {
                            id: target.id,
                            path: target.path.clone(),
                        },
                    ));
                }
                Err(e) => report.push(target.id, Outcome::Failed(e)),
            }
        }
        let pushes = if options.push_after {
            self.push_all(&committed).await
        } else {
            BatchReport::default()
        };
        for (id, oid, _) in committed {
            let push = pushes.outcome(id).map(|o| match o {
                Outcome::Done(()) => Ok(()),
                Outcome::Failed(e) => Err(e.clone()),
                Outcome::Skipped(reason) => Err(OpError::Refused(reason.clone())),
            });
            report.push(id, Outcome::Done(Committed { oid, push }));
        }
        Ok(report)
    }

    async fn push_all(&self, committed: &[(RepoId, Oid, BatchTarget)]) -> BatchReport<()> {
        let handles: Vec<_> = committed
            .iter()
            .map(|(id, _, target)| {
                let (actions, path) = (self.actions.clone(), target.path.clone());
                let spec = OpSpec::new(
                    Some(*id),
                    OpClass::Network,
                    format!("Push — {}", path.name()),
                );
                (
                    *id,
                    self.scheduler.submit(spec, move |ctx| {
                        actions.push(&path, &PushRequest::default(), ctx, ctx.cancel(), None)
                    }),
                )
            })
            .collect();
        let mut report = BatchReport::default();
        for (id, h) in handles {
            report.push(
                id,
                h.wait().await.map_or_else(Outcome::Failed, Outcome::Done),
            );
        }
        report
    }
}
