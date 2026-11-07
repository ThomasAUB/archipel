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

impl GitError {
    /// Whether a checkout was refused because it would overwrite local
    /// changes (modified tracked files or untracked files).
    #[must_use]
    pub fn would_overwrite_local_changes(&self) -> bool {
        matches!(self, Self::CommandFailed { stderr, .. } if stderr.contains("would be overwritten by checkout"))
    }
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

/// Errors from forge (GitHub, GitLab, …) adapters. Messages never contain
/// the token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForgeError {
    /// No token for this host.
    #[error("not signed in to {host}")]
    NotSignedIn {
        /// The host.
        host: String,
    },
    /// The forge rejected the token (expired, revoked, missing scope).
    #[error("{host} rejected the token (HTTP {status}); sign in again with a valid token")]
    Unauthorized {
        /// The host.
        host: String,
        /// HTTP status (401 or 403).
        status: u16,
    },
    /// The repository, branch or user does not exist (or is not visible).
    #[error("{host}: {what} not found")]
    NotFound {
        /// The host.
        host: String,
        /// What was looked up.
        what: String,
    },
    /// Any other HTTP error status.
    #[error("{host}: HTTP {status}: {message}")]
    Http {
        /// The host.
        host: String,
        /// HTTP status.
        status: u16,
        /// Message returned by the forge.
        message: String,
    },
    /// The host could not be reached (DNS, TLS, timeout…).
    #[error("cannot reach {host}: {message}")]
    Network {
        /// The host.
        host: String,
        /// Transport error.
        message: String,
    },
    /// The answer could not be understood.
    #[error("unexpected answer from {host}: {message}")]
    Decode {
        /// The host.
        host: String,
        /// Details.
        message: String,
    },
    /// No registered provider serves this forge kind or host.
    #[error("no forge provider for {0}")]
    UnknownForge(String),
    /// The OS keychain failed.
    #[error("keychain: {0}")]
    Keychain(String),
    /// The account list could not be read or written.
    #[error("forge accounts: {0}")]
    Accounts(String),
    /// The forge cannot do what was asked (e.g. a merge method GitLab
    /// only sets per project).
    #[error("{0}")]
    Unsupported(String),
    /// The forge's API rate limit is used up; the token is fine.
    #[error("{host}: API rate limit reached, try again later")]
    RateLimited {
        /// The host.
        host: String,
        /// When the forge accepts calls again, if it said.
        retry_at: Option<crate::Timestamp>,
    },
}
