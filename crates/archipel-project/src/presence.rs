//! Classification of a loaded project's repositories as present or missing.

use std::path::Path;

use archipel_model::project::{ProjectDef, RepoEntry};

/// Name of the Git metadata entry in a working tree.
pub(crate) const DOT_GIT: &str = ".git";

/// Entries that together identify a bare repository.
const BARE_MARKERS: [&str; 3] = ["HEAD", "objects", "refs"];

/// Whether a project's repository is usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoPresence {
    /// The path holds a Git repository (`.git` directory or file, or bare).
    Present,
    /// The path does not exist.
    PathNotFound,
    /// The path exists but is not a Git repository.
    NotARepository,
}

impl RepoPresence {
    /// Whether the repository can be opened.
    #[must_use]
    pub fn is_present(self) -> bool {
        self == Self::Present
    }
}

/// Checks a single repository path on disk.
///
/// A path is a repository when it contains a `.git` directory or file, or
/// when it looks like a bare repository (`HEAD`, `objects`, `refs`).
#[must_use]
pub fn repo_presence(path: &Path) -> RepoPresence {
    if !path.is_dir() {
        return RepoPresence::PathNotFound;
    }
    let dot_git = path.join(DOT_GIT);
    let is_bare = || BARE_MARKERS.iter().all(|m| path.join(m).exists());
    if dot_git.is_dir() || dot_git.is_file() || is_bare() {
        RepoPresence::Present
    } else {
        RepoPresence::NotARepository
    }
}

/// Classifies every repository of `project`, in display order.
#[must_use]
pub fn classify_repos(project: &ProjectDef) -> Vec<(&RepoEntry, RepoPresence)> {
    project
        .repos
        .iter()
        .map(|entry| (entry, repo_presence(entry.path.as_path())))
        .collect()
}
