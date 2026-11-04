//! Errors of the graph crate.

use std::path::PathBuf;

use archipel_model::{GitError, Oid};

/// Errors returned by [`HistoryView`](crate::HistoryView) and [`Searcher`](crate::Searcher).
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The Git backend failed (includes [`GitError::Cancelled`]).
    #[error(transparent)]
    Git(#[from] GitError),
    /// Reading or writing the topology cache file failed.
    #[error("topology cache i/o error on {}: {source}", path.display())]
    Io {
        /// The cache file involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The topology cache file is inconsistent (it is rebuilt on open).
    #[error("corrupt topology cache {}: {reason}", path.display())]
    Corrupt {
        /// The cache file involved.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
    /// The history mixes SHA-1 and SHA-256 object ids.
    #[error("history mixes object id lengths ({expected} and {found} bytes)")]
    MixedHashLengths {
        /// Length of the first id seen.
        expected: usize,
        /// Offending length.
        found: usize,
    },
    /// The history has more rows than the cache format can index.
    #[error("history too large for the topology cache (more than {max} commits)")]
    TooManyRows {
        /// Maximum supported number of rows.
        max: u32,
    },
    /// The backend returned no metadata for a commit of the window.
    #[error("no metadata returned for commit {0}")]
    MissingMeta(Oid),
    /// The view has no repository open (call `open` first).
    #[error("no repository is open in this history view")]
    NotOpen,
}

impl GraphError {
    /// Wraps an I/O error with the cache file path.
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Builds a corruption error.
    pub(crate) fn corrupt(path: &std::path::Path, reason: impl Into<String>) -> Self {
        Self::Corrupt {
            path: path.to_path_buf(),
            reason: reason.into(),
        }
    }

    /// A copy of this error, for reporting one background failure to several
    /// waiters (`std::io::Error` is not `Clone`: its kind and message are kept).
    pub(crate) fn duplicate(&self) -> Self {
        match self {
            Self::Git(e) => Self::Git(e.clone()),
            Self::Io { path, source } => Self::Io {
                path: path.clone(),
                source: std::io::Error::new(source.kind(), source.to_string()),
            },
            Self::Corrupt { path, reason } => Self::Corrupt {
                path: path.clone(),
                reason: reason.clone(),
            },
            Self::MixedHashLengths { expected, found } => Self::MixedHashLengths {
                expected: *expected,
                found: *found,
            },
            Self::TooManyRows { max } => Self::TooManyRows { max: *max },
            Self::MissingMeta(oid) => Self::MissingMeta(*oid),
            Self::NotOpen => Self::NotOpen,
        }
    }

    /// Whether this error means the operation was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Git(GitError::Cancelled))
    }
}
