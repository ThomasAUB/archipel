//! In-process reads with `gix` (gitoxide).
//!
//! Implements [`StatusReader`], [`RefReader`], [`HistoryReader`] and
//! [`StashReader`]. Every `gix` type stays private to this module; results are
//! converted to `archipel-model` types.

mod history;
mod refs;
mod status;

use archipel_model::{GitError, Oid, RepoPath};

/// Reads repositories in-process with `gix`.
///
/// Opening a repository reads its configuration (so `safe.directory`,
/// `core.*` and friends are honored); it is done per call, which keeps
/// configuration changes visible without any invalidation logic.
#[derive(Clone, Debug)]
pub struct GixReader {
    object_cache_bytes: usize,
}

impl GixReader {
    /// Creates a reader using an object cache of `object_cache_bytes` per open repository.
    #[must_use]
    pub fn new(object_cache_bytes: usize) -> Self {
        Self { object_cache_bytes }
    }

    fn open(&self, repo: &RepoPath) -> Result<gix::Repository, GitError> {
        // Cheap check first, so a missing repo maps to `NotARepository`.
        crate::gitdir::GitDirs::locate(repo.as_path())?;
        let mut r = gix::open(repo.as_path()).map_err(backend)?;
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
