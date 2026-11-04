//! A cooperative pause flag for long background computations.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use archipel_model::CancelToken;

/// How often a paused computation re-checks its cancel token.
const CANCEL_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Default)]
struct PauseState {
    paused: Mutex<bool>,
    resumed: Condvar,
}

/// A cheap, cloneable pause flag. A computation calls
/// [`wait_while_paused`](Self::wait_while_paused) between steps and blocks
/// (without spinning) until [`resume`](Self::resume) or cancellation.
#[derive(Clone, Debug, Default)]
pub struct PauseToken(Arc<PauseState>);

impl PauseToken {
    /// A fresh, running (not paused) token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks every computation observing this token to pause at its next step.
    pub fn pause(&self) {
        *self.lock() = true;
    }

    /// Resumes paused computations.
    pub fn resume(&self) {
        *self.lock() = false;
        self.0.resumed.notify_all();
    }

    /// Whether a pause is requested.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        *self.lock()
    }

    /// Blocks while paused. Returns `true` when `cancel` fired (paused or not).
    pub fn wait_while_paused(&self, cancel: &CancelToken) -> bool {
        let mut paused = self.lock();
        while *paused && !cancel.is_cancelled() {
            paused = self
                .0
                .resumed
                .wait_timeout(paused, CANCEL_POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        cancel.is_cancelled()
    }

    fn lock(&self) -> MutexGuard<'_, bool> {
        self.0.paused.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_token_does_not_block() {
        let token = PauseToken::new();
        assert!(!token.is_paused());
        assert!(!token.wait_while_paused(&CancelToken::new()));
    }

    #[test]
    fn cancel_releases_a_paused_wait() {
        let token = PauseToken::new();
        token.pause();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(token.wait_while_paused(&cancel));
    }

    #[test]
    fn resume_releases_a_paused_wait() {
        let token = PauseToken::new();
        token.pause();
        let waiter = {
            let token = token.clone();
            std::thread::spawn(move || token.wait_while_paused(&CancelToken::new()))
        };
        std::thread::sleep(Duration::from_millis(20));
        token.resume();
        assert!(!waiter.join().unwrap_or(true));
    }
}
