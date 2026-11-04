//! The incremental statistics engine.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::mem::size_of;
use std::ops::ControlFlow;
use std::sync::Arc;

use archipel_model::ports::{ChurnReader, CommitChurn, HistoryReader, RefReader, StatsStore};
use archipel_model::stats::{ActivityBucket, Hotspot, StatsDelta};
use archipel_model::{CancelToken, GitError, Oid, RepoId, RepoPath, StoreError};

use crate::error::StatsError;
use crate::pause::PauseToken;
use crate::provider::RepoTarget;

/// How many times an update restarts when `HEAD` moves under it.
const MAX_ATTEMPTS: u32 = 3;

/// Size of the in-memory delta built by one update.
///
/// The delta is bounded by the number of distinct keys (author × week, path),
/// never by the number of commits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeltaSize {
    /// Distinct (week, author) activity keys.
    pub activity_keys: usize,
    /// Distinct churned paths.
    pub churn_keys: usize,
    /// Approximate heap size of the aggregation maps, in bytes.
    pub approx_bytes: usize,
}

/// Result of a successful [`StatsEngine::update`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateOutcome {
    /// `HEAD` the statistics now reflect (`None` for an empty repository).
    pub head: Option<Oid>,
    /// Number of commits processed by this update.
    pub commits: u64,
    /// Whether the stored statistics were reset because history was rewritten.
    pub reset: bool,
    /// Size of the delta at its peak (just before it was applied).
    pub delta: DeltaSize,
}

#[derive(Clone, Copy, Default)]
struct ChurnTotals {
    added: u64,
    removed: u64,
    commits: u32,
}

/// Approximate fixed cost of one hash map slot, on top of the key/value size.
const MAP_SLOT_OVERHEAD: usize = size_of::<u64>();
const ACTIVITY_SLOT: usize = size_of::<((i64, String), u32)>() + MAP_SLOT_OVERHEAD;
const CHURN_SLOT: usize = size_of::<(String, ChurnTotals)>() + MAP_SLOT_OVERHEAD;

/// Aggregates streamed commits into per-key counters.
#[derive(Default)]
struct DeltaAccumulator {
    activity: HashMap<(i64, String), u32>,
    churn: HashMap<String, ChurnTotals>,
    approx_bytes: usize,
}

impl DeltaAccumulator {
    fn add(&mut self, commit: CommitChurn) {
        let key = (
            commit.author_time.week_index(),
            commit.author_email.to_lowercase(),
        );
        match self.activity.entry(key) {
            Entry::Occupied(mut e) => *e.get_mut() = e.get().saturating_add(1),
            Entry::Vacant(e) => {
                self.approx_bytes += ACTIVITY_SLOT + e.key().1.len();
                e.insert(1);
            }
        }
        for file in commit.files {
            let totals = match self.churn.entry(file.path) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    self.approx_bytes += CHURN_SLOT + e.key().len();
                    e.insert(ChurnTotals::default())
                }
            };
            totals.added += u64::from(file.added.unwrap_or(0));
            totals.removed += u64::from(file.removed.unwrap_or(0));
            totals.commits = totals.commits.saturating_add(1);
        }
    }

    fn size(&self) -> DeltaSize {
        DeltaSize {
            activity_keys: self.activity.len(),
            churn_keys: self.churn.len(),
            approx_bytes: self.approx_bytes,
        }
    }

    fn into_delta(self) -> StatsDelta {
        let mut activity: Vec<ActivityBucket> = self
            .activity
            .into_iter()
            .map(|((week, author), commits)| ActivityBucket {
                week,
                author,
                commits,
            })
            .collect();
        activity.sort_by(|a, b| a.week.cmp(&b.week).then_with(|| a.author.cmp(&b.author)));
        let mut churn: Vec<Hotspot> = self
            .churn
            .into_iter()
            .map(|(path, t)| Hotspot {
                path,
                added: t.added,
                removed: t.removed,
                commits: t.commits,
            })
            .collect();
        churn.sort_by(|a, b| a.path.cmp(&b.path));
        StatsDelta { activity, churn }
    }
}

/// Why one attempt did not produce a delta.
enum AttemptError {
    HeadMoved,
    Failed(StatsError),
}

impl From<StatsError> for AttemptError {
    fn from(error: StatsError) -> Self {
        Self::Failed(error)
    }
}

impl From<GitError> for AttemptError {
    fn from(error: GitError) -> Self {
        Self::Failed(error.into())
    }
}

impl From<StoreError> for AttemptError {
    fn from(error: StoreError) -> Self {
        Self::Failed(error.into())
    }
}

/// Keeps the per-repo statistics of a [`StatsStore`] up to date.
///
/// Each update streams only the commits added since the stored cursor,
/// aggregates them in a delta bounded by the number of distinct keys, and
/// applies the delta together with the new cursor in one store call. A
/// cancelled or failed update therefore never leaves partial counts: the next
/// update restarts from the old cursor.
///
/// All methods block. The caller runs them on a low-priority background
/// thread (the scheduler's stats worker), never on the UI thread, and may
/// pause it through a [`PauseToken`] while the user is busy.
pub struct StatsEngine {
    churn: Arc<dyn ChurnReader>,
    store: Arc<dyn StatsStore>,
    refs: Arc<dyn RefReader>,
    history: Arc<dyn HistoryReader>,
}

impl StatsEngine {
    /// Creates an engine from its ports.
    #[must_use]
    pub fn new(
        churn: Arc<dyn ChurnReader>,
        store: Arc<dyn StatsStore>,
        refs: Arc<dyn RefReader>,
        history: Arc<dyn HistoryReader>,
    ) -> Self {
        Self {
            churn,
            store,
            refs,
            history,
        }
    }

    /// Brings the statistics of one repository up to date with its `HEAD`.
    ///
    /// * Unchanged `HEAD`: returns immediately.
    /// * Cursor no longer an ancestor of `HEAD` (history rewritten): the whole
    ///   history is recomputed and the stored statistics replaced.
    /// * Otherwise only commits reachable from `HEAD` and not from the cursor
    ///   are read.
    ///
    /// Waits on `pause` between commits; returns [`StatsError::Cancelled`]
    /// without touching the store when `cancel` fires.
    pub fn update(
        &self,
        repo: RepoId,
        path: &RepoPath,
        cancel: &CancelToken,
        pause: &PauseToken,
    ) -> Result<UpdateOutcome, StatsError> {
        for _ in 0..MAX_ATTEMPTS {
            match self.attempt(repo, path, cancel, pause) {
                Ok(outcome) => return Ok(outcome),
                Err(AttemptError::Failed(e)) => return Err(e),
                Err(AttemptError::HeadMoved) => {
                    tracing::debug!(%repo, "HEAD moved during stats update, retrying");
                }
            }
        }
        Err(StatsError::HeadMoved {
            attempts: MAX_ATTEMPTS,
        })
    }

    /// Updates every repository of a project in turn, stopping at cancellation.
    #[must_use]
    pub fn update_all(
        &self,
        repos: &[RepoTarget],
        cancel: &CancelToken,
        pause: &PauseToken,
    ) -> Vec<(RepoId, Result<UpdateOutcome, StatsError>)> {
        let mut results = Vec::with_capacity(repos.len());
        for target in repos {
            if cancel.is_cancelled() {
                results.push((target.id, Err(StatsError::Cancelled)));
                continue;
            }
            let result = self.update(target.id, &target.path, cancel, pause);
            results.push((target.id, result));
        }
        results
    }

    fn attempt(
        &self,
        repo: RepoId,
        path: &RepoPath,
        cancel: &CancelToken,
        pause: &PauseToken,
    ) -> Result<UpdateOutcome, AttemptError> {
        cancel.check()?;
        let Some(head) = self.refs.resolve(path, "HEAD")? else {
            return Ok(UpdateOutcome::default());
        };
        let cursor = self.store.cursor(repo)?;
        if cursor == Some(head) {
            return Ok(UpdateOutcome {
                head: Some(head),
                ..UpdateOutcome::default()
            });
        }
        let rewritten = match cursor {
            Some(c) => !self.still_ancestor(path, c, head)?,
            None => false,
        };
        let since = if rewritten { None } else { cursor };

        let mut acc = DeltaAccumulator::default();
        let mut commits = 0u64;
        let mut first = None;
        let mut stopped = false;
        self.churn.walk_churn(
            path,
            since,
            &mut |commit| {
                if pause.wait_while_paused(cancel) {
                    stopped = true;
                    return ControlFlow::Break(());
                }
                first.get_or_insert(commit.oid);
                commits += 1;
                acc.add(commit);
                ControlFlow::Continue(())
            },
            cancel,
        )?;
        if stopped || cancel.is_cancelled() {
            return Err(AttemptError::Failed(StatsError::Cancelled));
        }
        // `git log` emits the tip it resolved first: anything else means HEAD
        // moved between `resolve` and the walk, and the cursor would be wrong.
        if first != Some(head) {
            return Err(AttemptError::HeadMoved);
        }

        let size = acc.size();
        let delta = acc.into_delta();
        if rewritten {
            // Not atomic with `apply_delta`: a crash in between leaves no
            // cursor, so the next update simply recomputes everything.
            self.store.reset_stats(repo)?;
        }
        self.store.apply_delta(repo, &delta, head)?;
        tracing::debug!(%repo, commits, rewritten, ?size, "stats updated");
        Ok(UpdateOutcome {
            head: Some(head),
            commits,
            reset: rewritten,
            delta: size,
        })
    }

    /// Whether `cursor` still exists and is an ancestor of `head`.
    fn still_ancestor(&self, path: &RepoPath, cursor: Oid, head: Oid) -> Result<bool, StatsError> {
        match self.history.is_ancestor(path, cursor, head) {
            Ok(answer) => Ok(answer),
            // A garbage-collected cursor makes `is_ancestor` fail; only then pay
            // for an existence check (one extra `git` call off the hot path).
            Err(e) => match self.refs.resolve(path, &cursor.to_hex())? {
                None => Ok(false),
                Some(_) => Err(e.into()),
            },
        }
    }
}
