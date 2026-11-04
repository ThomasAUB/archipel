//! Locating a repository's Git directory and reading its state files directly
//! (in-progress operations, last fetch time) without spawning processes.

use std::path::{Path, PathBuf};

use archipel_model::{GitError, RepoOp, Timestamp};

/// The Git directories of a working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDirs {
    /// Per-worktree Git directory (`.git`, or `.git/worktrees/<name>`).
    pub git_dir: PathBuf,
    /// Shared directory holding refs and objects (same as `git_dir` for a
    /// regular repository).
    pub common_dir: PathBuf,
}

impl GitDirs {
    /// Locates the Git directories of the working tree (or bare repo) at `repo`.
    pub fn locate(repo: &Path) -> Result<Self, GitError> {
        let dot_git = repo.join(".git");
        let git_dir = if dot_git.is_dir() {
            dot_git
        } else if dot_git.is_file() {
            read_gitdir_file(&dot_git, repo)?
        } else if repo.join("HEAD").is_file() && repo.join("objects").is_dir() {
            repo.to_path_buf()
        } else {
            return Err(GitError::NotARepository {
                path: repo.to_path_buf(),
            });
        };
        let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(text) => resolve(&git_dir, text.trim()),
            Err(_) => git_dir.clone(),
        };
        Ok(Self {
            git_dir,
            common_dir,
        })
    }

    /// The operation in progress, detected from state files.
    #[must_use]
    pub fn in_progress(&self) -> Option<RepoOp> {
        let g = &self.git_dir;
        if g.join("rebase-merge").is_dir() {
            Some(RepoOp::Rebase)
        } else if g.join("rebase-apply").is_dir() {
            if g.join("rebase-apply").join("applying").exists() {
                Some(RepoOp::ApplyMailbox)
            } else {
                Some(RepoOp::Rebase)
            }
        } else if g.join("MERGE_HEAD").is_file() {
            Some(RepoOp::Merge)
        } else if g.join("CHERRY_PICK_HEAD").is_file() {
            Some(RepoOp::CherryPick)
        } else if g.join("REVERT_HEAD").is_file() {
            Some(RepoOp::Revert)
        } else if g.join("BISECT_LOG").is_file() {
            Some(RepoOp::Bisect)
        } else {
            None
        }
    }

    /// Modification time of `FETCH_HEAD` (time of the last fetch).
    #[must_use]
    pub fn last_fetch(&self) -> Option<Timestamp> {
        [&self.git_dir, &self.common_dir]
            .iter()
            .find_map(|d| std::fs::metadata(d.join("FETCH_HEAD")).ok())
            .and_then(|m| m.modified().ok())
            .map(Timestamp::from_system)
    }
}

fn read_gitdir_file(dot_git: &Path, repo: &Path) -> Result<PathBuf, GitError> {
    let text = std::fs::read_to_string(dot_git).map_err(|e| GitError::Io {
        path: dot_git.to_path_buf(),
        message: e.to_string(),
    })?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| GitError::NotARepository {
            path: repo.to_path_buf(),
        })?;
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

/// Whether the working tree declares submodules.
#[must_use]
pub fn has_submodules(repo: &Path) -> bool {
    repo.join(".gitmodules").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_fixtures::TestRepo;

    #[test]
    fn locates_regular_repo_and_detects_merge() {
        let repo = TestRepo::new().unwrap();
        let dirs = GitDirs::locate(repo.path()).unwrap();
        assert_eq!(dirs.git_dir, repo.path().join(".git"));
        assert_eq!(dirs.in_progress(), None);
        std::fs::write(dirs.git_dir.join("MERGE_HEAD"), "x").unwrap();
        assert_eq!(dirs.in_progress(), Some(RepoOp::Merge));
    }

    #[test]
    fn locates_linked_worktree() {
        let repo = TestRepo::new().unwrap();
        repo.commit_all("init").unwrap();
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("wt");
        repo.git(&[
            "worktree",
            "add",
            "-q",
            wt_path.to_str().unwrap(),
            "-b",
            "side",
        ])
        .unwrap();
        let dirs = GitDirs::locate(&wt_path).unwrap();
        assert!(dirs.git_dir.ends_with("worktrees/wt") || dirs.git_dir.ends_with("worktrees\\wt"));
        assert_eq!(
            dirs.common_dir.canonicalize().unwrap(),
            repo.path().join(".git").canonicalize().unwrap()
        );
    }

    #[test]
    fn not_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            GitDirs::locate(dir.path()),
            Err(GitError::NotARepository { .. })
        ));
    }
}
