//! The main window: sidebar, tabs, dashboard, operations drawer, overlays
//! (PLAN §3.4).

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

use archipel_app::dashboard::{DashboardRow, RepoStatus};
use archipel_app::deptree::TreeRow;
use archipel_app::{App as Core, AppEvent, Command, Extensions, ProjectIcon};
use archipel_model::project::ProjectLocation;
use archipel_model::ui_state::{SortDirection, TabState, UiState};
use archipel_model::{ProjectId, RepoId, RepoPath, Timestamp};
use archipel_ops::OpEventKind;
use archipel_theme::ColorToken as T;
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Colorize as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    Action, Anchor, AnyElement, ClickEvent, Context, DragMoveEvent, Entity, FocusHandle,
    KeyDownEvent, MouseButton, MouseDownEvent, ObjectFit, PathPromptOptions, Pixels, Point,
    SharedString, Subscription, TextRun, Window, anchored, deferred, div, font, img, px, rems,
};

use crate::commit_panel::{CommitPanel, CommitPanelEvent};
use crate::forge_panel::{ForgePanel, ForgePanelEvent, ForgeTab};
use crate::palette::{Palette, PaletteEvent, PaletteItem};
use crate::quick_edit::{QuickEdit, QuickEditEvent};
use crate::repo_view::RepoView;
use crate::theme::{UiTheme, clean_dot, color, radius, row_height, text_size, tone, ui_font};
use crate::widgets::{
    Pane, PaneDrag, TextTooltip, button, chip, context_menu, dot, menu_item, menu_separator,
    primary_button, resize_pane, section, splitter,
};

gpui_kit::actions!(
    archipel,
    [
        OpenProjects,
        OpenActions,
        NextTab,
        PreviousTab,
        Undo,
        Redo,
        ToggleDrawer,
        Refresh,
        FetchAll,
        OpenCommit,
        SubmitCommit,
        ToggleHide,
        ShowHidden,
        ToggleTree,
        NewWindow
    ]
);

/// Relative widths of the dashboard columns.
const WIDTHS: [f32; 8] = [2.0, 1.4, 1.0, 1.2, 1.0, 0.8, 1.0, 0.9];

/// Height of the project color band at the top of the window.
const BAND_HEIGHT: f32 = 3.0;

/// The GPUI action for an `archipel-app` action id (keymap bindings).
pub fn action_for(id: &str) -> Option<Box<dyn Action>> {
    Some(match id {
        "palette.projects" => Box::new(OpenProjects),
        "palette.actions" => Box::new(OpenActions),
        "tabs.next" => Box::new(NextTab),
        "tabs.previous" => Box::new(PreviousTab),
        "history.undo" => Box::new(Undo),
        "history.redo" => Box::new(Redo),
        "drawer.toggle" => Box::new(ToggleDrawer),
        "repos.refresh" => Box::new(Refresh),
        "repos.fetch_all" => Box::new(FetchAll),
        "commit.open" => Box::new(OpenCommit),
        "commit.submit" => Box::new(SubmitCommit),
        "repos.toggle_hidden" => Box::new(ToggleHide),
        "repos.show_hidden" => Box::new(ShowHidden),
        "view.dependency_tree" => Box::new(ToggleTree),
        "window.new" => Box::new(NewWindow),
        _ => return None,
    })
}

/// What a palette overlay is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaletteKind {
    Projects,
    Actions,
    Checkout,
    Branch,
    Tag,
    Color,
    BackgroundTint,
    Icon,
    Category,
    RenameCategory,
}

enum Overlay {
    Palette(Entity<Palette>),
    Commit(Entity<CommitPanel>),
    Forge(Entity<ForgePanel>),
}

/// Lines kept in the operations drawer.
const DRAWER_LINES: usize = 300;

/// Drag payload of a repo tab: its index.
struct TabDrag(usize);

/// The tab label following the pointer while a tab is dragged.
struct TabGhost(SharedString);

impl Render for TabGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded(radius(cx))
            .whitespace_nowrap()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Accent))
            .text_color(color(cx, T::FgDefault))
            .shadow_md()
            .child(self.0.clone())
    }
}

/// The main window view.
pub struct Workspace {
    core: Arc<Core>,
    focus: FocusHandle,
    repo_views: HashMap<RepoId, Entity<RepoView>>,
    overlay: Option<(Overlay, Subscription)>,
    /// Quick edit pane of the dashboard (a plain click on a row).
    quick_edit: Option<(Entity<QuickEdit>, [Subscription; 2])>,
    log: VecDeque<(Timestamp, String, bool)>,
    notice: Option<String>,
    warnings: Vec<String>,
    /// Open repo context menu: repo and mouse position.
    row_menu: Option<(RepoId, Point<Pixels>)>,
    /// Repos the open category palette applies to.
    category_targets: Vec<RepoId>,
    /// Open category header menu: category and mouse position.
    category_menu: Option<(String, Point<Pixels>)>,
    /// Open tab context menu: tab index and mouse position.
    tab_menu: Option<(usize, Point<Pixels>)>,
    /// Category the open rename palette applies to.
    renaming_category: Option<String>,
    /// Open category filter dropdown: mouse position.
    category_filter_menu: Option<Point<Pixels>>,
    /// Where a click outside last closed that dropdown, so the same click
    /// landing on its button does not reopen it.
    category_filter_closed_at: Option<Point<Pixels>>,
    /// Open theme picker (status bar): where it opens.
    theme_menu: Option<Point<Pixels>>,
    /// Where a click outside last closed the theme picker (see
    /// `category_filter_closed_at`).
    theme_menu_closed_at: Option<Point<Pixels>>,
    /// The status bar's project color picker, left of the theme picker.
    accent_picker: Entity<ColorPickerState>,
    _accent_picked: Subscription,
    /// Folder scan progress: repos found, still running.
    scan: Option<(usize, bool)>,
    /// Submodule being opened from the tree (adding it to the project
    /// re-watches every repo, which can take a moment).
    opening: Option<RepoPath>,
    /// Window title last set.
    title: String,
}

impl Workspace {
    /// Builds the workspace and reopens the most recent project.
    pub fn new(
        core: Arc<Core>,
        warnings: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self::pump_events(&core, cx);
        crate::startup::mark("workspace built");
        window.on_next_frame(|_, _| crate::startup::mark("first frame"));
        window.on_window_should_close(cx, |window, cx| {
            crate::windows::closed(cx, window.window_handle());
            true
        });
        let accent_picker = cx.new(|cx| ColorPickerState::new(window, cx));
        let on_accent = cx.subscribe_in(
            &accent_picker,
            window,
            |this, _, event: &ColorPickerEvent, _, cx| {
                let ColorPickerEvent::Change(picked) = event;
                // Opaque: the project file holds `#rrggbb`.
                let hex = picked.map(|c| gpui_kit::Hsla { a: 1.0, ..c }.to_hex());
                let current = this.core.identity().map(|i| i.accent.to_hex());
                if hex.is_some() && hex != current {
                    this.dispatch(Command::SetAccent(hex), cx);
                }
            },
        );
        Self {
            core,
            focus,
            repo_views: HashMap::new(),
            overlay: None,
            quick_edit: None,
            log: VecDeque::new(),
            notice: None,
            warnings,
            row_menu: None,
            category_targets: Vec::new(),
            category_menu: None,
            tab_menu: None,
            renaming_category: None,
            category_filter_menu: None,
            category_filter_closed_at: None,
            theme_menu: None,
            theme_menu_closed_at: None,
            accent_picker,
            _accent_picked: on_accent,
            scan: None,
            opening: None,
            title: String::new(),
        }
    }

    /// Forwards core events to the UI thread.
    fn pump_events(core: &Arc<Core>, cx: &mut Context<Self>) {
        let mut events = core.subscribe();
        cx.spawn(async move |this, cx| {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if this.update(cx, |w, cx| w.on_event(event, cx)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if this.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
        .detach();
    }

    fn on_event(&mut self, event: AppEvent, cx: &mut Context<Self>) {
        match event {
            AppEvent::Op(op) => {
                let line = match &op.kind {
                    OpEventKind::Queued => None,
                    OpEventKind::Started => Some((format!("▶ {}", op.label), false)),
                    OpEventKind::Progress(p) => {
                        let total = p.total.map_or_else(String::new, |t| format!("/{t}"));
                        Some((
                            format!("… {} — {} {}{total}", op.label, p.phase, p.current),
                            false,
                        ))
                    }
                    OpEventKind::Finished(Ok(())) => Some((format!("✓ {}", op.label), false)),
                    OpEventKind::Finished(Err(e)) => Some((format!("✗ {} — {e}", op.label), true)),
                };
                if let Some((text, error)) = line {
                    // Progress lines replace the previous progress line of the op.
                    if text.starts_with('…')
                        && self.log.back().is_some_and(|(_, l, _)| l.starts_with('…'))
                    {
                        self.log.pop_back();
                    }
                    self.log.push_back((Timestamp::now(), text, error));
                    while self.log.len() > DRAWER_LINES {
                        self.log.pop_front();
                    }
                }
            }
            AppEvent::BatchFinished(label, done, failed, skipped) => {
                self.notice = Some(format!(
                    "{label}: {done} done, {failed} failed, {skipped} skipped"
                ));
            }
            AppEvent::Notice(text) => self.notice = Some(text),
            AppEvent::DegradedWatching(repos) => {
                self.notice = Some(format!(
                    "{} repo(s) cannot be watched fully (OS limit): they refresh when the window gets focus",
                    repos.len()
                ));
            }
            AppEvent::ProjectOpened(_) => {
                self.repo_views.clear();
                self.quick_edit = None;
                self.row_menu = None;
                self.category_menu = None;
                self.category_filter_menu = None;
                self.tab_menu = None;
                self.scan = None;
            }
            AppEvent::ScanProgress { found, running } => self.scan = Some((found, running)),
            AppEvent::RowsChanged(ids) => {
                let rows = self.core.rows();
                for id in ids {
                    let row = rows.iter().find(|r| r.id == id);
                    let summary = row.and_then(|r| r.summary());
                    if let Some(view) = self.repo_views.get(&id) {
                        view.update(cx, |v, cx| {
                            if let Some(row) = row {
                                v.on_forge(&row.ci, &row.forge, cx);
                            }
                            v.on_summary(summary, cx);
                        });
                    }
                    if let Some((pane, _)) = &self.quick_edit
                        && pane.read(cx).repo() == id
                    {
                        pane.update(cx, |p, cx| p.on_summary(summary, cx));
                    }
                }
            }
            AppEvent::UiStateChanged
            | AppEvent::ProjectsChanged
            | AppEvent::TreeChanged
            | AppEvent::ForgeAccountsChanged => {}
        }
        cx.notify();
    }

    fn dispatch(&mut self, command: Command, cx: &mut Context<Self>) {
        if let Err(e) = self.core.dispatch(command) {
            self.notice = Some(e.to_string());
        }
        cx.notify();
    }

    /// Opens the quick edit pane on a repo, or closes it when it already
    /// shows that repo.
    fn toggle_quick_edit(&mut self, id: RepoId, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .quick_edit
            .as_ref()
            .is_some_and(|(p, _)| p.read(cx).repo() == id)
        {
            self.quick_edit = None;
            self.focus.focus(window, cx);
            cx.notify();
            return;
        }
        let Some(row) = self.core.rows().into_iter().find(|r| r.id == id) else {
            return;
        };
        let core = self.core.clone();
        let pane = cx.new(|cx| QuickEdit::new(core, &row, window, cx));
        let sub = cx.subscribe_in(
            &pane,
            window,
            |this: &mut Self, _, event: &QuickEditEvent, window, cx| {
                match event {
                    QuickEditEvent::Closed => {
                        this.quick_edit = None;
                        this.focus.focus(window, cx);
                    }
                    QuickEditEvent::OpenTab(id) => {
                        this.quick_edit = None;
                        this.dispatch(Command::OpenRepoTab(*id), cx);
                    }
                }
                cx.notify();
            },
        );
        // Its open diff is drawn here, in place of the rows.
        let redraw = cx.observe(&pane, |_, _, cx| cx.notify());
        self.quick_edit = Some((pane, [sub, redraw]));
        cx.notify();
    }

    /// ↑ / ↓ / Esc on the dashboard go to the quick edit pane (its open
    /// diff first), unless a menu or overlay is up or an input has focus.
    fn quick_edit_key(&mut self, e: &KeyDownEvent, window: &Window, cx: &mut Context<Self>) {
        let m = &e.keystroke.modifiers;
        if m.control || m.alt || m.platform || m.shift {
            return;
        }
        if self.overlay.is_some() || self.row_menu.is_some() {
            return;
        }
        let on_dashboard = self
            .core
            .ui_state()
            .is_some_and(|ui| !matches!(ui.tabs.get(ui.active_tab), Some(TabState::Repo(_))));
        let Some((pane, _)) = &self.quick_edit else {
            return;
        };
        let keys_free = self.focus.is_focused(window) || pane.read(cx).is_focused(window);
        if on_dashboard && keys_free && pane.update(cx, |p, cx| p.on_key(&e.keystroke.key, cx)) {
            cx.stop_propagation();
        }
    }

    /// Opens a submodule in a tab off the UI thread, showing it as opening
    /// meanwhile.
    fn open_submodule(&mut self, path: RepoPath, cx: &mut Context<Self>) {
        if self.opening.is_some() {
            return;
        }
        self.opening = Some(path.clone());
        cx.notify();
        let core = self.core.clone();
        let task = cx.background_spawn(async move { core.dispatch(Command::OpenSubmodule(path)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.opening = None;
                if let Err(e) = result {
                    this.notice = Some(e.to_string());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The batch selection (empty: every repo).
    fn selected_or_all(&self) -> Vec<RepoId> {
        self.core.action_context().selected
    }

    // ----- overlays -----------------------------------------------------

    fn open_palette(&mut self, kind: PaletteKind, window: &mut Window, cx: &mut Context<Self>) {
        let (title, items, prompt) = match kind {
            PaletteKind::Projects => {
                let mut items = vec![
                    PaletteItem {
                        id: "+folder".into(),
                        label: "New project from a folder…".into(),
                        detail: "scan for repositories".into(),
                    },
                    PaletteItem {
                        id: "+file".into(),
                        label: "Open an archipel.toml…".into(),
                        detail: "shared project file".into(),
                    },
                ];
                let current = self.core.project().map(|(id, _)| id);
                for p in self.core.projects().unwrap_or_default() {
                    let ProjectLocation::SharedFile(file) = &p.location;
                    let mut detail = file.display().to_string();
                    if current.as_ref() == Some(&p.id) {
                        detail = format!("this window · {detail}");
                    } else if crate::windows::is_open(window, cx, &p.id) {
                        detail = format!("open in another window · {detail}");
                    }
                    let label = match self
                        .core
                        .project_icon(&p.id)
                        .as_ref()
                        .and_then(|i| i.text())
                    {
                        Some(icon) => format!("{icon}  {}", p.name),
                        None => p.name,
                    };
                    items.push(PaletteItem {
                        id: p.id.0,
                        label,
                        detail,
                    });
                }
                ("Switch project", items, false)
            }
            PaletteKind::Actions => {
                let ctx = self.core.action_context();
                let ext = self.core.extensions();
                let items = ext
                    .actions
                    .available(&ctx)
                    .into_iter()
                    .map(|a| PaletteItem {
                        id: a.id().to_owned(),
                        label: a.label().to_owned(),
                        detail: ext
                            .keymap
                            .keys_for(a.id())
                            .first()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    })
                    .collect();
                ("Actions", items, false)
            }
            PaletteKind::Checkout => (
                "Check out branch on selected repos (all when none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Branch => (
                "Create branch on selected repos (all when none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Tag => (
                "Create tag on selected repos (all when none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Color => (
                "Project color: #rrggbb (empty: automatic, from the project path)",
                Vec::new(),
                true,
            ),
            PaletteKind::BackgroundTint => (
                "Project background tint: % of the project color, 0–100 (empty: none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Icon => (
                "Project icon: emoji or image file (empty: none)",
                Vec::new(),
                true,
            ),
            PaletteKind::RenameCategory => {
                // A plain prompt: an empty Enter must not pick (merge into)
                // another category. Typing an existing name merges into it.
                let old = self.renaming_category.clone().unwrap_or_default();
                return self.show_palette(
                    kind,
                    format!("Rename category “{old}” (an existing name merges into it)"),
                    Vec::new(),
                    true,
                    window,
                    cx,
                );
            }
            PaletteKind::Category => {
                let n = self.category_targets.len();
                let title = if n > 1 {
                    format!("Category of {n} repos: pick one or type a new name")
                } else {
                    "Category: pick one or type a new name".to_owned()
                };
                let groups = self.core.sidebar_groups();
                let count = |c: &str| {
                    groups
                        .iter()
                        .find(|g| g.category.as_deref() == Some(c))
                        .map_or(0, |g| g.rows.len())
                };
                let mut items: Vec<PaletteItem> = self
                    .core
                    .categories()
                    .into_iter()
                    .map(|c| PaletteItem {
                        detail: match count(&c) {
                            1 => "1 repo".to_owned(),
                            k => format!("{k} repos"),
                        },
                        id: c.clone(),
                        label: c,
                    })
                    .collect();
                items.push(PaletteItem {
                    id: String::new(),
                    label: "No category".into(),
                    detail: "listed under Uncategorized".into(),
                });
                return self.show_palette(kind, title, items, true, window, cx);
            }
        };
        self.show_palette(kind, title, items, prompt, window, cx);
    }

    fn show_palette(
        &mut self,
        kind: PaletteKind,
        title: impl Into<SharedString>,
        items: Vec<PaletteItem>,
        prompt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = cx.new(|cx| Palette::new(title, items, prompt, window, cx));
        let sub = cx.subscribe_in(
            &palette,
            window,
            move |this: &mut Self, _, event: &PaletteEvent, window, cx| {
                this.overlay = None;
                this.focus.focus(window, cx);
                if let PaletteEvent::Confirmed(value) = event {
                    this.on_palette(kind, value.clone(), window, cx);
                }
                cx.notify();
            },
        );
        self.overlay = Some((Overlay::Palette(palette), sub));
        cx.notify();
    }

    /// Dismisses the open overlay (as Escape / Close would).
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn on_palette(
        &mut self,
        kind: PaletteKind,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.selected_or_all();
        match kind {
            PaletteKind::Projects => match value.as_str() {
                "+folder" => self.new_project_from_folder(cx),
                "+file" => self.open_project_file(cx),
                id => {
                    let id = ProjectId(id.to_owned());
                    // Already shown by another window: focus it, no duplicate.
                    if !crate::windows::focus_project(cx, &id, Some(window.window_handle())) {
                        if let Err(e) = self.core.open_project(&id) {
                            self.notice = Some(e.to_string());
                        }
                    }
                }
            },
            PaletteKind::Actions => self.run_action(&value, window, cx),
            PaletteKind::Checkout => self.dispatch(Command::CheckoutAll(value, targets), cx),
            PaletteKind::Branch => self.dispatch(Command::CreateBranchAll(value, targets), cx),
            PaletteKind::Tag => self.dispatch(Command::CreateTagAll(value, targets), cx),
            PaletteKind::Color => self.dispatch(Command::SetAccent(Some(value)), cx),
            PaletteKind::BackgroundTint => {
                self.dispatch(Command::SetBackgroundTint(Some(value)), cx);
            }
            PaletteKind::Icon => self.dispatch(Command::SetIcon(Some(value)), cx),
            PaletteKind::Category => {
                let ids = std::mem::take(&mut self.category_targets);
                let category = Some(value).filter(|v| !v.is_empty());
                self.dispatch(Command::SetCategory(ids, category), cx);
            }
            PaletteKind::RenameCategory => {
                if let Some(old) = self.renaming_category.take() {
                    self.dispatch(Command::RenameCategory(old, Some(value)), cx);
                }
            }
        }
    }

    /// Opens the category palette for `targets`.
    fn set_category(&mut self, targets: Vec<RepoId>, window: &mut Window, cx: &mut Context<Self>) {
        if targets.is_empty() {
            return;
        }
        self.category_targets = targets;
        self.open_palette(PaletteKind::Category, window, cx);
    }

    fn run_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let ctx = self.core.action_context();
        let command = self
            .core
            .extensions()
            .actions
            .get(id)
            .and_then(|a| a.command(&ctx));
        match command {
            Some(Command::Ui(ui)) => self.run_ui_command(&ui, window, cx),
            Some(command) => self.dispatch(command, cx),
            None => {}
        }
    }

    fn run_ui_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "palette.projects" => self.open_palette(PaletteKind::Projects, window, cx),
            "palette.actions" => self.open_palette(PaletteKind::Actions, window, cx),
            "project.new" => self.new_project_from_folder(cx),
            "project.add_repos" => self.add_repos(cx),
            "commit.open" | "commit.submit" => self.open_commit(window, cx),
            "repos.checkout_all" => self.open_palette(PaletteKind::Checkout, window, cx),
            "repos.branch_all" => self.open_palette(PaletteKind::Branch, window, cx),
            "repos.tag_all" => self.open_palette(PaletteKind::Tag, window, cx),
            "settings.appearance" => {
                let size = window.viewport_size();
                self.theme_menu = Some(Point::new(size.width, size.height));
            }
            "app.copy_diagnostics" => self.copy_diagnostics(cx),
            "window.new" => crate::windows::open(cx, None),
            "project.set_color" => self.open_palette(PaletteKind::Color, window, cx),
            "project.set_background_tint" => {
                self.open_palette(PaletteKind::BackgroundTint, window, cx);
            }
            "project.set_icon" => self.open_palette(PaletteKind::Icon, window, cx),
            "project.set_icon_image" => self.pick_icon_image(cx),
            "repos.set_category" => {
                let targets = self.core.action_context().selected;
                self.set_category(targets, window, cx);
            }
            "forge.accounts" => self.open_forge(ForgeTab::Accounts, window, cx),
            "forge.create_pull_requests" => {
                let ids = self.selected_or_all();
                self.open_forge(ForgeTab::PullRequests(ids), window, cx);
            }
            "forge.clone" => self.open_forge(ForgeTab::Clone, window, cx),
            "forge.open_pull_request" => self.open_pull_request(cx),
            "forge.open_ci" => self.open_ci(cx),
            other => self.notice = Some(format!("“{other}” is not available in this build")),
        }
        cx.notify();
    }

    fn open_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.project().is_none() {
            return;
        }
        let core = self.core.clone();
        let panel = cx.new(|cx| CommitPanel::new(core, window, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this: &mut Self, _, event: &CommitPanelEvent, window, cx| {
                match event {
                    CommitPanelEvent::Closed => {
                        this.overlay = None;
                        this.focus.focus(window, cx);
                    }
                    CommitPanelEvent::Finished(summary) => this.notice = Some(summary.clone()),
                }
                cx.notify();
            },
        );
        self.overlay = Some((Overlay::Commit(panel), sub));
    }

    /// Opens the forge panel on `tab` (reusing it when already open).
    fn open_forge(&mut self, tab: ForgeTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((Overlay::Forge(panel), _)) = &self.overlay {
            panel.update(cx, |p, cx| p.show(tab, window, cx));
            return;
        }
        let core = self.core.clone();
        let panel = cx.new(|cx| ForgePanel::new(core, tab, window, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this: &mut Self, _, event: &ForgePanelEvent, window, cx| {
                match event {
                    ForgePanelEvent::Closed => {
                        this.overlay = None;
                        this.focus.focus(window, cx);
                    }
                    ForgePanelEvent::Notice(text) => this.notice = Some(text.clone()),
                }
                cx.notify();
            },
        );
        self.overlay = Some((Overlay::Forge(panel), sub));
        cx.notify();
    }

    /// Opens the pull request of the active / selected repo in the browser.
    fn open_pull_request(&mut self, cx: &mut Context<Self>) {
        let ctx = self.core.action_context();
        let target = ctx
            .active_repo
            .or_else(|| (ctx.selected.len() == 1).then(|| ctx.selected[0]));
        let url = self
            .core
            .rows()
            .into_iter()
            .find(|r| Some(r.id) == target)
            .and_then(|r| r.forge.pull_request().map(|pr| pr.url.clone()));
        match url {
            Some(url) => cx.open_url(&url),
            None => self.notice = Some("No open pull request for this repository".into()),
        }
    }

    fn open_ci(&mut self, cx: &mut Context<Self>) {
        let ctx = self.core.action_context();
        let target = ctx
            .active_repo
            .or_else(|| (ctx.selected.len() == 1).then(|| ctx.selected[0]));
        let url = self
            .core
            .rows()
            .into_iter()
            .find(|r| Some(r.id) == target)
            .and_then(|r| r.ci.run().and_then(|run| run.url.clone()));
        match url {
            Some(url) => cx.open_url(&url),
            None => self.notice = Some("No CI run for this branch".into()),
        }
    }

    fn set_theme(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.global_mut::<UiTheme>().set(id);
        crate::theme::sync_toolkit_theme(cx);
        if let Err(e) = cx.global::<UiTheme>().remember() {
            self.notice = Some(format!("Theme not remembered: {e}"));
        }
        window.refresh();
        crate::windows::refresh_all(cx);
    }

    fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let rss = memory_stats::memory_stats().map_or(0, |m| m.physical_mem / (1024 * 1024));
        let mut text = format!(
            "Archipel {}\nOS: {} {}\nRSS: {rss} MiB\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        for c in self.core.cache_report() {
            let _ = writeln!(
                text,
                "cache {}: {} entries, {} / {} bytes",
                c.name, c.entries, c.bytes, c.budget
            );
        }
        for (_, line, _) in self.log.iter().rev().take(50) {
            text.push_str(line);
            text.push('\n');
        }
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        self.notice = Some("Diagnostics copied to the clipboard".into());
    }

    // ----- projects -----------------------------------------------------

    fn pick(
        directories: bool,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, std::path::PathBuf, &mut Context<Self>) + 'static,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |w, cx| then(w, path, cx));
                }
            }
        })
        .detach();
    }

    #[allow(clippy::unused_self)] // called like every other view action
    fn pick_icon_image(&mut self, cx: &mut Context<Self>) {
        Self::pick(false, cx, |w, file, cx| {
            let icon = file.to_string_lossy().into_owned();
            w.dispatch(Command::SetIcon(Some(icon)), cx);
        });
    }

    #[allow(clippy::unused_self)] // called like every other view action
    fn new_project_from_folder(&mut self, cx: &mut Context<Self>) {
        Self::pick(true, cx, |w, dir, cx| {
            if let Err(e) = w.core.new_project_from_folder(&dir) {
                w.notice = Some(e.to_string());
            }
            cx.notify();
        });
    }

    #[allow(clippy::unused_self)] // called like every other view action
    fn open_project_file(&mut self, cx: &mut Context<Self>) {
        Self::pick(false, cx, |w, file, cx| {
            let result = w
                .core
                .import_project_file(&file)
                .and_then(|id| w.core.open_project(&id));
            if let Err(e) = result {
                w.notice = Some(e.to_string());
            }
            cx.notify();
        });
    }

    #[allow(clippy::unused_self)] // called like every other view action
    fn add_repos(&mut self, cx: &mut Context<Self>) {
        Self::pick(true, cx, |w, dir, cx| {
            let found = w.core.scan(&dir).unwrap_or_default();
            let paths: Vec<RepoPath> = if dir.join(".git").exists() {
                vec![RepoPath::new(dir)]
            } else {
                found.into_iter().map(|r| r.path).collect()
            };
            if let Err(e) = w.core.add_repos(&paths) {
                w.notice = Some(e.to_string());
            }
            cx.notify();
        });
    }

    // ----- rendering ----------------------------------------------------

    fn render_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let identity = self.core.identity();
        let groups = self.core.sidebar_groups();
        let selected = self
            .core
            .ui_state()
            .map(|u| u.batch_selection())
            .unwrap_or_default();
        let hover = color(cx, T::BgHover);
        let selected_bg = color(cx, T::BgSelected);
        // Headers only once at least one repo has a category (spec §1).
        let grouped = groups.len() > 1 || groups.first().is_some_and(|g| g.category.is_some());
        let repo_row = |row: DashboardRow, cx: &mut Context<Self>| {
            let id = row.id;
            let (token, extra) = side_status(&row.status);
            div()
                .id(SharedString::from(format!("side-{}", row.id)))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .when(grouped, Styled::pl_5)
                .h(row_height(cx))
                .rounded(radius(cx))
                .cursor_pointer()
                .when(selected.contains(&id), |d| d.bg(selected_bg))
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    // Unlike the dashboard, a plain click opens the repo.
                    let cmd = row_click(id, e);
                    if matches!(cmd, Command::Select(_)) {
                        this.dispatch(cmd, cx);
                        this.dispatch(Command::OpenRepoTab(id), cx);
                    } else {
                        this.dispatch(cmd, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.row_menu = Some((id, e.position));
                        cx.notify();
                    }),
                )
                .child(dot(if token == T::StatusSuccess {
                    clean_dot()
                } else {
                    color(cx, token)
                }))
                .child(div().flex_1().min_w_0().truncate().child(row.name))
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(extra),
                )
                .into_any_element()
        };
        // Fits the longest visible name (repo or category), never narrower
        // than its footer buttons; capped at half the window.
        let (size, xs) = (text_size(cx), rems(0.75).to_pixels(window.rem_size()));
        let measure = |text: &str, size| text_width(text, size, window, cx).ceil();
        // List padding (px_1) and right border, plus a pixel of slack so
        // rounding never truncates the longest name.
        let frame = 8.0 + 1.0 + 1.0;
        // Repo row: left padding (pl_5 grouped, else px_2), right padding,
        // dot, and the gaps around the name.
        let row_frame = if grouped { 20.0 } else { 8.0 } + 8.0 + 8.0 + 2.0 * 8.0;
        // Group header: padding (px_2), arrow, and the gaps around the name.
        let header_frame = 16.0 + 12.0 + 2.0 * 4.0;
        // Footer: padding, and the button's padding and border.
        let mut fit: f32 = 16.0 + 18.0 + measure("Add repos…", size);
        for group in &groups {
            if grouped {
                let name = group.category.as_deref().unwrap_or("Uncategorized");
                let count = group.rows.len().to_string();
                fit = fit.max(frame + header_frame + measure(name, xs) + measure(&count, xs));
            }
            if group.collapsed {
                continue;
            }
            for row in &group.rows {
                let extra = side_status(&row.status).1;
                fit = fit.max(frame + row_frame + measure(&row.name, size) + measure(&extra, xs));
            }
        }
        let fit = fit.min(f32::from(window.viewport_size().width) / 2.0);
        let mut repos: Vec<AnyElement> = Vec::new();
        for (i, group) in groups.into_iter().enumerate() {
            if grouped {
                let key = group.key().to_owned();
                // A collapsed group holding the selection stays noticeable.
                let holds_selection =
                    group.collapsed && group.rows.iter().any(|r| selected.contains(&r.id));
                repos.push(
                    div()
                        .id(SharedString::from(format!("side-group-{i}")))
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .h(row_height(cx))
                        .rounded(radius(cx))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(color(
                            cx,
                            if holds_selection {
                                T::Accent
                            } else {
                                T::FgSubtle
                            },
                        ))
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dispatch(Command::ToggleCategory(key.clone()), cx);
                        }))
                        .when_some(group.category.clone(), |d, category| {
                            d.on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                    this.category_menu = Some((category.clone(), e.position));
                                    cx.notify();
                                }),
                            )
                        })
                        .child(div().w(px(12.0)).flex_none().child(if group.collapsed {
                            "▸"
                        } else {
                            "▾"
                        }))
                        .child(
                            div().flex_1().min_w_0().truncate().child(
                                group
                                    .category
                                    .clone()
                                    .unwrap_or_else(|| "Uncategorized".into()),
                            ),
                        )
                        .child(div().flex_none().child(group.rows.len().to_string()))
                        .into_any_element(),
                );
            }
            if !group.collapsed {
                for row in group.rows {
                    repos.push(repo_row(row, cx));
                }
            }
        }
        div()
            .w(px(fit))
            .border_r_1()
            .border_color(color(cx, T::BorderSubtle))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(color(cx, T::BgPanel))
            .child(
                div()
                    .id("project-switcher")
                    .m_2()
                    .px_2()
                    .py_1()
                    .rounded(radius(cx))
                    .border_1()
                    .border_color(color(cx, T::Border))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_palette(PaletteKind::Projects, window, cx);
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .child("PROJECT  (Ctrl+P)"),
                    )
                    .child(match identity {
                        None => div().child("No project — open or create one"),
                        Some(identity) => div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_color(color(cx, T::AccentText))
                            .children(identity.icon.map(|icon| project_icon(icon, cx)))
                            .child(div().truncate().child(identity.name)),
                    }),
            )
            .child(section("REPOSITORIES", cx))
            .child(
                div()
                    .id("side-repos")
                    .flex_1()
                    .overflow_y_scrollbar()
                    .px_1()
                    .children(repos),
            )
            .child(
                div()
                    .p_2()
                    .flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .child(
                        button("add-repo", "Add repos…", cx)
                            .on_click(cx.listener(|this, _, _, cx| this.add_repos(cx))),
                    ),
            )
    }

    fn render_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.core.ui_state().unwrap_or_default();
        let names: HashMap<RepoId, String> = self
            .core
            .rows()
            .into_iter()
            .map(|r| (r.id, r.name))
            .collect();
        let tabs = ui.tabs.iter().enumerate().map(|(i, tab)| {
            let label = match tab {
                TabState::Dashboard => "Dashboard".to_owned(),
                TabState::Repo(id) => names.get(id).cloned().unwrap_or_else(|| "repo".into()),
            };
            let active = i == ui.active_tab;
            let mut el = div()
                .id(SharedString::from(format!("tab-{i}")))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h_full()
                .flex_shrink_0()
                .whitespace_nowrap()
                .cursor_pointer()
                .border_b_2()
                .border_color(if active {
                    color(cx, T::Accent)
                } else {
                    color(cx, T::BgApp)
                })
                // Inactive labels stay at full text color: the accent underline
                // alone marks the active tab.
                .text_color(color(cx, T::FgDefault))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let () = this.core.update_ui_state(|u| u.active_tab = i);
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.tab_menu = Some((i, e.position));
                        cx.notify();
                    }),
                )
                // Every tab takes drops (on the dashboard: becomes the first
                // repo tab); the side bar shows where the dragged tab lands.
                .drag_over::<TabDrag>(move |s, d, _, cx| {
                    let s = s.bg(color(cx, T::BgHover));
                    if d.0 < i {
                        s.border_r_2().border_color(color(cx, T::Accent))
                    } else {
                        s.border_l_2().border_color(color(cx, T::Accent))
                    }
                })
                .on_drop(cx.listener(move |this, d: &TabDrag, _, cx| {
                    this.dispatch(Command::MoveTab { from: d.0, to: i }, cx);
                }))
                .child(label.clone());
            if i > 0 {
                let label = SharedString::from(label);
                el = el.on_drag(TabDrag(i), move |_, _, _, cx| {
                    cx.new(|_| TabGhost(label.clone()))
                });
                el = el.child(
                    div()
                        .id(SharedString::from(format!("tab-close-{i}")))
                        .text_color(color(cx, T::FgMuted))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Stop the click bubbling to the tab itself, whose handler
                            // would otherwise activate index `i` — now the next tab.
                            cx.stop_propagation();
                            this.dispatch(Command::CloseTab(i), cx);
                        }))
                        .child("×"),
                );
            }
            el
        });
        div()
            .h(px(34.0))
            .flex()
            .flex_none()
            .items_end()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .children(tabs)
    }

    fn render_dashboard_toolbar(&mut self, ui: &UiState, cx: &mut Context<Self>) -> AnyElement {
        let active = &ui.filters;
        let ext = self.core.extensions().clone();
        let hidden = self.core.hidden_count();
        let view_chips = div()
            .flex()
            .gap_1()
            .child(
                chip("view-tree", "Tree", ui.show_dependency_tree, cx).on_click(
                    cx.listener(|this, _, _, cx| this.dispatch(Command::ToggleDependencyTree, cx)),
                ),
            )
            .when(hidden > 0 || ui.show_hidden, |d| {
                d.child(
                    chip(
                        "view-hidden",
                        format!("Hidden ({hidden})"),
                        ui.show_hidden,
                        cx,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.dispatch(Command::ToggleShowHidden, cx)),
                    ),
                )
            });
        let scan = match self.scan {
            Some((found, true)) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!("Scanning the folder… {found} found"))
                    .child(button("scan-stop", "Stop", cx).on_click(
                        cx.listener(|this, _, _, cx| this.dispatch(Command::CancelScan, cx)),
                    )),
            ),
            _ => None,
        };
        let category_dropdown = self.render_category_filter(ui, cx);
        let filters = ext.filters.filters().iter().map(|f| {
            let id = f.id().to_owned();
            let active = active.contains(&id);
            chip(
                SharedString::from(format!("filter-{id}")),
                f.label().to_owned(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.dispatch(Command::ToggleFilter(id.clone()), cx);
            }))
        });
        div()
            .flex()
            .flex_wrap()
            .justify_between()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .child(div().flex().flex_wrap().gap_1().children(filters))
                    .children(category_dropdown)
                    .child(view_chips)
                    .children(scan),
            )
            .into_any_element()
    }

    fn render_dashboard(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.core.project().is_none() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(color(cx, T::FgMuted))
                .child("Archipel works on projects: groups of repositories.")
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            primary_button("new-project", "New project from a folder…", cx)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.new_project_from_folder(cx)),
                                ),
                        )
                        .child(
                            button("open-project", "Open an archipel.toml…", cx)
                                .on_click(cx.listener(|this, _, _, cx| this.open_project_file(cx))),
                        ),
                )
                .into_any_element();
        }
        let ui = self.core.ui_state().unwrap_or_default();
        let ext = self.core.extensions().clone();
        let rows = self.core.visible_rows();
        let now = Timestamp::now();
        let toolbar = self.render_dashboard_toolbar(&ui, cx);
        let widths = WIDTHS;
        let header = div()
            .flex()
            .px_3()
            .h(row_height(cx))
            .items_center()
            .text_xs()
            .text_color(color(cx, T::FgSubtle))
            .children(ext.columns.columns().iter().enumerate().map(|(i, col)| {
                let id = col.id().to_owned();
                let arrow = match (
                    ui.sort_column.as_deref() == Some(col.id()),
                    ui.sort_direction,
                ) {
                    (true, SortDirection::Ascending) => " ▲",
                    (true, SortDirection::Descending) => " ▼",
                    _ => "",
                };
                div()
                    .id(SharedString::from(format!("col-{id}")))
                    .flex_grow(widths.get(i).copied().unwrap_or(1.0))
                    .flex_basis(px(0.0))
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dispatch(Command::SortBy(id.clone()), cx);
                    }))
                    .child(format!("{}{arrow}", col.header().to_uppercase()))
            }));
        let selected = ui.batch_selection();
        let body: Vec<AnyElement> = if ui.show_dependency_tree {
            let tree = self.core.dependency_tree();
            self.render_tree(&tree, &rows, &selected, cx)
        } else {
            rows.iter()
                .map(|row| {
                    let cells = row_cells(&ext, row, now, 0, cx);
                    Self::row_shell(row, &selected, cx)
                        .children(cells)
                        .into_any_element()
                })
                .collect()
        };
        // A repo removed or hidden since its pane opened closes the pane.
        if let Some((pane, _)) = &self.quick_edit {
            let repo = pane.read(cx).repo();
            if !rows.iter().any(|r| r.id == repo) {
                self.quick_edit = None;
            }
        }
        let pane = self.quick_edit.as_ref().map(|(p, _)| p.clone());
        // A file clicked in the pane shows its diff in place of the rows.
        let diff = pane
            .as_ref()
            .and_then(|p| p.update(cx, QuickEdit::render_diff));
        let diff_shown = diff.is_some();
        let main = match diff {
            Some(diff) => diff,
            None => div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(toolbar)
                .child(header)
                .child(
                    div()
                        .id("dash-rows")
                        .flex_1()
                        .overflow_y_scrollbar()
                        .children(body),
                )
                .into_any_element(),
        };
        let width = ui.quick_edit_width;
        // The pane lies over the rows' right edge instead of narrowing them,
        // so toggling it leaves the list in place. A diff (only shown with
        // the pane open) is kept clear of it, splitter included.
        let main = if diff_shown {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .pr(px(width + 5.0))
                .child(main)
                .into_any_element()
        } else {
            main
        };
        div()
            .flex_1()
            .flex()
            .relative()
            .overflow_hidden()
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<PaneDrag>, _, cx| {
                // Every row listening for `PaneDrag` sees every splitter's
                // drag: only the quick edit pane is ours.
                let pane = e.drag(cx).0;
                if pane != Pane::QuickEdit {
                    return;
                }
                let (x, bounds) = (e.event.position.x, e.bounds);
                this.core
                    .update_ui_state(|ui| resize_pane(ui, pane, x, bounds));
                cx.notify();
            }))
            .child(main)
            .when_some(pane, |d, pane| {
                d.child(
                    div()
                        .id("quick-edit-overlay")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .flex()
                        .occlude()
                        .child(splitter(Pane::QuickEdit, cx).bg(color(cx, T::BgPanel)))
                        .child(div().w(px(width)).h_full().flex().flex_col().child(pane)),
                )
            })
            .into_any_element()
    }

    /// The clickable frame of a dashboard row (selection, dimming, menus).
    fn row_shell(
        row: &DashboardRow,
        selected: &[RepoId],
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = row.id;
        let hover = color(cx, T::BgHover);
        let selected_bg = color(cx, T::BgSelected);
        div()
            .id(SharedString::from(format!("dash-{id}")))
            .flex()
            .items_center()
            .px_3()
            .py_1()
            .min_h(row_height(cx))
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .cursor_pointer()
            .when(selected.contains(&id), |d| d.bg(selected_bg))
            .when(row.stale && !row.hidden, |d| d.opacity(0.75))
            .when(row.hidden, |d| d.opacity(0.45))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                let command = row_click(id, e);
                let plain = command == Command::Select(Some(id));
                this.dispatch(command, cx);
                if plain {
                    this.toggle_quick_edit(id, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.row_menu = Some((id, e.position));
                    cx.notify();
                }),
            )
    }

    /// The dependency tree (spec §9): project repos keep their dashboard
    /// columns, with their update buttons on hover when they are
    /// submodules; other submodules show their state and actions.
    fn render_tree(
        &self,
        tree: &[TreeRow],
        rows: &[DashboardRow],
        selected: &[RepoId],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let ext = self.core.extensions().clone();
        let now = Timestamp::now();
        let by_id: HashMap<RepoId, &DashboardRow> = rows.iter().map(|r| (r.id, r)).collect();
        let rest: f32 = WIDTHS[1..].iter().sum();
        let opening = self.opening.as_ref();
        tree.iter()
            .enumerate()
            .map(|(i, node)| {
                let key = node.key.clone();
                let arrow = match (node.has_children, node.collapsed) {
                    (false, _) => " ",
                    (true, true) => "▸",
                    (true, false) => "▾",
                };
                let out_of_sync = node.state.as_ref().is_some_and(|s| s.out_of_sync);
                let name = div()
                    .flex_grow(WIDTHS[0])
                    .flex_basis(px(0.0))
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .font_family(crate::theme::mono_font(cx))
                            .text_color(color(cx, T::FgSubtle))
                            .child(node.prefix.clone()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("tree-toggle-{i}")))
                            .flex_none()
                            .w(px(16.0))
                            .text_color(color(cx, T::FgMuted))
                            .when(node.has_children, gpui_kit::Styled::cursor_pointer)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.dispatch(Command::ToggleTreeNode(key.clone()), cx);
                            }))
                            .child(arrow),
                    )
                    .child(div().truncate().child(node.name.clone()))
                    .when(out_of_sync, |d| {
                        d.child(
                            div()
                                .ml_2()
                                .text_xs()
                                .text_color(color(cx, T::StatusConflict))
                                .child("≠ pinned"),
                        )
                    });
                match node.repo.and_then(|id| by_id.get(&id)) {
                    Some(row) => {
                        let cells = row_cells(&ext, row, now, 1, cx);
                        let actions = Self::node_actions(i, node, true, opening, cx);
                        let group = SharedString::from(format!("tree-row-{i}"));
                        let hover = color(cx, T::BgHover);
                        Self::row_shell(row, selected, cx)
                            .relative()
                            .group(group.clone())
                            .child(name)
                            .children(cells)
                            .when(!actions.is_empty(), |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .right_0()
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .bg(hover)
                                        .opacity(0.0)
                                        .group_hover(group, |s| s.opacity(1.0))
                                        .children(actions),
                                )
                            })
                            .into_any_element()
                    }
                    None => div()
                        .id(SharedString::from(format!("tree-{i}")))
                        .flex()
                        .items_center()
                        .px_3()
                        .py_1()
                        .min_h(row_height(cx))
                        .border_b_1()
                        .border_color(color(cx, T::BorderSubtle))
                        .child(name)
                        .child(
                            div()
                                .flex_grow(rest)
                                .flex_basis(px(0.0))
                                .flex()
                                .items_center()
                                .gap_2()
                                .overflow_hidden()
                                .child(Self::node_state(node, cx))
                                .children(Self::node_actions(i, node, false, opening, cx)),
                        )
                        .into_any_element(),
                }
            })
            .collect()
    }

    fn node_state(node: &TreeRow, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = &node.state else {
            return div().into_any_element();
        };
        let short = |o: Option<archipel_model::Oid>| o.map_or_else(|| "?".into(), |o| o.short(8));
        let (text, token) = if !state.initialized {
            (
                format!("not initialized · pins {}", short(state.pinned)),
                T::FgMuted,
            )
        } else if state.out_of_sync {
            (
                format!(
                    "checked out {} ≠ pinned {}",
                    short(state.checked_out),
                    short(state.pinned)
                ),
                T::StatusConflict,
            )
        } else {
            (format!("at {}", short(state.checked_out)), T::FgMuted)
        };
        div()
            .flex()
            .gap_2()
            .text_xs()
            .child(div().truncate().text_color(color(cx, token)).child(text))
            .when(state.dirty, |d| {
                d.child(div().text_color(color(cx, T::StatusDirty)).child("dirty"))
            })
            .into_any_element()
    }

    /// A submodule node's buttons; a project repo (`in_project`) has its
    /// row's own open and menu, so it only gets the update buttons.
    fn node_actions(
        i: usize,
        node: &TreeRow,
        in_project: bool,
        opening: Option<&RepoPath>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(state) = node.state.clone() else {
            return Vec::new();
        };
        let update = |label: &'static str, init: bool, recursive: bool, cx: &mut Context<Self>| {
            let state = state.clone();
            button(SharedString::from(format!("tree-{label}-{i}")), label, cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.dispatch(
                        Command::UpdateSubmodules {
                            parent: state.parent.clone(),
                            paths: vec![state.rel_path.clone()],
                            init,
                            recursive,
                        },
                        cx,
                    );
                }))
                .into_any_element()
        };
        let mut out = Vec::new();
        if state.initialized {
            out.push(update("Update", false, false, cx));
            out.push(update("Update all", false, true, cx));
            if in_project {
                return out;
            }
            let path = node.path.clone();
            out.push(if opening == Some(&path) {
                div()
                    .text_color(color(cx, T::FgMuted))
                    .child("Opening…")
                    .into_any_element()
            } else {
                button(SharedString::from(format!("tree-open-{i}")), "Open", cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_submodule(path.clone(), cx);
                    }))
                    .into_any_element()
            });
            let path = node.path.clone();
            out.push(
                button(
                    SharedString::from(format!("tree-add-{i}")),
                    "Add to project",
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dispatch(Command::AddRepos(vec![path.clone()]), cx);
                }))
                .into_any_element(),
            );
        } else {
            out.push(update("Init", true, false, cx));
            out.push(update("Init all", true, true, cx));
        }
        out
    }

    /// The context menu of a repo row (right click).
    /// Row menu entry: open the repo's pull request, or create pull requests
    /// for the targeted repos (spec §8).
    fn forge_menu_item(
        &self,
        id: RepoId,
        targets: Vec<RepoId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = color(cx, T::BgHover);
        let open = self
            .core
            .rows()
            .into_iter()
            .find(|r| r.id == id)
            .and_then(|r| r.forge.pull_request().map(|pr| (pr.number, pr.url.clone())));
        let base = div()
            .id("menu-forge")
            .px_3()
            .py_1()
            .cursor_pointer()
            .hover(move |s| s.bg(hover));
        match open {
            Some((number, url)) => base
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.row_menu = None;
                    cx.open_url(&url);
                    cx.notify();
                }))
                .child(format!("Open pull request #{number}"))
                .into_any_element(),
            None => base
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.row_menu = None;
                    this.open_forge(ForgeTab::PullRequests(targets.clone()), window, cx);
                }))
                .child("Create pull request…")
                .into_any_element(),
        }
    }

    /// Row menu entry: open the CI run of the repo's branch, when it has one.
    fn ci_menu_item(&self, id: RepoId, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hover = color(cx, T::BgHover);
        let url = self
            .core
            .rows()
            .into_iter()
            .find(|r| r.id == id)
            .and_then(|r| r.ci.run().and_then(|run| run.url.clone()))?;
        Some(
            div()
                .id("menu-ci")
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.row_menu = None;
                    cx.open_url(&url);
                    cx.notify();
                }))
                .child("Open CI run")
                .into_any_element(),
        )
    }

    /// The category filter: one dropdown button whatever the number of
    /// categories, its menu a checklist (several can be selected).
    fn render_category_filter(
        &mut self,
        ui: &UiState,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let keys = self.core.category_filter_keys();
        if keys.is_empty() {
            return None;
        }
        let label_of = |key: &str| {
            if key.is_empty() {
                "Uncategorized".to_owned()
            } else {
                key.to_owned()
            }
        };
        let active: Vec<&String> = keys
            .iter()
            .filter(|k| ui.category_filters.contains(k))
            .collect();
        let summary = match active.as_slice() {
            [] => "Category: all".to_owned(),
            [one] => format!("Category: {}", label_of(one)),
            [first, rest @ ..] => format!("Category: {} +{}", label_of(first), rest.len()),
        };
        let button = chip(
            "category-filter",
            format!("{summary} ▾"),
            !active.is_empty(),
            cx,
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                let closed_now = this.category_filter_closed_at.take() == Some(e.position);
                this.category_filter_menu = match this.category_filter_menu {
                    Some(_) => None,
                    None if closed_now => None,
                    None => Some(e.position),
                };
                cx.stop_propagation();
                cx.notify();
            }),
        );
        let menu = self.category_filter_menu.map(|position| {
            let hover = color(cx, T::BgHover);
            let accent = color(cx, T::AccentText);
            let items = keys.iter().map(|key| {
                let checked = ui.category_filters.contains(key);
                let k = key.clone();
                div()
                    .id(SharedString::from(format!("category-filter-{key}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dispatch(Command::ToggleCategoryFilter(k.clone()), cx);
                    }))
                    .child(div().w(px(14.0)).text_color(accent).child(if checked {
                        "✓"
                    } else {
                        ""
                    }))
                    .child(label_of(key))
            });
            let clear = (!ui.category_filters.is_empty()).then(|| {
                let selected = ui.category_filters.clone();
                div()
                    .id("category-filter-clear")
                    .mt_1()
                    .pt_1()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .text_color(color(cx, T::FgMuted))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        for key in &selected {
                            this.dispatch(Command::ToggleCategoryFilter(key.clone()), cx);
                        }
                    }))
                    .child("Show all categories")
            });
            let menu = div()
                .id("category-filter-menu")
                .occlude()
                .min_w(px(180.0))
                .max_h(px(360.0))
                .overflow_y_scrollbar()
                .py_1()
                .flex()
                .flex_col()
                .font_family(ui_font(cx))
                .text_size(text_size(cx))
                .text_color(color(cx, T::FgDefault))
                .whitespace_nowrap()
                .bg(color(cx, T::BgElevated))
                .border_1()
                .border_color(color(cx, T::Border))
                .rounded(radius(cx))
                .shadow_md()
                .on_mouse_down_out(cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.category_filter_menu = None;
                    this.category_filter_closed_at = Some(e.position);
                    cx.notify();
                }))
                .children(items)
                .children(clear);
            deferred(anchored().position(position).snap_to_window().child(menu)).with_priority(1)
        });
        Some(div().child(button).children(menu).into_any_element())
    }

    /// Right-click menu of a sidebar category header: rename, remove.
    fn render_category_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (category, position) = self.category_menu.clone()?;
        let hover = color(cx, T::BgHover);
        let rename = category.clone();
        let menu = div()
            .occlude()
            .min_w(px(200.0))
            .py_1()
            .flex()
            .flex_col()
            .font_family(ui_font(cx))
            .text_size(text_size(cx))
            .text_color(color(cx, T::FgDefault))
            .whitespace_nowrap()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_md()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.category_menu = None;
                cx.notify();
            }))
            .child(
                div()
                    .id("category-menu-rename")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.category_menu = None;
                        this.renaming_category = Some(rename.clone());
                        this.open_palette(PaletteKind::RenameCategory, window, cx);
                    }))
                    .child("Rename category…"),
            )
            .child(
                div()
                    .id("category-menu-remove")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .text_color(color(cx, T::StatusError))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.category_menu = None;
                        this.dispatch(Command::RenameCategory(category.clone(), None), cx);
                    }))
                    .child("Remove category (repos stay in the project)"),
            );
        Some(
            deferred(anchored().position(position).snap_to_window().child(menu))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// Right-click menu of a tab: close it, the others, those to its right, all.
    fn render_tab_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (index, position) = self.tab_menu?;
        let count = self.core.ui_state()?.tabs.len();
        let mut entries = Vec::new();
        if index > 0 {
            entries.push(("Close", Command::CloseTab(index)));
            entries.push(("Close others", Command::CloseOtherTabs(index)));
        }
        if index + 1 < count {
            entries.push(("Close tabs to the right", Command::CloseTabsToRight(index)));
        }
        let rows = entries.into_iter().enumerate().map(|(k, (label, cmd))| {
            menu_item(("tab-menu", k), label, false, cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab_menu = None;
                    this.dispatch(cmd.clone(), cx);
                }))
                .into_any_element()
        });
        let menu = context_menu(cx)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.tab_menu = None;
                cx.notify();
            }))
            .children(rows)
            .when(index > 0 || index + 1 < count, |m| {
                m.child(menu_separator(cx))
            })
            .child(
                menu_item("tab-menu-all", "Close all", false, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.tab_menu = None;
                        this.dispatch(Command::CloseAllTabs, cx);
                    },
                )),
            );
        Some(
            deferred(anchored().position(position).snap_to_window().child(menu))
                .with_priority(1)
                .into_any_element(),
        )
    }

    fn render_row_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, position) = self.row_menu?;
        let ui = self.core.ui_state()?;
        // Acting on a selected row acts on the whole selection.
        let selection = ui.batch_selection();
        let targets = if selection.contains(&id) {
            selection
        } else {
            vec![id]
        };
        let all_hidden = targets.iter().all(|r| ui.is_hidden(*r));
        let n = targets.len();
        let hover = color(cx, T::BgHover);
        let subtle = color(cx, T::FgSubtle);
        let item = |eid: &'static str,
                    label: String,
                    shortcut: Option<&'static str>,
                    command: Command,
                    cx: &mut Context<Self>| {
            div()
                .id(eid)
                .flex()
                .justify_between()
                .gap_6()
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.row_menu = None;
                    this.dispatch(command.clone(), cx);
                }))
                .child(label)
                .children(shortcut.map(|k| div().text_color(subtle).child(k)))
        };
        let plural = if n > 1 {
            format!(" {n} repos")
        } else {
            String::new()
        };
        let (hide_label, hide_shortcut) = if all_hidden {
            (format!("Unhide{plural}"), None)
        } else {
            (format!("Hide{plural}"), Some("Ctrl+H"))
        };
        let menu = div()
            .occlude()
            .min_w(px(200.0))
            .py_1()
            .flex()
            .flex_col()
            .font_family(ui_font(cx))
            .text_size(text_size(cx))
            .text_color(color(cx, T::FgDefault))
            .whitespace_nowrap()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_md()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.row_menu = None;
                cx.notify();
            }))
            .child(item(
                "menu-open",
                "Open".into(),
                None,
                Command::OpenRepoTab(id),
                cx,
            ))
            .child(item(
                "menu-hide",
                hide_label,
                hide_shortcut,
                Command::ToggleHidden(targets.clone()),
                cx,
            ))
            .child({
                let targets = targets.clone();
                div()
                    .id("menu-category")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.row_menu = None;
                        this.set_category(targets.clone(), window, cx);
                    }))
                    .child(if n > 1 {
                        format!("Set category of{plural}…")
                    } else {
                        "Set category…".to_owned()
                    })
            })
            .child(item(
                "menu-fetch",
                format!("Fetch{plural}"),
                None,
                Command::Fetch(targets.clone()),
                cx,
            ))
            .child(self.forge_menu_item(id, targets, cx))
            .children(self.ci_menu_item(id, cx))
            .child(
                div()
                    .id("menu-remove")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .text_color(color(cx, T::StatusError))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.row_menu = None;
                        if let Err(e) = this.core.remove_repo(id) {
                            this.notice = Some(e.to_string());
                        }
                        cx.notify();
                    }))
                    .child("Remove from project"),
            );
        Some(
            deferred(anchored().position(position).snap_to_window().child(menu))
                .with_priority(1)
                .into_any_element(),
        )
    }

    fn render_drawer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.log.iter().rev().take(80).map(|(_, text, error)| {
            div()
                .text_xs()
                .font_family(crate::theme::mono_font(cx))
                .text_color(if *error {
                    color(cx, T::StatusError)
                } else {
                    color(cx, T::FgMuted)
                })
                .child(text.clone())
        });
        div()
            .id("drawer")
            .h(px(180.0))
            .flex_none()
            .overflow_y_scrollbar()
            .p_2()
            .bg(color(cx, T::BgPanel))
            .border_t_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(section("OPERATIONS  (Ctrl+J)", cx))
            .children(lines)
    }

    fn render_status(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.notice.clone().or_else(|| self.warnings.first().cloned()).unwrap_or_else(|| {
            "Ctrl+P switch project · Ctrl+Shift+P actions · Ctrl+K commit · Ctrl+Z undo · Ctrl+H hide · Ctrl+N new window".into()
        });
        div()
            .id("status")
            .h(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .px_3()
            .text_xs()
            .text_color(color(cx, T::FgMuted))
            .bg(color(cx, T::BgPanel))
            .border_t_1()
            .border_color(color(cx, T::BorderSubtle))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| {
                this.notice = None;
                cx.notify();
            }))
            .child(div().flex_1().min_w_0().truncate().child(text))
            .children(self.render_accent_picker(window, cx))
            .child(self.render_theme_picker(cx))
    }

    /// The status bar's project color button: a swatch of the window's
    /// accent, opening a color picker. None without a project.
    fn render_accent_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let accent = self.core.identity()?.accent;
        // Follow the accent (project switch, palette prompt) unless the
        // picker is open: its own changes come back here.
        self.accent_picker.update(cx, |state, cx| {
            let shown = state
                .value()
                .map(|c| gpui_kit::Hsla { a: 1.0, ..c }.to_hex());
            if !state.is_open() && shown.as_deref() != Some(accent.to_hex().as_str()) {
                state.set_value(crate::theme::hsla(accent), window, cx);
            }
        });
        Some(
            div()
                .id("accent-picker")
                .flex_none()
                .ml_2()
                // The status bar's click dismisses its notice; not this one.
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    ColorPicker::new(&self.accent_picker)
                        .xsmall()
                        .anchor(Anchor::BottomRight)
                        .accessibility_label("Project color"),
                ),
        )
    }

    /// The status bar's theme picker: the current theme's name, opening a
    /// list of every theme.
    fn render_theme_picker(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = cx.global::<UiTheme>();
        let current = ui.theme.id.clone();
        let themes: Vec<(String, String)> = ui
            .registry
            .themes()
            .map(|t| (t.id.clone(), t.name.clone()))
            .collect();
        let name = ui.theme.name.clone();
        let hover = color(cx, T::BgHover);
        let fg = color(cx, T::FgDefault);
        let label = div()
            .id("theme-picker")
            .flex_none()
            .ml_2()
            .px_1()
            .rounded(radius(cx))
            .whitespace_nowrap()
            .hover(move |s| s.text_color(fg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    let closed_now = this.theme_menu_closed_at.take() == Some(e.position);
                    this.theme_menu = match this.theme_menu {
                        Some(_) => None,
                        None if closed_now => None,
                        None => Some(e.position),
                    };
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            // The status bar's click dismisses its notice; not this one.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(format!("{name} ▾"));
        let menu = self.theme_menu.map(|position| {
            let accent = color(cx, T::AccentText);
            let items = themes.into_iter().map(|(id, name)| {
                let checked = id == current;
                div()
                    .id(SharedString::from(format!("theme-{id}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.theme_menu = None;
                        this.set_theme(&id, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(div().w(px(14.0)).text_color(accent).child(if checked {
                        "✓"
                    } else {
                        ""
                    }))
                    .child(name)
            });
            let menu = div()
                .id("theme-menu")
                .occlude()
                .min_w(px(160.0))
                .max_h(px(360.0))
                .overflow_y_scrollbar()
                .py_1()
                .flex()
                .flex_col()
                .font_family(ui_font(cx))
                .text_size(text_size(cx))
                .text_color(color(cx, T::FgDefault))
                .whitespace_nowrap()
                .bg(color(cx, T::BgElevated))
                .border_1()
                .border_color(color(cx, T::Border))
                .rounded(radius(cx))
                .shadow_md()
                .on_mouse_down_out(cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.theme_menu = None;
                    this.theme_menu_closed_at = Some(e.position);
                    cx.notify();
                }))
                .children(items);
            deferred(anchored().position(position).snap_to_window().child(menu)).with_priority(1)
        });
        div().child(label).children(menu)
    }

    fn active_repo_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<RepoView>> {
        let ui = self.core.ui_state()?;
        let TabState::Repo(id) = ui.tabs.get(ui.active_tab)? else {
            return None;
        };
        let row = self.core.rows().into_iter().find(|r| r.id == *id)?;
        let core = self.core.clone();
        Some(
            self.repo_views
                .entry(*id)
                .or_insert_with(|| cx.new(|cx| RepoView::new(core, row.path, row.name, window, cx)))
                .clone(),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window identity (spec §2): this window's project colors for every
        // view drawn below, and the project name first in the title.
        let identity = self.core.identity();
        crate::theme::apply_window_colors(
            cx,
            identity.as_ref().map(|i| i.accent),
            identity.as_ref().and_then(|i| i.background_tint),
        );
        let title = identity.as_ref().map_or_else(
            || "Archipel".to_owned(),
            archipel_app::ProjectIdentity::window_title,
        );
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let row_menu = self.render_row_menu(cx);
        let category_menu = self.render_category_menu(cx);
        let tab_menu = self.render_tab_menu(cx);
        let content: AnyElement = match self.active_repo_view(window, cx) {
            Some(view) => div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .child(view)
                .into_any_element(),
            None => self.render_dashboard(cx),
        };
        let drawer_open = self.core.drawer_open();
        let overlay: Option<AnyElement> = self.overlay.as_ref().map(|(o, _)| {
            let view = match o {
                Overlay::Palette(p) => p.clone().into_any_element(),
                Overlay::Commit(c) => c.clone().into_any_element(),
                Overlay::Forge(f) => f.clone().into_any_element(),
            };
            // A click outside the overlay (which occludes its own area)
            // dismisses it, without reaching what lies underneath.
            div()
                .id("overlay-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.close_overlay(window, cx);
                }))
                .child(view)
                .into_any_element()
        });
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(color(cx, T::BgApp))
            .text_color(color(cx, T::FgDefault))
            .font_family(ui_font(cx))
            .text_size(text_size(cx))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                this.quick_edit_key(e, window, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenProjects, w, cx| {
                this.open_palette(PaletteKind::Projects, w, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenActions, w, cx| {
                this.open_palette(PaletteKind::Actions, w, cx);
            }))
            .on_action(
                cx.listener(|this, _: &NextTab, _, cx| this.dispatch(Command::CycleTab(true), cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| {
                this.dispatch(Command::CycleTab(false), cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleDrawer, _, cx| {
                this.dispatch(Command::ToggleDrawer, cx);
            }))
            .on_action(
                cx.listener(|this, _: &Refresh, _, cx| this.dispatch(Command::RefreshAll, cx)),
            )
            .on_action(cx.listener(|this, _: &FetchAll, _, cx| {
                let t = this.selected_or_all();
                this.dispatch(Command::Fetch(t), cx);
            }))
            .on_action(cx.listener(|this, _: &OpenCommit, w, cx| this.open_commit(w, cx)))
            .on_action(cx.listener(|this, _: &ToggleHide, w, cx| {
                this.run_action("repos.toggle_hidden", w, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowHidden, _, cx| {
                this.dispatch(Command::ToggleShowHidden, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleTree, _, cx| {
                this.dispatch(Command::ToggleDependencyTree, cx);
            }))
            .on_action(cx.listener(|_, _: &NewWindow, _, cx| crate::windows::open(cx, None)))
            .on_action(cx.listener(|this, _: &SubmitCommit, w, cx| this.open_commit(w, cx)))
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                let ctx = this.core.action_context();
                if let Some(repo) = ctx.active_repo.or(ctx.selected.first().copied()) {
                    this.dispatch(Command::Undo(repo), cx);
                } else {
                    this.notice = Some("Select a repository to undo its last operation".into());
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                let ctx = this.core.action_context();
                if let Some(repo) = ctx.active_repo.or(ctx.selected.first().copied()) {
                    this.dispatch(Command::Redo(repo), cx);
                }
            }))
            .when(identity.is_some(), |d| {
                d.child(
                    div()
                        .h(px(BAND_HEIGHT))
                        .flex_none()
                        .bg(color(cx, T::Accent)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .child(self.render_sidebar(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(self.render_tabs(cx))
                            .child(content),
                    ),
            )
            .when(drawer_open, |d| d.child(self.render_drawer(cx)))
            .child(self.render_status(window, cx))
            .children(overlay)
            .children(row_menu)
            .children(category_menu)
            .children(tab_menu)
    }
}

/// A sidebar row's status dot color and its ahead/behind badge.
fn side_status(status: &RepoStatus) -> (T, String) {
    match status {
        RepoStatus::Ready(s) if s.changes.conflicted > 0 || s.in_progress.is_some() => {
            (T::StatusConflict, String::new())
        }
        RepoStatus::Ready(s) => {
            let sync = match (s.ahead(), s.behind()) {
                (0, 0) => String::new(),
                (a, 0) => format!("↑{a}"),
                (0, b) => format!("↓{b}"),
                (a, b) => format!("↑{a}↓{b}"),
            };
            (
                if s.changes.is_dirty() {
                    T::StatusDirty
                } else {
                    T::StatusSuccess
                },
                sync,
            )
        }
        RepoStatus::Loading => (T::FgSubtle, String::new()),
        RepoStatus::Missing | RepoStatus::Error(_) => (T::StatusError, String::new()),
    }
}

/// Width of one line of `text` in the UI font at `size`.
fn text_width(text: &str, size: Pixels, window: &Window, cx: &gpui_kit::App) -> f32 {
    let run = TextRun {
        len: text.len(),
        font: font(ui_font(cx)),
        color: color(cx, T::FgDefault),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.to_owned().into(), size, &[run], None);
    f32::from(line.width)
}

/// Plain click selects (and opens the quick edit pane), Ctrl+click adds to
/// the selection, double click opens.
fn row_click(id: RepoId, e: &ClickEvent) -> Command {
    if e.click_count() > 1 {
        Command::OpenRepoTab(id)
    } else if e.modifiers().control || e.modifiers().platform {
        Command::ToggleSelect(id)
    } else {
        Command::Select(Some(id))
    }
}

/// The dashboard cells of a row, from column `from` on.
fn row_cells(
    ext: &Extensions,
    row: &DashboardRow,
    now: Timestamp,
    from: usize,
    cx: &gpui_kit::App,
) -> Vec<AnyElement> {
    ext.columns
        .columns()
        .iter()
        .enumerate()
        .skip(from)
        .map(|(i, col)| {
            let cell = col.cell(row, now);
            let tooltip = col.tooltip(row);
            let detail = if i == 0 && row.hidden {
                Some(
                    cell.detail
                        .map_or_else(|| "hidden".to_owned(), |d| format!("hidden · {d}")),
                )
            } else {
                cell.detail
            };
            div()
                .id(i)
                .flex_grow(WIDTHS.get(i).copied().unwrap_or(1.0))
                .flex_basis(px(0.0))
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_color(tone(cx, cell.tone))
                        .child(cell.text),
                )
                .when(i == 0, |d| {
                    d.children(detail.map(|t| {
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .child(t)
                    }))
                })
                .when_some(tooltip, |d, lines| {
                    d.tooltip(move |_, cx| TextTooltip::view(&lines, cx))
                })
                .into_any_element()
        })
        .collect()
}

/// A project icon next to its name: the text, or the image sized to the
/// line (nothing is drawn when the file cannot be read).
fn project_icon(icon: ProjectIcon, cx: &gpui_kit::App) -> AnyElement {
    match icon {
        ProjectIcon::Text(text) => div().child(text).into_any_element(),
        ProjectIcon::Image(path) => img(path)
            .size(rems(1.25))
            .flex_none()
            .rounded(radius(cx))
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
    }
}
