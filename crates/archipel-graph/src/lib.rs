//! # archipel-graph
//!
//! Commit graph: on-disk topology cache, lane layout with checkpoints, paged history.
//!
//! * Responsibility: show a history of any size while holding only a small,
//!   bounded part of it in memory (PLAN §2.5). The topology (ids + parent row
//!   indices) lives in a compact cache file per repository, read by positioned
//!   reads; the lane layout keeps a checkpoint every N rows so any window is
//!   replayed from the nearest one; commit metadata is loaded per visible
//!   window into a byte-budgeted LRU.
//! * Dependencies: port traits and types of `archipel-model`
//!   ([`HistoryReader`](archipel_model::ports::HistoryReader),
//!   [`RefReader`](archipel_model::ports::RefReader)), and the pure LRU of
//!   `archipel-cache`. Never a Git adapter: backends are injected.
//! * Main types: [`HistoryView`] (open / rows / meta / refresh / unload),
//!   [`GraphRow`] and [`Edge`] (what the UI draws), [`Searcher`] (background
//!   search), [`LaneState`] (the layout algorithm, usable on its own),
//!   [`GraphError`].

mod error;
mod layout;
mod search;
mod stream;
mod topology;
mod view;

pub use error::GraphError;
pub use layout::{Edge, EdgeKind, LaneState, ParentLink, RowLayout};
pub use search::{SearchQuery, SearchResult, Searcher};
pub use view::{
    CacheStatus, Completion, GraphOptions, GraphRow, GraphSize, HistoryView, MemoryUsage,
    OpenReport, RefreshOutcome,
};
