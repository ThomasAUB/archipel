//! # archipel-watch
//!
//! File watching: maps file system events to fine-grained summary
//! invalidations ([`Scope`](archipel_model::ports::Scope)).
//!
//! * Responsibility: implement the [`ChangeSource`](archipel_model::ports::ChangeSource)
//!   port. Watch each repository's worktree and Git directories, classify
//!   every event path into the parts of the summary it makes stale, drop
//!   noise (lock files, objects, ignored files, reads), debounce per
//!   repository, and degrade to Git-directory-only watching when the OS
//!   refuses a recursive worktree watch.
//! * Allowed dependencies: `archipel-model` (ports and types), `notify`,
//!   `ignore`, `thiserror`, `tracing`. No other workspace crate; `notify`
//!   types never appear in the public API.
//! * Main types:
//!   * [`classify()`] / [`Area`]: pure path → scope classification;
//!     [`RepoClassifier`] adds the area lookup and ignore rules.
//!   * [`Batcher`]: debouncing policy, separate from the source;
//!     [`Debouncer`] (quiet delay + max latency) is the default.
//!   * [`NotifyChangeSource`]: the `notify`-backed change source (one shared
//!     OS watcher and one dispatcher thread for all repositories).
//!   * [`RepoLayout`]: worktree / Git dir / common dir of a repository.

mod classify;
mod debounce;
mod layout;
mod source;

pub use classify::{Area, RepoClassifier, classify};
pub use debounce::{Batcher, DebounceConfig, Debouncer};
pub use layout::{Depth, RepoLayout, WatchPath};
pub use source::NotifyChangeSource;
