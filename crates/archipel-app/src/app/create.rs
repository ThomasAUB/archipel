//! Adding a repository to the open project by creating it: `git init` in a
//! local folder, new or holding files already, with an optional first
//! commit of those files (issue #68).

use std::path::{Path, PathBuf};

use archipel_model::RepoPath;
use archipel_model::requests::{CommitMode, CommitRequest};
use archipel_ops::OpError;

use super::{App, AppError, AppEvent};

/// What to create (the New Repository dialog).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateRepoRequest {
    /// Folder to make a repository; created when missing.
    pub dir: PathBuf,
    /// Name of the first branch; Git's `init.defaultBranch` when `None`.
    pub initial_branch: Option<String>,
    /// Commits the files already in the folder.
    pub initial_commit: bool,
}

/// Message of the first commit, when asked for.
pub const INITIAL_COMMIT_MESSAGE: &str = "Initial commit";

/// Why `dir` cannot become a new repository, if it cannot.
fn refusal(dir: &Path) -> Option<String> {
    if dir.as_os_str().is_empty() {
        return Some("Choose a folder".into());
    }
    if dir.is_file() {
        return Some(format!("{} is a file", dir.display()));
    }
    if dir.join(".git").exists() {
        return Some(format!(
            "{} is already a repository: add it with Add repos…",
            dir.display()
        ));
    }
    let root = archipel_project::outermost_repo_root(dir)?;
    Some(format!(
        "{} is inside the repository {}",
        dir.display(),
        root.display()
    ))
}

/// Whether `dir` holds something besides its `.git`.
fn has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .is_ok_and(|mut entries| entries.any(|e| e.is_ok_and(|e| e.file_name() != ".git")))
}

impl App {
    /// Why `dir` cannot become a new repository (the dialog's live check),
    /// `None` when it can.
    #[must_use]
    pub fn create_repo_refusal(dir: &Path) -> Option<String> {
        refusal(&std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()))
    }

    /// Creates a repository in `request.dir`, commits its files when asked
    /// (and there are some), and adds it to the open project, revealed.
    /// Blocking: run it off the UI thread.
    pub fn create_repo(&self, request: &CreateRepoRequest) -> Result<RepoPath, AppError> {
        if self.project().is_none() {
            return Err(AppError::NoProject);
        }
        let dir = std::path::absolute(&request.dir).unwrap_or_else(|_| request.dir.clone());
        if let Some(why) = refusal(&dir) {
            return Err(AppError::InvalidInput(why));
        }
        let branch = request
            .initial_branch
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty());
        let git = &self.services.git;
        let path = git
            .repo_init
            .init_repo(&dir, branch)
            .map_err(OpError::from)?;
        let committed = request.initial_commit && has_files(&dir);
        if committed {
            git.index.stage(&path, &[]).map_err(OpError::from)?;
            git.commits
                .commit(
                    &path,
                    &CommitRequest {
                        message: INITIAL_COMMIT_MESSAGE.into(),
                        mode: CommitMode::New,
                    },
                )
                .map_err(OpError::from)?;
        }
        self.add_repos(std::slice::from_ref(&path))?;
        self.reveal_repos(&[path.id()]);
        self.emit(AppEvent::Notice(format!(
            "Created a repository in {}{}",
            dir.display(),
            if committed {
                " with its files committed"
            } else {
                ""
            }
        )));
        Ok(path)
    }
}
