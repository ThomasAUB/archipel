//! # archipel-stats
//!
//! Streaming, incremental repository statistics (spec §7, PLAN Phase 7).
//!
//! * Responsibility: keep per-repo statistics up to date incrementally
//!   ([`StatsEngine`]) and turn them into view models through pluggable
//!   [`StatProvider`]s held in a [`StatsRegistry`] (ARCHITECTURE §5). Results
//!   exist per repository and aggregated across the repositories of a project.
//! * Allowed dependencies: `archipel-model` (types and port traits only),
//!   `thiserror`, `tracing`. Never an adapter crate: Git access goes through
//!   [`ChurnReader`](archipel_model::ports::ChurnReader),
//!   [`RefReader`](archipel_model::ports::RefReader) and
//!   [`HistoryReader`](archipel_model::ports::HistoryReader); persistence
//!   through [`StatsStore`](archipel_model::ports::StatsStore).
//! * Main types:
//!   * [`StatsEngine`] — streams new commits since the stored cursor and applies
//!     one bounded delta atomically; pausable ([`PauseToken`]) and cancellable.
//!   * [`StatProvider`] / [`StatsRegistry`] — the extension point; the three
//!     built-ins ([`ActivityProvider`], [`BranchesProvider`],
//!     [`HotspotsProvider`]) are registered exactly like a fork's provider.
//!   * [`StatView`] — view models (activity, branches, hotspots, generic table).
//!   * [`StatsError`] — typed errors wrapping `GitError` / `StoreError`.
//!
//! Every call is blocking: callers run the engine and the providers on a
//! low-priority background thread, never on the UI thread.

mod builtin;
mod config;
mod default_branch;
mod engine;
mod error;
mod pause;
mod provider;
mod registry;
mod view;

pub use builtin::{
    ACTIVITY_ID, ActivityProvider, BRANCHES_ID, BranchesProvider, HOTSPOTS_ID, HotspotsProvider,
};
pub use config::{
    DEFAULT_HOTSPOT_LIMIT, DEFAULT_STALE_BRANCH_DAYS, FALLBACK_DEFAULT_BRANCHES, StatsConfig,
};
pub use default_branch::{DefaultBranch, detect_default_branch};
pub use engine::{DeltaSize, StatsEngine, UpdateOutcome};
pub use error::StatsError;
pub use pause::PauseToken;
pub use provider::{RepoTarget, StatProvider, StatSources};
pub use registry::{ProjectStats, ProviderResult, ProviderView, RepoStats, StatsRegistry};
pub use view::{
    ActivityView, BranchRow, BranchesView, HotspotRow, HotspotsView, StatView, TableView,
};
