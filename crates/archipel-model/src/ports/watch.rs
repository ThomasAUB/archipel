//! Change sources: something that tells which parts of a repo summary are stale.

use std::any::Any;
use std::ops::{BitOr, BitOrAssign};
use std::sync::Arc;

use crate::error::WatchError;
use crate::ids::{RepoId, RepoPath};

/// Set of summary parts made stale by a change (a tiny bit set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Scope(u8);

impl Scope {
    /// Nothing.
    pub const NONE: Self = Self(0);
    /// Working copy or index changed → status counts.
    pub const STATUS: Self = Self(1);
    /// `HEAD`, branches or tags moved → head, upstream, describe.
    pub const REFS: Self = Self(1 << 1);
    /// Stash list changed.
    pub const STASH: Self = Self(1 << 2);
    /// `FETCH_HEAD` written → last fetch time (and remote refs).
    pub const FETCH: Self = Self(1 << 3);
    /// Merge/rebase/cherry-pick state files changed.
    pub const OP_STATE: Self = Self(1 << 4);
    /// Repository configuration changed (upstreams, remotes).
    pub const CONFIG: Self = Self(1 << 5);
    /// Everything.
    pub const ALL: Self = Self(0b11_1111);

    /// Whether every bit of `other` is set.
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether at least one bit of `other` is set.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Whether no bit is set.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Scope {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Scope {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// "These parts of this repo's summary are stale."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalidation {
    /// Repository.
    pub repo: RepoId,
    /// Stale parts.
    pub scope: Scope,
}

/// Receives invalidations, possibly from a watcher thread. Must not block.
pub trait InvalidationSink: Send + Sync {
    /// Called once per (debounced) batch of changes.
    fn invalidate(&self, invalidation: Invalidation);
}

impl<F: Fn(Invalidation) + Send + Sync> InvalidationSink for F {
    fn invalidate(&self, invalidation: Invalidation) {
        self(invalidation);
    }
}

/// How much of a repository a watch covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// `.git/` and the whole worktree.
    Full,
    /// Only `.git/` (OS watch limit reached): worktree changes need a refresh on focus.
    GitDirOnly,
}

/// An active watch; dropping it stops watching.
pub struct WatchHandle {
    /// What the watch covers.
    pub coverage: Coverage,
    /// Opaque guard owned by the implementation.
    pub guard: Box<dyn Any + Send>,
}

impl std::fmt::Debug for WatchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchHandle")
            .field("coverage", &self.coverage)
            .finish_non_exhaustive()
    }
}

/// Emits invalidations for a repository until the handle is dropped.
pub trait ChangeSource: Send + Sync {
    /// Starts watching `path`; invalidations for `repo` go to `sink`.
    fn watch(
        &self,
        repo: RepoId,
        path: &RepoPath,
        sink: Arc<dyn InvalidationSink>,
    ) -> Result<WatchHandle, WatchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_set_operations() {
        let mut s = Scope::STATUS;
        s |= Scope::REFS;
        assert!(s.contains(Scope::STATUS));
        assert!(s.intersects(Scope::REFS | Scope::STASH));
        assert!(!s.contains(Scope::STASH));
        assert!(Scope::ALL.contains(s));
        assert!(Scope::NONE.is_empty());
    }
}
