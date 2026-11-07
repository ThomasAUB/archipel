//! The dashboard's quick edit pane: a plain click on a repo row opens it
//! beside the rows, for the everyday edits that do not need the full repo
//! tab: sync (fetch / pull / push), stash, undo, create or switch branch,
//! stage / unstage / discard files, commit.

use std::sync::Arc;

use archipel_app::App as Core;
use archipel_app::dashboard::DashboardRow;
use archipel_model::changes::FileChange;
use archipel_model::ports::DiffSide;
use archipel_model::{HeadState, RepoId, RepoPath, RepoSummary};
use archipel_ops::OpClass;
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, EventEmitter, FocusHandle, KeyDownEvent, Window, div};

use crate::diff_view::{self, DiffDoc};
use crate::staging::{self, Job, Staging, StagingEvent};
use crate::theme::color;
use crate::widgets::{SHORT_OID, button, icon_button};

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
    /// Actions, branch, files and commit box, as in the repo tab.
    staging: staging::Panel,
    status: Option<String>,
    /// The working-copy file whose diff is open, in place of the
    /// dashboard rows; its diff is `None` while it loads.
    open_diff: Option<(String, DiffSide, Option<Arc<DiffDoc>>)>,
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
            name: row.name.clone(),
            changes: Vec::new(),
            staging,
            status: None,
            open_diff: None,
            focus: cx.focus_handle(),
        };
        pane.load_changes(cx);
        pane
    }

    /// The repo shown.
    pub fn repo(&self) -> RepoId {
        self.id
    }

    /// The repo was refreshed (file watcher, focus, F5).
    pub fn on_summary(&mut self, summary: Option<&RepoSummary>, cx: &mut Context<Self>) {
        staging::on_summary(self, summary, cx);
        self.load_changes(cx);
    }

    fn load_changes(&mut self, cx: &mut Context<Self>) {
        let status = self.core.repo_actions().ports().status.clone();
        let path = self.path.clone();
        let task =
            cx.background_spawn(async move { status.changes(&path).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(c) => v.changes = c,
                    Err(e) => v.status = Some(e),
                }
                // The open diff follows the file, as in the repo tab.
                if let Some((file, side, _)) = &v.open_diff {
                    let file = file.clone();
                    match diff_view::follow(&v.changes, &file, *side) {
                        Some(side) => v.open_worktree_file(file, side, cx),
                        None => v.open_diff = None,
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the diff of `file` on `side` in place of the dashboard rows.
    /// The diff shown stays until the new one is read when it is the same
    /// file (a reload), so the view does not flicker.
    fn open_worktree_file(&mut self, file: String, side: DiffSide, cx: &mut Context<Self>) {
        let shown = match self.open_diff.take() {
            Some((f, s, doc)) if f == file && s == side => doc,
            _ => None,
        };
        self.open_diff = Some((file.clone(), side, shown));
        let diffs = self.core.diff_reader();
        let path = self.path.clone();
        let task = {
            let file = file.clone();
            cx.background_spawn(async move {
                diffs
                    .worktree_diff(&path, side, Some(&file))
                    .map(|d| Arc::new(DiffDoc::new(d)))
            })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                let Some((f, s, doc)) = &mut v.open_diff else {
                    return;
                };
                if *f != file || *s != side {
                    return; // another file was opened meanwhile
                }
                match result {
                    Ok(d) => *doc = Some(d),
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Whether the pane itself has focus (not one of its inputs).
    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// The open diff, drawn by the workspace in place of the dashboard rows.
    pub fn render_diff(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (file, side, doc) = self.open_diff.clone()?;
        Some(diff_view::render_open(
            self,
            file,
            diff_view::side_note(side),
            Some(side),
            doc,
            cx,
        ))
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
        let Some((file, side, _)) = &self.open_diff else {
            return false;
        };
        let files = diff_view::worktree_files(&self.changes);
        if let Some((file, side)) = diff_view::step(&files, file, *side, delta) {
            self.open_worktree_file(file, side, cx);
        }
        true
    }

    /// Runs a write on a background thread, then refreshes the repo.
    fn run_boxed(&mut self, class: OpClass, label: String, cx: &mut Context<Self>, job: Job) {
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
                v.load_changes(cx);
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
                            ),
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
            .flex()
            .flex_col()
            .bg(color(cx, T::BgPanel))
            .track_focus(&self.focus)
            // Only Esc from here: ↑ / ↓ belong to the inputs.
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" && v.on_key("escape", cx) {
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

    fn run_job_as(&mut self, class: OpClass, label: String, cx: &mut Context<Self>, job: Job) {
        self.run_boxed(class, label, cx, job);
    }

    fn report(&mut self, text: String, cx: &mut Context<Self>) {
        self.status = Some(text);
        cx.notify();
    }

    fn open_file(&self) -> Option<(&str, DiffSide)> {
        self.open_diff.as_ref().map(|(f, s, _)| (f.as_str(), *s))
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
            self.open_worktree_file(file, side, cx);
        }
    }
}
