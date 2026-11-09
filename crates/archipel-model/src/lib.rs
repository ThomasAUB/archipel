//! # archipel-model
//!
//! Pure data types and **port traits** shared by every Archipel crate.
//!
//! * Responsibility: define the vocabulary of the application (repos, summaries,
//!   commits, diffs, projects, journal entries…) and the small role-based
//!   interfaces that adapters implement and features consume.
//! * Dependencies: none inside the workspace; no I/O, no third-party adapter types.
//! * Main traits: see [`ports`] — Git readers/writers, project source/discovery,
//!   stores, change sources, forge access (pull requests, account directory,
//!   sign-in, credentials).
//! * Fakes: with the `testing` feature, `testing` provides an in-memory fake of
//!   every port trait so consumers can be tested without real repositories.

pub mod cancel;
pub mod changes;
pub mod diff;
pub mod error;
pub mod forge;
pub mod history;
pub mod ids;
pub mod journal;
pub mod ports;
pub mod project;
pub mod refs;
pub mod requests;
pub mod settings;
pub mod stats;
pub mod submodule;
pub mod summary;
pub mod time;
pub mod tree;
pub mod ui_state;

#[cfg(feature = "testing")]
pub mod testing;

pub use cancel::{CancelToken, NoProgress, Progress, ProgressSink};
pub use error::{ForgeError, GitError, ProjectError, StoreError, WatchError};
pub use ids::{Oid, ProjectId, RepoId, RepoPath};
pub use summary::{
    ChangeCounts, ChangedFiles, ContainingBranch, HeadState, RepoOp, RepoSummary, Upstream,
};
pub use time::Timestamp;
