//! The application: project sessions, dashboard state, commands.
//!
//! Threading contract: every public method returns quickly (it only touches
//! in-memory state and small store reads); Git work is submitted to the
//! scheduler and its results arrive as [`AppEvent`]s.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Instant;

use archipel_model::journal::JournalEntry;
use archipel_model::ports::{
    ChangeSource, Coverage, Invalidation, ProjectRegistry, ProjectSource, RepoDiscovery,
    SummaryStore, UiStateStore, WatchHandle,
};
use archipel_model::project::{ProjectDef, ProjectLocation, RegistryEntry, RepoEntry};
use archipel_model::requests::{FetchRequest, TagRequest};
use archipel_model::settings::Settings;
use archipel_model::ui_state::{SortDirection, TabState, UiState};
use archipel_model::{ProjectError, ProjectId, RepoId, RepoPath, StoreError, Timestamp};
use archipel_ops::batch::BatchTarget;
use archipel_ops::{
    BatchOps, BatchReport, Direction, GitPorts, Journal, MultiCommit, OpClass, OpError, OpEvent,
    OpSpec, RepoActions, Scheduler, UndoService,
};
use tokio::sync::broadcast;

use crate::actions::{ActionContext, ActionRegistry, Command};
use crate::dashboard::{ColumnRegistry, DashboardRow, FilterRegistry, RepoStatus, visible_rows};
use crate::forge::ForgeRegistry;
use crate::keymap::Keymap;

/// Errors surfaced to the UI.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    /// Project file or registry problem.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// Store problem.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// An operation failed.
    #[error(transparent)]
    Op(#[from] OpError),
    /// The command needs an open project.
    #[error("no project is open")]
    NoProject,
    /// Unknown project or repo.
    #[error("not found: {0}")]
    NotFound(String),
}

/// Something the UI should react to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// A project was opened (rows painted from cache).
    ProjectOpened(ProjectId),
    /// Rows changed (refreshed summaries).
    RowsChanged(Vec<RepoId>),
    /// Tabs, filters, sort or selection changed.
    UiStateChanged,
    /// The watcher could not cover every worktree: refresh on focus.
    DegradedWatching(Vec<RepoId>),
    /// A batch finished: (label, done, failed, skipped).
    BatchFinished(String, usize, usize, usize),
    /// Undo / redo result message.
    Notice(String),
    /// An operation event (operations drawer).
    Op(OpEvent),
    /// The registry changed (project created, removed, renamed).
    ProjectsChanged,
}

/// Ports injected by the composition root.
#[derive(Clone)]
pub struct Services {
    /// Git ports used by actions.
    pub git: GitPorts,
    /// Project files.
    pub projects: Arc<dyn ProjectSource>,
    /// Folder scan.
    pub discovery: Arc<dyn RepoDiscovery>,
    /// Known projects.
    pub registry: Arc<dyn ProjectRegistry>,
    /// Cached summaries.
    pub summaries: Arc<dyn SummaryStore>,
    /// Per-project UI state.
    pub ui_state: Arc<dyn UiStateStore>,
    /// Journal storage.
    pub journal: Arc<dyn archipel_model::ports::JournalStore>,
    /// File watching.
    pub watcher: Arc<dyn ChangeSource>,
    /// Per-commit churn for statistics.
    pub churn: Arc<dyn archipel_model::ports::ChurnReader>,
    /// Statistics storage.
    pub stats: Arc<dyn archipel_model::ports::StatsStore>,
    /// Named in-memory caches (memory report).
    pub caches: Arc<archipel_cache::CacheRegistry>,
    /// Folder of on-disk caches (graph topology files).
    pub cache_dir: std::path::PathBuf,
}

/// Registries a fork may extend before building the app.
#[derive(Clone)]
pub struct Extensions {
    /// Dashboard columns.
    pub columns: ColumnRegistry,
    /// Dashboard filters.
    pub filters: FilterRegistry,
    /// Palette actions.
    pub actions: ActionRegistry,
    /// Forges for "open in browser".
    pub forges: Arc<ForgeRegistry>,
    /// Keymap.
    pub keymap: Keymap,
}

impl Default for Extensions {
    fn default() -> Self {
        Self {
            columns: ColumnRegistry::with_builtins(),
            filters: FilterRegistry::with_builtins(),
            actions: ActionRegistry::with_builtins(),
            forges: Arc::new(ForgeRegistry::with_builtins()),
            keymap: Keymap::defaults(),
        }
    }
}

/// An open project.
struct Session {
    id: ProjectId,
    location: ProjectLocation,
    def: ProjectDef,
    rows: Vec<DashboardRow>,
    ui: UiState,
    watches: Vec<WatchHandle>,
    pending: HashSet<RepoId>,
    drawer_open: bool,
}

/// The application core.
pub struct App {
    me: Weak<App>,
    services: Services,
    settings: Settings,
    ext: Extensions,
    scheduler: Arc<Scheduler>,
    actions: RepoActions,
    batches: Arc<BatchOps>,
    commit: Arc<MultiCommit>,
    undo: UndoService,
    session: Mutex<Option<Session>>,
    events: broadcast::Sender<AppEvent>,
}

/// Event channel capacity; slow subscribers miss old events and re-read state.
const EVENT_CAPACITY: usize = 4096;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl App {
    /// Builds the application on `runtime`.
    #[must_use]
    pub fn new(
        services: Services,
        settings: Settings,
        ext: Extensions,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let scheduler = Arc::new(Scheduler::new(runtime, &settings));
        let journal = Journal::new(
            services.journal.clone(),
            services.git.refs.clone(),
            services.git.stash_writer.clone(),
            settings.journal_limit,
        );
        let actions = RepoActions::new(services.git.clone(), journal.clone());
        let batches = Arc::new(BatchOps::new(scheduler.clone(), actions.clone()));
        let commit = Arc::new(MultiCommit::new(
            scheduler.clone(),
            actions.clone(),
            batches.clone(),
        ));
        let undo = UndoService::new(services.git.clone(), journal);
        let events = broadcast::channel(EVENT_CAPACITY).0;
        let app = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            services,
            settings,
            ext,
            scheduler,
            actions,
            batches,
            commit,
            undo,
            session: Mutex::new(None),
            events,
        });
        app.forward_op_events();
        app
    }

    fn forward_op_events(&self) {
        let mut ops = self.scheduler.subscribe();
        let events = self.events.clone();
        self.scheduler.runtime().spawn(async move {
            loop {
                match ops.recv().await {
                    Ok(e) => {
                        let _ = events.send(AppEvent::Op(e));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn emit(&self, event: AppEvent) {
        // No subscriber yet is fine.
        let _ = self.events.send(event);
    }

    /// Subscribes to application events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    /// Settings in effect.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Extension registries.
    #[must_use]
    pub fn extensions(&self) -> &Extensions {
        &self.ext
    }

    /// The scheduler (for frontends that run their own jobs, e.g. diffs).
    #[must_use]
    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.scheduler
    }

    /// Single-repo actions.
    #[must_use]
    pub fn repo_actions(&self) -> &RepoActions {
        &self.actions
    }

    /// The multi-repo commit use case.
    #[must_use]
    pub fn multi_commit(&self) -> &Arc<MultiCommit> {
        &self.commit
    }

    /// The undo service (for compensations and plans).
    #[must_use]
    pub fn undo_service(&self) -> &UndoService {
        &self.undo
    }

    /// Batch operations.
    #[must_use]
    pub fn batches(&self) -> &Arc<BatchOps> {
        &self.batches
    }

    /// A new commit-graph view for a repo tab (open it on a worker thread).
    #[must_use]
    pub fn history_view(&self) -> archipel_graph::HistoryView {
        archipel_graph::HistoryView::new(
            self.services.git.history.clone(),
            self.services.git.refs.clone(),
            self.services.caches.clone(),
            self.services.cache_dir.join("graph"),
            archipel_graph::GraphOptions::from_settings(&self.settings),
        )
    }

    /// The incremental statistics engine (run it at low priority).
    #[must_use]
    pub fn stats_engine(&self) -> archipel_stats::StatsEngine {
        archipel_stats::StatsEngine::new(
            self.services.churn.clone(),
            self.services.stats.clone(),
            self.services.git.refs.clone(),
            self.services.git.history.clone(),
        )
    }

    /// Sources the statistics providers read from.
    #[must_use]
    pub fn stat_sources(&self) -> archipel_stats::StatSources {
        archipel_stats::StatSources {
            store: self.services.stats.clone(),
            refs: self.services.git.refs.clone(),
            history: self.services.git.history.clone(),
        }
    }

    /// Memory used by every named cache (diagnostics).
    #[must_use]
    pub fn cache_report(&self) -> Vec<archipel_cache::CacheReport> {
        self.services.caches.report()
    }

    // ----- projects -------------------------------------------------------

    /// Known projects, most recently opened first.
    pub fn projects(&self) -> Result<Vec<RegistryEntry>, AppError> {
        let mut list = self.services.registry.list_projects()?;
        list.sort_by(|a, b| {
            b.last_opened
                .cmp(&a.last_opened)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(list)
    }

    /// Creates a project and registers it. Returns its id.
    pub fn create_project(
        &self,
        location: ProjectLocation,
        def: &ProjectDef,
    ) -> Result<ProjectId, AppError> {
        self.services.projects.save(&location, def)?;
        let id = match &location {
            ProjectLocation::Private(id) => id.clone(),
            ProjectLocation::SharedFile(path) => ProjectId(format!("file:{}", path.display())),
        };
        self.services.registry.upsert_project(&RegistryEntry {
            id: id.clone(),
            name: def.name.clone(),
            location,
            last_opened: None,
        })?;
        self.emit(AppEvent::ProjectsChanged);
        Ok(id)
    }

    /// Registers an existing `.gitproject.toml` (e.g. from a teammate).
    pub fn import_project_file(&self, path: &Path) -> Result<ProjectId, AppError> {
        let location = ProjectLocation::SharedFile(path.to_path_buf());
        let def = self.services.projects.load(&location)?;
        let id = ProjectId(format!("file:{}", path.display()));
        self.services.registry.upsert_project(&RegistryEntry {
            id: id.clone(),
            name: def.name,
            location,
            last_opened: None,
        })?;
        self.emit(AppEvent::ProjectsChanged);
        Ok(id)
    }

    /// Removes a project from the registry (never deletes files).
    pub fn remove_project(&self, id: &ProjectId) -> Result<(), AppError> {
        if lock(&self.session).as_ref().is_some_and(|s| &s.id == id) {
            self.close_project();
        }
        self.services.registry.remove_project(id)?;
        self.emit(AppEvent::ProjectsChanged);
        Ok(())
    }

    /// Scans a folder for repositories (blocking; run off the UI thread or
    /// through the scheduler for big trees — the scan is fast, see PLAN Phase 1).
    pub fn scan(
        &self,
        root: &Path,
    ) -> Result<Vec<archipel_model::project::DiscoveredRepo>, AppError> {
        Ok(self.services.discovery.scan(
            root,
            &archipel_model::project::ScanOptions::default(),
            &archipel_model::CancelToken::new(),
        )?)
    }

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
        let cached = self.services.summaries.load_summaries(id)?;
        let ui = self
            .services
            .ui_state
            .load_ui_state(id)?
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
                        .map_or(RepoStatus::Loading, |(_, s)| RepoStatus::Ready(s.clone()))
                } else {
                    RepoStatus::Missing
                };
                let stale = status != RepoStatus::Missing;
                DashboardRow {
                    id: rid,
                    name: repo.display_name(),
                    path: repo.path.clone(),
                    stale,
                    status,
                }
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
            drawer_open: false,
        });
        if let Err(e) = self.services.registry.mark_opened(id, Timestamp::now()) {
            tracing::warn!(error = %e, "could not update the recent list");
        }
        tracing::info!(project = %id, ms = started.elapsed().as_millis(), "project painted from cache");
        self.emit(AppEvent::ProjectOpened(id.clone()));
        self.start_watching();
        self.refresh_all();
        Ok(())
    }

    /// Unloads the current project: saves UI state and summaries, stops
    /// watchers, drops rows (only the persisted summary stays).
    pub fn close_project(&self) {
        let Some(session) = lock(&self.session).take() else {
            return;
        };
        if let Err(e) = self
            .services
            .ui_state
            .save_ui_state(&session.id, &session.ui)
        {
            tracing::warn!(error = %e, "could not save UI state");
        }
        self.persist_rows(&session.id, &session.rows);
        drop(session.watches);
    }

    fn persist_rows(&self, id: &ProjectId, rows: &[DashboardRow]) {
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

    /// Adds repositories to the open project and saves the file.
    pub fn add_repos(&self, paths: &[RepoPath]) -> Result<(), AppError> {
        let (location, def) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            for p in paths {
                if s.def.repos.iter().all(|r| r.path != *p) {
                    s.def.repos.push(RepoEntry {
                        path: p.clone(),
                        alias: None,
                    });
                    s.rows.push(DashboardRow {
                        id: p.id(),
                        name: p.name(),
                        path: p.clone(),
                        status: RepoStatus::Loading,
                        stale: true,
                    });
                }
            }
            (s.location.clone(), s.def.clone())
        };
        self.services.projects.save(&location, &def)?;
        self.start_watching();
        for p in paths {
            self.refresh_repo(p.id());
        }
        self.emit(AppEvent::RowsChanged(
            paths.iter().map(RepoPath::id).collect(),
        ));
        Ok(())
    }

    /// Removes a repository from the open project (files are untouched).
    pub fn remove_repo(&self, repo: RepoId) -> Result<(), AppError> {
        let (location, def) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            s.def.repos.retain(|r| r.path.id() != repo);
            s.rows.retain(|r| r.id != repo);
            (s.location.clone(), s.def.clone())
        };
        self.services.projects.save(&location, &def)?;
        self.start_watching();
        self.emit(AppEvent::RowsChanged(vec![repo]));
        Ok(())
    }

    // ----- dashboard ------------------------------------------------------

    /// Every row of the open project, in project order.
    #[must_use]
    pub fn rows(&self) -> Vec<DashboardRow> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default()
    }

    /// Rows after filters and sorting from the UI state.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<DashboardRow> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        let sort =
            s.ui.sort_column
                .as_deref()
                .and_then(|id| self.ext.columns.get(id))
                .map(|c| (c.as_ref(), s.ui.sort_direction));
        visible_rows(&s.rows, &self.ext.filters, &s.ui.filters, sort)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The UI state of the open project.
    #[must_use]
    pub fn ui_state(&self) -> Option<UiState> {
        lock(&self.session).as_ref().map(|s| s.ui.clone())
    }

    /// Updates the UI state (panel sizes, …) with `f`.
    pub fn update_ui_state(&self, f: impl FnOnce(&mut UiState)) {
        if let Some(s) = lock(&self.session).as_mut() {
            f(&mut s.ui);
        }
        self.emit(AppEvent::UiStateChanged);
    }

    /// Whether the operations drawer is open.
    #[must_use]
    pub fn drawer_open(&self) -> bool {
        lock(&self.session).as_ref().is_some_and(|s| s.drawer_open)
    }

    /// Context for action availability.
    #[must_use]
    pub fn action_context(&self) -> ActionContext {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return ActionContext::default();
        };
        let active_repo = match s.ui.tabs.get(s.ui.active_tab) {
            Some(TabState::Repo(r)) => Some(*r),
            _ => None,
        };
        ActionContext {
            project_open: true,
            selected: s.ui.selected_repo.into_iter().collect(),
            active_repo,
        }
    }

    // ----- refresh --------------------------------------------------------

    /// Recomputes every summary in parallel, then caches them.
    pub fn refresh_all(&self) {
        let ids: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .map(|s| s.rows.iter().map(|r| r.id).collect())
            .unwrap_or_default();
        for id in ids {
            self.refresh_repo(id);
        }
    }

    /// Recomputes one summary (coalesced: at most one pending refresh per repo).
    pub fn refresh_repo(&self, repo: RepoId) {
        let path = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            if !s.rows.iter().any(|r| r.id == repo) || !s.pending.insert(repo) {
                return;
            }
            // Stale while the refresh is queued (the UI can show a spinner).
            let Some(row) = s.rows.iter_mut().find(|r| r.id == repo) else {
                return;
            };
            row.stale = true;
            row.path.clone()
        };
        let status = self.services.git.status.clone();
        let me = self.me.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Read,
            format!("Refresh {}", path.name()),
        );
        let _handle = self.scheduler.submit(spec, move |_| {
            let result = status.summary(&path);
            if let Some(app) = me.upgrade() {
                app.apply_summary(repo, &path, result);
            }
            Ok(())
        });
    }

    fn apply_summary(
        &self,
        repo: RepoId,
        path: &RepoPath,
        result: Result<archipel_model::RepoSummary, archipel_model::GitError>,
    ) {
        let finished_all = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            s.pending.remove(&repo);
            if let Some(row) = s.rows.iter_mut().find(|r| r.id == repo) {
                row.stale = false;
                row.status = match result {
                    _ if !path.as_path().exists() => RepoStatus::Missing,
                    Ok(summary) => RepoStatus::Ready(summary),
                    Err(archipel_model::GitError::NotARepository { .. }) => RepoStatus::Missing,
                    Err(e) => RepoStatus::Error(e.to_string()),
                };
            }
            s.pending.is_empty().then(|| (s.id.clone(), s.rows.clone()))
        };
        self.emit(AppEvent::RowsChanged(vec![repo]));
        if let Some((id, rows)) = finished_all {
            self.persist_rows(&id, &rows);
        }
    }

    fn start_watching(&self) {
        let targets: Vec<(RepoId, RepoPath)> = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            s.watches.clear();
            s.rows
                .iter()
                .filter(|r| r.path.as_path().exists())
                .map(|r| (r.id, r.path.clone()))
                .collect()
        };
        let me = self.me.clone();
        let sink: Arc<dyn archipel_model::ports::InvalidationSink> =
            Arc::new(move |inv: Invalidation| {
                if let Some(app) = me.upgrade() {
                    app.refresh_repo(inv.repo);
                }
            });
        let mut handles = Vec::new();
        let mut degraded = Vec::new();
        for (id, path) in targets {
            match self.services.watcher.watch(id, &path, sink.clone()) {
                Ok(h) => {
                    if h.coverage == Coverage::GitDirOnly {
                        degraded.push(id);
                    }
                    handles.push(h);
                }
                Err(e) => {
                    tracing::warn!(error = %e, repo = %path.as_path().display(), "cannot watch repository");
                    degraded.push(id);
                }
            }
        }
        if let Some(s) = lock(&self.session).as_mut() {
            s.watches = handles;
        }
        if !degraded.is_empty() {
            self.emit(AppEvent::DegradedWatching(degraded));
        }
    }

    /// Called when the window regains focus: refreshes repos whose
    /// worktree is not watched (degraded mode), or everything when asked.
    pub fn on_focus(&self) {
        let degraded: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .map(|s| {
                let watched = s
                    .watches
                    .iter()
                    .filter(|w| w.coverage == Coverage::Full)
                    .count();
                if watched == s.rows.len() {
                    Vec::new()
                } else {
                    s.rows.iter().map(|r| r.id).collect()
                }
            })
            .unwrap_or_default();
        for id in degraded {
            self.refresh_repo(id);
        }
    }

    // ----- commands -------------------------------------------------------

    fn targets(&self, ids: &[RepoId]) -> Vec<BatchTarget> {
        lock(&self.session)
            .as_ref()
            .map(|s| {
                s.rows
                    .iter()
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

    fn path_of(&self, repo: RepoId) -> Option<RepoPath> {
        lock(&self.session)
            .as_ref()
            .and_then(|s| s.rows.iter().find(|r| r.id == repo).map(|r| r.path.clone()))
    }

    fn report<T>(&self, label: &str, report: &BatchReport<T>) {
        let (d, f, s) = report.counts();
        self.emit(AppEvent::BatchFinished(label.to_owned(), d, f, s));
        for (repo, _) in &report.per_repo {
            self.refresh_repo(*repo);
        }
    }

    /// Executes a command. Long work runs in the background.
    pub fn dispatch(&self, command: Command) -> Result<(), AppError> {
        match command {
            Command::RefreshAll => self.refresh_all(),
            Command::RefreshRepo(r) => self.refresh_repo(r),
            Command::Fetch(ids) => self.spawn_batch("Fetch", &ids, |b, t| async move {
                b.fetch_all(&t, FetchRequest::default()).await
            }),
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
            Command::ToggleFilter(id) => self.update_ui_state(|ui| {
                if let Some(i) = ui.filters.iter().position(|f| *f == id) {
                    ui.filters.remove(i);
                } else {
                    ui.filters.push(id);
                }
            }),
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
            Command::Select(repo) => self.update_ui_state(|ui| ui.selected_repo = repo),
            Command::OpenRepoTab(repo) => self.update_ui_state(|ui| {
                let tab = TabState::Repo(repo);
                ui.active_tab = ui.tabs.iter().position(|t| *t == tab).unwrap_or_else(|| {
                    ui.tabs.push(tab);
                    ui.tabs.len() - 1
                });
            }),
            Command::CloseTab(index) => self.update_ui_state(|ui| {
                if index > 0 && index < ui.tabs.len() {
                    ui.tabs.remove(index);
                    if ui.active_tab >= index {
                        ui.active_tab = ui.active_tab.saturating_sub(1);
                    }
                }
            }),
            Command::CycleTab(forward) => self.update_ui_state(|ui| {
                let n = ui.tabs.len().max(1);
                ui.active_tab = if forward {
                    (ui.active_tab + 1) % n
                } else {
                    (ui.active_tab + n - 1) % n
                };
            }),
            Command::ToggleDrawer => {
                if let Some(s) = lock(&self.session).as_mut() {
                    s.drawer_open = !s.drawer_open;
                }
                self.emit(AppEvent::UiStateChanged);
            }
            Command::Ui(_) => {}
        }
        Ok(())
    }

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

    fn spawn_undo(&self, repo: RepoId, direction: Direction) -> Result<(), AppError> {
        let path = self
            .path_of(repo)
            .ok_or_else(|| AppError::NotFound(repo.to_string()))?;
        let undo = self.undo.clone();
        let me = self.me.clone();
        let label = if direction == Direction::Undo {
            "Undo"
        } else {
            "Redo"
        };
        let spec = OpSpec::new(Some(repo), OpClass::Write, label);
        let _h = self.scheduler.submit(spec, move |_| {
            let entry: Option<JournalEntry> = match direction {
                Direction::Undo => undo.next_undo(repo, 200)?,
                Direction::Redo => undo.next_redo(repo, 200)?,
            };
            let message = match entry {
                None => format!("nothing to {}", label.to_lowercase()),
                Some(e) => match undo.execute(&path, &e, direction) {
                    Ok(note) => note.map_or_else(
                        || format!("{label}: {}", e.label),
                        |n| format!("{label}: {} ({n})", e.label),
                    ),
                    Err(err) => format!("{label} refused: {err}"),
                },
            };
            if let Some(app) = me.upgrade() {
                app.emit(AppEvent::Notice(message));
                app.refresh_repo(repo);
            }
            Ok(())
        });
        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.close_project();
    }
}
