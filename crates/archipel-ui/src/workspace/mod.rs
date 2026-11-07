//! The main window: sidebar, tabs, dashboard, operations drawer, overlays
//! (PLAN §3.4).
//!
//! The view is split by area: [`sidebar`], [`tabs`], [`dashboard`] and its
//! dependency [`tree`], [`menus`] (right-click menus), [`status_bar`] (and
//! the operations drawer), [`overlays`] (palettes, commit and forge
//! panels) and [`commands`] (actions run from the keymap or the palette).

mod commands;
mod dashboard;
mod menus;
mod overlays;
mod sidebar;
mod status_bar;
mod tabs;
mod tree;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use archipel_app::{App as Core, AppEvent, Command};
use archipel_model::ui_state::TabState;
use archipel_model::{RepoId, RepoPath, Timestamp};
use archipel_ops::OpEventKind;
use archipel_theme::ColorToken as T;
use gpui_kit::component::Colorize as _;
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Action, AnyElement, Context, Entity, FocusHandle, KeyDownEvent, Pixels, Point, Subscription,
    Window, div, px,
};

use crate::quick_edit::QuickEdit;
use crate::repo_view::RepoView;
use crate::theme::{color, text_size, ui_font};

use overlays::{Overlay, PaletteKind};

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

/// Lines kept in the operations drawer.
const DRAWER_LINES: usize = 300;

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
    /// Refreshes repos the watcher cannot fully cover when the window is
    /// activated.
    _activation: Subscription,
    /// Folder scan progress: repos found, still running.
    scan: Option<(usize, bool)>,
    /// Submodule being opened from the tree (adding it to the project
    /// re-watches every repo, which can take a moment).
    opening: Option<RepoPath>,
    /// Window title last set.
    title: String,
    /// Release whose status bar badge the user dismissed.
    dismissed_update: Option<String>,
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
        let activation = cx.observe_window_activation(window, |this, window, _| {
            if window.is_window_active() {
                this.core.on_focus();
            }
        });
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
            _activation: activation,
            scan: None,
            opening: None,
            title: String::new(),
            dismissed_update: None,
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
                        // Missed events: resync every repo as if each changed.
                        let resync = this.update(cx, |w, cx| {
                            let ids = w.core.rows().iter().map(|r| r.id).collect();
                            w.on_event(AppEvent::RowsChanged(ids), cx);
                        });
                        if resync.is_err() {
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
                self.overlay = None;
                self.category_targets.clear();
                self.renaming_category = None;
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
            AppEvent::UiStateChanged => self.drop_closed_repo_views(),
            AppEvent::ProjectsChanged
            | AppEvent::TreeChanged
            | AppEvent::ForgeAccountsChanged
            | AppEvent::UpdateAvailable(_) => {}
        }
        cx.notify();
    }

    /// Frees the views of repo tabs that were closed: each holds a graph
    /// and reloads on every refresh of its repo.
    fn drop_closed_repo_views(&mut self) {
        let Some(ui) = self.core.ui_state() else {
            return;
        };
        self.repo_views
            .retain(|id, _| ui.tabs.contains(&TabState::Repo(*id)));
    }

    fn dispatch(&mut self, command: Command, cx: &mut Context<Self>) {
        if let Err(e) = self.core.dispatch(command) {
            self.notice = Some(e.to_string());
        }
        cx.notify();
    }
}

impl Render for Workspace {
    #[allow(clippy::too_many_lines)] // declarative element tree
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
        let overlay = self.render_overlay(cx);
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
