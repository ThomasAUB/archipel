//! Publishing a repository that has no remote yet (one made with New…):
//! the repository is created on a forge account, or an existing URL is
//! given, added as `origin`, and the current branch pushed there and
//! tracked.

use std::path::Path;

use archipel_model::RepoId;
use archipel_model::forge::{ForgeAccount, Namespace, NewRemoteRepo, RemoteRepo};
use archipel_model::requests::PushRequest;
use archipel_model::{HeadState, RepoPath};
use archipel_ops::{OpClass, OpError, RepoActions};

use super::{App, AppError, AppEvent, CloneProtocol};

/// The remote a published repository gets.
pub const PUBLISH_REMOTE: &str = "origin";

/// Where to publish a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishTarget {
    /// A new repository on a forge account.
    Forge {
        /// The account creating it.
        account: ForgeAccount,
        /// Where: the user, an organization or a group.
        namespace: Namespace,
        /// Name, description, visibility.
        repo: NewRemoteRepo,
        /// The URL `origin` gets.
        protocol: CloneProtocol,
    },
    /// An existing (empty) repository, by URL.
    Url(String),
}

/// A published repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishOutcome {
    /// The branch pushed.
    pub branch: String,
    /// The URL `origin` points at.
    pub url: String,
    /// The repository created on the forge, if one was.
    pub created: Option<RemoteRepo>,
}

/// A forge repository name for a folder: its name, with characters
/// forges refuse turned into `-`.
#[must_use]
pub fn publish_name(dir: &Path) -> String {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The branch to publish, or why the repository cannot be: `HEAD` on a
/// branch with commits, no remote yet.
fn check(a: &RepoActions, p: &RepoPath) -> Result<String, OpError> {
    let summary = a.ports().status.summary(p)?;
    let branch = match summary.head {
        HeadState::Branch(branch) => branch,
        HeadState::Unborn(branch) => {
            return Err(OpError::Refused(format!(
                "{branch} has no commit yet: commit before publishing"
            )));
        }
        HeadState::Detached(_) => {
            return Err(OpError::Refused(
                "HEAD is detached: check out the branch to publish".into(),
            ));
        }
    };
    let remotes = a.ports().remotes.remotes(p)?;
    if let Some(remote) = remotes.first() {
        return Err(OpError::Refused(format!(
            "the repository already has the remote {}: push to it",
            remote.name
        )));
    }
    Ok(branch)
}

impl App {
    /// Publishes repo row `repo`, which has no remote: creates the
    /// repository on the forge (or takes the URL given), adds it as
    /// `origin`, then pushes the current branch and tracks it. Nothing is
    /// created when the repository cannot be published (no commit, a remote
    /// already); a failed push leaves `origin` set, so Push retries it.
    pub async fn publish_repo(
        &self,
        repo: RepoId,
        target: PublishTarget,
    ) -> Result<PublishOutcome, AppError> {
        let path = self
            .path_of(repo)
            .ok_or_else(|| AppError::NotFound(repo.to_string()))?;
        let (main, _) = self.main_row(repo)?;
        if let PublishTarget::Forge { repo, .. } = &target
            && repo.name.trim().is_empty()
        {
            return Err(AppError::InvalidInput("Name the repository".into()));
        }
        if let PublishTarget::Url(url) = &target
            && url.trim().is_empty()
        {
            return Err(AppError::InvalidInput("Enter the repository URL".into()));
        }
        let forge = self.services.forge.clone();
        let label = match &target {
            PublishTarget::Forge { account, .. } => format!("Publish to {}", account.host),
            PublishTarget::Url(_) => "Publish".to_owned(),
        };
        let outcome = self
            .submit_repo_job(&path, OpClass::Network, label, move |a, p, op| {
                let branch = check(a, p)?;
                let (url, created) = match target {
                    PublishTarget::Url(url) => (url.trim().to_owned(), None),
                    PublishTarget::Forge {
                        account,
                        namespace,
                        mut repo,
                        protocol,
                    } => {
                        repo.name = repo.name.trim().to_owned();
                        let created = forge
                            .directory
                            .create_repository(&account, &namespace, &repo)
                            .map_err(|e| OpError::Refused(e.to_string()))?;
                        let url = match protocol {
                            CloneProtocol::Https => created.https_url.clone(),
                            CloneProtocol::Ssh => created.ssh_url.clone(),
                        };
                        (url, Some(created))
                    }
                };
                a.set_remote_url(p, PUBLISH_REMOTE, &url)?;
                let request = PushRequest {
                    remote: Some(PUBLISH_REMOTE.to_owned()),
                    refspec: Some(branch.clone()),
                    set_upstream: true,
                    ..PushRequest::default()
                };
                a.push(p, &request, op, op.cancel(), None)
                    .map_err(|e| match &created {
                        Some(c) => OpError::Refused(format!(
                            "{} was created and added as {PUBLISH_REMOTE}, but the push \
                             failed: {e}",
                            c.path
                        )),
                        None => e,
                    })?;
                Ok(PublishOutcome {
                    branch,
                    url,
                    created,
                })
            })
            .wait()
            .await;
        // `origin` may be set even when the push failed.
        self.reload_forge_of(main);
        self.refresh_repo(repo);
        let outcome = outcome?;
        self.emit(AppEvent::Notice(format!(
            "Published {} to {}",
            outcome.branch,
            outcome
                .created
                .as_ref()
                .map_or(outcome.url.as_str(), |c| c.path.as_str())
        )));
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_names_keep_what_forges_accept() {
        assert_eq!(publish_name(Path::new("/x/my-tool_2.0")), "my-tool_2.0");
        assert_eq!(publish_name(Path::new("/x/My Tool (old)")), "My-Tool--old-");
        assert_eq!(publish_name(Path::new("/")), "");
    }
}
