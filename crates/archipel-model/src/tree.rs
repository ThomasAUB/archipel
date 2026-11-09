//! The files of a repository at a commit or in the working copy (the repo
//! view's file browser).

use serde::{Deserialize, Serialize};

/// What a [`TreeEntry`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// An executable file.
    Executable,
    /// A symbolic link (its content is the target path).
    Symlink,
    /// A submodule (gitlink): no content in this repository.
    Submodule,
}

impl EntryKind {
    /// The kind of a Git tree mode (`100644`, `100755`, `120000`, `160000`).
    #[must_use]
    pub fn from_mode(mode: &str) -> Self {
        match mode {
            "100755" => Self::Executable,
            "120000" => Self::Symlink,
            "160000" => Self::Submodule,
            _ => Self::File,
        }
    }
}

/// One file of a repository; folders are implied by the paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TreeEntry {
    /// Path relative to the repository root, `/`-separated.
    pub path: String,
    /// What the entry is.
    pub kind: EntryKind,
}

impl TreeEntry {
    /// A regular file at `path`.
    #[must_use]
    pub fn file(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::File,
        }
    }
}
