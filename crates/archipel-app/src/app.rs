//! The application: project sessions, dashboard state, commands.
//!
//! Threading contract: every public method returns quickly (it only touches
//! in-memory state and small store reads); Git work is submitted to the
//! scheduler and its results arrive as [`AppEvent`]s.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use archipel_model::journal::JournalEntry;
use archipel_model::ports::{
    ChangeSource, Coverage, Invalidation, ProjectRegistry, ProjectSource, RepoDiscovery, Scope,
    SummaryStore, UiStateStore, WatchHandle,
};
use archipel_model::project::{
    ProjectDef, ProjectLocation, ProjectSettings, RegistryEntry, RepoEntry, ScanOptions,
};
use archipel_model::requests::{FetchRequest, SubmoduleUpdate, TagRequest};
use archipel_model::settings::Settings;
use archipel_model::submodule::SubmoduleNode;
use archipel_model::ui_state::{SortDirection, TabState, UiState};
use archipel_model::{
    CancelToken, ProjectError, ProjectId, RepoId, RepoPath, StoreError, Timestamp,
};
use archipel_ops::batch::BatchTarget;
use archipel_ops::{
    BatchOps, BatchReport, Direction, GitPorts, Journal, MultiCommit, OpClass, OpError, OpEvent,
    OpSpec, RepoActions, Scheduler, UndoService,
};
use tokio::sync::broadcast;

use crate::actions::{ActionContext, ActionRegistry, Command};
use crate::dashboard::{ColumnRegistry, DashboardRow, FilterRegistry, RepoStatus, visible_rows};
use crate::deptree::{DependencySource, SubmoduleSource, TreeInput, TreeRow, build_tree};
use crate::forge::{BranchCi, ForgePorts, ForgeRegistry, ForgeStatus};
use crate::groups::{
    RepoGroup, category_filter_keys, distinct_categories, group_by_category, normalize_category,
    passes_category_filter,
};

mod auto_fetch;
mod forge_flow;
mod update_check;

use crate::keymap::Keymap;
pub use forge_flow::{
    CloneOutcome, CloneProtocol, PullRequestOutcome, PullRequestPlan, PullRequestTarget,
};

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

/// What opening a directory leads to (spec §1 "Launch from a directory").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryTarget {
    /// The project that opens.
    pub id: ProjectId,
    /// The `archipel.toml` found in the directory, if any.
    pub project_file: Option<PathBuf>,
    /// The folder scanned for repositories, where the project file is
    /// written when there is none: the directory, or the outermost
    /// repository containing it.
    pub scan_root: PathBuf,
}

/// How a project's window is recognized (spec §2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIdentity {
    /// Project name.
    pub name: String,
    /// Accent color of the project's windows.
    pub accent: archipel_theme::Color,
    /// The accent is the automatic one (derived from the project path).
    pub automatic_accent: bool,
    /// Percentage of the accent tinting the window background (`None`:
    /// untinted).
    pub background_tint: Option<u8>,
    /// Icon, if any.
    pub icon: Option<ProjectIcon>,
    /// One or two letters for badges.
    pub initials: String,
}

impl ProjectIdentity {
    /// Window title: project name first so truncated titles stay readable.
    /// An image icon cannot go in a title; a text one leads it.
    #[must_use]
    pub fn window_title(&self) -> String {
        match self.icon.as_ref().and_then(ProjectIcon::text) {
            Some(icon) => format!("{icon} {} — Archipel", self.name),
            None => format!("{} — Archipel", self.name),
        }
    }
}

/// A project icon (spec §2), read from the `icon` setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectIcon {
    /// Text drawn as is: an emoji, a symbol.
    Text(String),
    /// An image file (absolute path).
    Image(PathBuf),
}

/// Extensions that make an `icon` value an image file.
const ICON_IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"];

impl ProjectIcon {
    /// Reads an `icon` setting: a value naming an image file (`logo.png`,
    /// `assets/icon.svg`) is a path, relative to the project's folder
    /// `base`; anything else is text. `None` when blank, or for a relative
    /// image path in a project without a folder.
    #[must_use]
    pub fn parse(value: &str, base: Option<&Path>) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        let path = Path::new(value);
        if !is_image_path(path) {
            return Some(Self::Text(value.to_owned()));
        }
        if path.is_absolute() {
            return Some(Self::Image(path.to_path_buf()));
        }
        base.map(|b| Self::Image(b.join(path)))
    }

    /// The text of a text icon.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            Self::Image(_) => None,
        }
    }
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ICON_IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The folder a project's relative paths start from: the project file's folder.
fn project_folder(location: &ProjectLocation) -> Option<PathBuf> {
    let ProjectLocation::SharedFile(file) = location;
    file.parent().map(Path::to_path_buf)
}

/// An `icon` value as stored: an image inside the project's folder is kept
/// relative to it, with `/` separators, so the file works for the whole team.
fn stored_icon(value: &str, folder: Option<&Path>) -> String {
    let path = Path::new(value);
    let relative = folder
        .filter(|_| path.is_absolute() && is_image_path(path))
        .and_then(|f| path.strip_prefix(f).ok());
    match relative {
        Some(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        None => value.to_owned(),
    }
}

/// Initials of a project name: first letters of the first two words, or
/// the first two letters of a single word.
#[must_use]
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    letters.to_uppercase()
}

/// A user-typed color as `#rrggbb` (`None` when blank).
fn parse_color(color: Option<String>) -> Result<Option<String>, AppError> {
    let Some(hex) = color.map(|a| a.trim().to_owned()).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    let hex = if hex.starts_with('#') {
        hex
    } else {
        format!("#{hex}")
    };
    archipel_theme::Color::from_hex(&hex)
        .map_err(|e| AppError::InvalidInput(format!("{hex} is not a color (#rrggbb): {e}")))?;
    Ok(Some(hex))
}

/// Validates a percentage typed by the user (`15` or `15%`, 0–100); empty
/// means none.
fn parse_percent(percent: Option<String>) -> Result<Option<u8>, AppError> {
    let Some(text) = percent
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
    else {
        return Ok(None);
    };
    text.trim_end_matches('%')
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|&p| p <= 100)
        .map(Some)
        .ok_or_else(|| AppError::InvalidInput(format!("{text} is not a percentage (0–100)")))
}

/// Id of the project registered for a shared project file.
#[must_use]
pub fn file_project_id(file: &Path) -> ProjectId {
    ProjectId(format!("file:{}", file.display()))
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
    /// Running folder scan (launch from a directory).
    scan: Option<CancelToken>,
    /// Submodules read per repo, while the dependency tree is shown.
    dependencies: HashMap<RepoId, Vec<SubmoduleNode>>,
    /// Repos whose dependencies are being read.
    tree_pending: HashSet<RepoId>,
    /// Branch and time of the last forge status read, per repo.
    forge_checked: HashMap<RepoId, (String, Instant)>,
    /// Repos whose forge status is being read.
    forge_pending: HashSet<RepoId>,
}

/// Found repos are added in batches: at most this many…
const SCAN_BATCH: usize = 16;
/// …or after this long, so the list fills in smoothly without re-watching
/// every repo for each one found.
const SCAN_FLUSH: Duration = Duration::from_millis(150);

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

/// Applies a tab command (open, close, cycle, move) to the UI state.
fn tab_command(ui: &mut UiState, command: &Command) {
    match *command {
        Command::OpenRepoTab(repo) => {
            let tab = TabState::Repo(repo);
            ui.active_tab = ui.tabs.iter().position(|t| *t == tab).unwrap_or_else(|| {
                ui.tabs.push(tab);
                ui.tabs.len() - 1
            });
        }
        Command::CloseTab(index) => {
            if index > 0 && index < ui.tabs.len() {
                ui.tabs.remove(index);
                if ui.active_tab >= index {
                    ui.active_tab = ui.active_tab.saturating_sub(1);
                }
            }
        }
        Command::CloseOtherTabs(index) => {
            if index < ui.tabs.len() {
                ui.active_tab = index;
                close_tabs(ui, |i| i == index, index);
            }
        }
        Command::CloseTabsToRight(index) => close_tabs(ui, |i| i <= index, index),
        Command::CloseAllTabs => close_tabs(ui, |_| false, 0),
        Command::CycleTab(forward) => {
            let n = ui.tabs.len().max(1);
            ui.active_tab = if forward {
                (ui.active_tab + 1) % n
            } else {
                (ui.active_tab + n - 1) % n
            };
        }
        Command::MoveTab { from, to } => {
            let len = ui.tabs.len();
            if from == 0 || from >= len || from == to {
                return;
            }
            let to = to.clamp(1, len - 1);
            let active = ui.tabs.get(ui.active_tab).cloned();
            let tab = ui.tabs.remove(from);
            ui.tabs.insert(to, tab);
            ui.active_tab = active
                .and_then(|t| ui.tabs.iter().position(|x| *x == t))
                .unwrap_or(0);
        }
        _ => {}
    }
}

/// Closes the tabs whose index fails `keep` (never the dashboard). The active
/// tab stays active when kept; otherwise tab `focus` becomes active when kept,
/// else the dashboard.
fn close_tabs(ui: &mut UiState, keep: impl Fn(usize) -> bool, focus: usize) {
    let active = ui.tabs.get(ui.active_tab).cloned();
    let focus = ui.tabs.get(focus).cloned();
    let mut i = 0;
    ui.tabs.retain(|_| {
        let kept = i == 0 || keep(i);
        i += 1;
        kept
    });
    let position = |tab: Option<TabState>| tab.and_then(|t| ui.tabs.iter().position(|x| *x == t));
    ui.active_tab = position(active).or_else(|| position(focus)).unwrap_or(0);
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
        let ProjectLocation::SharedFile(path) = &location;
        let id = file_project_id(path);
        self.services.registry.upsert_project(&RegistryEntry {
            id: id.clone(),
            name: def.name.clone(),
            location,
            last_opened: None,
        })?;
        self.emit(AppEvent::ProjectsChanged);
        Ok(id)
    }

    /// Registers an existing `archipel.toml` (e.g. from a teammate).
    pub fn import_project_file(&self, path: &Path) -> Result<ProjectId, AppError> {
        let location = ProjectLocation::SharedFile(path.to_path_buf());
        let def = self.services.projects.load(&location)?;
        let id = file_project_id(path);
        self.services.registry.upsert_project(&RegistryEntry {
            id: id.clone(),
            name: def.name,
            location,
            last_opened: None,
        })?;
        self.emit(AppEvent::ProjectsChanged);
        Ok(id)
    }

    /// Creates a shared project for a folder: writes `<dir>/archipel.toml`
    /// listing the repos found below it, registers and opens it. A folder
    /// that already holds a project file opens that file instead (never
    /// overwritten).
    pub fn new_project_from_folder(&self, dir: &Path) -> Result<ProjectId, AppError> {
        let dir = archipel_project::normalize_dir(dir).map_err(|e| ProjectError::Io {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?;
        let file = dir.join(archipel_project::PROJECT_FILE_NAME);
        let id = if file.is_file() {
            self.import_project_file(&file)?
        } else {
            let repos = self.scan(&dir)?;
            if repos.is_empty() {
                return Err(AppError::NotFound(format!(
                    "no repository found below {}",
                    dir.display()
                )));
            }
            let def = ProjectDef {
                name: dir
                    .file_name()
                    .map_or_else(|| "Project".into(), |n| n.to_string_lossy().into_owned()),
                repos: repos
                    .into_iter()
                    .map(|r| RepoEntry {
                        path: r.path,
                        alias: None,
                        category: None,
                    })
                    .collect(),
                settings: ProjectSettings {
                    accent: Some(
                        archipel_theme::accent_for_key(&file_project_id(&file).0).to_hex(),
                    ),
                    ..ProjectSettings::default()
                },
            };
            self.create_project(ProjectLocation::SharedFile(file), &def)?
        };
        self.open_project(&id)?;
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

    /// What opening `dir` would open, without opening it (lets a frontend
    /// focus the window already showing that project).
    pub fn directory_target(&self, dir: &Path) -> Result<DirectoryTarget, AppError> {
        let dir = archipel_project::normalize_dir(dir).map_err(|e| ProjectError::Io {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?;
        if !dir.is_dir() {
            return Err(AppError::NotFound(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        let file = dir.join(archipel_project::PROJECT_FILE_NAME);
        if file.is_file() {
            return Ok(DirectoryTarget {
                id: file_project_id(&file),
                project_file: Some(file),
                scan_root: dir,
            });
        }
        let root = archipel_project::outermost_repo_root(&dir).unwrap_or(dir);
        let file = root.join(archipel_project::PROJECT_FILE_NAME);
        Ok(DirectoryTarget {
            id: file_project_id(&file),
            project_file: file.is_file().then_some(file),
            scan_root: root,
        })
    }

    /// Opens the app on a directory (spec §1 "Launch from a directory"):
    /// its `archipel.toml` (or the one at the outermost enclosing repo root)
    /// when there is one. Otherwise writes `archipel.toml` at the scan root,
    /// shows the project at once and fills it in by a background,
    /// cancellable scan; relaunching then opens that file.
    pub fn open_directory(&self, dir: &Path) -> Result<ProjectId, AppError> {
        let target = self.directory_target(dir)?;
        if let Some(file) = &target.project_file {
            let id = self.import_project_file(file)?;
            self.open_project(&id)?;
            return Ok(id);
        }
        let name = target.scan_root.file_name().map_or_else(
            || target.scan_root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let def = ProjectDef {
            name,
            repos: Vec::new(),
            settings: ProjectSettings {
                accent: Some(archipel_theme::accent_for_key(&target.id.0).to_hex()),
                ..ProjectSettings::default()
            },
        };
        let file = target.scan_root.join(archipel_project::PROJECT_FILE_NAME);
        let id = self.create_project(ProjectLocation::SharedFile(file), &def)?;
        self.open_project(&id)?;
        self.start_scan(&id, target.scan_root);
        Ok(id)
    }

    fn start_scan(&self, project: &ProjectId, root: PathBuf) {
        let cancel = CancelToken::new();
        {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut().filter(|s| &s.id == project) else {
                return;
            };
            if let Some(old) = s.scan.replace(cancel.clone()) {
                old.cancel();
            }
        }
        self.emit(AppEvent::ScanProgress {
            found: 0,
            running: true,
        });
        let discovery = self.services.discovery.clone();
        let me = self.me.clone();
        let project = project.clone();
        let spec = OpSpec::new(None, OpClass::Read, format!("Scan {}", root.display()));
        let _handle = self.scheduler.submit(spec, move |_| {
            let pending = Mutex::new((Vec::new(), Instant::now(), 0_usize));
            let flush = |paths: Vec<RepoPath>, found: usize| {
                if let Some(app) = me.upgrade() {
                    app.add_found(&project, &paths, found);
                }
            };
            let result = discovery.scan_each(&root, &ScanOptions::default(), &cancel, &|repo| {
                let batch = {
                    let mut p = lock(&pending);
                    p.0.push(repo.path);
                    p.2 += 1;
                    (p.0.len() >= SCAN_BATCH || p.1.elapsed() >= SCAN_FLUSH).then(|| {
                        p.1 = Instant::now();
                        (std::mem::take(&mut p.0), p.2)
                    })
                };
                if let Some((paths, found)) = batch {
                    flush(paths, found);
                }
            });
            let (rest, found) = {
                let mut p = lock(&pending);
                (std::mem::take(&mut p.0), p.2)
            };
            flush(rest, found);
            if let Some(app) = me.upgrade() {
                app.finish_scan(&project, &cancel, found);
            }
            result.map_err(|e| OpError::Worker(e.to_string()))
        });
    }

    /// Adds repos found by the scan that the project does not list yet.
    fn add_found(&self, project: &ProjectId, paths: &[RepoPath], found: usize) {
        let mut new: Vec<RepoPath> = {
            let guard = lock(&self.session);
            let Some(s) = guard.as_ref().filter(|s| &s.id == project) else {
                return;
            };
            paths
                .iter()
                .filter(|p| s.def.repos.iter().all(|r| r.path.id() != p.id()))
                .cloned()
                .collect()
        };
        // The scan reports in no particular order; keep each batch tidy.
        new.sort_by(|a, b| a.as_path().cmp(b.as_path()));
        if !new.is_empty()
            && let Err(e) = self.add_repos(&new)
        {
            tracing::warn!(error = %e, "cannot add scanned repositories");
        }
        self.emit(AppEvent::ScanProgress {
            found,
            running: true,
        });
    }

    fn finish_scan(&self, project: &ProjectId, cancel: &CancelToken, found: usize) {
        if let Some(s) = lock(&self.session).as_mut().filter(|s| &s.id == project) {
            // Only clear our own scan, not one started after it.
            if s.scan.as_ref().is_some_and(|c| c.same_as(cancel)) {
                s.scan = None;
            }
        }
        self.emit(AppEvent::ScanProgress {
            found,
            running: false,
        });
    }

    /// Whether a folder scan is running for the open project.
    #[must_use]
    pub fn scanning(&self) -> bool {
        lock(&self.session)
            .as_ref()
            .is_some_and(|s| s.scan.is_some())
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
        let ui: UiState = self
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
                        .map_or(RepoStatus::Loading, |(_, s)| {
                            RepoStatus::Ready(Box::new(s.clone()))
                        })
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
                    hidden: ui.is_hidden(rid),
                    forge: ForgeStatus::Unknown,
                    ci: BranchCi::Unknown,
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
            scan: None,
            dependencies: HashMap::new(),
            tree_pending: HashSet::new(),
            forge_checked: HashMap::new(),
            forge_pending: HashSet::new(),
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
                        category: None,
                    });
                    s.rows.push(DashboardRow {
                        id: p.id(),
                        name: p.name(),
                        path: p.clone(),
                        status: RepoStatus::Loading,
                        stale: true,
                        hidden: s.ui.is_hidden(p.id()),
                        forge: ForgeStatus::Unknown,
                        ci: BranchCi::Unknown,
                    });
                }
            }
            (s.location.clone(), s.def.clone())
        };
        self.services.projects.save(&location, &def)?;
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
        let (location, def) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            s.def.repos.retain(|r| r.path.id() != repo);
            s.rows.retain(|r| r.id != repo);
            s.ui.hidden_repos.retain(|r| *r != repo);
            s.ui.selection.retain(|r| *r != repo);
            s.dependencies.remove(&repo);
            s.forge_checked.remove(&repo);
            (s.location.clone(), s.def.clone())
        };
        self.services.projects.save(&location, &def)?;
        self.start_watching();
        self.emit(AppEvent::RowsChanged(vec![repo]));
        Ok(())
    }

    /// Sets (or clears, when `None` or blank) the category of repos and
    /// saves the project definition (spec §1). A name matching an existing
    /// category but for case reuses that category's spelling.
    pub fn set_category(&self, ids: &[RepoId], category: Option<&str>) -> Result<(), AppError> {
        if ids.is_empty() {
            return Ok(());
        }
        let (location, def) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            let category = normalize_category(category).map(|c| {
                distinct_categories(s.def.repos.iter().map(|r| r.category.as_deref()))
                    .into_iter()
                    .find(|existing| existing.to_lowercase() == c.to_lowercase())
                    .unwrap_or(c)
            });
            for entry in s
                .def
                .repos
                .iter_mut()
                .filter(|r| ids.contains(&r.path.id()))
            {
                entry.category.clone_from(&category);
            }
            (s.location.clone(), s.def.clone())
        };
        self.services.projects.save(&location, &def)?;
        self.emit(AppEvent::RowsChanged(ids.to_vec()));
        Ok(())
    }

    /// Renames a category on every repo using it, or removes it (its repos
    /// become uncategorized) when `new` is `None` or blank. Renaming onto
    /// another existing category (any case) merges into it; a change of
    /// case only is kept as typed. The collapsed state follows the rename.
    pub fn rename_category(&self, old: &str, new: Option<&str>) -> Result<(), AppError> {
        let new = normalize_category(new);
        let (location, def, ids) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            let new = new.map(|c| {
                distinct_categories(s.def.repos.iter().map(|r| r.category.as_deref()))
                    .into_iter()
                    .find(|e| e != old && e.to_lowercase() == c.to_lowercase())
                    .unwrap_or(c)
            });
            let mut ids = Vec::new();
            for entry in s
                .def
                .repos
                .iter_mut()
                .filter(|r| r.category.as_deref() == Some(old))
            {
                entry.category.clone_from(&new);
                ids.push(entry.path.id());
            }
            if ids.is_empty() {
                return Ok(());
            }
            let ui = &mut s.ui;
            let was_collapsed = ui.collapsed_categories.iter().any(|k| k == old);
            ui.collapsed_categories.retain(|k| k != old);
            if let Some(new) = new.clone().filter(|_| was_collapsed)
                && !ui.collapsed_categories.contains(&new)
            {
                ui.collapsed_categories.push(new);
            }
            // The category filter follows too; a removed category's repos
            // are now uncategorized.
            if ui.category_filters.iter().any(|k| k == old) {
                ui.category_filters.retain(|k| k != old);
                let new = new.unwrap_or_default();
                if !ui.category_filters.contains(&new) {
                    ui.category_filters.push(new);
                }
            }
            (s.location.clone(), s.def.clone(), ids)
        };
        self.services.projects.save(&location, &def)?;
        self.emit(AppEvent::RowsChanged(ids));
        self.emit(AppEvent::UiStateChanged);
        Ok(())
    }

    /// Collapses or expands a sidebar category group (empty: "Uncategorized").
    fn toggle_category(&self, key: String) {
        self.update_ui_state(|ui| {
            if let Some(i) = ui.collapsed_categories.iter().position(|k| *k == key) {
                ui.collapsed_categories.remove(i);
            } else {
                ui.collapsed_categories.push(key);
            }
        });
    }

    /// Categories of the open project's repos, sorted by name.
    #[must_use]
    pub fn categories(&self) -> Vec<String> {
        lock(&self.session).as_ref().map_or_else(Vec::new, |s| {
            distinct_categories(s.def.repos.iter().map(|r| r.category.as_deref()))
        })
    }

    /// Toggles a dashboard category filter (empty: "Uncategorized").
    fn toggle_category_filter(&self, key: String) {
        self.update_ui_state(|ui| {
            if let Some(i) = ui.category_filters.iter().position(|k| *k == key) {
                ui.category_filters.remove(i);
            } else {
                ui.category_filters.push(key);
            }
        });
    }

    /// Keys of the dashboard category filter chips: the categories sorted by
    /// name, then `""` ("Uncategorized") when some repo has none; empty when
    /// the project has no category.
    #[must_use]
    pub fn category_filter_keys(&self) -> Vec<String> {
        lock(&self.session).as_ref().map_or_else(Vec::new, |s| {
            category_filter_keys(s.def.repos.iter().map(|r| r.category.as_deref()))
        })
    }

    /// The sidebar: rows that are not hidden, grouped by category (spec §1).
    #[must_use]
    pub fn sidebar_groups(&self) -> Vec<RepoGroup> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        let categories: HashMap<RepoId, &str> = s
            .def
            .repos
            .iter()
            .filter_map(|r| Some((r.path.id(), r.category.as_deref()?)))
            .collect();
        let rows = s.rows.iter().filter(|r| !r.hidden).cloned().collect();
        group_by_category(
            rows,
            |id| categories.get(&id).copied(),
            &s.ui.collapsed_categories,
        )
    }

    // ----- dashboard ------------------------------------------------------

    /// Every row of the open project that is not hidden, in project order
    /// (sidebar, tabs, multi-repo commit).
    #[must_use]
    pub fn rows(&self) -> Vec<DashboardRow> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.rows.iter().filter(|r| !r.hidden).cloned().collect())
            .unwrap_or_default()
    }

    /// Every row, hidden ones included.
    #[must_use]
    pub fn all_rows(&self) -> Vec<DashboardRow> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default()
    }

    /// Number of hidden repos of the open project.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        lock(&self.session)
            .as_ref()
            .map_or(0, |s| s.rows.iter().filter(|r| r.hidden).count())
    }

    /// Rows after filters and sorting from the UI state; hidden rows are
    /// left out unless "show hidden" is on.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<DashboardRow> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        Self::visible_rows_of(&self.ext, s)
    }

    fn visible_rows_of(ext: &Extensions, s: &Session) -> Vec<DashboardRow> {
        let sort =
            s.ui.sort_column
                .as_deref()
                .and_then(|id| ext.columns.get(id))
                .map(|c| (c.as_ref(), s.ui.sort_direction));
        let categories: HashMap<RepoId, Option<&str>> = s
            .def
            .repos
            .iter()
            .map(|r| (r.path.id(), r.category.as_deref()))
            .collect();
        let keys = category_filter_keys(categories.values().copied());
        let shown: Vec<DashboardRow> = s
            .rows
            .iter()
            .filter(|r| s.ui.show_hidden || !r.hidden)
            .filter(|r| {
                let category = categories.get(&r.id).copied().flatten();
                passes_category_filter(&s.ui.category_filters, &keys, category)
            })
            .cloned()
            .collect();
        visible_rows(&shown, &ext.filters, &s.ui.filters, sort)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The dependency tree of the visible rows (spec §9); empty when the
    /// tree is not shown. Nodes whose submodules are still being read show
    /// without children until [`AppEvent::TreeChanged`].
    #[must_use]
    pub fn dependency_tree(&self) -> Vec<TreeRow> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref().filter(|s| s.ui.show_dependency_tree) else {
            return Vec::new();
        };
        let rows = Self::visible_rows_of(&self.ext, s);
        let skip: HashSet<RepoId> = if s.ui.show_hidden {
            HashSet::new()
        } else {
            s.ui.hidden_repos.iter().copied().collect()
        };
        build_tree(&TreeInput {
            rows: &rows,
            dependencies: &s.dependencies,
            collapsed: &s.ui.collapsed_nodes,
            skip: &skip,
        })
    }

    /// Icon of any known project (switcher): personal choice, else the team
    /// default of its project file.
    #[must_use]
    pub fn project_icon(&self, id: &ProjectId) -> Option<ProjectIcon> {
        if self.project().is_some_and(|(open, _)| &open == id) {
            return self.identity()?.icon;
        }
        let entry = self
            .services
            .registry
            .list_projects()
            .ok()?
            .into_iter()
            .find(|e| &e.id == id)?;
        let folder = project_folder(&entry.location);
        let personal = self.services.ui_state.load_ui_state(id).ok().flatten();
        let icon = match personal.and_then(|u| u.icon) {
            Some(icon) => icon,
            None => {
                self.services
                    .projects
                    .load(&entry.location)
                    .ok()?
                    .settings
                    .icon?
            }
        };
        ProjectIcon::parse(&icon, folder.as_deref())
    }

    /// How the open project's window is recognized (spec §2): the color
    /// and icon of the project definition (a legacy personal choice wins),
    /// else automatic.
    #[must_use]
    pub fn identity(&self) -> Option<ProjectIdentity> {
        let guard = lock(&self.session);
        let s = guard.as_ref()?;
        let parse = |hex: Option<&str>| hex.and_then(|h| archipel_theme::Color::from_hex(h).ok());
        let chosen =
            parse(s.ui.accent.as_deref()).or_else(|| parse(s.def.settings.accent.as_deref()));
        let icon =
            s.ui.icon
                .as_deref()
                .or(s.def.settings.icon.as_deref())
                .and_then(|i| ProjectIcon::parse(i, project_folder(&s.location).as_deref()));
        Some(ProjectIdentity {
            name: s.def.name.clone(),
            accent: chosen.unwrap_or_else(|| archipel_theme::accent_for_key(&s.id.0)),
            automatic_accent: chosen.is_none(),
            background_tint: s.def.settings.background_tint.filter(|&t| t > 0),
            icon,
            initials: initials(&s.def.name),
        })
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
            selected: s.ui.batch_selection(),
            active_repo,
        }
    }

    // ----- refresh --------------------------------------------------------

    /// Reloads the open project's definition from where it is stored (the
    /// project file, edited by hand or by a teammate): name, color, icon and
    /// repo list. Repos that stay keep their status; new ones are added,
    /// removed ones dropped. The UI state is kept.
    pub fn reload_project(&self) -> Result<(), AppError> {
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
                    match old.iter().position(|r| r.id == rid) {
                        Some(i) => DashboardRow {
                            name: repo.display_name(),
                            ..old.swap_remove(i)
                        },
                        None => DashboardRow {
                            id: rid,
                            name: repo.display_name(),
                            path: repo.path.clone(),
                            status: if repo.path.as_path().exists() {
                                RepoStatus::Loading
                            } else {
                                RepoStatus::Missing
                            },
                            stale: true,
                            hidden: s.ui.is_hidden(rid),
                            forge: ForgeStatus::Unknown,
                            ci: BranchCi::Unknown,
                        },
                    }
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

    /// Recomputes every summary in parallel, then caches them.
    pub fn refresh_all(&self) {
        let ids: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .map(|s| s.rows.iter().filter(|r| !r.hidden).map(|r| r.id).collect())
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
            // Hidden repos are never refreshed (spec §1: saves CPU and RAM).
            if !s.rows.iter().any(|r| r.id == repo && !r.hidden) || !s.pending.insert(repo) {
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
                    Ok(summary) => RepoStatus::Ready(Box::new(summary)),
                    Err(archipel_model::GitError::NotARepository { .. }) => RepoStatus::Missing,
                    Err(e) => RepoStatus::Error(e.to_string()),
                };
            }
            s.pending.is_empty().then(|| (s.id.clone(), s.rows.clone()))
        };
        self.emit(AppEvent::RowsChanged(vec![repo]));
        self.load_tree(repo);
        self.load_forge(repo, false);
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
                .filter(|r| !r.hidden && r.path.as_path().exists())
                .map(|r| (r.id, r.path.clone()))
                .collect()
        };
        let me = self.me.clone();
        let sink: Arc<dyn archipel_model::ports::InvalidationSink> =
            Arc::new(move |inv: Invalidation| {
                let Some(app) = me.upgrade() else { return };
                // Only `FETCH_HEAD` written: a fetch that moved no ref.
                if inv.scope == Scope::FETCH {
                    app.note_fetched(inv.repo);
                } else {
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
                let shown = s.rows.iter().filter(|r| !r.hidden);
                if watched == shown.clone().count() {
                    Vec::new()
                } else {
                    shown.map(|r| r.id).collect()
                }
            })
            .unwrap_or_default();
        for id in degraded {
            self.refresh_repo(id);
        }
        self.refresh_forge_stale();
    }

    // ----- commands -------------------------------------------------------

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
    ///
    /// UI state a command changes (filters, sort, tabs, …) is saved right
    /// away, so it survives the app being killed rather than closed.
    pub fn dispatch(&self, command: Command) -> Result<(), AppError> {
        let before = lock(&self.session)
            .as_ref()
            .map(|s| (s.id.clone(), s.ui.clone()));
        let result = self.run_command(command);
        if let Some(s) = lock(&self.session).as_ref()
            && before.is_some_and(|(id, ui)| id == s.id && ui != s.ui)
            && let Err(e) = self.services.ui_state.save_ui_state(&s.id, &s.ui)
        {
            tracing::warn!(error = %e, "could not save UI state");
        }
        result
    }

    fn run_command(&self, command: Command) -> Result<(), AppError> {
        match command {
            Command::RefreshAll => {
                self.reload_project()?;
                self.refresh_all();
                self.load_tree_if_shown();
            }
            Command::RefreshForge => self.refresh_forge_all(),
            Command::RefreshRepo(r) => self.refresh_repo(r),
            Command::Fetch(ids) => {
                self.auto_fetch_reset(&ids);
                self.spawn_batch("Fetch", &ids, |b, t| async move {
                    b.fetch_all(&t, FetchRequest::default()).await
                });
            }
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
            Command::ToggleDrawer => {
                if let Some(s) = lock(&self.session).as_mut() {
                    s.drawer_open = !s.drawer_open;
                }
                self.emit(AppEvent::UiStateChanged);
            }
            Command::Ui(_) => {}
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
            Command::CancelScan => {
                let scan = lock(&self.session).as_ref().and_then(|s| s.scan.clone());
                if let Some(scan) = scan {
                    scan.cancel();
                }
            }
            Command::AddRepos(paths) => self.add_repos(&paths)?,
            Command::ToggleDependencyTree => {
                self.update_ui_state(|ui| ui.show_dependency_tree = !ui.show_dependency_tree);
                self.load_tree_if_shown();
            }
            Command::ToggleTreeNode(key) => self.update_ui_state(|ui| {
                if let Some(i) = ui.collapsed_nodes.iter().position(|k| *k == key) {
                    ui.collapsed_nodes.remove(i);
                } else {
                    ui.collapsed_nodes.push(key);
                }
            }),
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
            Command::SetAccent(accent) => {
                let accent = parse_color(accent)?;
                self.save_identity(|settings, ui| {
                    settings.accent = accent;
                    ui.accent = None;
                })?;
            }
            Command::SetBackgroundTint(tint) => {
                let tint = parse_percent(tint)?;
                self.save_identity(|settings, _| settings.background_tint = tint)?;
            }
            Command::SetIcon(icon) => {
                let folder = lock(&self.session)
                    .as_ref()
                    .and_then(|s| project_folder(&s.location));
                let icon = icon
                    .map(|i| stored_icon(i.trim(), folder.as_deref()))
                    .filter(|i| !i.is_empty());
                self.save_identity(|settings, ui| {
                    settings.icon = icon;
                    ui.icon = None;
                })?;
            }
            // Every other command is handled by `dispatch`.
            _ => {}
        }
        Ok(())
    }

    /// Changes the project's colors or icon in its definition, so a project
    /// file carries them for the whole team (spec §2), and drops any older
    /// personal override.
    fn save_identity(
        &self,
        f: impl FnOnce(&mut ProjectSettings, &mut UiState),
    ) -> Result<(), AppError> {
        let (location, def) = {
            let mut guard = lock(&self.session);
            let s = guard.as_mut().ok_or(AppError::NoProject)?;
            f(&mut s.def.settings, &mut s.ui);
            (s.location.clone(), s.def.clone())
        };
        self.services.projects.save(&location, &def)?;
        self.emit(AppEvent::UiStateChanged);
        Ok(())
    }

    /// Hides or unhides repos: hidden ones leave the selection and tabs and
    /// stop being watched and refreshed; unhidden ones are watched and
    /// refreshed again.
    fn set_hidden(&self, ids: &[RepoId], hidden: bool) {
        if ids.is_empty() {
            return;
        }
        {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            for row in s.rows.iter_mut().filter(|r| ids.contains(&r.id)) {
                row.hidden = hidden;
            }
            let ui = &mut s.ui;
            if hidden {
                for id in ids {
                    if !ui.hidden_repos.contains(id) {
                        ui.hidden_repos.push(*id);
                    }
                }
                if !ui.show_hidden && ui.selected_repo.is_some_and(|r| ids.contains(&r)) {
                    ui.selected_repo = None;
                }
                ui.selection.retain(|r| !ids.contains(r));
                let active = ui.tabs.get(ui.active_tab).cloned();
                ui.tabs
                    .retain(|t| !matches!(t, TabState::Repo(r) if ids.contains(r)));
                ui.active_tab = active
                    .and_then(|a| ui.tabs.iter().position(|t| *t == a))
                    .unwrap_or(0);
            } else {
                ui.hidden_repos.retain(|r| !ids.contains(r));
            }
            // Persist now: hiding is a deliberate choice, not layout noise.
            if let Err(e) = self.services.ui_state.save_ui_state(&s.id, &s.ui) {
                tracing::warn!(error = %e, "could not save UI state");
            }
        }
        self.start_watching();
        if !hidden {
            for id in ids {
                self.refresh_repo(*id);
                self.load_tree(*id);
            }
        }
        self.emit(AppEvent::RowsChanged(ids.to_vec()));
        self.emit(AppEvent::UiStateChanged);
    }

    fn load_tree_if_shown(&self) {
        let ids: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .filter(|s| s.ui.show_dependency_tree)
            .map(|s| s.rows.iter().filter(|r| !r.hidden).map(|r| r.id).collect())
            .unwrap_or_default();
        for id in ids {
            self.load_tree(id);
        }
    }

    /// Reads the submodules of one repo in the background, when the tree is
    /// shown (coalesced: at most one pending read per repo).
    fn load_tree(&self, repo: RepoId) {
        let path = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut().filter(|s| s.ui.show_dependency_tree) else {
                return;
            };
            let Some(path) = s
                .rows
                .iter()
                .find(|r| r.id == repo && !r.hidden)
                .map(|r| r.path.clone())
            else {
                return;
            };
            if !path.as_path().is_dir() || !s.tree_pending.insert(repo) {
                return;
            }
            path
        };
        let source = self.ext.dependencies.clone();
        let git = self.services.git.clone();
        let me = self.me.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Read,
            format!("Read submodules of {}", path.name()),
        );
        let _handle = self.scheduler.submit(spec, move |_| {
            let result = source.dependencies(&git, &path);
            if let Some(app) = me.upgrade() {
                let changed = {
                    let mut guard = lock(&app.session);
                    guard.as_mut().is_some_and(|s| {
                        s.tree_pending.remove(&repo);
                        let nodes = result.as_ref().cloned().unwrap_or_default();
                        s.dependencies.insert(repo, nodes.clone()) != Some(nodes)
                    })
                };
                if changed {
                    app.emit(AppEvent::TreeChanged);
                }
            }
            result.map(drop).map_err(OpError::from)
        });
    }

    fn update_submodules(&self, parent: RepoPath, request: SubmoduleUpdate) {
        let writer = self.services.git.submodule_writer.clone();
        let me = self.me.clone();
        let what = if request.paths.is_empty() {
            parent.name()
        } else {
            request.paths.join(", ")
        };
        let verb = if request.init { "Init" } else { "Update" };
        let spec = OpSpec::new(
            Some(parent.id()),
            OpClass::Network,
            format!("{verb} submodule {what}"),
        );
        let _handle = self.scheduler.submit(spec, move |ctx| {
            let result = writer.update_submodules(&parent, &request, ctx, ctx.cancel());
            if let Some(app) = me.upgrade() {
                app.refresh_all();
                app.load_tree_if_shown();
            }
            result.map_err(OpError::from)
        });
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
