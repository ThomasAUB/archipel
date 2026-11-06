//! The application: project sessions, dashboard state, commands.
//!
//! Threading contract: every public method returns quickly (it only touches
//! in-memory state and small store reads); Git work is submitted to the
//! scheduler and its results arrive as [`AppEvent`]s.
//!
//! This file holds the [`App`] core (construction, events, services); the
//! use cases live in the child modules, each an `impl App` block.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use archipel_model::ports::{
    ChangeSource, ProjectRegistry, ProjectSource, RepoDiscovery, SummaryStore, UiStateStore,
};
use archipel_model::settings::Settings;
use archipel_model::{ProjectError, ProjectId, RepoId, RepoPath, StoreError};
use archipel_ops::{
    BatchOps, GitPorts, Journal, MultiCommit, OpClass, OpContext, OpError, OpEvent, OpHandle,
    OpSpec, RepoActions, Scheduler, UndoService,
};
use tokio::sync::broadcast;

use crate::actions::ActionRegistry;
use crate::dashboard::{ColumnRegistry, FilterRegistry};
use crate::deptree::{DependencySource, SubmoduleSource};
use crate::forge::{ForgePorts, ForgeRegistry};
use crate::keymap::Keymap;

mod auto_fetch;
mod categories;
mod dispatch;
mod forge_flow;
mod identity;
mod projects;
mod refresh;
mod scan;
mod session;
mod tabs;
mod tree;
mod undo;
mod update_check;
mod view;

pub use forge_flow::{
    CloneOutcome, CloneProtocol, PullRequestOutcome, PullRequestPlan, PullRequestTarget,
};
pub use identity::{ProjectIcon, ProjectIdentity, initials};
pub use projects::{DirectoryTarget, file_project_id};
use session::Session;

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
    /// A value typed by the user is not valid.
    #[error("{0}")]
    InvalidInput(String),
    /// A forge (GitHub, GitLab…) call failed.
    #[error(transparent)]
    Forge(#[from] archipel_model::ForgeError),
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
    /// Folder scan of a launch from a directory: repos found so far, and
    /// whether it is still running.
    ScanProgress {
        /// Repositories found (new or already in the project).
        found: usize,
        /// The scan is still running.
        running: bool,
    },
    /// The dependency tree data changed (spec §9).
    TreeChanged,
    /// Forge accounts changed (sign in, sign out).
    ForgeAccountsChanged,
    /// The update check found a release newer than the running version.
    UpdateAvailable(archipel_model::forge::Release),
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
    /// Commit and working copy diffs.
    pub diffs: Arc<dyn archipel_model::ports::DiffReader>,
    /// Per-commit churn for statistics.
    pub churn: Arc<dyn archipel_model::ports::ChurnReader>,
    /// Statistics storage.
    pub stats: Arc<dyn archipel_model::ports::StatsStore>,
    /// Named in-memory caches (memory report).
    pub caches: Arc<archipel_cache::CacheRegistry>,
    /// Folder of on-disk caches (graph topology files).
    pub cache_dir: std::path::PathBuf,
    /// Forge integration (spec §8).
    pub forge: ForgePorts,
    /// Where Archipel's releases are published; `None` disables the update
    /// check (builds updated by a package manager, tests).
    pub updates: Option<Arc<dyn archipel_model::ports::ReleaseFeed>>,
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
    /// Where the repo dependency tree comes from (spec §9).
    pub dependencies: Arc<dyn DependencySource>,
}

impl Default for Extensions {
    fn default() -> Self {
        Self {
            columns: ColumnRegistry::with_builtins(),
            filters: FilterRegistry::with_builtins(),
            actions: ActionRegistry::with_builtins(),
            forges: Arc::new(ForgeRegistry::with_builtins()),
            keymap: Keymap::defaults(),
            dependencies: Arc::new(SubmoduleSource),
        }
    }
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
    /// Background fetch plan (per window, like the session).
    auto_fetch: Mutex<auto_fetch::AutoFetch>,
    /// Update check (shared by the sibling cores).
    updates: Arc<update_check::UpdateCheck>,
}

/// Event channel capacity; slow subscribers miss old events and re-read state.
const EVENT_CAPACITY: usize = 4096;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Removes `item` from `list` when there, else appends it.
fn toggle<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if let Some(i) = list.iter().position(|x| *x == item) {
        list.remove(i);
    } else {
        list.push(item);
    }
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
        let settings_auto_fetch = settings.auto_fetch_secs;
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
            auto_fetch: Mutex::new(auto_fetch::AutoFetch::new(Duration::from_secs(
                settings_auto_fetch,
            ))),
            updates: update_check::UpdateCheck::new(),
        });
        app.forward_op_events();
        app.forward_updates();
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

    /// Another application instance sharing everything (ports, scheduler,
    /// caches, settings, extensions) except the open project: one per window
    /// (spec §2 "Several instances side by side"). An extra window costs only
    /// its project's data.
    #[must_use]
    pub fn sibling(&self) -> Arc<Self> {
        let events = broadcast::channel(EVENT_CAPACITY).0;
        let settings_auto_fetch = self.settings.auto_fetch_secs;
        let app = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            services: self.services.clone(),
            settings: self.settings.clone(),
            ext: self.ext.clone(),
            scheduler: self.scheduler.clone(),
            actions: self.actions.clone(),
            batches: self.batches.clone(),
            commit: self.commit.clone(),
            undo: self.undo.clone(),
            session: Mutex::new(None),
            events,
            auto_fetch: Mutex::new(auto_fetch::AutoFetch::new(Duration::from_secs(
                settings_auto_fetch,
            ))),
            updates: self.updates.clone(),
        });
        app.forward_op_events();
        app.forward_updates();
        app
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

    /// Runs `job` on one repository through the scheduler: it shows in the
    /// operations drawer, waits for the repo's other writes, and network
    /// jobs share the network pool. The context reports progress and
    /// carries the cancellation token.
    pub fn submit_repo_job<T, F>(
        &self,
        repo: &RepoPath,
        class: OpClass,
        label: impl Into<String>,
        job: F,
    ) -> OpHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&RepoActions, &RepoPath, &OpContext) -> Result<T, OpError> + Send + 'static,
    {
        let actions = self.actions.clone();
        let path = repo.clone();
        let spec = OpSpec::new(Some(repo.id()), class, label);
        self.scheduler
            .submit(spec, move |ctx| job(&actions, &path, ctx))
    }

    /// The multi-repo commit use case.
    #[must_use]
    pub fn multi_commit(&self) -> &Arc<MultiCommit> {
        &self.commit
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

    /// History reads (run them off the UI thread).
    #[must_use]
    pub fn history_reader(&self) -> Arc<dyn archipel_model::ports::HistoryReader> {
        self.services.git.history.clone()
    }

    /// Diff reads (run them off the UI thread).
    #[must_use]
    pub fn diff_reader(&self) -> Arc<dyn archipel_model::ports::DiffReader> {
        self.services.diffs.clone()
    }

    /// Memory used by every named cache (diagnostics).
    #[must_use]
    pub fn cache_report(&self) -> Vec<archipel_cache::CacheReport> {
        self.services.caches.report()
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.close_project();
    }
}
