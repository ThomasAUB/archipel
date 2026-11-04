//! Cooperative cancellation and progress reporting for long operations.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::GitError;

/// A cheap, cloneable cancellation flag. Long operations poll it between steps.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, non-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation; every clone observes it.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Whether `other` is a clone of this token (same flag).
    #[must_use]
    pub fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Returns `Err(GitError::Cancelled)` when cancellation was requested.
    pub fn check(&self) -> Result<(), GitError> {
        if self.is_cancelled() {
            Err(GitError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// One progress update of a running operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Human readable phase, e.g. "Receiving objects".
    pub phase: String,
    /// Units done so far.
    pub current: u64,
    /// Total units, when known.
    pub total: Option<u64>,
}

/// Receives progress updates. Implementations must be cheap and non-blocking.
pub trait ProgressSink: Send + Sync {
    /// Called for each progress update, possibly from a worker thread.
    fn report(&self, progress: Progress);
}

/// A sink that ignores every update.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn report(&self, _progress: Progress) {}
}

impl<F: Fn(Progress) + Send + Sync> ProgressSink for F {
    fn report(&self, progress: Progress) {
        self(progress);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_shared_between_clones() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(b.check().is_ok());
        a.cancel();
        assert!(matches!(b.check(), Err(GitError::Cancelled)));
    }
}
