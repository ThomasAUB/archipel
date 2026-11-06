//! Update check: asks the release feed for the latest Archipel release, at
//! start (after a delay) then every
//! [`Settings::update_check_hours`](archipel_model::settings::Settings), and
//! tells every window when it is newer than the running version.
//!
//! * One check loop for the process, whichever window starts it; the found
//!   release is shared by the sibling cores.
//! * Failures (offline, rate limit) stay in the debug log and are retried
//!   sooner than the interval.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use archipel_model::forge::Release;
use tokio::sync::watch;

use super::{App, AppEvent};

/// The running version.
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Delay before retrying a check that failed.
const RETRY: Duration = Duration::from_secs(3600);

/// State shared by the sibling cores.
#[derive(Debug)]
pub(super) struct UpdateCheck {
    started: AtomicBool,
    found: watch::Sender<Option<Release>>,
}

impl UpdateCheck {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            started: AtomicBool::new(false),
            found: watch::Sender::new(None),
        })
    }
}

/// `x.y.z` with an optional `-pre` / `+build` suffix, as a sortable key: a
/// release sorts after its pre-releases.
fn version_key(version: &str) -> Option<([u64; 3], bool)> {
    let (core, suffix) = version.split_at(version.find(['-', '+']).unwrap_or(version.len()));
    let pre = suffix.starts_with('-');
    let mut key = [0; 3];
    let mut parts = core.split('.');
    for slot in &mut key {
        if let Some(part) = parts.next() {
            *slot = part.parse().ok()?;
        }
    }
    parts.next().is_none().then_some((key, !pre))
}

/// Whether `candidate` is a newer version than `current` (unparsable
/// versions never are).
fn is_newer(candidate: &str, current: &str) -> bool {
    match (version_key(candidate), version_key(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

impl App {
    /// Starts checking for a newer release, the first check after `delay`
    /// (no-op without a release feed, when `update_check_hours` is 0, or
    /// when a window already started it). Frontends call it; one-shot tools
    /// (CLI) do not.
    pub fn start_update_check(&self, delay: Duration) {
        let hours = self.settings.update_check_hours;
        let Some(feed) = self.services.updates.clone() else {
            return;
        };
        if hours == 0 || self.updates.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let interval = Duration::from_secs(hours.saturating_mul(3600));
        let state = self.updates.clone();
        self.scheduler.runtime().spawn(async move {
            tokio::time::sleep(delay).await;
            loop {
                let feed = feed.clone();
                let wait = match tokio::task::spawn_blocking(move || feed.latest_release()).await {
                    Ok(Ok(latest)) => {
                        let newer = latest.filter(|r| is_newer(&r.version, CURRENT));
                        state.found.send_if_modified(|found| {
                            let changed = *found != newer;
                            *found = newer;
                            changed
                        });
                        interval
                    }
                    Ok(Err(e)) => {
                        tracing::debug!(error = %e, "update check failed");
                        interval.min(RETRY)
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "update check panicked");
                        interval.min(RETRY)
                    }
                };
                tokio::time::sleep(wait).await;
            }
        });
    }

    /// The newer release found by the update check, if any.
    #[must_use]
    pub fn available_update(&self) -> Option<Release> {
        self.updates.found.borrow().clone()
    }

    /// Turns a found release into [`AppEvent::UpdateAvailable`] on this
    /// core's channel. The task outlives a closed window only until the next
    /// release found.
    pub(super) fn forward_updates(&self) {
        let mut found = self.updates.found.subscribe();
        let me = self.me.clone();
        self.scheduler.runtime().spawn(async move {
            while found.changed().await.is_ok() {
                let release = found.borrow_and_update().clone();
                let Some(app) = me.upgrade() else { return };
                if let Some(release) = release {
                    app.emit(AppEvent::UpdateAvailable(release));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0+build.5", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("1.2.3.4", "0.1.0"));
    }
}
