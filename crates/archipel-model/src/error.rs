//! Error types returned by the port traits.
//!
//! Every variant carries the context needed to display the error to the user
//! (repository, exact command, stderr).

use std::path::PathBuf;

/// Errors from Git readers and writers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitError {
    /// The path is not a Git repository.
    #[error("not a git repository: {}", path.display())]
    NotARepository {
        /// The offending path.
        path: PathBuf,
    },
    /// A `git` subprocess exited with a failure status.
    #[error("`{command}` failed (exit code {exit_code:?}): {stderr}")]
    CommandFailed {
        /// The exact command line, without environment or credentials.
        command: String,
        /// Process exit code, when the process exited normally.
        exit_code: Option<i32>,
        /// Captured standard error (hook output, conflicts…).
        stderr: String,
    },
    /// Output of a command could not be parsed.
    #[error("could not parse {what}: {detail}")]
    Parse {
        /// What was being parsed.
        what: &'static str,
        /// Details, usually the offending line.
        detail: String,
    },
    /// Error raised by an in-process Git backend.
    #[error("git backend error: {0}")]
    Backend(String),
    /// I/O error on a given path.
    #[error("i/o error on {}: {message}", path.display())]
    Io {
        /// The path involved.
        path: PathBuf,
        /// The underlying error message.
        message: String,
    },
    /// The operation was cancelled through its `CancelToken`.
    #[error("operation cancelled")]
    Cancelled,
    /// The operation exceeded its time limit.
    #[error("operation timed out after {seconds}s")]
    TimedOut {
        /// The limit that was hit.
        seconds: u64,
    },
    /// The backend does not implement this operation.
    #[error("operation not supported by this backend: {0}")]
    Unsupported(&'static str),
    /// The request is invalid (e.g. a malformed branch name).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

/// Errors from persistent stores.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The underlying database failed.
    #[error("store backend error: {0}")]
    Backend(String),
    /// Stored data could not be decoded.
    #[error("corrupt stored data: {0}")]
    Corrupt(String),
    /// The requested record does not exist.
    #[error("not found: {0}")]
    NotFound(String),
}

/// Errors from loading, saving or discovering projects.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    /// The project file could not be read or written.
    #[error("i/o error on {}: {message}", path.display())]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying error message.
        message: String,
    },
    /// The project file is not valid.
    #[error("invalid project file {}: {message}", path.display())]
    Invalid {
        /// The file involved.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
    /// The file uses a schema version this build does not understand.
    #[error("unsupported project file version {found} (max supported {supported})")]
    UnsupportedVersion {
        /// Version found in the file.
        found: u32,
        /// Highest version this build reads.
        supported: u32,
    },
    /// Error from the store holding private projects.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Errors from file watching.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    /// The OS refused to add a watch (e.g. inotify limit reached).
    #[error("cannot watch {}: {message}", path.display())]
    LimitReached {
        /// The path that could not be watched.
        path: PathBuf,
        /// The OS error message.
        message: String,
    },
    /// Any other watcher failure.
    #[error("watcher error: {0}")]
    Backend(String),
}
