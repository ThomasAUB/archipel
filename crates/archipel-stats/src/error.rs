//! Errors of the statistics crate.

use archipel_model::{GitError, StoreError};

/// Errors from computing or reading statistics.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatsError {
    /// A Git read failed.
    #[error(transparent)]
    Git(GitError),
    /// The statistics store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The update was cancelled; the store was left untouched.
    #[error("statistics update cancelled")]
    Cancelled,
    /// `HEAD` kept moving while the history was being read.
    #[error("HEAD moved during the statistics update ({attempts} attempts)")]
    HeadMoved {
        /// Number of attempts made.
        attempts: u32,
    },
}

impl From<GitError> for StatsError {
    fn from(error: GitError) -> Self {
        match error {
            GitError::Cancelled => Self::Cancelled,
            other => Self::Git(other),
        }
    }
}
