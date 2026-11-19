//! Remote configuration: which remotes a repo has, and pointing one at a
//! new URL. Remotes live in the shared configuration, so a worktree row
//! acts on its repo, and every row of the repo follows.

use archipel_model::RepoId;
use archipel_model::ports::RemoteInfo;
use archipel_ops::{OpClass, OpError};

use super::{App, AppError, AppEvent, lock};

impl App {
    /// The remotes of the repo of row `repo`. Blocking: run it off the UI
    /// thread.
    pub fn remotes(&self, repo: RepoId) -> Result<Vec<RemoteInfo>, AppError> {
        let (_, path) = self.main_row(repo)?;
        Ok(self
            .services
            .git
            .remotes
            .remotes(&path)
            .map_err(OpError::from)?)
    }

    /// Points remote `remote` of the repo of row `repo` at `url` on the
    /// repo's write queue, then reads the forge status of its rows again:
    /// the forge, the pull request and CI come from the remote URL.
    pub(super) fn set_remote_url(
        &self,
        repo: RepoId,
        remote: String,
        url: &str,
    ) -> Result<(), AppError> {
        let url = url.trim().to_owned();
        let (main, path) = self.main_row(repo)?;
        let me = self.me.clone();
        let _h = self.submit_repo_job(
            &path,
            OpClass::Write,
            format!("Set {remote} URL"),
            move |a, p, _| {
                let push_url = a.set_remote_url(p, &remote, &url)?;
                if let Some(app) = me.upgrade() {
                    app.emit(AppEvent::Notice(match push_url {
                        Some(push) => format!(
                            "{remote} now fetches from {url}; it still pushes to {push} \
                             (remote.{remote}.pushurl)"
                        ),
                        None => format!("{remote} now points at {url}"),
                    }));
                    app.reload_forge_of(main);
                }
                Ok(())
            },
        );
        Ok(())
    }

    /// Reads again the forge status of repo row `main` and of its linked
    /// worktree rows.
    pub(super) fn reload_forge_of(&self, main: RepoId) {
        let ids: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .map(|s| {
                s.rows
                    .iter()
                    .filter(|r| s.main_of(r.id) == main)
                    .map(|r| r.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.load_forge(id, true);
        }
    }
}
