//! In-process reads with `gix` (gitoxide).
//!
//! Implements [`StatusReader`], [`RefReader`], [`HistoryReader`] and
//! [`StashReader`]. Every `gix` type stays private to this module; results are
//! converted to `archipel-model` types.

mod history;
mod refs;
mod status;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use archipel_model::{GitError, Oid, RepoPath};

/// Opened repositories kept by a [`GixReader`] (see ADR 0002, "handle cache").
#[derive(Default)]
struct Handles {
    repos: HashMap<PathBuf, (Option<SystemTime>, gix::ThreadSafeRepository)>,
}

/// Maximum number of opened repositories kept (a few projects' worth).
const MAX_HANDLES: usize = 64;

/// Reads repositories in-process with `gix`.
///
/// Opening a repository reads its configuration (so `safe.directory`,
/// `core.*` and friends are honored). Opened repositories are cached per
/// path and re-opened when the repository's `config` file changes, so
/// upstream or remote edits are seen immediately. Refs, the index and
/// objects are always read fresh.
#[derive(Clone)]
pub struct GixReader {
    object_cache_bytes: usize,
    handles: Option<Arc<Mutex<Handles>>>,
}

impl std::fmt::Debug for GixReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GixReader")
            .field("object_cache_bytes", &self.object_cache_bytes)
            .field("cache_handles", &self.handles.is_some())
            .finish()
    }
}

impl GixReader {
    /// Creates a reader using an object cache of `object_cache_bytes` per
    /// open repository, caching opened repositories.
    #[must_use]
    pub fn new(object_cache_bytes: usize) -> Self {
        Self {
            object_cache_bytes,
            handles: Some(Arc::default()),
        }
    }

    /// A reader that opens the repository on every call (no handle cache).
    #[must_use]
    pub fn uncached(object_cache_bytes: usize) -> Self {
        Self {
            object_cache_bytes,
            handles: None,
        }
    }

    fn open(&self, repo: &RepoPath) -> Result<gix::Repository, GitError> {
        // Cheap check first, so a missing repo maps to `NotARepository`.
        let dirs = crate::gitdir::GitDirs::locate(repo.as_path())?;
        let Some(handles) = &self.handles else {
            let mut r = gix::open(repo.as_path()).map_err(backend)?;
            r.object_cache_size_if_unset(self.object_cache_bytes);
            return Ok(r);
        };
        let stamp = std::fs::metadata(dirs.common_dir.join("config"))
            .and_then(|m| m.modified())
            .ok();
        let key = repo.as_path().to_path_buf();
        let cached = handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .repos
            .get(&key)
            .filter(|(s, _)| *s == stamp)
            .map(|(_, r)| r.clone());
        let shared = if let Some(r) = cached {
            r
        } else {
            let opened = gix::ThreadSafeRepository::open(repo.as_path()).map_err(backend)?;
            let mut h = handles.lock().unwrap_or_else(PoisonError::into_inner);
            if h.repos.len() >= MAX_HANDLES {
                h.repos.clear();
            }
            h.repos.insert(key, (stamp, opened.clone()));
            opened
        };
        let mut r = shared.to_thread_local();
        r.object_cache_size_if_unset(self.object_cache_bytes);
        Ok(r)
    }
}

impl Default for GixReader {
    fn default() -> Self {
        Self::new(4 * 1024 * 1024)
    }
}

pub(crate) fn backend(e: impl std::fmt::Display) -> GitError {
    GitError::Backend(e.to_string())
}

pub(crate) fn to_oid(id: impl AsRef<gix::oid>) -> Result<Oid, GitError> {
    Oid::from_bytes(id.as_ref().as_bytes())
        .ok_or_else(|| GitError::Backend("unsupported object id length".into()))
}

pub(crate) fn to_gix(oid: Oid) -> Result<gix::ObjectId, GitError> {
    gix::ObjectId::try_from(oid.as_bytes()).map_err(backend)
}
