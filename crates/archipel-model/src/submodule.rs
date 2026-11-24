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

/// Where `git submodule add` puts the repository at `url` when given no
/// path: its last path component, without `.git` (`git@host:team/lib.git`
/// → `lib`). Empty when the URL has no usable name.
#[must_use]
pub fn default_submodule_path(url: &str) -> String {
    let url = url.trim().trim_end_matches(['/', '\\']);
    let url = url.strip_suffix(".git").unwrap_or(url);
    let url = url.trim_end_matches(['/', '\\']);
    url.rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::default_submodule_path;

    #[test]
    fn default_path_is_the_last_component_without_dot_git() {
        for (url, path) in [
            ("https://github.com/team/lib.git", "lib"),
            ("https://github.com/team/lib", "lib"),
            ("https://github.com/team/lib/", "lib"),
            ("git@github.com:team/lib.git", "lib"),
            ("git@host:lib.git", "lib"),
            ("../sibling.git", "sibling"),
            (r"C:\repos\lib", "lib"),
            ("  https://host/a/b.git  ", "b"),
            ("", ""),
        ] {
            assert_eq!(default_submodule_path(url), path, "{url}");
        }
    }
}
