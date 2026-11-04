//! # archipel-cache
//!
//! Byte-budgeted LRU caches and the SQLite store implementing every store trait.

pub mod lru;

pub use lru::{ByteLru, CacheRegistry, CacheReport, CacheStats, SharedLru};
