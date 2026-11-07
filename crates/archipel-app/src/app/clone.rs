//! Adding repositories to the open project by cloning them: from plain
//! URLs, or from repositories listed on a forge account.
//!
//! Clones run in parallel on the scheduler's network pool (with progress in
//! the operations drawer); the ones that succeed are appended to the project
//! file and revealed (unhidden, category expanded, selected).

use std::path::{Path, PathBuf};

use archipel_model::forge::RemoteRepo;
use archipel_model::project::ProjectLocation;
use archipel_model::{RepoId, RepoPath};
use archipel_ops::{OpClass, OpError, OpSpec};

use super::{App, AppError, AppEvent};

/// URL used to clone from an account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CloneProtocol {
    /// HTTPS (Git credential helper).
    #[default]
    Https,
    /// SSH (keys).
    Ssh,
}

/// Result of cloning one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneOutcome {
    /// What was cloned: the forge path, or the URL.
    pub repo: String,
    /// The local repository, or the error.
    pub result: Result<RepoPath, String>,
}

/// One clone to run: label, URL, folder name under the destination.
struct CloneJob {
    label: String,
    url: String,
    dir: String,
}

/// The folder `git clone <url>` would create: the last path component of
/// the URL, without a trailing `.git` (`https://host/acme/lib.git` → `lib`,
/// `git@host:acme/lib` → `lib`, `/srv/git/lib.git/` → `lib`). `None` when
/// the URL has no usable name.
#[must_use]
pub fn clone_dir_name(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(['/', '\\']);
    let url = url.strip_suffix(".git").unwrap_or(url);
    let url = url.trim_end_matches(['/', '\\']);
    // scp-like `user@host:path`, or a bare `host:path`.
    let name = url.rsplit(['/', '\\', ':']).next()?.trim();
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

/// Splits pasted text into URLs (one per line or separated by spaces).
#[must_use]
pub fn split_clone_urls(text: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    for url in text.split_whitespace() {
        if !urls.iter().any(|u| u == url) {
            urls.push(url.to_owned());
        }
    }
    urls
}

impl App {
    /// Where clones go by default: the folder holding the project file.
    #[must_use]
    pub fn default_clone_folder(&self) -> Option<PathBuf> {
        self.project_location().and_then(|location| match location {
            ProjectLocation::SharedFile(file) => file.parent().map(Path::to_path_buf),
        })
    }

    /// Clones `urls` side by side into `folder` (each in the folder
    /// `git clone` would name it) and adds the clones to the open project.
    /// A folder that already holds a repository is added as is.
    pub async fn clone_urls_into_project(
        &self,
        urls: &[String],
        folder: &Path,
    ) -> Result<Vec<CloneOutcome>, AppError> {
        let mut jobs = Vec::new();
        for url in urls {
            let url = url.trim();
            if url.is_empty() {
                continue;
            }
            let dir = clone_dir_name(url).ok_or_else(|| {
                AppError::InvalidInput(format!("cannot name a folder after “{url}”"))
            })?;
            jobs.push(CloneJob {
                label: url.to_owned(),
                url: url.to_owned(),
                dir,
            });
        }
        // Same name twice (`acme/lib` and `other/lib`): number them.
        for i in 1..jobs.len() {
            let taken = |name: &str, jobs: &[CloneJob]| jobs.iter().any(|j| j.dir == name);
            if taken(&jobs[i].dir, &jobs[..i]) {
                let base = jobs[i].dir.clone();
                let mut n = 2;
                while taken(&format!("{base}-{n}"), &jobs) {
                    n += 1;
                }
                jobs[i].dir = format!("{base}-{n}");
            }
        }
        self.run_clones(jobs, folder).await
    }

    /// Clones forge `repos` side by side into `folder` (in parallel, bounded
    /// by the network pool) and adds the clones to the open project. A
    /// folder that already holds the repository is added as is.
    pub async fn clone_into_project(
        &self,
        repos: &[RemoteRepo],
        folder: &Path,
        protocol: CloneProtocol,
    ) -> Result<Vec<CloneOutcome>, AppError> {
        let jobs = repos
            .iter()
            .map(|repo| {
                // Same short name in two groups: use the full path instead.
                let clash = repos
                    .iter()
                    .filter(|r| r.name == repo.name)
                    .nth(1)
                    .is_some();
                CloneJob {
                    label: repo.path.clone(),
                    url: match protocol {
                        CloneProtocol::Https => repo.https_url.clone(),
                        CloneProtocol::Ssh => repo.ssh_url.clone(),
                    },
                    dir: if clash {
                        repo.path.replace('/', "-")
                    } else {
                        repo.name.clone()
                    },
                }
            })
            .collect();
        self.run_clones(jobs, folder).await
    }

    async fn run_clones(
        &self,
        jobs: Vec<CloneJob>,
        folder: &Path,
    ) -> Result<Vec<CloneOutcome>, AppError> {
        if self.project().is_none() {
            return Err(AppError::NoProject);
        }
        if jobs.is_empty() {
            return Err(AppError::InvalidInput("no repository selected".into()));
        }
        let mut handles = Vec::new();
        for job in jobs {
            let dest = folder.join(&job.dir);
            let url = job.url;
            let remote = self.services.git.remote.clone();
            let spec = OpSpec::new(None, OpClass::Network, format!("Clone {}", job.label));
            let handle = self.scheduler.submit(spec, move |ctx| {
                if dest.join(".git").exists() {
                    return Ok(RepoPath::new(&dest));
                }
                if dest.exists() {
                    return Err(OpError::Refused(format!(
                        "{} already exists",
                        dest.display()
                    )));
                }
                Ok(remote.clone_repo(&url, &dest, ctx, ctx.cancel())?)
            });
            handles.push((job.label, handle));
        }
        let mut outcomes = Vec::new();
        for (repo, handle) in handles {
            outcomes.push(CloneOutcome {
                repo,
                result: handle.wait().await.map_err(|e| e.to_string()),
            });
        }
        let cloned: Vec<RepoPath> = outcomes
            .iter()
            .filter_map(|o| o.result.as_ref().ok().cloned())
            .collect();
        if !cloned.is_empty() {
            self.add_repos(&cloned)?;
            let ids: Vec<RepoId> = cloned.iter().map(RepoPath::id).collect();
            self.reveal_repos(&ids);
        }
        self.emit(AppEvent::Notice(format!(
            "Clone: {} added to the project, {} failed",
            cloned.len(),
            outcomes.len() - cloned.len()
        )));
        Ok(outcomes)
    }
}

#[cfg(test)]
mod tests {
    use super::{clone_dir_name, split_clone_urls};

    #[test]
    fn dir_name_follows_git_clone() {
        let cases = [
            ("https://github.com/acme/lib.git", Some("lib")),
            ("https://github.com/acme/lib", Some("lib")),
            ("https://github.com/acme/lib/", Some("lib")),
            ("https://gitlab.com/group/sub/tool.git/", Some("tool")),
            ("git@github.com:acme/lib.git", Some("lib")),
            ("git@host:lib.git", Some("lib")),
            ("ssh://git@host:2222/acme/lib.git", Some("lib")),
            ("file:///srv/git/lib.git", Some("lib")),
            ("/srv/git/lib", Some("lib")),
            (r"C:\repos\lib.git", Some("lib")),
            ("  https://host/a/b.git  ", Some("b")),
            ("", None),
            ("https://host/..", None),
            ("/", None),
        ];
        for (url, want) in cases {
            assert_eq!(clone_dir_name(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn urls_split_on_whitespace_without_duplicates() {
        assert_eq!(
            split_clone_urls(" a\nb  a\tc \n"),
            vec!["a".to_owned(), "b".into(), "c".into()]
        );
        assert!(split_clone_urls("  \n").is_empty());
    }
}
