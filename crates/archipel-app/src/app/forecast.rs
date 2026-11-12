//! Conflict forecast (spec §3): after each refresh of a row (a fetch that
//! moved refs, a commit, a checkout) and each forge read (the pull request
//! target may change), checks in the background whether the current branch
//! would conflict with its upstream and with its pull request's target (the
//! default branch without one).
//!
//! * Nothing is recomputed while the row's inputs (`HEAD`, upstream, last
//!   fetch, pull request target) stay the same; a side whose commits did not
//!   move keeps its result (cache keyed by both commit ids and the strategy).
//! * One forecast runs at a time, as [`OpClass::Background`]: skipped when
//!   the repo is busy (retried a little later), cancelled when the user
//!   writes to it, never in the drawer.
//! * The upstream forecast follows the repo's `pull.rebase` setting; the
//!   target forecast checks a merge, as the forge does.

use std::time::Duration;

use archipel_model::forecast::MergeStrategy;
use archipel_model::{CancelToken, GitError, Oid, RepoId, RepoPath, Timestamp, Upstream};
use archipel_ops::{GitPorts, OpClass, OpError, OpSpec};

use super::forge_flow::{default_target, upstream_remote};
use super::{App, AppEvent, lock};
use crate::dashboard::DashboardRow;
use crate::forecast::{ConflictForecast, Forecast, ForecastBase};

/// Delay before retrying the forecast of a repo that was busy (a forge read
/// or a write on it: both hold the repo).
const BUSY_RETRY: Duration = Duration::from_secs(2);

/// What the forecast of a row is computed from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ForecastInputs {
    branch: String,
    head: Oid,
    upstream: Option<Upstream>,
    last_fetch: Option<Timestamp>,
    /// Number and target branch of the open pull request.
    pull_request: Option<(u64, String)>,
    /// Remote of the upstream, where the target branch is looked up.
    remote: String,
}

impl ForecastInputs {
    /// `None` when there is nothing to forecast: no branch checked out, or
    /// no commit on it.
    fn of(row: &DashboardRow) -> Option<Self> {
        let summary = row.summary()?;
        Some(Self {
            branch: summary.head.branch()?.to_owned(),
            head: summary.head_oid?,
            upstream: summary.upstream.clone(),
            last_fetch: summary.last_fetch,
            pull_request: row
                .forge
                .pull_request()
                .map(|pr| (pr.number, pr.target.clone())),
            remote: upstream_remote(row).unwrap_or_else(|| "origin".to_owned()),
        })
    }
}

impl App {
    /// Queues the conflict forecast of `repo` when what it depends on
    /// changed since the last one.
    pub(super) fn load_forecast(&self, repo: RepoId) {
        if !self.settings.conflict_forecast {
            return;
        }
        let cleared = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            let Some(row) = s.rows.iter_mut().find(|r| r.id == repo && !r.hidden) else {
                return;
            };
            match ForecastInputs::of(row) {
                None => {
                    s.forecast_inputs.remove(&repo);
                    s.forecast_queue.retain(|r| *r != repo);
                    let had = row.forecast != ConflictForecast::default();
                    row.forecast = ConflictForecast::default();
                    had
                }
                Some(inputs) => {
                    if s.forecast_inputs.get(&repo) == Some(&inputs) {
                        return;
                    }
                    s.forecast_inputs.insert(repo, inputs);
                    if !s.forecast_queue.contains(&repo) {
                        s.forecast_queue.push_back(repo);
                    }
                    false
                }
            }
        };
        if cleared {
            self.emit(AppEvent::RowsChanged(vec![repo]));
        }
        self.run_next_forecast();
    }

    /// Starts the next queued forecast, unless one is running.
    fn run_next_forecast(&self) {
        let (repo, path, inputs, previous) = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            if s.forecast_running.is_some() {
                return;
            }
            let next = loop {
                let Some(repo) = s.forecast_queue.pop_front() else {
                    return;
                };
                let row = s.rows.iter().find(|r| r.id == repo && !r.hidden);
                if let (Some(row), Some(inputs)) = (row, s.forecast_inputs.get(&repo)) {
                    break (repo, row.path.clone(), inputs.clone(), row.forecast.clone());
                }
            };
            s.forecast_running = Some(next.0);
            next
        };
        let git = self.services.git.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Background,
            format!("Conflict forecast — {}", path.name()),
        );
        let job_inputs = inputs.clone();
        let handle = self.scheduler.submit(spec, move |ctx| {
            Ok(compute(&git, &path, &job_inputs, &previous, ctx.cancel()))
        });
        let me = self.me.clone();
        self.scheduler.runtime().spawn(async move {
            let result = handle.wait().await;
            if let Some(app) = me.upgrade() {
                app.apply_forecast(repo, &inputs, result);
            }
        });
    }

    /// Queues again the forecast of a repo that was busy, unless its inputs
    /// changed meanwhile (it is queued already, or has nothing to forecast).
    fn retry_forecast(&self, repo: RepoId) {
        {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            if !s.forecast_inputs.contains_key(&repo)
                || s.forecast_queue.contains(&repo)
                || s.forecast_running == Some(repo)
            {
                return;
            }
            s.forecast_queue.push_back(repo);
        }
        self.run_next_forecast();
    }

    /// Stores a forecast in its row (when its inputs are still current),
    /// then starts the next one.
    fn apply_forecast(
        &self,
        repo: RepoId,
        inputs: &ForecastInputs,
        result: Result<ConflictForecast, OpError>,
    ) {
        let (changed, retry) = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            if s.forecast_running == Some(repo) {
                s.forecast_running = None;
            }
            let current = s.forecast_inputs.get(&repo) == Some(inputs);
            match result {
                Ok(forecast) if current => match s.rows.iter_mut().find(|r| r.id == repo) {
                    Some(row) if row.forecast != forecast => {
                        row.forecast = forecast;
                        (true, false)
                    }
                    _ => (false, false),
                },
                // Inputs changed meanwhile: the repo is queued again.
                Ok(_) => (false, false),
                Err(e) => {
                    let busy = matches!(e, OpError::Busy) || e.is_cancelled();
                    if !busy {
                        tracing::debug!(error = %e, %repo, "conflict forecast failed");
                    }
                    (false, busy && current)
                }
            }
        };
        if changed {
            self.emit(AppEvent::RowsChanged(vec![repo]));
        }
        if retry {
            let me = self.me.clone();
            self.scheduler.runtime().spawn(async move {
                tokio::time::sleep(BUSY_RETRY).await;
                if let Some(app) = me.upgrade() {
                    app.retry_forecast(repo);
                }
            });
        }
        self.run_next_forecast();
    }
}

/// Computes both forecasts of a repo (blocking; on a scheduler worker),
/// reusing those of `previous` whose commits did not move. A side that
/// cannot be computed is left out.
fn compute(
    git: &GitPorts,
    path: &RepoPath,
    inputs: &ForecastInputs,
    previous: &ConflictForecast,
    cancel: &CancelToken,
) -> ConflictForecast {
    let upstream = upstream_forecast(git, path, inputs, previous.upstream.as_ref(), cancel)
        .unwrap_or_else(|e| {
            tracing::debug!(error = %e, repo = %path.as_path().display(), "upstream forecast failed");
            None
        });
    let target = target_forecast(git, path, inputs, previous.target.as_ref(), cancel)
        .unwrap_or_else(|e| {
            tracing::debug!(error = %e, repo = %path.as_path().display(), "target forecast failed");
            None
        });
    ConflictForecast { upstream, target }
}

/// Against the upstream, when the branch is behind it: what a pull would
/// do (conflicts only when it also has commits of its own; otherwise it
/// fast-forwards) and the paths it would change.
fn upstream_forecast(
    git: &GitPorts,
    path: &RepoPath,
    inputs: &ForecastInputs,
    previous: Option<&Forecast>,
    cancel: &CancelToken,
) -> Result<Option<Forecast>, GitError> {
    let Some(u) = inputs.upstream.as_ref().filter(|u| !u.gone && u.behind > 0) else {
        return Ok(None);
    };
    let Some(theirs) = git.refs.resolve(path, &u.name)? else {
        return Ok(None);
    };
    let ours = inputs.head;
    let asked = git.forecaster.pull_strategy(path, &inputs.branch)?;
    if let Some(p) = previous.filter(|p| p.is_for(ours, theirs, asked)) {
        return Ok(Some(p.clone()));
    }
    let (strategy, conflicts) = if u.ahead > 0 {
        let f = git.forecaster.forecast(path, ours, theirs, asked, cancel)?;
        (f.strategy, f.conflicts)
    } else {
        (asked, Vec::new())
    };
    let mut incoming = git.forecaster.incoming_paths(path, ours, theirs)?;
    incoming.truncate(Forecast::MAX_INCOMING);
    Ok(Some(forecast(
        ForecastBase::Upstream,
        u.name.clone(),
        (ours, theirs, asked, strategy),
        conflicts,
        incoming,
    )))
}

/// Against the target of the pull request, or the default branch; none
/// on that branch itself, or when it is the upstream (already checked).
fn target_forecast(
    git: &GitPorts,
    path: &RepoPath,
    inputs: &ForecastInputs,
    previous: Option<&Forecast>,
    cancel: &CancelToken,
) -> Result<Option<Forecast>, GitError> {
    let (name, base) = match &inputs.pull_request {
        Some((number, target)) => (target.clone(), ForecastBase::PullRequest(*number)),
        None => (
            default_target(git, path, &inputs.remote),
            ForecastBase::DefaultBranch,
        ),
    };
    if name == inputs.branch {
        return Ok(None);
    }
    let remote_name = format!("{}/{name}", inputs.remote);
    let (against, theirs) = match git.refs.resolve(path, &remote_name)? {
        Some(oid) => (remote_name, oid),
        None => match git.refs.resolve(path, &name)? {
            Some(oid) => (name, oid),
            None => return Ok(None),
        },
    };
    if inputs.upstream.as_ref().is_some_and(|u| u.name == against) {
        return Ok(None);
    }
    let ours = inputs.head;
    let asked = MergeStrategy::Merge;
    if let Some(p) = previous.filter(|p| p.is_for(ours, theirs, asked)) {
        // The label may change (a pull request opened on the same target).
        return Ok(Some(Forecast {
            base,
            against,
            ..p.clone()
        }));
    }
    let f = git.forecaster.forecast(path, ours, theirs, asked, cancel)?;
    Ok(Some(forecast(
        base,
        against,
        (ours, theirs, asked, f.strategy),
        f.conflicts,
        Vec::new(),
    )))
}

fn forecast(
    base: ForecastBase,
    against: String,
    (ours, theirs, asked, strategy): (Oid, Oid, MergeStrategy, MergeStrategy),
    mut conflicts: Vec<String>,
    incoming: Vec<String>,
) -> Forecast {
    let total = u32::try_from(conflicts.len()).unwrap_or(u32::MAX);
    conflicts.truncate(Forecast::MAX_PATHS);
    Forecast {
        base,
        against,
        ours,
        theirs,
        asked,
        strategy,
        conflicts,
        total,
        incoming,
    }
}
