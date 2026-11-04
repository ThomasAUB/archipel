//! Submodules and the repo dependency tree (spec §9).

use serde::{Deserialize, Serialize};

use crate::ids::{Oid, RepoPath};

/// One submodule declared by a repository (a single level, not recursive).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleInfo {
    /// Path relative to the parent working tree, `/`-separated (as Git prints it).
    pub name: String,
    /// Absolute path of the submodule working tree.
    pub path: RepoPath,
    /// Whether the submodule is initialized (cloned and checked out).
    pub initialized: bool,
    /// Commit recorded by the parent (its index), if any.
    pub pinned: Option<Oid>,
    /// Commit checked out in the submodule, when initialized.
    pub checked_out: Option<Oid>,
}

impl SubmoduleInfo {
    /// The checked-out commit differs from the one the parent pins.
    #[must_use]
    pub fn is_out_of_sync(&self) -> bool {
        self.initialized
            && self.pinned.is_some()
            && self.checked_out.is_some()
            && self.pinned != self.checked_out
    }
}

/// A submodule with its working-copy state and its own submodules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleNode {
    /// The submodule itself.
    pub info: SubmoduleInfo,
    /// Uncommitted changes inside the submodule (only known when initialized).
    pub dirty: bool,
    /// Nested submodules (empty when not initialized).
    pub children: Vec<SubmoduleNode>,
}
