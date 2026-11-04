//! Batch operations over several repositories (spec §3 batch actions).
//!
//! Every batch returns a [`BatchReport`] with one [`Outcome`] per repo: there
//! is never a silent all-or-nothing, and failed repos can be retried.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use archipel_model::journal::BatchId;
use archipel_model::requests::{CheckoutTarget, FetchRequest, PullMode, TagRequest};
use archipel_model::{RepoId, RepoPath, Timestamp};

use crate::actions::RepoActions;
use crate::error::OpError;
use crate::scheduler::{OpClass, OpHandle, OpSpec, Scheduler};

/// Result of a batch operation on one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<T> {
    /// Succeeded.
    Done(T),
    /// Failed; the error carries the exact command and its output.
    Failed(OpError),
    /// Not attempted, with the reason (e.g. "branch does not exist").
    Skipped(String),
}

impl<T> Outcome<T> {
    /// Whether the operation succeeded.
    #[must_use]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done(_))
    }
}

/// Per-repository report of a batch operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchReport<T> {
    /// Outcomes in submission order.
    pub per_repo: Vec<(RepoId, Outcome<T>)>,
}

impl<T> Default for BatchReport<T> {
    fn default() -> Self {
        Self {
            per_repo: Vec::new(),
        }
    }
}

impl<T> BatchReport<T> {
    /// Appends an outcome.
    pub fn push(&mut self, repo: RepoId, outcome: Outcome<T>) {
        self.per_repo.push((repo, outcome));
    }

    /// Repos whose operation failed (to offer "retry failed").
    #[must_use]
    pub fn failed(&self) -> Vec<RepoId> {
        self.per_repo
            .iter()
            .filter(|(_, o)| matches!(o, Outcome::Failed(_)))
            .map(|(r, _)| *r)
            .collect()
    }

    /// Counts of (done, failed, skipped).
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        self.per_repo
            .iter()
            .fold((0, 0, 0), |(d, f, s), (_, o)| match o {
                Outcome::Done(_) => (d + 1, f, s),
                Outcome::Failed(_) => (d, f + 1, s),
                Outcome::Skipped(_) => (d, f, s + 1),
            })
    }

    /// The outcome for one repo.
    #[must_use]
    pub fn outcome(&self, repo: RepoId) -> Option<&Outcome<T>> {
        self.per_repo
            .iter()
            .find(|(r, _)| *r == repo)
            .map(|(_, o)| o)
    }
}

/// A repository taking part in a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTarget {
    /// Repository id.
    pub id: RepoId,
    /// Repository path.
    pub path: RepoPath,
}

impl BatchTarget {
    /// Builds a target from a path.
    #[must_use]
    pub fn new(path: RepoPath) -> Self {
        Self {
            id: path.id(),
            path,
        }
    }
}

/// Runs batch operations through the scheduler, in parallel across repos.
pub struct BatchOps {
    scheduler: Arc<Scheduler>,
    actions: RepoActions,
    counter: AtomicU64,
}

impl BatchOps {
    /// Creates the batch runner.
    #[must_use]
    pub fn new(scheduler: Arc<Scheduler>, actions: RepoActions) -> Self {
        Self {
            scheduler,
            actions,
            counter: AtomicU64::new(0),
        }
    }

    /// A batch id unique across sessions (time-based, plus a counter).
    pub fn new_batch_id(&self) -> BatchId {
        let seconds = u64::try_from(Timestamp::now().0).unwrap_or(0);
        BatchId((seconds << 20) | (self.counter.fetch_add(1, Ordering::Relaxed) & 0xF_FFFF))
    }

    async fn run<T, F>(
        &self,
        targets: &[BatchTarget],
        class: OpClass,
        label: &str,
        job: F,
    ) -> BatchReport<T>
    where
        T: Send + 'static,
        F: Fn(&RepoActions, &BatchTarget, &crate::OpContext) -> Result<Outcome<T>, OpError>
            + Send
            + Sync
            + Clone
            + 'static,
    {
        let handles: Vec<(RepoId, OpHandle<Outcome<T>>)> = targets
            .iter()
            .map(|t| {
                let (actions, target, job) = (self.actions.clone(), t.clone(), job.clone());
                let spec = OpSpec::new(Some(t.id), class, format!("{label} — {}", t.path.name()));
                (
                    t.id,
                    self.scheduler
                        .submit(spec, move |ctx| job(&actions, &target, ctx)),
                )
            })
            .collect();
        let mut report = BatchReport::default();
        for (repo, handle) in handles {
            let outcome = handle.wait().await.unwrap_or_else(Outcome::Failed);
            report.push(repo, outcome);
        }
        report
    }

    /// Fetches every repo.
    pub async fn fetch_all(
        &self,
        targets: &[BatchTarget],
        request: FetchRequest,
    ) -> BatchReport<()> {
        self.run(targets, OpClass::Network, "Fetch", move |a, t, ctx| {
            a.fetch(&t.path, &request, ctx, ctx.cancel())
                .map(Outcome::Done)
        })
        .await
    }

    /// Pulls every repo (fast-forward only by default).
    pub async fn pull_all(&self, targets: &[BatchTarget], mode: PullMode) -> BatchReport<()> {
        let batch = Some(self.new_batch_id());
        self.run(targets, OpClass::Network, "Pull", move |a, t, ctx| {
            let summary = a.ports().status.summary(&t.path)?;
            if summary.upstream.is_none() {
                return Ok(Outcome::Skipped(
                    "the current branch has no upstream".into(),
                ));
            }
            a.pull(&t.path, mode, ctx, ctx.cancel(), batch)
                .map(Outcome::Done)
        })
        .await
    }

    /// Checks out the same branch everywhere; repos without it are skipped.
    pub async fn checkout_all(&self, targets: &[BatchTarget], branch: &str) -> BatchReport<()> {
        let batch = Some(self.new_batch_id());
        let branch = branch.to_owned();
        self.run(
            targets,
            OpClass::Write,
            &format!("Checkout {branch}"),
            move |a, t, _| {
                let branches = a.ports().refs.branches(&t.path)?;
                if branches
                    .iter()
                    .any(|b| !b.is_remote && b.name == branch && b.is_head)
                {
                    return Ok(Outcome::Skipped(format!("already on {branch}")));
                }
                let target = if branches.iter().any(|b| !b.is_remote && b.name == branch) {
                    CheckoutTarget::Branch(branch.clone())
                } else if let Some(remote) = branches.iter().find(|b| {
                    b.is_remote && b.name.split_once('/').map(|(_, n)| n) == Some(&branch)
                }) {
                    CheckoutTarget::TrackRemote(remote.name.clone())
                } else {
                    return Ok(Outcome::Skipped(format!("branch {branch} does not exist")));
                };
                a.checkout(&t.path, &target, batch).map(Outcome::Done)
            },
        )
        .await
    }

    /// Creates a branch on every repo (at `start`, or `HEAD`) and checks it
    /// out.
    pub async fn create_branch_all(
        &self,
        targets: &[BatchTarget],
        name: &str,
        start: Option<&str>,
    ) -> BatchReport<()> {
        let batch = Some(self.new_batch_id());
        let (name, start) = (name.to_owned(), start.map(str::to_owned));
        self.run(
            targets,
            OpClass::Write,
            &format!("Create branch {name}"),
            move |a, t, _| {
                a.create_branch(&t.path, &name, start.as_deref(), batch)?;
                a.checkout(&t.path, &CheckoutTarget::Branch(name.clone()), batch)
                    .map(Outcome::Done)
            },
        )
        .await
    }

    /// Creates a tag on every repo.
    pub async fn create_tag_all(
        &self,
        targets: &[BatchTarget],
        request: TagRequest,
    ) -> BatchReport<()> {
        let batch = Some(self.new_batch_id());
        let label = format!("Create tag {}", request.name);
        self.run(targets, OpClass::Write, &label, move |a, t, _| {
            a.create_tag(&t.path, &request, batch).map(Outcome::Done)
        })
        .await
    }
}
