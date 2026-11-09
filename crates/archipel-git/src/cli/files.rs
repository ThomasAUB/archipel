//! Worktree file contents, read and written directly on disk (the diff
//! view's file editor). Not a Git command, but the worktree is this
//! adapter's to touch.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use archipel_model::ports::WorktreeFiles;
use archipel_model::{GitError, RepoPath};

use super::GitCli;

impl WorktreeFiles for GitCli {
    fn read_file(&self, repo: &RepoPath, path: &str) -> Result<Vec<u8>, GitError> {
        let file = worktree_path(repo, path)?;
        std::fs::read(&file).map_err(|e| io_error(&file, &e))
    }

    fn write_file(&self, repo: &RepoPath, path: &str, contents: &[u8]) -> Result<(), GitError> {
        let file = worktree_path(repo, path)?;
        write_replacing(&file, contents).map_err(|e| io_error(&file, &e))
    }

    fn create_file(&self, repo: &RepoPath, path: &str, contents: &[u8]) -> Result<(), GitError> {
        let file = worktree_path(repo, path)?;
        let create = || {
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)?;
            out.write_all(contents)?;
            out.sync_all()
        };
        create().map_err(|e| io_error(&file, &e))
    }
}

/// `path` under the worktree of `repo`; refused when it is absolute or
/// climbs out (`..`).
fn worktree_path(repo: &RepoPath, path: &str) -> Result<PathBuf, GitError> {
    let relative = Path::new(path);
    let inside = !path.is_empty()
        && relative
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(GitError::InvalidRequest(format!(
            "{path} is not a path inside the worktree"
        )));
    }
    Ok(repo.as_path().join(relative))
}

/// Replaces an existing file through a temporary sibling renamed over it,
/// so a failed write never leaves it half written. The file keeps its
/// permissions (an executable script stays executable).
fn write_replacing(file: &Path, contents: &[u8]) -> std::io::Result<()> {
    let permissions = std::fs::metadata(file)?.permissions();
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let temp = file.with_file_name(format!(".{name}.archipel-{}", std::process::id()));
    let result = (|| {
        let mut out = std::fs::File::create(&temp)?;
        out.write_all(contents)?;
        out.sync_all()?;
        drop(out);
        std::fs::set_permissions(&temp, permissions)?;
        std::fs::rename(&temp, file)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn io_error(path: &Path, e: &std::io::Error) -> GitError {
    GitError::Io {
        path: path.to_owned(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_must_stay_inside_the_worktree() {
        let repo = RepoPath::new("/repo");
        assert!(worktree_path(&repo, "src/main.rs").is_ok());
        assert!(worktree_path(&repo, "./a.txt").is_ok());
        for bad in ["", "../outside", "src/../../outside", "/etc/passwd"] {
            assert!(worktree_path(&repo, bad).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn write_replaces_the_file_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let repo = RepoPath::new(dir.path());
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.txt"), b"old\r\n").unwrap();
        let cli = GitCli::default();
        assert_eq!(cli.read_file(&repo, "src/a.txt").unwrap(), b"old\r\n");
        cli.write_file(&repo, "src/a.txt", b"new\r\n").unwrap();
        assert_eq!(cli.read_file(&repo, "src/a.txt").unwrap(), b"new\r\n");
        let names: Vec<_> = std::fs::read_dir(dir.path().join("src"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.txt"]);
    }

    #[test]
    fn create_refuses_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = RepoPath::new(dir.path());
        let cli = GitCli::default();
        cli.create_file(&repo, "new.txt", b"x").unwrap();
        assert_eq!(cli.read_file(&repo, "new.txt").unwrap(), b"x");
        let err = cli.create_file(&repo, "new.txt", b"y").unwrap_err();
        assert!(matches!(err, GitError::Io { .. }), "{err}");
        assert_eq!(cli.read_file(&repo, "new.txt").unwrap(), b"x");
    }

    #[test]
    fn write_refuses_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = RepoPath::new(dir.path());
        let err = GitCli::default()
            .write_file(&repo, "gone.txt", b"x")
            .unwrap_err();
        assert!(matches!(err, GitError::Io { .. }), "{err}");
    }
}
