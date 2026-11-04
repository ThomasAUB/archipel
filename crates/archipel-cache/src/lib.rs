//! # archipel-cache
//!
//! **Responsibility:** the application's memory and disk caches.
//!
//! - [`lru`]: byte-budgeted LRU caches with named budgets (PLAN §5.1), so
//!   memory use can be reported per cache.
//! - [`sqlite`]: [`SqliteStore`], the SQLite database in the user data dir
//!   implementing every store trait of `archipel_model::ports::store`
//!   (summaries, UI state, project registry, operation journal,
//!   statistics; PLAN §2.1). Project definitions live in `archipel.toml`.
//!
//! **Dependencies:** `archipel-model` (types and port traits), `rusqlite`
//! (bundled SQLite), `serde`/`serde_json` (records are stored as JSON),
//! `tracing`. No `rusqlite` type appears in the public API (ARCHITECTURE §2.3).

pub mod lru;
pub mod sqlite;

pub use lru::{ByteLru, CacheRegistry, CacheReport, CacheStats, SharedLru};
pub use sqlite::SqliteStore;
