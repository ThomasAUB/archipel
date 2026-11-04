//! Errors of operations.

use archipel_model::{ForgeError, GitError, StoreError};

/// Why an operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpError {
    /// A Git command or backend failed (carries the command and stderr).
    #[error(transparent)]
    Git(#[from] GitError),
    /// The journal store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A forge (GitHub, GitLab…) call failed.
    #[error(transparent)]
    Forge(#[from] ForgeError),
    /// The operation was cancelled before or while running.
    #[error("operation cancelled")]
    Cancelled,
    /// The operation was refused because it would lose work or is invalid.
    #[error("{0}")]
    Refused(String),
    /// The worker running the operation panicked or was shut down.
    #[error("operation worker failed: {0}")]
    Worker(String),
}

impl OpError {
    /// Whether the error is a cancellation (from the token or Git).
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Git(GitError::Cancelled))
    }
}
