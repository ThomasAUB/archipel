//! The dashboard's quick edit pane: a plain click on a repo row opens it
//! beside the rows, for the everyday edits that do not need the full repo
//! tab: sync (fetch / pull / push), stash, undo, create or switch branch,
//! stage / unstage / discard files, commit.

use std::sync::Arc;

use archipel_app::App as Core;
use archipel_app::dashboard::DashboardRow;
use archipel_app::forecast::ConflictForecast;
use archipel_model::changes::FileChange;
use archipel_model::ports::DiffSide;
use archipel_model::{HeadState, RepoId, RepoPath, RepoSummary};
use archipel_ops::OpClass;
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, EventEmitter, FocusHandle, KeyDownEvent, Window, div};

use crate::diff_view;
use crate::forecast_badge::forecast_badge;
use crate::latest::Latest;
use crate::staging::{self, NotedJob, Staging, StagingEvent};
use crate::theme::{color, staging_surface};
use crate::widgets::{SHORT_OID, button, icon_button};
use crate::working_copy::{self, WorkingCopy, WorktreeDiff};

/// What the pane reports to the workspace.
pub enum QuickEditEvent {
    /// The pane was closed.
    Closed,
    /// Open the repo in its full tab.
    OpenTab(RepoId),
}

/// The pane of one repo.
pub struct QuickEdit {
    core: Arc<Core>,
    id: RepoId,
    path: RepoPath,
    name: String,
    changes: Vec<FileChange>,
    /// The reads of [`Self::changes`]: only the latest one lands.
    change_reads: Latest,
    /// Actions, branch, files and commit box, as in the repo tab.
    staging: staging::Panel,
    status: Option<String>,
    /// The working-copy file whose diff is open, in place of the
    /// dashboard rows.
    open_diff: Option<WorktreeDiff>,
    /// Whether the branch would conflict with its upstream or its target.
    forecast: ConflictForecast,
    focus: FocusHandle,
}

impl EventEmitter<QuickEditEvent> for QuickEdit {}
impl EventEmitter<StagingEvent> for QuickEdit {}

impl QuickEdit {
    /// Opens the pane on a dashboard row and loads its working copy.
    pub fn new(
        core: Arc<Core>,
        row: &DashboardRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let staging = staging::Panel::new(row.summary().cloned(), window, cx);
        let mut pane = Self {
            core,
            id: row.id,
            path: row.path.clone(),
            name: row.label(),
            changes: Vec::new(),
            change_reads: Latest::default(),
            staging,
            status: None,
            open_diff: None,
            forecast: row.forecast.clone(),
            focus: cx.focus_handle(),
        };
        working_copy::load_changes(&mut pane, cx);
        crate::conventional::refresh(&mut pane, cx);
        pane
    }

    /// The repo shown.
    pub fn repo(&self) -> RepoId {
        self.id
    }

    /// The repo was refreshed (file watcher, focus, F5).
    pub fn on_summary(&mut self, summary: Option<&RepoSummary>, cx: &mut Context<Self>) {
        staging::on_summary(self, summary, cx);
        working_copy::load_changes(self, cx);
    }

    /// The conflict forecast of the repo was computed again.
    pub fn on_forecast(&mut self, forecast: &ConflictForecast, cx: &mut Context<Self>) {
        if self.forecast != *forecast {
            self.forecast = forecast.clone();
            cx.notify();
        }
    }

    /// Whether the pane itself has focus (not one of its inputs).
    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// The open diff, drawn by the workspace in place of the dashboard rows.
    pub fn render_diff(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let WorktreeDiff { file, side, doc } = self.open_diff.clone()?;
        Some(diff_view::render_open(
            self,
            file,
            diff_view::side_note(side),
            Some(side),
            doc,
            None,
            window,
            cx,
        ))
    }

    /// A key for the open diff's text selection (Ctrl+C copies it, Esc
    /// drops it). Whether the key was used.
    pub fn on_diff_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(doc) = self.open_diff.as_ref().and_then(|d| d.doc.clone()) else {
            return false;
        };
        diff_view::on_key(self, &doc, e, cx)
    }

    /// A key while the dashboard shows: Esc closes the diff, else the
    /// pane; ↑ / ↓ move through the files while a diff is open. Returns
    /// whether the key was used.
    pub fn on_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let delta = match key {
            "escape" => {
                if self.open_diff.take().is_some() {
                    cx.notify();
                } else {
                    cx.emit(QuickEditEvent::Closed);
                }
                return true;
            }
            "down" => 1,
            "up" => -1,
            _ => return false,
        };
        working_copy::step_worktree_file(self, delta, cx)
    }

    /// Runs a write on a background thread, then refreshes the repo.
    fn run_boxed(&mut self, class: OpClass, label: String, cx: &mut Context<Self>, job: NotedJob) {
        self.status = Some(format!("{label}…"));
        staging::submit_job(
            &self.core,
            &self.path,
            class,
            label,
            job,
            cx,
            |v, status, cx| {
                v.status = Some(status);
                v.core.refresh_repo(v.id);
                working_copy::load_changes(v, cx);
            },
        );
        cx.notify();
    }

    // ----- rendering ----------------------------------------------------

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let (head, sync) = self.staging.summary().map_or_else(Default::default, |s| {
            let head = match &s.head {
                HeadState::Branch(b) | HeadState::Unborn(b) => b.clone(),
                HeadState::Detached(oid) => format!("detached at {}", oid.short(SHORT_OID)),
            };
            let sync = match &s.upstream {
                None if s.head.branch().is_some() => "no upstream".to_owned(),
                None => String::new(),
                Some(u) if u.gone => format!("{} gone", u.name),
                Some(u) => match (u.ahead, u.behind) {
                    (0, 0) => "up to date".to_owned(),
                    (a, 0) => format!("↑{a}"),
                    (0, b) => format!("↓{b}"),
                    (a, b) => format!("↑{a} ↓{b}"),
                },
            };
            (head, sync)
        });
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().child(self.name.clone()))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(color(cx, T::FgMuted))
                                    .child(head),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(color(cx, T::FgSubtle))
                                    .child(sync),
                            )
                            .children(forecast_badge(
                                "qe-forecast",
                                &self.forecast,
                                self.staging.summary(),
                                cx,
                            )),
                    ),
            )
            .child(
                button("qe-open", "Open", cx)
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(QuickEditEvent::OpenTab(id)))),
            )
            .child(
                icon_button("qe-close", IconName::Close, "Close (Esc)", cx)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(QuickEditEvent::Closed))),
            )
    }
}

impl Render for QuickEdit {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("quick-edit")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(staging_surface(cx))
            .track_focus(&self.focus)
            // Only Esc from here: ↑ / ↓ belong to the inputs.
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| {
                if v.on_diff_key(e, cx) || e.keystroke.key == "escape" && v.on_key("escape", cx) {
                    cx.stop_propagation();
                }
            }))
            .child(div().px_2().pt_2().child(self.render_header(cx)))
            // The same panel as the repo tab's uncommitted changes.
            .child(staging::render(self, cx))
            .children(self.status.clone().map(|s| {
                div()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .truncate()
                    .text_color(color(cx, T::FgMuted))
                    .child(s)
            }))
            .children(crate::ignore::render(self, cx))
    }
}

impl Staging for QuickEdit {
    fn core(&self) -> &Arc<Core> {
        &self.core
    }

    fn repo_path(&self) -> &RepoPath {
        &self.path
    }

    fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    fn panel(&mut self) -> &mut staging::Panel {
        &mut self.staging
    }

    fn run_noted_job(
        &mut self,
        class: OpClass,
        label: String,
        cx: &mut Context<Self>,
        job: NotedJob,
    ) {
        self.run_boxed(class, label, cx, job);
    }

    fn report(&mut self, text: String, cx: &mut Context<Self>) {
        self.status = Some(text);
        cx.notify();
    }

    fn open_file(&self) -> Option<(&str, DiffSide)> {
        self.open_diff.as_ref().map(|d| (d.file.as_str(), d.side))
    }

    fn keys_focus(&self) -> &FocusHandle {
        &self.focus
    }

    fn click_file(
        &mut self,
        file: String,
        side: DiffSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_file() == Some((&file, side)) {
            self.open_diff = None;
            cx.notify();
        } else {
            self.focus.focus(window, cx);
            working_copy::open_worktree_file(self, file, side, cx);
        }
    }
}

impl WorkingCopy for QuickEdit {
    fn change_reads(&mut self) -> &mut Latest {
        &mut self.change_reads
    }

    fn set_changes(&mut self, changes: Vec<FileChange>) {
        self.changes = changes;
    }

    fn worktree_diff(&mut self) -> Option<&mut WorktreeDiff> {
        self.open_diff.as_mut()
    }

    fn show_worktree_diff(&mut self, diff: Option<WorktreeDiff>) {
        self.open_diff = diff;
    }
}
