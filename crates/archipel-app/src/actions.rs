//! Actions (palette, menus, batch bar, shortcuts) and the commands they send.
//!
//! Extension point: [`Action`], registered in [`ActionRegistry`]. Every
//! built-in action is registered there too; the keymap refers to actions by id.

use std::sync::Arc;

use archipel_model::{RepoId, RepoPath};

/// A request to the application, executed by `App::dispatch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Reload the current project's definition (project file), then
    /// recompute every summary.
    RefreshAll,
    /// Recompute one repo.
    RefreshRepo(RepoId),
    /// Re-read the forge status (pull request, CI, review) of every repo.
    RefreshForge,
    /// Fetch the given repos (all when empty).
    Fetch(Vec<RepoId>),
    /// Pull the given repos (all when empty).
    Pull(Vec<RepoId>),
    /// Check out a branch on the given repos (all when empty).
    CheckoutAll(String, Vec<RepoId>),
    /// Create a branch on the given repos.
    CreateBranchAll(String, Vec<RepoId>),
    /// Create a lightweight tag on the given repos.
    CreateTagAll(String, Vec<RepoId>),
    /// Undo the last operation of a repo.
    Undo(RepoId),
    /// Redo the last undone operation of a repo.
    Redo(RepoId),
    /// Toggle a dashboard filter.
    ToggleFilter(String),
    /// Sort the dashboard by a column (toggles direction when already sorted).
    SortBy(String),
    /// Select a repo in the dashboard / sidebar.
    Select(Option<RepoId>),
    /// Open (or focus) a repo tab.
    OpenRepoTab(RepoId),
    /// Close a tab (the dashboard tab cannot be closed).
    CloseTab(usize),
    /// Close every tab but the dashboard and the given one, which becomes active.
    CloseOtherTabs(usize),
    /// Close the tabs after the given one.
    CloseTabsToRight(usize),
    /// Close every tab but the dashboard.
    CloseAllTabs,
    /// Activate the next (`true`) or previous tab.
    CycleTab(bool),
    /// Move a tab to another position (drag and drop); the dashboard tab
    /// stays first. The active tab keeps following its tab.
    MoveTab {
        /// Index of the moved tab.
        from: usize,
        /// Index it ends up at.
        to: usize,
    },
    /// Show or hide the operations drawer.
    ToggleDrawer,
    /// Add or remove a repo from the multi-selection (Ctrl+click).
    ToggleSelect(RepoId),
    /// Hide repos (spec §1): out of the dashboard, batches, watching.
    HideRepos(Vec<RepoId>),
    /// Unhide repos (every hidden repo when empty).
    UnhideRepos(Vec<RepoId>),
    /// Hide the given repos, or unhide them when they are all hidden.
    ToggleHidden(Vec<RepoId>),
    /// List hidden repos (dimmed) in the dashboard, or not.
    ToggleShowHidden,
    /// Set the category of repos (spec §1); `None` or blank clears it.
    SetCategory(Vec<RepoId>, Option<String>),
    /// Collapse or expand a sidebar category group (empty: "Uncategorized").
    ToggleCategory(String),
    /// Toggle a dashboard category filter (empty: "Uncategorized"); rows
    /// pass when they are in any of the selected categories.
    ToggleCategoryFilter(String),
    /// Rename a category on every repo using it; `None` or blank removes
    /// it (its repos become uncategorized).
    RenameCategory(String, Option<String>),
    /// Stop the folder scan of a launch from a directory.
    CancelScan,
    /// Add repositories to the open project.
    AddRepos(Vec<RepoPath>),
    /// Show the repo dependency tree, or the flat list (spec §9).
    ToggleDependencyTree,
    /// Collapse or expand a node of the dependency tree (its path key).
    ToggleTreeNode(String),
    /// `git submodule update` in `parent` for `paths` (all when empty).
    UpdateSubmodules {
        /// Repository declaring the submodules.
        parent: RepoPath,
        /// Submodule paths relative to `parent`.
        paths: Vec<String>,
        /// Initialize them first.
        init: bool,
        /// Recurse into nested submodules.
        recursive: bool,
    },
    /// Open a submodule as a repo tab (adding it to the project if needed).
    OpenSubmodule(RepoPath),
    /// Project accent color (`#rrggbb`; automatic when `None`), saved in
    /// the project definition.
    SetAccent(Option<String>),
    /// Percentage (0–100) of the project color tinting its window
    /// background (`None`: untinted), saved in the project definition.
    SetBackgroundTint(Option<String>),
    /// Project icon (`None`: none), saved in the project definition: an
    /// emoji, or an image file (kept relative to the project's folder).
    SetIcon(Option<String>),
    /// UI-only commands (palettes, dialogs) the frontend handles itself.
    Ui(String),
    /// A fork's own use case, run against the app's public API: the way an
    /// [`Action`] does something no built-in command covers.
    Custom(CustomCommand),
}

/// What an action can see to decide availability and build its command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionContext {
    /// A project is open.
    pub project_open: bool,
    /// Repos selected in the dashboard (or the active repo tab).
    pub selected: Vec<RepoId>,
    /// The repo of the active tab, if it is a repo tab.
    pub active_repo: Option<RepoId>,
}

/// An action of the palette.
pub trait Action: Send + Sync {
    /// Stable id (keymap, menus).
    fn id(&self) -> &str;
    /// Label in the palette.
    fn label(&self) -> &str;
    /// Whether it can run now.
    fn is_available(&self, ctx: &ActionContext) -> bool;
    /// The command to dispatch (`None`: nothing to do).
    fn command(&self, ctx: &ActionContext) -> Option<Command>;
}

/// Runs a fork's use case: blocking work belongs in a scheduler job (see
/// `App::submit_repo_job`), not here.
pub type CustomRun = dyn Fn(&crate::App) -> Result<(), crate::AppError> + Send + Sync;

/// A command defined outside the core ([`Command::Custom`]).
#[derive(Clone)]
pub struct CustomCommand {
    /// Name, for logs and debugging.
    pub name: String,
    /// What it does.
    pub run: Arc<CustomRun>,
}

impl CustomCommand {
    /// A custom command named `name`.
    pub fn new(
        name: impl Into<String>,
        run: impl Fn(&crate::App) -> Result<(), crate::AppError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            run: Arc::new(run),
        }
    }
}

impl std::fmt::Debug for CustomCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CustomCommand").field(&self.name).finish()
    }
}

/// Two custom commands are equal when they are the same closure.
impl PartialEq for CustomCommand {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.run, &other.run)
    }
}

impl Eq for CustomCommand {}

/// Builds the command of a [`SimpleAction`].
pub type CommandBuilder = dyn Fn(&ActionContext) -> Option<Command> + Send + Sync;

/// An action defined by closures; handy for built-ins and forks.
pub struct SimpleAction {
    id: String,
    label: String,
    available: fn(&ActionContext) -> bool,
    build: Box<CommandBuilder>,
}

impl SimpleAction {
    /// Creates an action.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        available: fn(&ActionContext) -> bool,
        build: impl Fn(&ActionContext) -> Option<Command> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            available,
            build: Box::new(build),
        }
    }
}

impl Action for SimpleAction {
    fn id(&self) -> &str {
        &self.id
    }
    fn label(&self) -> &str {
        &self.label
    }
    fn is_available(&self, ctx: &ActionContext) -> bool {
        (self.available)(ctx)
    }
    fn command(&self, ctx: &ActionContext) -> Option<Command> {
        (self.build)(ctx)
    }
}

fn always(_: &ActionContext) -> bool {
    true
}
fn with_project(ctx: &ActionContext) -> bool {
    ctx.project_open
}
fn with_selection(ctx: &ActionContext) -> bool {
    ctx.project_open && !ctx.selected.is_empty()
}
fn with_repo(ctx: &ActionContext) -> bool {
    ctx.project_open && (ctx.active_repo.is_some() || ctx.selected.len() == 1)
}

fn target_repo(ctx: &ActionContext) -> Option<RepoId> {
    ctx.active_repo
        .or_else(|| (ctx.selected.len() == 1).then(|| ctx.selected[0]))
}

/// All actions, by id.
#[derive(Clone, Default)]
pub struct ActionRegistry {
    actions: Vec<Arc<dyn Action>>,
}

impl ActionRegistry {
    /// The built-in actions.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::default();
        let ui = |id: &'static str, label: &'static str, available: fn(&ActionContext) -> bool| {
            Arc::new(SimpleAction::new(id, label, available, move |_| {
                Some(Command::Ui(id.to_owned()))
            }))
        };
        r.register(ui("palette.projects", "Switch project…", always));
        r.register(ui("palette.actions", "Show all actions…", always));
        r.register(ui("project.new", "New project…", always));
        r.register(ui("project.add_repos", "Add repositories…", with_project));
        r.register(ui(
            "project.clone",
            "Clone a repository into the project…",
            with_project,
        ));
        r.register(ui("commit.open", "Commit on several repos…", with_project));
        r.register(ui("commit.submit", "Commit", with_project));
        r.register(ui(
            "repos.checkout_all",
            "Check out a branch on selected repos…",
            with_project,
        ));
        r.register(ui(
            "repos.branch_all",
            "Create a branch on selected repos…",
            with_project,
        ));
        r.register(ui(
            "repos.tag_all",
            "Create a tag on selected repos…",
            with_project,
        ));
        r.register(ui("settings.appearance", "Appearance and themes…", always));
        r.register(ui("app.copy_diagnostics", "Copy diagnostics", always));
        r.register(Arc::new(SimpleAction::new(
            "repos.refresh",
            "Refresh",
            with_project,
            |_| Some(Command::RefreshAll),
        )));
        r.register(Arc::new(SimpleAction::new(
            "repos.fetch_all",
            "Fetch selected repos (all when none)",
            with_project,
            |c| Some(Command::Fetch(c.selected.clone())),
        )));
        r.register(Arc::new(SimpleAction::new(
            "repos.pull_all",
            "Pull selected repos (all when none)",
            with_project,
            |c| Some(Command::Pull(c.selected.clone())),
        )));
        r.register(Arc::new(SimpleAction::new(
            "history.undo",
            "Undo last operation",
            with_repo,
            |c| target_repo(c).map(Command::Undo),
        )));
        r.register(Arc::new(SimpleAction::new(
            "history.redo",
            "Redo",
            with_repo,
            |c| target_repo(c).map(Command::Redo),
        )));
        r.register(Arc::new(SimpleAction::new(
            "tabs.next",
            "Next tab",
            with_project,
            |_| Some(Command::CycleTab(true)),
        )));
        r.register(Arc::new(SimpleAction::new(
            "tabs.previous",
            "Previous tab",
            with_project,
            |_| Some(Command::CycleTab(false)),
        )));
        r.register(Arc::new(SimpleAction::new(
            "drawer.toggle",
            "Toggle operations drawer",
            always,
            |_| Some(Command::ToggleDrawer),
        )));
        r.register_window_and_view_actions();
        r.register(Arc::new(SimpleAction::new(
            "repo.open",
            "Open repository tab",
            with_repo,
            |c| target_repo(c).map(Command::OpenRepoTab),
        )));
        r
    }

    /// Windows, hidden repos, dependency tree and project identity.
    fn register_window_and_view_actions(&mut self) {
        let r = self;
        let ui = |id: &'static str, label: &'static str, available: fn(&ActionContext) -> bool| {
            Arc::new(SimpleAction::new(id, label, available, move |_| {
                Some(Command::Ui(id.to_owned()))
            }))
        };
        r.register(ui("window.new", "New window", always));
        r.register(ui(
            "project.set_color",
            "Set the project color…",
            with_project,
        ));
        r.register(ui(
            "project.set_background_tint",
            "Set the project background tint…",
            with_project,
        ));
        r.register(ui(
            "project.set_icon",
            "Set the project icon…",
            with_project,
        ));
        r.register(ui(
            "project.set_icon_image",
            "Set the project icon from an image…",
            with_project,
        ));
        r.register(Arc::new(SimpleAction::new(
            "repos.toggle_hidden",
            "Hide / unhide selected repos",
            with_selection,
            |c| Some(Command::ToggleHidden(c.selected.clone())),
        )));
        r.register(ui(
            "repos.set_category",
            "Set the category of selected repos…",
            with_selection,
        ));
        r.register(Arc::new(SimpleAction::new(
            "repos.show_hidden",
            "Show / don't show hidden repos",
            with_project,
            |_| Some(Command::ToggleShowHidden),
        )));
        r.register(Arc::new(SimpleAction::new(
            "repos.unhide_all",
            "Unhide every repo",
            with_project,
            |_| Some(Command::UnhideRepos(Vec::new())),
        )));
        r.register(Arc::new(SimpleAction::new(
            "view.dependency_tree",
            "Toggle the repo dependency tree",
            with_project,
            |_| Some(Command::ToggleDependencyTree),
        )));
        r.register(Arc::new(SimpleAction::new(
            "project.cancel_scan",
            "Stop scanning the folder",
            with_project,
            |_| Some(Command::CancelScan),
        )));
        r.register_forge_actions();
    }

    /// Forge integration (spec §8). The panels are frontend dialogs.
    fn register_forge_actions(&mut self) {
        let r = self;
        let ui = |id: &'static str, label: &'static str, available: fn(&ActionContext) -> bool| {
            Arc::new(SimpleAction::new(id, label, available, move |_| {
                Some(Command::Ui(id.to_owned()))
            }))
        };
        r.register(ui(
            "forge.accounts",
            "Forge accounts (GitHub, GitLab)…",
            always,
        ));
        r.register(ui(
            "forge.create_pull_requests",
            "Create pull requests for selected repos (all when none)…",
            with_project,
        ));
        r.register(ui(
            "forge.clone",
            "Clone repositories from a forge account…",
            with_project,
        ));
        r.register(ui(
            "forge.open_pull_request",
            "Open the pull request in the browser",
            with_repo,
        ));
        r.register(ui(
            "forge.open_ci",
            "Open the CI run of the branch in the browser",
            with_repo,
        ));
        r.register(Arc::new(SimpleAction::new(
            "forge.refresh",
            "Refresh pull request and CI status",
            with_project,
            |_| Some(Command::RefreshForge),
        )));
    }

    /// Adds an action or replaces the one with the same id.
    pub fn register(&mut self, action: Arc<dyn Action>) {
        match self.actions.iter().position(|a| a.id() == action.id()) {
            Some(i) => self.actions[i] = action,
            None => self.actions.push(action),
        }
    }

    /// An action by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Action>> {
        self.actions.iter().find(|a| a.id() == id)
    }

    /// Actions available in `ctx`, for the palette.
    #[must_use]
    pub fn available(&self, ctx: &ActionContext) -> Vec<&Arc<dyn Action>> {
        self.actions
            .iter()
            .filter(|a| a.is_available(ctx))
            .collect()
    }

    /// Every action.
    #[must_use]
    pub fn all(&self) -> &[Arc<dyn Action>] {
        &self.actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Keymap;

    #[test]
    fn every_default_shortcut_has_an_action() {
        let actions = ActionRegistry::with_builtins();
        for (key, id) in Keymap::defaults().iter() {
            assert!(actions.get(id).is_some(), "{key} → {id} has no action");
        }
    }

    #[test]
    fn availability_depends_on_context() {
        let actions = ActionRegistry::with_builtins();
        let undo = actions.get("history.undo").unwrap();
        let none = ActionContext::default();
        assert!(!undo.is_available(&none));
        let ctx = ActionContext {
            project_open: true,
            selected: vec![RepoId(4)],
            active_repo: None,
        };
        assert!(undo.is_available(&ctx));
        assert_eq!(undo.command(&ctx), Some(Command::Undo(RepoId(4))));
        assert!(actions.available(&none).len() < actions.all().len());
    }
}
