//! Per-file working copy changes.

use serde::{Deserialize, Serialize};

/// Kind of change of one side (index or worktree) of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeKind {
    /// No change on this side.
    Unmodified,
    /// Content or mode changed.
    Modified,
    /// File added.
    Added,
    /// File deleted.
    Deleted,
    /// File renamed (see [`FileChange::original_path`]).
    Renamed,
    /// File copied.
    Copied,
    /// Type changed (e.g. file ↔ symlink).
    TypeChanged,
}

/// Overall state of a changed path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileState {
    /// Tracked file with index and/or worktree changes.
    Tracked {
        /// Change between `HEAD` and the index (staged).
        staged: ChangeKind,
        /// Change between the index and the worktree (unstaged).
        unstaged: ChangeKind,
    },
    /// File not tracked by Git.
    Untracked,
    /// Unresolved merge conflict.
    Conflicted,
}

/// One changed path of the working copy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Path relative to the repository root, `/`-separated.
    pub path: String,
    /// Source path for renames and copies.
    pub original_path: Option<String>,
    /// State of the path.
    pub state: FileState,
}

impl FileChange {
    /// Whether the change has a staged part.
    #[must_use]
    pub fn is_staged(&self) -> bool {
        matches!(self.state, FileState::Tracked { staged, .. } if staged != ChangeKind::Unmodified)
    }

    /// Whether the change has an unstaged part (untracked counts as unstaged).
    #[must_use]
    pub fn is_unstaged(&self) -> bool {
        match self.state {
            FileState::Tracked { unstaged, .. } => unstaged != ChangeKind::Unmodified,
            FileState::Untracked | FileState::Conflicted => true,
        }
    }
}
