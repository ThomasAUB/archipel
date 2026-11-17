//! Actions (palette, menus, batch bar, shortcuts) and the commands they send.
//!
//! Extension point: [`Action`], registered in [`ActionRegistry`]. Every
//! built-in action is registered there too; the keymap refers to actions by id.

use std::sync::Arc;

use archipel_model::worktree::{SetView, WorktreeAddRequest};
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
    /// Rerun the failed CI jobs of the current branch of the given repos
    /// (all when empty).
    RerunFailedCi(Vec<RepoId>),
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
    /// Remove a repository from the open project (its files are untouched),
    /// with its linked worktrees unless listed themselves.
    RemoveRepo(RepoId),
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
    /// List or unlist the linked worktrees of a repo under it (spec §10).
    ToggleWorktrees(RepoId),
    /// Create a worktree of a repo (any row of it), then copy its local
    /// files, and open it in a tab with `open`.
    AddWorktree {
        /// A row of the repo.
        repo: RepoId,
        /// Where and what.
        request: WorktreeAddRequest,
        /// `git submodule update --init --recursive` in it.
        init_submodules: bool,
        /// Copy the repo's local files (`copy_to_worktrees`) into it.
        copy_files: bool,
        /// Open it in a tab once created.
        open: bool,
    },
    /// Remove a linked worktree (undoable on its repo).
    RemoveWorktree {
        /// Its row.
        worktree: RepoId,
        /// Remove it although it has changes (snapshotted for undo).
        force: bool,
        /// Also delete its branch.
        delete_branch: bool,
    },
    /// Lock (`Some(reason)`) or unlock (`None`) a linked worktree.
    LockWorktree(RepoId, Option<String>),
    /// Forget the missing worktrees of the given repos (all when empty);
    /// locked ones are kept.
    PruneWorktrees(Vec<RepoId>),
    /// What the dashboard shows of worktrees (the Set Selector).
    SetWorktreeView(SetView),
    /// In a set view, list the repos outside the set, dimmed.
    ToggleSetOthers,
    /// Create a Worktree Set, or add repos to one (Retry failed).
    CreateWorktreeSet(crate::NewSetRequest),
    /// Remove members of a Worktree Set (its row, how), as one batch.
    FinishWorktreeSet {
        /// Set name.
        name: String,
        /// The members removed (their rows) and how.
        removals: Vec<(RepoId, archipel_ops::RemoveOptions)>,
    },
    /// Forget a Worktree Set (its worktrees stay).
    ForgetWorktreeSet(String),
    /// Rename a Worktree Set.
    RenameWorktreeSet(String, String),
    /// Point a remote of a repo at a new URL, adding the remote when
    /// missing (the repo's shared configuration, for every worktree).
    SetRemoteUrl {
        /// The repo (or one of its worktrees).
        repo: RepoId,
        /// Remote name (`origin`).
        remote: String,
        /// New fetch URL.
        url: String,
    },
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
    /// Repos selected in the dashboard, the batch selection. Commands on
    /// "the selected repos (all when none)" take it as is: the core
    /// resolves an empty one (`App::resolve_targets`).
    pub selected: Vec<RepoId>,
    /// The repo of the active tab, if it is a repo tab.
    pub active_repo: Option<RepoId>,
}

impl ActionContext {
    /// The one repo an action on a single repo acts on: the active tab's,
    /// else the selected repo when exactly one is.
    #[must_use]
    pub fn target_repo(&self) -> Option<RepoId> {
        self.active_repo.or(match self.selected[..] {
            [only] => Some(only),
            _ => None,
        })
    }
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
    ctx.project_open && ctx.target_repo().is_some()
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
        r.register(ui(
            "repos.branches",
            "Clean up branches: merged and stale, on selected repos…",
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
            |c| c.target_repo().map(Command::Undo),
        )));
        r.register(Arc::new(SimpleAction::new(
            "history.redo",
            "Redo",
            with_repo,
            |c| c.target_repo().map(Command::Redo),
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
        r.register_worktree_actions();
        r.register(Arc::new(SimpleAction::new(
            "repo.open",
            "Open repository tab",
            with_repo,
            |c| c.target_repo().map(Command::OpenRepoTab),
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
        r.register(ui("window.next", "Next window", always));
        r.register(ui("window.previous", "Previous window", always));
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

    /// Worktrees (spec §10). The dialogs are frontend ones.
    fn register_worktree_actions(&mut self) {
        let r = self;
        let ui = |id: &'static str, label: &'static str, available: fn(&ActionContext) -> bool| {
            Arc::new(SimpleAction::new(id, label, available, move |_| {
                Some(Command::Ui(id.to_owned()))
            }))
        };
        r.register(ui("palette.worktrees", "Switch worktree…", with_project));
        r.register(ui("worktree.new", "New worktree…", with_repo));
        r.register(ui("worktree.new_set", "New worktree set…", with_project));
        r.register(ui("worktree.remove", "Remove worktree…", with_repo));
        r.register(ui("worktree.lock", "Lock / unlock worktree…", with_repo));
        r.register(ui("worktree.reveal", "Reveal worktree folder", with_repo));
        r.register(ui("worktree.copy_path", "Copy worktree path", with_repo));
        r.register(ui(
            "worktree.terminal",
            "Open worktree in terminal",
            with_repo,
        ));
        r.register(ui(
            "worktree.editor",
            "Open worktree in external editor",
            with_repo,
        ));
        r.register(ui(
            "worktree.finish_set",
            "Finish worktree set…",
            with_project,
        ));
        r.register(Arc::new(SimpleAction::new(
            "worktree.prune",
            "Prune missing worktrees",
            with_project,
            |c| Some(Command::PruneWorktrees(c.selected.clone())),
        )));
        r.register(Arc::new(SimpleAction::new(
            "worktree.view_all",
            "Worktrees: show all, nested under their repo",
            with_project,
            |_| Some(Command::SetWorktreeView(SetView::All)),
        )));
        r.register(Arc::new(SimpleAction::new(
            "worktree.view_main",
            "Worktrees: main worktrees only",
            with_project,
            |_| Some(Command::SetWorktreeView(SetView::MainOnly)),
        )));
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
            "forge.pull_requests",
            "Open pull requests of selected repos (all when none): review, merge…",
            with_project,
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
            "forge.rerun_failed_ci",
            "Rerun failing CI of selected repos (all when none)",
            with_project,
            |c| Some(Command::RerunFailedCi(c.selected.clone())),
        )));
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
