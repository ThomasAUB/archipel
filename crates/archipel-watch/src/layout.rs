//! Where a repository's files live, and which paths to watch.
//!
//! Small reimplementation of Git directory discovery (adapters do not depend
//! on each other): a `.git` directory, a `.git` file pointing to a linked
//! worktree's Git directory (`gitdir:`), whose `commondir` points to the
//! shared directory, or a bare repository.

use std::path::{Path, PathBuf};

use archipel_model::WatchError;

/// Name of the Git directory (or file) inside a working tree.
const DOT_GIT: &str = ".git";
/// Prefix of the line of a `.git` file naming the Git directory.
const GITDIR_PREFIX: &str = "gitdir:";
/// File of a linked worktree's Git dir naming the common directory.
const COMMONDIR_FILE: &str = "commondir";
/// Directory holding refs, watched recursively when the whole Git dir is not.
const REFS_DIR: &str = "refs";

/// Canonical locations of a repository's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoLayout {
    /// Working tree root (`None` for a bare repository).
    pub worktree: Option<PathBuf>,
    /// Per-worktree Git directory.
    pub git_dir: PathBuf,
    /// Shared directory (refs, config, objects); equals `git_dir` except for
    /// linked worktrees.
    pub common_dir: PathBuf,
}

/// Recursion of one watched path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Depth {
    /// The directory's direct entries only.
    Shallow,
    /// The whole subtree.
    Recursive,
}

/// One path to watch.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WatchPath {
    /// Canonical path.
    pub path: PathBuf,
    /// Recursion.
    pub depth: Depth,
}

impl WatchPath {
    fn new(path: PathBuf, depth: Depth) -> Self {
        Self { path, depth }
    }

    /// Whether this watch already reports events for `other`.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        if self.path == other.path {
            self.depth == Depth::Recursive || other.depth == Depth::Shallow
        } else {
            self.depth == Depth::Recursive && other.path.starts_with(&self.path)
        }
    }
}

impl RepoLayout {
    /// Locates the repository at `repo` and canonicalizes every path, so that
    /// event paths (built by the OS watcher from the watched path) can be
    /// matched by prefix whatever spelling the caller used.
    pub fn locate(repo: &Path) -> Result<Self, WatchError> {
        let repo = canonical(repo)?;
        let dot_git = repo.join(DOT_GIT);
        let (worktree, git_dir) = if dot_git.is_dir() {
            (Some(repo.clone()), dot_git)
        } else if dot_git.is_file() {
            (
                Some(repo.clone()),
                canonical(&read_gitdir_file(&dot_git, &repo)?)?,
            )
        } else if repo.join("HEAD").is_file() && repo.join("objects").is_dir() {
            (None, repo.clone())
        } else {
            return Err(WatchError::Backend(format!(
                "not a git repository: {}",
                repo.display()
            )));
        };
        let common_dir = match std::fs::read_to_string(git_dir.join(COMMONDIR_FILE)) {
            Ok(text) => canonical(&resolve(&git_dir, text.trim()))?,
            Err(_) => git_dir.clone(),
        };
        Ok(Self {
            worktree,
            git_dir,
            common_dir,
        })
    }

    /// Recursive watch of the worktree, if any.
    #[must_use]
    pub fn worktree_watch(&self) -> Option<WatchPath> {
        self.worktree
            .as_ref()
            .map(|w| WatchPath::new(w.clone(), Depth::Recursive))
    }

    /// Watches covering the Git directories without the object store: each
    /// Git dir shallowly (state files, operation dirs appearing) and the
    /// shared `refs/` recursively.
    #[must_use]
    pub fn git_dir_watches(&self) -> Vec<WatchPath> {
        let mut out = vec![WatchPath::new(self.git_dir.clone(), Depth::Shallow)];
        if self.common_dir != self.git_dir {
            out.push(WatchPath::new(self.common_dir.clone(), Depth::Shallow));
        }
        let refs = self.common_dir.join(REFS_DIR);
        if refs.is_dir() {
            out.push(WatchPath::new(refs, Depth::Recursive));
        }
        out
    }

    /// Git dir watches not already covered by the worktree watch (the Git dir
    /// of a regular repository lives inside its worktree).
    #[must_use]
    pub fn git_dir_watches_outside_worktree(&self) -> Vec<WatchPath> {
        let Some(wt) = self.worktree_watch() else {
            return self.git_dir_watches();
        };
        self.git_dir_watches()
            .into_iter()
            .filter(|w| !wt.covers(w))
            .collect()
    }
}

fn canonical(path: &Path) -> Result<PathBuf, WatchError> {
    std::fs::canonicalize(path)
        .map_err(|e| WatchError::Backend(format!("cannot resolve {}: {e}", path.display())))
}

fn read_gitdir_file(dot_git: &Path, repo: &Path) -> Result<PathBuf, WatchError> {
    let text = std::fs::read_to_string(dot_git)
        .map_err(|e| WatchError::Backend(format!("cannot read {}: {e}", dot_git.display())))?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix(GITDIR_PREFIX))
        .map(str::trim)
        .ok_or_else(|| WatchError::Backend(format!("no gitdir line in {}", dot_git.display())))?;
    Ok(resolve(repo, target))
}

fn resolve(base: &Path, target: &str) -> PathBuf {
    let p = Path::new(target);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_fixtures::TestRepo;

    #[test]
    fn regular_repo_git_dir_is_covered_by_worktree() {
        let repo = TestRepo::new().unwrap();
        let layout = RepoLayout::locate(repo.path()).unwrap();
        assert_eq!(layout.git_dir, layout.common_dir);
        assert!(layout.git_dir.ends_with(".git"));
        assert!(layout.git_dir_watches_outside_worktree().is_empty());
        assert_eq!(layout.git_dir_watches().len(), 2);
    }

    #[test]
    fn linked_worktree_watches_its_git_dirs() {
        let repo = TestRepo::new().unwrap();
        repo.commit_all("init").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path().join("wt");
        repo.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"])
            .unwrap();
        let layout = RepoLayout::locate(&wt).unwrap();
        assert_eq!(
            layout.common_dir,
            std::fs::canonicalize(repo.path().join(".git")).unwrap()
        );
        assert!(layout.git_dir.starts_with(&layout.common_dir));
        let outside = layout.git_dir_watches_outside_worktree();
        assert_eq!(outside.len(), 3, "{outside:?}");
    }

    #[test]
    fn bare_repo_and_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        archipel_fixtures::git(tmp.path(), &["init", "-q", "--bare", "b.git"]).unwrap();
        let layout = RepoLayout::locate(&tmp.path().join("b.git")).unwrap();
        assert_eq!(layout.worktree, None);
        assert!(layout.worktree_watch().is_none());
        assert!(RepoLayout::locate(tmp.path()).is_err());
    }

    #[test]
    fn coverage() {
        let rec = WatchPath::new(PathBuf::from("/a"), Depth::Recursive);
        let shallow = WatchPath::new(PathBuf::from("/a"), Depth::Shallow);
        let child = WatchPath::new(PathBuf::from("/a/b"), Depth::Recursive);
        assert!(rec.covers(&shallow));
        assert!(rec.covers(&child));
        assert!(!shallow.covers(&rec));
        assert!(!shallow.covers(&child));
        assert!(!child.covers(&rec));
    }
}
