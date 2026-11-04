//! The main window: sidebar, tabs, dashboard, operations drawer, overlays
//! (PLAN §3.4).

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

use archipel_app::dashboard::RepoStatus;
use archipel_app::{App as Core, AppEvent, Command};
use archipel_model::project::{ProjectDef, ProjectLocation, ProjectSettings, RepoEntry};
use archipel_model::ui_state::{SortDirection, TabState};
use archipel_model::{ProjectId, RepoId, RepoPath, Timestamp};
use archipel_ops::OpEventKind;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    Action, AnyElement, ClickEvent, Context, Entity, FocusHandle, PathPromptOptions, SharedString,
    Subscription, Window, div, px,
};

use crate::commit_panel::{CommitPanel, CommitPanelEvent};
use crate::palette::{Palette, PaletteEvent, PaletteItem};
use crate::repo_view::RepoView;
use crate::theme::{UiTheme, color, radius, row_height, text_size, tone, ui_font};
use crate::widgets::{button, chip, dot, primary_button, section};

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
        SubmitCommit
    ]
);

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
}

enum Overlay {
    Palette(Entity<Palette>),
    Commit(Entity<CommitPanel>),
}

/// Lines kept in the operations drawer.
const DRAWER_LINES: usize = 300;

/// The main window view.
pub struct Workspace {
    core: Arc<Core>,
    focus: FocusHandle,
    repo_views: HashMap<RepoId, Entity<RepoView>>,
    overlay: Option<(Overlay, Subscription)>,
    log: VecDeque<(Timestamp, String, bool)>,
    notice: Option<String>,
    warnings: Vec<String>,
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
        window.on_window_should_close(cx, |_, cx| {
            cx.quit();
            true
        });
        Self {
            core,
            focus,
            repo_views: HashMap::new(),
            overlay: None,
            log: VecDeque::new(),
            notice: None,
            warnings,
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
            AppEvent::ProjectOpened(_) => self.repo_views.clear(),
            AppEvent::RowsChanged(_) | AppEvent::UiStateChanged | AppEvent::ProjectsChanged => {}
        }
        cx.notify();
    }

    fn dispatch(&mut self, command: Command, cx: &mut Context<Self>) {
        if let Err(e) = self.core.dispatch(command) {
            self.notice = Some(e.to_string());
        }
        cx.notify();
    }

    fn selected_or_all(&self) -> Vec<RepoId> {
        self.core
            .ui_state()
            .and_then(|ui| ui.selected_repo)
            .into_iter()
            .collect()
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
                        label: "Open a .gitproject.toml…".into(),
                        detail: "shared project file".into(),
                    },
                ];
                for p in self.core.projects().unwrap_or_default() {
                    let detail = match &p.location {
                        ProjectLocation::SharedFile(f) => f.display().to_string(),
                        ProjectLocation::Private(_) => "private".into(),
                    };
                    items.push(PaletteItem {
                        id: p.id.0,
                        label: p.name,
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
        };
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
                    if let Err(e) = self.core.open_project(&ProjectId(id.to_owned())) {
                        self.notice = Some(e.to_string());
                    }
                }
            },
            PaletteKind::Actions => self.run_action(&value, window, cx),
            PaletteKind::Checkout => self.dispatch(Command::CheckoutAll(value, targets), cx),
            PaletteKind::Branch => self.dispatch(Command::CreateBranchAll(value, targets), cx),
            PaletteKind::Tag => self.dispatch(Command::CreateTagAll(value, targets), cx),
        }
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
            "settings.appearance" => self.cycle_theme(window, cx),
            "app.copy_diagnostics" => self.copy_diagnostics(cx),
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

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = cx.global::<UiTheme>();
        let ids: Vec<String> = theme.registry.ids().map(str::to_owned).collect();
        let current = ids.iter().position(|i| *i == theme.theme.id).unwrap_or(0);
        let next = ids
            .get((current + 1) % ids.len().max(1))
            .cloned()
            .unwrap_or_default();
        cx.global_mut::<UiTheme>().set(&next);
        self.notice = Some(format!("Theme: {}", cx.global::<UiTheme>().theme.name));
        window.refresh();
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
    fn new_project_from_folder(&mut self, cx: &mut Context<Self>) {
        Self::pick(true, cx, |w, dir, cx| {
            let result = w
                .core
                .scan(&dir)
                .map_err(|e| e.to_string())
                .and_then(|repos| {
                    if repos.is_empty() {
                        return Err(format!("no repository found below {}", dir.display()));
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
                            })
                            .collect(),
                        settings: ProjectSettings::default(),
                    };
                    let id = ProjectId(format!("dir:{}", dir.display()));
                    w.core
                        .create_project(ProjectLocation::Private(id.clone()), &def)
                        .map_err(|e| e.to_string())?;
                    w.core.open_project(&id).map_err(|e| e.to_string())
                });
            if let Err(e) = result {
                w.notice = Some(e);
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

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.core.project();
        let rows = self.core.rows();
        let selected = self.core.ui_state().and_then(|u| u.selected_repo);
        let hover = color(cx, T::BgHover);
        let selected_bg = color(cx, T::BgSelected);
        let repos = rows.into_iter().map(|row| {
            let id = row.id;
            let (token, extra) = match &row.status {
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
            };
            div()
                .id(SharedString::from(format!("side-{}", row.id)))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .h(row_height(cx))
                .rounded(radius(cx))
                .cursor_pointer()
                .when(selected == Some(id), |d| d.bg(selected_bg))
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    let command = if e.click_count() > 1 {
                        Command::OpenRepoTab(id)
                    } else {
                        Command::Select(Some(id))
                    };
                    this.dispatch(command, cx);
                }))
                .child(dot(token, cx))
                .child(div().flex_1().overflow_hidden().child(row.name))
                .child(
                    div()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(extra),
                )
        });
        div()
            .w(px(240.0))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(color(cx, T::BgPanel))
            .border_r_1()
            .border_color(color(cx, T::BorderSubtle))
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
                    .child(project.map_or_else(
                        || "No project — open or create one".to_owned(),
                        |(_, def)| def.name,
                    )),
            )
            .child(section("REPOSITORIES", cx))
            .child(
                div()
                    .id("side-repos")
                    .flex_1()
                    .overflow_y_scroll()
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
                    )
                    .child(
                        button("theme", "Theme", cx).on_click(
                            cx.listener(|this, _, window, cx| this.cycle_theme(window, cx)),
                        ),
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
                .cursor_pointer()
                .border_b_2()
                .border_color(if active {
                    color(cx, T::Accent)
                } else {
                    color(cx, T::BgApp)
                })
                .text_color(if active {
                    color(cx, T::FgDefault)
                } else {
                    color(cx, T::FgMuted)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    let () = this.core.update_ui_state(|u| u.active_tab = i);
                    cx.notify();
                }))
                .child(label);
            if i > 0 {
                el = el.child(
                    div()
                        .id(SharedString::from(format!("tab-close-{i}")))
                        .text_color(color(cx, T::FgSubtle))
                        .on_click(cx.listener(move |this, _, _, cx| {
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

    fn render_dashboard_toolbar(
        &mut self,
        active: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ext = self.core.extensions().clone();
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
        let batch =
            div()
                .flex()
                .gap_2()
                .child(
                    button("b-fetch", "Fetch", cx).on_click(cx.listener(|this, _, _, cx| {
                        let t = this.selected_or_all();
                        this.dispatch(Command::Fetch(t), cx);
                    })),
                )
                .child(
                    button("b-pull", "Pull", cx).on_click(cx.listener(|this, _, _, cx| {
                        let t = this.selected_or_all();
                        this.dispatch(Command::Pull(t), cx);
                    })),
                )
                .child(button("b-checkout", "Checkout…", cx).on_click(
                    cx.listener(|this, _, w, cx| this.open_palette(PaletteKind::Checkout, w, cx)),
                ))
                .child(button("b-branch", "Branch…", cx).on_click(
                    cx.listener(|this, _, w, cx| this.open_palette(PaletteKind::Branch, w, cx)),
                ))
                .child(button("b-tag", "Tag…", cx).on_click(
                    cx.listener(|this, _, w, cx| this.open_palette(PaletteKind::Tag, w, cx)),
                ))
                .child(
                    primary_button("b-commit", "Commit…", cx)
                        .on_click(cx.listener(|this, _, w, cx| this.open_commit(w, cx))),
                );
        div()
            .flex()
            .justify_between()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(div().flex().gap_1().children(filters))
            .child(batch)
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
                            button("open-project", "Open a .gitproject.toml…", cx)
                                .on_click(cx.listener(|this, _, _, cx| this.open_project_file(cx))),
                        ),
                )
                .into_any_element();
        }
        let ui = self.core.ui_state().unwrap_or_default();
        let ext = self.core.extensions().clone();
        let rows = self.core.visible_rows();
        let now = Timestamp::now();
        let toolbar = self.render_dashboard_toolbar(&ui.filters, cx);
        let widths = [2.0_f32, 1.4, 1.0, 1.2, 1.0, 0.8];
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
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dispatch(Command::SortBy(id.clone()), cx);
                    }))
                    .child(format!("{}{arrow}", col.header().to_uppercase()))
            }));
        let hover = color(cx, T::BgHover);
        let selected_bg = color(cx, T::BgSelected);
        let body = rows.iter().map(|row| {
            let id = row.id;
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
                .when(ui.selected_repo == Some(id), |d| d.bg(selected_bg))
                .when(row.stale, |d| d.opacity(0.75))
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    let command = if e.click_count() > 1 {
                        Command::OpenRepoTab(id)
                    } else {
                        Command::Select(Some(id))
                    };
                    this.dispatch(command, cx);
                }))
                .children(ext.columns.columns().iter().enumerate().map(|(i, col)| {
                    let cell = col.cell(row, now);
                    div()
                        .flex_grow(widths.get(i).copied().unwrap_or(1.0))
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
                            d.children(cell.detail.map(|t| {
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(color(cx, T::FgSubtle))
                                    .child(t)
                            }))
                        })
                }))
        });
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(toolbar)
            .child(header)
            .child(
                div()
                    .id("dash-rows")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(body),
            )
            .into_any_element()
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
            .overflow_y_scroll()
            .p_2()
            .bg(color(cx, T::BgPanel))
            .border_t_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(section("OPERATIONS  (Ctrl+J)", cx))
            .children(lines)
    }

    fn render_status(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let text = self.notice.clone().or_else(|| self.warnings.first().cloned()).unwrap_or_else(|| {
            "Ctrl+P switch project · Ctrl+Shift+P actions · Ctrl+K commit · Ctrl+Z undo · Ctrl+J operations".into()
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
            .child(text)
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
        let overlay: Option<AnyElement> = self.overlay.as_ref().map(|(o, _)| match o {
            Overlay::Palette(p) => p.clone().into_any_element(),
            Overlay::Commit(c) => c.clone().into_any_element(),
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
            .child(
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .child(self.render_sidebar(cx))
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
            .child(self.render_status(cx))
            .children(overlay)
    }
}
