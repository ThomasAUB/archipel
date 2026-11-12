//! The command bus: [`App::dispatch`] runs a [`Command`], in place for UI
//! state changes, on the scheduler for Git work (batches over repos).

use std::sync::Arc;

use archipel_model::requests::{FetchRequest, SubmoduleUpdate, TagRequest};
use archipel_model::ui_state::SortDirection;
use archipel_model::{RepoId, RepoPath};
use archipel_ops::batch::BatchTarget;
use archipel_ops::{BatchOps, BatchReport, Direction};

use super::tabs::tab_command;
use super::{App, AppError, AppEvent, lock, toggle};
use crate::actions::Command;

impl App {
    /// Executes a command. Long work runs in the background.
    ///
    /// UI state a command changes (filters, sort, tabs, …) is saved right
    /// away, so it survives the app being killed rather than closed.
    pub fn dispatch(&self, command: Command) -> Result<(), AppError> {
        let before = lock(&self.session)
            .as_ref()
            .map(|s| (s.id.clone(), s.ui.clone()));
        let result = self.run_command(command);
        let changed = lock(&self.session).as_ref().and_then(|s| {
            before
                .is_some_and(|(id, ui)| id == s.id && ui != s.ui)
                .then(|| (s.id.clone(), s.ui.clone()))
        });
        // Saved after releasing the session lock: views read it every frame.
        if let Some((id, ui)) = changed {
            self.save_ui_state(&id, &ui);
        }
        result
    }

    /// Refresh, batch, undo, dashboard and tab commands.
    fn run_command(&self, command: Command) -> Result<(), AppError> {
        match command {
            Command::RefreshAll => {
                // A project file broken by hand must not stop the repos
                // already listed from refreshing.
                if let Err(e) = self.reload_project() {
                    self.emit(AppEvent::Notice(format!(
                        "cannot reload the project file: {e}"
                    )));
                }
                self.refresh_all();
                self.load_tree_if_shown();
            }
            Command::RefreshForge => self.refresh_forge_all(),
            Command::RerunFailedCi(ids) => self.rerun_failed_ci(&ids),
            Command::RefreshRepo(r) => self.refresh_repo(r),
            Command::Fetch(ids) => self.fetch(&ids, false),
            Command::Pull(ids) => {
                let mode = self.settings.pull_mode;
                self.spawn_batch("Pull", &ids, move |b, t| async move {
                    b.pull_all(&t, mode).await
                });
            }
            Command::CheckoutAll(branch, ids) => {
                self.spawn_batch("Checkout", &ids, move |b, t| async move {
                    b.checkout_all(&t, &branch).await
                });
            }
            Command::CreateBranchAll(name, ids) => {
                self.spawn_batch("Create branch", &ids, move |b, t| async move {
                    b.create_branch_all(&t, &name, None).await
                });
            }
            Command::CreateTagAll(name, ids) => {
                let request = TagRequest {
                    name,
                    target: None,
                    message: None,
                };
                self.spawn_batch("Create tag", &ids, move |b, t| async move {
                    b.create_tag_all(&t, request).await
                });
            }
            Command::Undo(repo) => self.spawn_undo(repo, Direction::Undo)?,
            Command::Redo(repo) => self.spawn_undo(repo, Direction::Redo)?,
            Command::ToggleFilter(id) => self.update_ui_state(|ui| toggle(&mut ui.filters, id)),
            Command::SortBy(column) => self.update_ui_state(|ui| {
                if ui.sort_column.as_deref() == Some(column.as_str()) {
                    ui.sort_direction = match ui.sort_direction {
                        SortDirection::Ascending => SortDirection::Descending,
                        SortDirection::Descending => SortDirection::Ascending,
                    };
                } else {
                    ui.sort_column = Some(column);
                    ui.sort_direction = SortDirection::Ascending;
                }
            }),
            Command::Select(repo) => self.update_ui_state(|ui| {
                ui.selected_repo = repo;
                ui.selection.clear();
            }),
            command @ (Command::OpenRepoTab(_)
            | Command::CloseTab(_)
            | Command::CloseOtherTabs(_)
            | Command::CloseTabsToRight(_)
            | Command::CloseAllTabs
            | Command::CycleTab(_)
            | Command::MoveTab { .. }) => self.update_ui_state(|ui| tab_command(ui, &command)),
            Command::ToggleDrawer => self.toggle_drawer(),
            Command::Ui(_) => {}
            Command::Custom(custom) => (custom.run)(self)?,
            other => self.dispatch_project(other)?,
        }
        Ok(())
    }

    /// Selection, hidden repos, scan, dependency tree and identity commands.
    fn dispatch_project(&self, command: Command) -> Result<(), AppError> {
        match command {
            Command::ToggleSelect(repo) => self.update_ui_state(|ui| {
                if ui.selected_repo == Some(repo) {
                    ui.selected_repo = ui.selection.pop();
                } else if let Some(i) = ui.selection.iter().position(|r| *r == repo) {
                    ui.selection.remove(i);
                } else if ui.selected_repo.is_none() {
                    ui.selected_repo = Some(repo);
                } else {
                    ui.selection.push(repo);
                }
            }),
            Command::HideRepos(ids) => self.set_hidden(&ids, true),
            Command::UnhideRepos(ids) => {
                let ids = if ids.is_empty() {
                    self.ui_state().map(|u| u.hidden_repos).unwrap_or_default()
                } else {
                    ids
                };
                self.set_hidden(&ids, false);
            }
            Command::ToggleHidden(ids) => {
                let all_hidden = self
                    .ui_state()
                    .is_some_and(|u| !ids.is_empty() && ids.iter().all(|r| u.is_hidden(*r)));
                self.set_hidden(&ids, !all_hidden);
            }
            Command::ToggleShowHidden => {
                self.update_ui_state(|ui| ui.show_hidden = !ui.show_hidden);
            }
            Command::SetCategory(ids, category) => self.set_category(&ids, category.as_deref())?,
            Command::ToggleCategory(key) => self.toggle_category(key),
            Command::ToggleCategoryFilter(key) => self.toggle_category_filter(key),
            Command::RenameCategory(old, new) => self.rename_category(&old, new.as_deref())?,
            Command::CancelScan => self.cancel_scan(),
            Command::AddRepos(paths) => self.add_repos(&paths)?,
            Command::ToggleDependencyTree => {
                self.update_ui_state(|ui| ui.show_dependency_tree = !ui.show_dependency_tree);
                self.load_tree_if_shown();
            }
            Command::ToggleTreeNode(key) => {
                self.update_ui_state(|ui| toggle(&mut ui.collapsed_nodes, key));
            }
            Command::UpdateSubmodules {
                parent,
                paths,
                init,
                recursive,
            } => self.update_submodules(
                parent,
                SubmoduleUpdate {
                    paths,
                    init,
                    recursive,
                },
            ),
            Command::OpenSubmodule(path) => {
                let id = path.id();
                if !self.all_rows().iter().any(|r| r.id == id) {
                    self.add_repos(std::slice::from_ref(&path))?;
                }
                self.set_hidden(&[id], false);
                self.dispatch(Command::OpenRepoTab(id))?;
            }
            Command::SetAccent(accent) => self.set_accent(accent)?,
            Command::SetBackgroundTint(tint) => self.set_background_tint(tint)?,
            Command::SetIcon(icon) => self.set_icon(icon)?,
            // Every other command is handled by `dispatch`.
            _ => {}
        }
        Ok(())
    }

    /// The shown repos among `ids` (all shown repos when empty) that have a
    /// summary, as batch targets.
    fn targets(&self, ids: &[RepoId]) -> Vec<BatchTarget> {
        lock(&self.session)
            .as_ref()
            .map(|s| {
                s.rows
                    .iter()
                    .filter(|r| !r.hidden)
                    .filter(|r| ids.is_empty() || ids.contains(&r.id))
                    .filter(|r| r.summary().is_some())
                    .map(|r| BatchTarget {
                        id: r.id,
                        path: r.path.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Path of a repo of the open project.
    pub(super) fn path_of(&self, repo: RepoId) -> Option<RepoPath> {
        lock(&self.session)
            .as_ref()
            .and_then(|s| s.rows.iter().find(|r| r.id == repo).map(|r| r.path.clone()))
    }

    /// Tells the UI a batch finished and refreshes the repos it touched.
    fn report<T>(&self, label: &str, report: &BatchReport<T>) {
        let (d, f, s) = report.counts();
        self.emit(AppEvent::BatchFinished(label.to_owned(), d, f, s));
        for (repo, _) in &report.per_repo {
            self.refresh_repo(*repo);
        }
    }

    /// Fetches every remote of `ids`; `prune` also drops the remote-tracking
    /// branches deleted on their remote.
    pub(super) fn fetch(&self, ids: &[RepoId], prune: bool) {
        self.auto_fetch_reset(ids);
        let request = FetchRequest {
            remote: None,
            prune,
        };
        self.spawn_batch("Fetch", ids, move |b, t| async move {
            b.fetch_all(&t, request).await
        });
    }

    /// Runs a batch over the targets of `ids` on the app runtime, then
    /// reports it as `label`.
    fn spawn_batch<T, F, Fut>(&self, label: &'static str, ids: &[RepoId], run: F)
    where
        T: Send + 'static,
        F: FnOnce(Arc<BatchOps>, Vec<BatchTarget>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = BatchReport<T>> + Send + 'static,
    {
        let targets = self.targets(ids);
        let batches = self.batches.clone();
        let me = self.me.clone();
        self.scheduler.runtime().spawn(async move {
            let report = run(batches, targets).await;
            if let Some(app) = me.upgrade() {
                app.report(label, &report);
            }
        });
    }
}
