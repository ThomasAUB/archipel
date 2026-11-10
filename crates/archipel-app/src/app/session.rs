//! The open project: its [`Session`] (definition, dashboard rows, UI state,
//! watchers), opening, closing and reloading it, and edits of its repo list.
//!
//! * Opening paints from cached summaries at once, then refreshes in the
//!   background (spec §2: < 200 ms).
//! * Every edit of the definition is saved to the project file right away
//!   ([`App::edit_definition`]).

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use archipel_model::ports::WatchHandle;
use archipel_model::project::{ProjectDef, ProjectLocation, RegistryEntry, RepoEntry};
use archipel_model::submodule::SubmoduleNode;
use archipel_model::ui_state::UiState;
use archipel_model::{CancelToken, ProjectId, RepoId, RepoPath, Timestamp};

use super::{App, AppError, AppEvent, lock};
use crate::dashboard::{DashboardRow, RepoStatus};
use crate::forge::{BranchCi, ForgeStatus};

/// An open project.
pub(super) struct Session {
    pub(super) id: ProjectId,
    pub(super) location: ProjectLocation,
    pub(super) def: ProjectDef,
    pub(super) rows: Vec<DashboardRow>,
    pub(super) ui: UiState,
    pub(super) watches: Vec<WatchHandle>,
    pub(super) pending: HashSet<RepoId>,
    /// Repos refreshed while their summary was being read: read again once
    /// it lands, as that read may predate the change (a file saved meanwhile).
    pub(super) reread: HashSet<RepoId>,
    pub(super) drawer_open: bool,
    /// Running folder scan (launch from a directory).
    pub(super) scan: Option<CancelToken>,
    /// Submodules read per repo, while the dependency tree is shown.
    pub(super) dependencies: HashMap<RepoId, Vec<SubmoduleNode>>,
    /// Repos whose dependencies are being read.
    pub(super) tree_pending: HashSet<RepoId>,
    /// Branch and time of the last forge status read, per repo.
    pub(super) forge_checked: HashMap<RepoId, (String, Instant)>,
    /// Repos whose last forge read failed in passing (rate limit, network,
    /// forge-side error): read again sooner than fresh ones.
    pub(super) forge_failed: HashSet<RepoId>,
    /// Repos whose forge status is being read.
    pub(super) forge_pending: HashSet<RepoId>,
    /// Repos refreshed while a read was pending: read again once it lands,
    /// as the pending read may predate what the refresh was asked for.
    pub(super) forge_reread: HashSet<RepoId>,
    /// Rate-limited forge hosts, and when they accept calls again.
    pub(super) forge_backoff: HashMap<String, Timestamp>,
    /// Repos whose CI is followed closely until then, though it may show
    /// nothing yet (a pull request was just opened: its runs take a moment
    /// to be registered).
    pub(super) ci_watch: HashMap<RepoId, Instant>,
    /// Repos with a CI follow-up read scheduled.
    pub(super) ci_poll: HashSet<RepoId>,
}

impl Session {
    /// The category of each repo of the definition, in project order.
    pub(super) fn repo_categories(&self) -> impl Iterator<Item = Option<&str>> + Clone {
        self.def.repos.iter().map(|r| r.category.as_deref())
    }
}

/// A dashboard row for `repo`, before any forge read; hidden when the UI
/// state says so.
fn new_row(repo: &RepoEntry, status: RepoStatus, stale: bool, ui: &UiState) -> DashboardRow {
    let id = repo.path.id();
    DashboardRow {
        id,
        name: repo.display_name(),
        path: repo.path.clone(),
        status,
        stale,
        hidden: ui.is_hidden(id),
        forge: ForgeStatus::Unknown,
        ci: BranchCi::Unknown,
        forge_limit: None,
    }
}

impl App {
    /// Opens a project: paints from cached summaries immediately, then
    /// refreshes in the background and starts watching (spec §2: < 200 ms).
    pub fn open_project(&self, id: &ProjectId) -> Result<(), AppError> {
        let started = Instant::now();
        let entry = self
            .services
            .registry
            .list_projects()?
            .into_iter()
            .find(|e| &e.id == id)
            .ok_or_else(|| AppError::NotFound(id.to_string()))?;
        let def = self.services.projects.load(&entry.location)?;
        // Both are caches: an unreadable one costs a refresh, never the opening.
        let cached = self
            .services
            .summaries
            .load_summaries(id)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "cached summaries unreadable; refreshing");
                Vec::new()
            });
        let ui: UiState = self
            .services
            .ui_state
            .load_ui_state(id)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "saved UI state unreadable; using defaults");
                None
            })
            .unwrap_or_default();
        let rows = def
            .repos
            .iter()
            .map(|repo| {
                let rid = repo.path.id();
                let status = if repo.path.as_path().exists() {
                    cached
                        .iter()
                        .find(|(r, _)| *r == rid)
                        .map_or(RepoStatus::Loading, |(_, s)| {
                            RepoStatus::Ready(Box::new(s.clone()))
                        })
                } else {
                    RepoStatus::Missing
                };
                let stale = status != RepoStatus::Missing;
                new_row(repo, status, stale, &ui)
            })
            .collect();
        self.close_project();
        *lock(&self.session) = Some(Session {
            id: id.clone(),
            location: entry.location,
            def,
            rows,
            ui,
            watches: Vec::new(),
            pending: HashSet::new(),
            reread: HashSet::new(),
            drawer_open: false,
            scan: None,
            dependencies: HashMap::new(),
            tree_pending: HashSet::new(),
            forge_checked: HashMap::new(),
            forge_failed: HashSet::new(),
            forge_pending: HashSet::new(),
            forge_reread: HashSet::new(),
            forge_backoff: HashMap::new(),
            ci_watch: HashMap::new(),
            ci_poll: HashSet::new(),
        });
        if let Err(e) = self.services.registry.mark_opened(id, Timestamp::now()) {
            tracing::warn!(error = %e, "could not update the recent list");
        }
        tracing::info!(project = %id, ms = started.elapsed().as_millis(), "project painted from cache");
        self.emit(AppEvent::ProjectOpened(id.clone()));
        self.start_watching();
        self.refresh_all();
        self.load_tree_if_shown();
        Ok(())
    }

    /// Unloads the current project: saves UI state and summaries, stops
    /// watchers, drops rows (only the persisted summary stays).
    pub fn close_project(&self) {
        let Some(session) = lock(&self.session).take() else {
            return;
        };
        if let Some(scan) = &session.scan {
            scan.cancel();
        }
        self.save_ui_state(&session.id, &session.ui);
        self.persist_rows(&session.id, &session.rows);
        drop(session.watches);
    }

    /// Saves the UI state of project `id`; a failure is only logged.
    pub(super) fn save_ui_state(&self, id: &ProjectId, ui: &UiState) {
        if let Err(e) = self.services.ui_state.save_ui_state(id, ui) {
            tracing::warn!(error = %e, "could not save UI state");
        }
    }

    /// Caches the summaries of `rows` for the next opening of project `id`.
    pub(super) fn persist_rows(&self, id: &ProjectId, rows: &[DashboardRow]) {
        let summaries: Vec<_> = rows
            .iter()
            .filter_map(|r| r.summary().map(|s| (r.id, s.clone())))
            .collect();
        if let Err(e) = self.services.summaries.save_summaries(id, &summaries) {
            tracing::warn!(error = %e, "could not cache summaries");
        }
    }

    /// The open project's definition.
    #[must_use]
    pub fn project(&self) -> Option<(ProjectId, ProjectDef)> {
        lock(&self.session)
            .as_ref()
            .map(|s| (s.id.clone(), s.def.clone()))
    }

    /// Where the open project is stored.
    #[must_use]
    pub fn project_location(&self) -> Option<ProjectLocation> {
        lock(&self.session).as_ref().map(|s| s.location.clone())
    }

    /// Edits the open project with `f`, then saves its definition where it
    /// is stored. `f` returns `None` when it changed nothing: the file is
    /// left alone and `Ok(None)` returned.
    pub(super) fn edit_definition<R>(
        &self,
        f: impl FnOnce(&mut Session) -> Option<R>,
    ) -> Result<Option<R>, AppError> {
        let (location, def, result) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            let Some(result) = f(s) else {
                return Ok(None);
            };
            (s.location.clone(), s.def.clone(), result)
        };
        self.services.projects.save(&location, &def)?;
        Ok(Some(result))
    }

    /// Adds repositories to the open project and saves the file.
    pub fn add_repos(&self, paths: &[RepoPath]) -> Result<(), AppError> {
        self.edit_definition(|s| {
            for p in paths {
                if s.def.repos.iter().all(|r| r.path != *p) {
                    let entry = RepoEntry {
                        path: p.clone(),
                        alias: None,
                        category: None,
                    };
                    s.rows
                        .push(new_row(&entry, RepoStatus::Loading, true, &s.ui));
                    s.def.repos.push(entry);
                }
            }
            Some(())
        })?;
        self.start_watching();
        for p in paths {
            self.refresh_repo(p.id());
            self.load_tree(p.id());
        }
        self.emit(AppEvent::RowsChanged(
            paths.iter().map(RepoPath::id).collect(),
        ));
        Ok(())
    }

    /// Removes a repository from the open project (files are untouched).
    pub fn remove_repo(&self, repo: RepoId) -> Result<(), AppError> {
        self.edit_definition(|s| {
            s.def.repos.retain(|r| r.path.id() != repo);
            s.rows.retain(|r| r.id != repo);
            s.ui.hidden_repos.retain(|r| *r != repo);
            s.ui.selection.retain(|r| *r != repo);
            s.dependencies.remove(&repo);
            s.forge_checked.remove(&repo);
            s.forge_failed.remove(&repo);
            s.ci_watch.remove(&repo);
            Some(())
        })?;
        self.start_watching();
        self.emit(AppEvent::RowsChanged(vec![repo]));
        Ok(())
    }

    /// Reloads the open project's definition from where it is stored (the
    /// project file, edited by hand or by a teammate): name, color, icon and
    /// repo list. Repos that stay keep their status; new ones are added,
    /// removed ones dropped. The UI state is kept.
    pub(super) fn reload_project(&self) -> Result<(), AppError> {
        let Some(location) = lock(&self.session).as_ref().map(|s| s.location.clone()) else {
            return Err(AppError::NoProject);
        };
        let def = self.services.projects.load(&location)?;
        let (id, name) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            let mut old = std::mem::take(&mut s.rows);
            s.rows = def
                .repos
                .iter()
                .map(|repo| {
                    let rid = repo.path.id();
                    if let Some(i) = old.iter().position(|r| r.id == rid) {
                        return DashboardRow {
                            name: repo.display_name(),
                            ..old.swap_remove(i)
                        };
                    }
                    let status = if repo.path.as_path().exists() {
                        RepoStatus::Loading
                    } else {
                        RepoStatus::Missing
                    };
                    new_row(repo, status, true, &s.ui)
                })
                .collect();
            let kept: HashSet<RepoId> = s.rows.iter().map(|r| r.id).collect();
            s.ui.selection.retain(|r| kept.contains(r));
            s.dependencies.retain(|r, _| kept.contains(r));
            s.def = def;
            (s.id.clone(), s.def.name.clone())
        };
        let entry = self
            .services
            .registry
            .list_projects()?
            .into_iter()
            .find(|e| e.id == id && e.name != name);
        if let Some(entry) = entry {
            self.services
                .registry
                .upsert_project(&RegistryEntry { name, ..entry })?;
            self.emit(AppEvent::ProjectsChanged);
        }
        self.start_watching();
        self.emit(AppEvent::UiStateChanged);
        Ok(())
    }
}
