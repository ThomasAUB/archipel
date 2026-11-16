//! Worktree writes (spec §10, WORKTREES.md §6): create, remove (undoable on
//! the repo), lock, prune, run on the repo's write queue; the repo's
//! worktree list is read again after each, so rows follow at once.

use std::path::PathBuf;

use archipel_model::worktree::{LocationVars, SetView, WorktreeAddRequest, expand_location};
use archipel_model::{RepoId, RepoPath};
use archipel_ops::{OpClass, OpError, RemoveOptions, WorktreeLoss, WorktreeSetup, same_folder};

use super::{App, AppError, AppEvent, lock};
use crate::actions::Command;

impl App {
    /// The repo row `repo` belongs to and its path.
    fn main_row(&self, repo: RepoId) -> Result<(RepoId, RepoPath), AppError> {
        let main = self.main_repo_of(repo);
        let path = self
            .path_of(main)
            .ok_or_else(|| AppError::NotFound(repo.to_string()))?;
        Ok((main, path))
    }

    /// Reads the worktree list of repo `main` at once (inside a job), so the
    /// rows follow what the job did.
    fn reload_worktrees_now(&self, main: &RepoPath) {
        let result = self.services.git.worktrees.worktrees(main);
        self.apply_worktrees(main.id(), result);
    }

    /// Where a new worktree of the repo of row `repo` on `branch` goes,
    /// from the `worktree_location` template (spec §10).
    #[must_use]
    pub fn default_worktree_folder(&self, repo: RepoId, branch: &str) -> Option<PathBuf> {
        let guard = lock(&self.session);
        let s = guard.as_ref()?;
        let main = s.main_of(repo);
        let row = s.row(main)?;
        let vars = LocationVars {
            project: &s.def.name,
            repo: &row.name,
            branch,
            set: "",
        };
        Some(expand_location(
            &self.settings.worktree_location,
            &vars,
            row.path.as_path(),
        ))
    }

    /// The `copy_to_worktrees` patterns of the repo of row `repo`.
    #[must_use]
    pub fn copy_patterns(&self, repo: RepoId) -> Vec<String> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        let main = s.main_of(repo);
        s.def
            .repos
            .iter()
            .find(|r| r.path.id() == main)
            .map(|r| r.copy_to_worktrees.clone())
            .unwrap_or_default()
    }

    /// What removing linked worktree row `worktree` would lose. Blocking:
    /// run it off the UI thread.
    pub fn worktree_loss(&self, worktree: RepoId) -> Result<WorktreeLoss, AppError> {
        let path = self
            .path_of(worktree)
            .ok_or_else(|| AppError::NotFound(worktree.to_string()))?;
        Ok(self.actions.worktree_loss(&path)?)
    }

    /// Tells the user how a worktree job ended.
    fn notice_result(&self, result: &Result<String, OpError>) {
        self.emit(AppEvent::Notice(match result {
            Ok(text) => text.clone(),
            Err(e) => e.to_string(),
        }));
    }

    /// Creates a worktree of the repo of row `repo` (spec §10).
    pub(super) fn add_worktree(
        &self,
        repo: RepoId,
        request: WorktreeAddRequest,
        init_submodules: bool,
        copy_files: bool,
        open: bool,
    ) -> Result<(), AppError> {
        let (main, path) = self.main_row(repo)?;
        let setup = WorktreeSetup {
            copy: if copy_files {
                self.copy_patterns(main)
            } else {
                Vec::new()
            },
            init_submodules,
            batch: None,
        };
        let me = self.me.clone();
        let label = format!(
            "New worktree {}",
            request
                .path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        );
        let _h = self.submit_repo_job(&path, OpClass::Write, label, move |a, p, ctx| {
            let result = a
                .add_worktree(p, &request, &setup, ctx, ctx.cancel())
                .map(|copied| match copied.len() {
                    0 => format!("Worktree created at {}", request.path.display()),
                    n => format!(
                        "Worktree created at {} ({n} local file(s) copied)",
                        request.path.display()
                    ),
                });
            if let Some(app) = me.upgrade() {
                app.reload_worktrees_now(p);
                if result.is_ok() {
                    app.update_ui_state(|ui| {
                        if !ui.expanded_worktrees.contains(&main) {
                            ui.expanded_worktrees.push(main);
                        }
                    });
                    // Its row, as Git spells its path.
                    let new = app
                        .worktrees_of(main)
                        .into_iter()
                        .find(|w| same_folder(w.path.as_path(), &request.path))
                        .map_or_else(|| RepoPath::new(request.path.clone()).id(), |w| w.path.id());
                    if open && let Err(e) = app.dispatch(Command::OpenRepoTab(new)) {
                        tracing::warn!(error = %e, "cannot open the new worktree");
                    }
                    app.start_watching();
                }
                app.notice_result(&result);
            }
            result.map(drop)
        });
        Ok(())
    }

    /// Removes linked worktree row `worktree` (spec §10): undoable with
    /// `Ctrl+Z` on its repo. Refused while locked.
    pub(super) fn remove_worktree(
        &self,
        worktree: RepoId,
        force: bool,
        delete_branch: bool,
    ) -> Result<(), AppError> {
        let (main, path) = self.main_row(worktree)?;
        let row = self
            .all_rows()
            .into_iter()
            .find(|r| r.id == worktree && r.parent() == Some(main))
            .ok_or_else(|| AppError::InvalidInput("the main worktree cannot be removed".into()))?;
        if row.worktree.as_ref().is_some_and(|w| w.locked.is_some()) {
            return Err(AppError::InvalidInput(
                "the worktree is locked: unlock it first".into(),
            ));
        }
        let wt = row.path.clone();
        let label = row.label();
        let repo_name = row.name.clone();
        let options = RemoveOptions {
            force,
            delete_branch,
            batch: None,
        };
        let me = self.me.clone();
        let _h = self.submit_repo_job(
            &path,
            OpClass::Write,
            format!("Remove worktree {label}"),
            move |a, p, _| {
                let result = a.remove_worktree(p, &wt, options).map(|()| {
                    format!("Removed worktree {label} (Ctrl+Z on {repo_name} brings it back)")
                });
                if let Some(app) = me.upgrade() {
                    app.reload_worktrees_now(p);
                    app.notice_result(&result);
                }
                result.map(drop)
            },
        );
        Ok(())
    }

    /// Locks (`Some(reason)`) or unlocks a linked worktree row.
    pub(super) fn lock_worktree(
        &self,
        worktree: RepoId,
        reason: Option<String>,
    ) -> Result<(), AppError> {
        let (main, path) = self.main_row(worktree)?;
        if main == worktree {
            return Err(AppError::InvalidInput(
                "the main worktree cannot be locked".into(),
            ));
        }
        let wt = self
            .path_of(worktree)
            .ok_or_else(|| AppError::NotFound(worktree.to_string()))?;
        let me = self.me.clone();
        let label = if reason.is_some() { "Lock" } else { "Unlock" };
        let _h = self.submit_repo_job(
            &path,
            OpClass::Write,
            format!("{label} worktree {}", wt.name()),
            move |a, p, _| {
                let result = a.lock_worktree(p, &wt, reason.as_deref());
                if let Some(app) = me.upgrade() {
                    app.reload_worktrees_now(p);
                }
                result
            },
        );
        Ok(())
    }

    /// Prunes the missing worktrees of `ids` and of the repos among them
    /// (all when empty, see [`App::resolve_targets`]).
    pub(super) fn prune_worktrees(&self, ids: &[RepoId]) {
        let mains: Vec<RepoPath> = {
            let guard = lock(&self.session);
            let Some(s) = guard.as_ref() else { return };
            let targets: Vec<RepoId> = s.target_rows(ids).into_iter().map(|r| r.id).collect();
            let mut mains: Vec<RepoId> = Vec::new();
            for row in s.rows.iter().filter(|r| !r.hidden) {
                let Some(w) = &row.worktree else { continue };
                let Some(parent) = w.parent else { continue };
                let wanted = targets.contains(&row.id) || targets.contains(&parent);
                if w.missing && w.locked.is_none() && wanted && !mains.contains(&parent) {
                    mains.push(parent);
                }
            }
            mains
                .iter()
                .filter_map(|id| s.row(*id).map(|r| r.path.clone()))
                .collect()
        };
        if mains.is_empty() {
            self.emit(AppEvent::Notice(
                "No missing worktree to prune (locked ones are kept)".into(),
            ));
            return;
        }
        for path in mains {
            let me = self.me.clone();
            let _h = self.submit_repo_job(
                &path,
                OpClass::Write,
                format!("Prune worktrees of {}", path.name()),
                move |a, p, _| {
                    let result = a.prune_worktrees(p);
                    if let Some(app) = me.upgrade() {
                        app.reload_worktrees_now(p);
                    }
                    result
                },
            );
        }
    }

    /// Switches what the dashboard shows of worktrees (the Set Selector).
    pub(super) fn set_worktree_view(&self, view: SetView) {
        self.update_ui_state(|ui| ui.set_view = view);
        self.start_watching();
    }
}
