//! Periodic background fetch: keeps remote-tracking refs fresh without the
//! user fetching by hand, at a cost close to zero.
//!
//! * One repo at a time, spread over the interval
//!   ([`Settings::auto_fetch_secs`](archipel_model::settings::Settings)),
//!   repos shown in a tab or selected first.
//! * Runs as [`OpClass::Background`]: skipped when the repo is busy and
//!   cancelled as soon as the user writes to it; never in the drawer.
//! * Per-repo back-off: a fetch that brings nothing doubles the repo's
//!   interval (up to [`MAX_BACKOFF`] times the base), one that brings
//!   something resets it; a failing repo (offline, credentials) waits the
//!   longest interval. A manual fetch resets the repos it covers.
//! * A fetch that brings nothing only updates the "last fetch" time; one that
//!   moves refs refreshes the repo.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use archipel_model::ui_state::TabState;
use archipel_model::{ProjectId, RepoId, Timestamp};
use archipel_ops::{OpClass, OpError, OpHandle, OpSpec};

use super::{App, AppEvent, lock};
use crate::dashboard::RepoStatus;

/// Longest interval, as a multiple of the base one.
const MAX_BACKOFF: u32 = 8;
/// Delay before retrying a repo that was busy (or taken over by the user).
const BUSY_RETRY: Duration = Duration::from_secs(30);
/// Bounds of the delay between two fetches.
const MIN_PACE: Duration = Duration::from_secs(2);
const MAX_PACE: Duration = Duration::from_secs(30);

/// How a background fetch went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fetched {
    Updated,
    Unchanged,
    Busy,
    Failed,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    due: Instant,
    interval: Duration,
}

/// When each repo of the open project is fetched next.
#[derive(Debug)]
pub(super) struct AutoFetch {
    base: Duration,
    started: bool,
    project: Option<ProjectId>,
    slots: HashMap<RepoId, Slot>,
}

impl AutoFetch {
    pub(super) fn new(base: Duration) -> Self {
        Self {
            base,
            started: false,
            project: None,
            slots: HashMap::new(),
        }
    }

    fn max_interval(&self) -> Duration {
        self.base * MAX_BACKOFF
    }

    /// Follows the open project's repos; a new repo is due one interval
    /// after its last fetch (right away if never fetched).
    fn sync(
        &mut self,
        project: &ProjectId,
        repos: &[(RepoId, Option<Timestamp>)],
        now: Instant,
        wall: Timestamp,
    ) {
        if self.project.as_ref() != Some(project) {
            self.project = Some(project.clone());
            self.slots.clear();
        }
        self.slots
            .retain(|id, _| repos.iter().any(|(r, _)| r == id));
        for (id, last) in repos {
            let base = self.base;
            self.slots.entry(*id).or_insert_with(|| {
                let age = last.map_or(base, |t| {
                    Duration::from_secs(u64::try_from(wall.0 - t.0).unwrap_or(0))
                });
                Slot {
                    due: now + base.saturating_sub(age),
                    interval: base,
                }
            });
        }
    }

    /// The repo to fetch now: the most overdue, `priority` ones first.
    fn next(&self, now: Instant, priority: &[RepoId]) -> Option<RepoId> {
        self.slots
            .iter()
            .filter(|(_, s)| s.due <= now)
            .min_by_key(|(id, s)| (!priority.contains(id), s.due))
            .map(|(id, _)| *id)
    }

    fn record(&mut self, repo: RepoId, fetched: Fetched, now: Instant) {
        let (base, max) = (self.base, self.max_interval());
        let Some(slot) = self.slots.get_mut(&repo) else {
            return;
        };
        match fetched {
            Fetched::Busy => {
                slot.due = now + BUSY_RETRY;
                return;
            }
            Fetched::Updated => slot.interval = base,
            Fetched::Unchanged => slot.interval = (slot.interval * 2).min(max),
            Fetched::Failed => slot.interval = max,
        }
        slot.due = now + slot.interval;
    }

    /// A manual fetch of `repos` (every repo when empty).
    fn reset(&mut self, repos: &[RepoId], now: Instant) {
        for (id, slot) in &mut self.slots {
            if repos.is_empty() || repos.contains(id) {
                *slot = Slot {
                    due: now + self.base,
                    interval: self.base,
                };
            }
        }
    }

    /// Delay between two fetches: the interval spread over the repos.
    fn pace(&self) -> Duration {
        let n = u32::try_from(self.slots.len()).unwrap_or(u32::MAX).max(1);
        (self.base / n).clamp(MIN_PACE, MAX_PACE)
    }

    /// Delay until the next repo is due, when none is.
    fn idle(&self, now: Instant) -> Duration {
        self.slots
            .values()
            .map(|s| s.due.saturating_duration_since(now))
            .min()
            .unwrap_or(MAX_PACE)
            .clamp(MIN_PACE, MAX_PACE)
    }
}

impl App {
    /// Starts fetching the open project's repos in the background (no-op
    /// when `auto_fetch_secs` is 0 or already started). Frontends call it;
    /// one-shot tools (CLI) do not.
    pub fn start_auto_fetch(&self) {
        {
            let mut plan = lock(&self.auto_fetch);
            if plan.base.is_zero() || plan.started {
                return;
            }
            plan.started = true;
        }
        let me = self.me.clone();
        self.scheduler.runtime().spawn(async move {
            loop {
                // The app is not kept alive while waiting.
                let Some(app) = me.upgrade() else { return };
                let next = app.auto_fetch_next();
                drop(app);
                let (repo, handle) = match next {
                    Ok(next) => next,
                    Err(wait) => {
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                };
                let result = handle.wait().await;
                let Some(app) = me.upgrade() else { return };
                let wait = app.auto_fetch_done(repo, result);
                drop(app);
                tokio::time::sleep(wait).await;
            }
        });
    }

    /// Submits the fetch of the next due repo, or tells how long to wait.
    fn auto_fetch_next(&self) -> Result<(RepoId, OpHandle<bool>), Duration> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Err(MAX_PACE);
        };
        let repos: Vec<_> = s
            .rows
            .iter()
            .filter(|r| !r.hidden)
            .filter_map(|r| r.summary().map(|sum| (r.id, sum.last_fetch)))
            .collect();
        let priority: Vec<RepoId> =
            s.ui.tabs
                .iter()
                .filter_map(|t| match t {
                    TabState::Repo(id) => Some(*id),
                    TabState::Dashboard => None,
                })
                .chain(s.ui.selected_repo)
                .chain(s.ui.selection.iter().copied())
                .collect();
        let now = Instant::now();
        let mut plan = lock(&self.auto_fetch);
        plan.sync(&s.id, &repos, now, Timestamp::now());
        let Some(repo) = plan.next(now, &priority) else {
            return Err(plan.idle(now));
        };
        let Some(row) = s.rows.iter().find(|r| r.id == repo) else {
            return Err(MIN_PACE);
        };
        let path = row.path.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Background,
            format!("Auto fetch — {}", path.name()),
        );
        let actions = self.actions.clone();
        let handle = self.scheduler.submit(spec, move |ctx| {
            actions.fetch_background(&path, ctx.cancel())
        });
        Ok((repo, handle))
    }

    /// Records a background fetch; returns the delay before the next one.
    fn auto_fetch_done(&self, repo: RepoId, result: Result<bool, OpError>) -> Duration {
        let fetched = match result {
            Ok(true) => Fetched::Updated,
            Ok(false) => Fetched::Unchanged,
            Err(OpError::Busy) => Fetched::Busy,
            Err(e) if e.is_cancelled() => Fetched::Busy,
            Err(e) => {
                tracing::debug!(error = %e, %repo, "background fetch failed");
                Fetched::Failed
            }
        };
        match fetched {
            Fetched::Updated => self.refresh_repo(repo),
            Fetched::Unchanged => self.note_fetched(repo),
            Fetched::Busy | Fetched::Failed => {}
        }
        let mut plan = lock(&self.auto_fetch);
        plan.record(repo, fetched, Instant::now());
        plan.pace()
    }

    /// A manual fetch: the background one starts over for these repos.
    pub(super) fn auto_fetch_reset(&self, repos: &[RepoId]) {
        lock(&self.auto_fetch).reset(repos, Instant::now());
    }

    /// A fetch that moved no ref: only the "last fetch" time changes, no
    /// need to recompute the summary.
    pub(super) fn note_fetched(&self, repo: RepoId) {
        {
            let mut guard = lock(&self.session);
            let Some(row) = guard
                .as_mut()
                .and_then(|s| s.rows.iter_mut().find(|r| r.id == repo))
            else {
                return;
            };
            let RepoStatus::Ready(summary) = &mut row.status else {
                return;
            };
            summary.last_fetch = Some(Timestamp::now());
        }
        self.emit(AppEvent::RowsChanged(vec![repo]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_secs(600);

    fn project() -> ProjectId {
        crate::file_project_id(std::path::Path::new("/p/archipel.toml"))
    }

    fn plan(repos: &[(RepoId, Option<Timestamp>)], now: Instant) -> AutoFetch {
        let mut p = AutoFetch::new(BASE);
        p.sync(&project(), repos, now, Timestamp(10_000));
        p
    }

    #[test]
    fn repos_are_due_one_interval_after_their_last_fetch() {
        let now = Instant::now();
        let p = plan(
            &[
                (RepoId(1), None),
                (RepoId(2), Some(Timestamp(10_000 - 60))),
                (RepoId(3), Some(Timestamp(10_000 - 6000))),
            ],
            now,
        );
        assert_eq!(p.slots[&RepoId(1)].due, now);
        assert_eq!(p.slots[&RepoId(2)].due, now + Duration::from_secs(540));
        assert_eq!(p.slots[&RepoId(3)].due, now);
    }

    #[test]
    fn priority_repos_go_first_then_the_most_overdue() {
        let now = Instant::now();
        let mut p = plan(&[(RepoId(1), None), (RepoId(2), None)], now);
        let later = now + Duration::from_secs(5);
        p.slots.get_mut(&RepoId(1)).unwrap().due = later;
        assert_eq!(p.next(later, &[]), Some(RepoId(2)));
        assert_eq!(p.next(later, &[RepoId(1)]), Some(RepoId(1)));
        p.record(RepoId(1), Fetched::Unchanged, now);
        p.record(RepoId(2), Fetched::Unchanged, now);
        assert_eq!(p.next(now, &[]), None);
        assert_eq!(p.idle(now), MAX_PACE);
    }

    #[test]
    fn interval_backs_off_while_nothing_comes_and_resets_otherwise() {
        let now = Instant::now();
        let mut p = plan(&[(RepoId(1), None)], now);
        let interval = |p: &AutoFetch| p.slots[&RepoId(1)].interval;
        p.record(RepoId(1), Fetched::Unchanged, now);
        assert_eq!(interval(&p), BASE * 2);
        for _ in 0..10 {
            p.record(RepoId(1), Fetched::Unchanged, now);
        }
        assert_eq!(interval(&p), BASE * MAX_BACKOFF);
        p.record(RepoId(1), Fetched::Updated, now);
        assert_eq!(interval(&p), BASE);
        p.record(RepoId(1), Fetched::Failed, now);
        assert_eq!(interval(&p), BASE * MAX_BACKOFF);
        p.record(RepoId(1), Fetched::Busy, now);
        assert_eq!(p.slots[&RepoId(1)].due, now + BUSY_RETRY);
        p.reset(&[], now);
        assert_eq!(interval(&p), BASE);
        assert_eq!(p.slots[&RepoId(1)].due, now + BASE);
    }

    #[test]
    fn follows_the_project_and_spreads_fetches() {
        let now = Instant::now();
        let mut p = plan(&[(RepoId(1), None), (RepoId(2), None)], now);
        p.sync(&project(), &[(RepoId(2), None)], now, Timestamp(10_000));
        assert_eq!(p.slots.len(), 1);
        assert_eq!(p.pace(), MAX_PACE);
        let many: Vec<_> = (0..100).map(|i| (RepoId(i), None)).collect();
        p.sync(&project(), &many, now, Timestamp(10_000));
        assert_eq!(p.pace(), Duration::from_secs(6));
        let other = crate::file_project_id(std::path::Path::new("/q/archipel.toml"));
        p.sync(&other, &[(RepoId(7), None)], now, Timestamp(10_000));
        assert_eq!(p.slots.keys().copied().collect::<Vec<_>>(), [RepoId(7)]);
    }
}
