//! Debouncing policy, independent of where invalidations come from.
//!
//! A [`Batcher`] receives raw invalidations with the time they were observed
//! and decides when to release them. Time is always passed in by the caller,
//! so policies are tested with a fake clock (plain [`Instant`] arithmetic),
//! never by sleeping.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::{Duration, Instant};

use archipel_model::RepoId;
use archipel_model::ports::{Invalidation, Scope};
use archipel_model::settings::Settings;

/// Policy deciding when pending invalidations are released.
pub trait Batcher: Send {
    /// Records an invalidation observed at `now`.
    fn push(&mut self, invalidation: Invalidation, now: Instant);
    /// Removes and returns the invalidations due at `now`.
    fn take_due(&mut self, now: Instant) -> Vec<Invalidation>;
    /// Earliest instant at which something becomes due (`None` when idle).
    fn next_deadline(&self) -> Option<Instant>;
    /// Drops anything pending for `repo` (its watch was stopped).
    fn forget(&mut self, repo: RepoId);
}

/// Multiplier from the quiet delay to the default maximum latency.
const MAX_LATENCY_FACTOR: u32 = 8;

/// Timing of the [`Debouncer`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebounceConfig {
    /// A repo is flushed once no event arrived for this long.
    pub quiet: Duration,
    /// Upper bound between the first pending event and the flush, so a
    /// continuous stream (a build writing files) still refreshes periodically.
    pub max_latency: Duration,
}

impl DebounceConfig {
    /// Quiet delay `quiet`, max latency `MAX_LATENCY_FACTOR × quiet`.
    #[must_use]
    pub fn with_quiet(quiet: Duration) -> Self {
        Self {
            quiet,
            max_latency: quiet.saturating_mul(MAX_LATENCY_FACTOR),
        }
    }

    /// From [`Settings::watch_debounce_ms`].
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self::with_quiet(Duration::from_millis(settings.watch_debounce_ms))
    }
}

impl Default for DebounceConfig {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    scope: Scope,
    first: Instant,
    last: Instant,
}

impl Pending {
    fn due_at(&self, config: DebounceConfig) -> Instant {
        let quiet = self.last + config.quiet;
        let cap = self.first + config.max_latency;
        quiet.min(cap)
    }
}

/// Per-repo trailing-edge debouncer: scopes are OR-ed while events keep
/// coming; a repo is flushed after `quiet` without events, or `max_latency`
/// after its first pending event, whichever comes first.
#[derive(Debug)]
pub struct Debouncer {
    config: DebounceConfig,
    pending: HashMap<RepoId, Pending>,
}

impl Debouncer {
    /// Creates an idle debouncer.
    #[must_use]
    pub fn new(config: DebounceConfig) -> Self {
        Self {
            config,
            pending: HashMap::new(),
        }
    }
}

impl Batcher for Debouncer {
    fn push(&mut self, invalidation: Invalidation, now: Instant) {
        if invalidation.scope.is_empty() {
            return;
        }
        match self.pending.entry(invalidation.repo) {
            Entry::Occupied(mut e) => {
                let p = e.get_mut();
                p.scope |= invalidation.scope;
                p.last = p.last.max(now);
            }
            Entry::Vacant(e) => {
                e.insert(Pending {
                    scope: invalidation.scope,
                    first: now,
                    last: now,
                });
            }
        }
    }

    fn take_due(&mut self, now: Instant) -> Vec<Invalidation> {
        let config = self.config;
        let mut due = Vec::new();
        self.pending.retain(|&repo, p| {
            if p.due_at(config) <= now {
                due.push(Invalidation {
                    repo,
                    scope: p.scope,
                });
                false
            } else {
                true
            }
        });
        due
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|p| p.due_at(self.config)).min()
    }

    fn forget(&mut self, repo: RepoId) {
        self.pending.remove(&repo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Duration = Duration::from_millis(150);
    const MAX: Duration = Duration::from_millis(1000);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn inv(repo: u64, scope: Scope) -> Invalidation {
        Invalidation {
            repo: RepoId(repo),
            scope,
        }
    }

    fn debouncer() -> Debouncer {
        Debouncer::new(DebounceConfig {
            quiet: QUIET,
            max_latency: MAX,
        })
    }

    #[test]
    fn idle_has_no_deadline() {
        let d = debouncer();
        assert_eq!(d.next_deadline(), None);
    }

    #[test]
    fn coalesces_and_flushes_after_quiet_period() {
        let t0 = Instant::now();
        let mut d = debouncer();
        d.push(inv(1, Scope::STATUS), t0);
        d.push(inv(1, Scope::REFS), t0 + ms(50));
        assert_eq!(d.next_deadline(), Some(t0 + ms(200)));
        assert!(d.take_due(t0 + ms(199)).is_empty());
        assert_eq!(
            d.take_due(t0 + ms(200)),
            vec![inv(1, Scope::STATUS | Scope::REFS)]
        );
        assert_eq!(d.next_deadline(), None);
        assert!(d.take_due(t0 + ms(10_000)).is_empty());
    }

    #[test]
    fn repos_are_independent() {
        let t0 = Instant::now();
        let mut d = debouncer();
        d.push(inv(1, Scope::STATUS), t0);
        d.push(inv(2, Scope::STASH), t0 + ms(100));
        assert_eq!(d.take_due(t0 + ms(150)), vec![inv(1, Scope::STATUS)]);
        assert_eq!(d.next_deadline(), Some(t0 + ms(250)));
        assert_eq!(d.take_due(t0 + ms(250)), vec![inv(2, Scope::STASH)]);
    }

    #[test]
    fn continuous_stream_flushes_at_max_latency() {
        let t0 = Instant::now();
        let mut d = debouncer();
        let step = ms(100); // always shorter than the quiet delay
        let mut flushes = Vec::new();
        let mut now = t0;
        while now <= t0 + ms(2500) {
            d.push(inv(1, Scope::STATUS), now);
            for f in d.take_due(now) {
                flushes.push((now - t0, f));
            }
            now += step;
        }
        let times: Vec<Duration> = flushes.iter().map(|(t, _)| *t).collect();
        assert_eq!(times, vec![ms(1000), ms(2100)]);
    }

    #[test]
    fn empty_scopes_and_forgotten_repos_are_dropped() {
        let t0 = Instant::now();
        let mut d = debouncer();
        d.push(inv(1, Scope::NONE), t0);
        assert_eq!(d.next_deadline(), None);
        d.push(inv(1, Scope::STATUS), t0);
        d.forget(RepoId(1));
        assert!(d.take_due(t0 + MAX).is_empty());
    }

    #[test]
    fn config_from_settings() {
        let c = DebounceConfig::default();
        assert_eq!(c.quiet, ms(150));
        assert_eq!(c.max_latency, ms(1200));
    }
}
