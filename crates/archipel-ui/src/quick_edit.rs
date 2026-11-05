//! The dashboard's quick edit pane: a plain click on a repo row opens it
//! beside the rows, for the everyday edits that do not need the full repo
//! tab: sync (fetch / pull / push), stash, undo, create or switch branch,
//! stage / unstage / discard files, commit.

use std::sync::Arc;

use archipel_app::dashboard::DashboardRow;
use archipel_app::{App as Core, Command};
use archipel_model::changes::FileChange;
use archipel_model::ports::DiffSide;
use archipel_model::requests::{
    CheckoutTarget, FetchRequest, PullMode, SequenceAction, StashApplyMode, StashRequest,
};
use archipel_model::{CancelToken, HeadState, NoProgress, RepoId, RepoOp, RepoPath, RepoSummary};
use archipel_ops::{OpError, RepoActions};
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Div, Entity, EventEmitter, FocusHandle, KeyDownEvent, SharedString,
    Stateful, Subscription, Window, div, px,
};

use crate::diff_view::{self, DiffDoc};
use crate::staging::{self, Job, Staging};
use crate::theme::{color, row_height};
use crate::widgets::{button, icon_button, section};

/// Width of the pane.
pub const WIDTH: f32 = 360.0;

/// Height of the branch switcher list before it scrolls.
const BRANCH_LIST_HEIGHT: f32 = 160.0;

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
    summary: Option<RepoSummary>,
    changes: Vec<FileChange>,
    /// Local branches other than the current one, while the switcher is
    /// open (`None`: closed).
    branches: Option<Vec<String>>,
    branch_input: Entity<InputState>,
    message: Entity<InputState>,
    amend: bool,
    /// The Discard button clicked once (see [`crate::staging`]).
    armed_discard: Option<String>,
    status: Option<String>,
    /// The working-copy file whose diff is open, in place of the
    /// dashboard rows; its diff is `None` while it loads.
    open_diff: Option<(String, DiffSide, Option<Arc<DiffDoc>>)>,
    focus: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<QuickEditEvent> for QuickEdit {}

impl QuickEdit {
    /// Opens the pane on a dashboard row and loads its working copy.
    pub fn new(
        core: Arc<Core>,
        row: &DashboardRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let branch_input = cx.new(|cx| InputState::new(window, cx).placeholder("New branch name"));
        let message = cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        let subscriptions = [
            cx.subscribe_in(
                &branch_input,
                window,
                |v, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        v.create_branch(window, cx);
                    }
                },
            ),
            cx.subscribe_in(&message, window, |v, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    staging::commit(v, false, window, cx);
                }
            }),
        ];
        let mut pane = Self {
            core,
            id: row.id,
            path: row.path.clone(),
            name: row.name.clone(),
            summary: row.summary().cloned(),
            changes: Vec::new(),
            branches: None,
            branch_input,
            message,
            amend: false,
            armed_discard: None,
            status: None,
            open_diff: None,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
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
        if let Some(s) = summary {
            self.summary = Some(s.clone());
        }
        self.load_changes(cx);
        if self.branches.is_some() {
            self.load_branches(cx);
        }
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

    /// Reads the local branches into the switcher (opening it).
    fn load_branches(&mut self, cx: &mut Context<Self>) {
        let refs = self.core.repo_actions().ports().refs.clone();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let branches = refs.branches(&path).map_err(|e| e.to_string())?;
            Ok::<_, String>(
                branches
                    .into_iter()
                    .filter(|b| !b.is_remote && !b.is_head)
                    .map(|b| b.name)
                    .collect::<Vec<_>>(),
            )
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(b) => v.branches = Some(b),
                    Err(e) => v.status = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_switcher(&mut self, cx: &mut Context<Self>) {
        if self.branches.take().is_none() {
            self.load_branches(cx);
        }
        cx.notify();
    }

    /// Runs a write on a background thread, then refreshes the repo.
    fn run(
        &mut self,
        label: impl Into<String>,
        cx: &mut Context<Self>,
        job: impl FnOnce(&RepoActions, &RepoPath) -> Result<(), OpError> + Send + 'static,
    ) {
        self.run_boxed(label.into(), cx, Box::new(job));
    }

    fn run_boxed(&mut self, label: String, cx: &mut Context<Self>, job: Job) {
        let actions = self.core.repo_actions().clone();
        let path = self.path.clone();
        let task = cx.background_spawn(async move { job(&actions, &path) });
        self.status = Some(format!("{label}…"));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                v.status = Some(match result {
                    Ok(()) => format!("{label}: done"),
                    Err(e) => format!("{label} failed: {e}"),
                });
                v.core.refresh_repo(v.id);
                v.load_changes(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Undo or redo through the app (it owns the journal), reporting a
    /// refusal in the pane.
    fn history(&mut self, command: Command, cx: &mut Context<Self>) {
        if let Err(e) = self.core.dispatch(command) {
            self.status = Some(e.to_string());
            cx.notify();
        }
    }

    /// Creates the branch of the name field on `HEAD` and checks it out.
    fn create_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.branch_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.status = Some("Write a branch name first".into());
            cx.notify();
            return;
        }
        self.branch_input
            .update(cx, |b, cx| b.set_value("", window, cx));
        self.run(format!("Create branch {name}"), cx, move |a, p| {
            a.create_branch(p, &name, None, None)?;
            a.checkout(p, &CheckoutTarget::Branch(name), None)
        });
    }

    fn switch_to(&mut self, branch: String, cx: &mut Context<Self>) {
        self.branches = None;
        self.run(format!("Checkout {branch}"), cx, move |a, p| {
            a.checkout(p, &CheckoutTarget::Branch(branch), None)
        });
    }

    // ----- rendering ----------------------------------------------------

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let (head, sync) = self.summary.as_ref().map_or_else(Default::default, |s| {
            let head = match &s.head {
                HeadState::Branch(b) | HeadState::Unborn(b) => b.clone(),
                HeadState::Detached(oid) => format!("detached at {}", oid.short(8)),
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

    /// Fetch, pull, push, stash, pop, undo, redo.
    fn render_actions(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let stashes = self.summary.as_ref().map_or(0, |s| s.stash_count);
        let job = |eid: &'static str,
                   label: &'static str,
                   cx: &mut Context<Self>,
                   job: fn(&RepoActions, &RepoPath) -> Result<(), OpError>| {
            small(button(eid, label, cx))
                .on_click(cx.listener(move |v, _, _, cx| v.run(label, cx, job)))
        };
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(job("qe-fetch", "Fetch", cx, |a, p| {
                a.fetch(
                    p,
                    &FetchRequest::default(),
                    &NoProgress,
                    &CancelToken::new(),
                )
            }))
            .child(job("qe-pull", "Pull", cx, |a, p| {
                a.pull(
                    p,
                    PullMode::FastForwardOnly,
                    &NoProgress,
                    &CancelToken::new(),
                    None,
                )
            }))
            .child(job("qe-push", "Push", cx, staging::push_current))
            .child(job("qe-stash", "Stash", cx, |a, p| {
                a.stash_push(p, &StashRequest::default()).map(drop)
            }))
            .when(stashes > 0, |d| {
                d.child(job("qe-pop", "Pop", cx, |a, p| {
                    a.stash_apply(p, 0, StashApplyMode::Pop)
                }))
            })
            .child(
                small(button("qe-undo", "Undo", cx))
                    .on_click(cx.listener(move |v, _, _, cx| v.history(Command::Undo(id), cx))),
            )
            .child(
                small(button("qe-redo", "Redo", cx))
                    .on_click(cx.listener(move |v, _, _, cx| v.history(Command::Redo(id), cx))),
            )
    }

    /// The merge / rebase / … in progress, with Continue and Abort.
    fn render_in_progress(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let op = self.summary.as_ref()?.in_progress?;
        let what = match op {
            RepoOp::Merge => "Merge",
            RepoOp::Rebase => "Rebase",
            RepoOp::CherryPick => "Cherry-pick",
            RepoOp::Revert => "Revert",
            // Not continued from here: the full tab or a terminal.
            RepoOp::Bisect | RepoOp::ApplyMailbox => return None,
        };
        let sequence = |eid: &'static str, label: &'static str, action, cx: &mut Context<Self>| {
            small(button(eid, label, cx)).on_click(cx.listener(move |v, _, _, cx| {
                v.run(
                    format!("{label} {}", what.to_lowercase()),
                    cx,
                    move |a, p| a.sequence(p, action),
                );
            }))
        };
        Some(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_2()
                .py_1()
                .border_1()
                .border_color(color(cx, T::StatusConflict))
                .text_color(color(cx, T::StatusConflict))
                .child(format!("{what} in progress"))
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(sequence(
                            "qe-continue",
                            "Continue",
                            SequenceAction::Continue,
                            cx,
                        ))
                        .child(sequence("qe-abort", "Abort", SequenceAction::Abort, cx)),
                )
                .into_any_element(),
        )
    }

    fn render_branch(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = color(cx, T::BgHover);
        let open = self.branches.is_some();
        let list = self.branches.clone().map(|branches| {
            let rows: Vec<AnyElement> = if branches.is_empty() {
                vec![
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(color(cx, T::FgSubtle))
                        .child("No other local branch")
                        .into_any_element(),
                ]
            } else {
                branches
                    .into_iter()
                    .map(|b| {
                        let name = b.clone();
                        div()
                            .id(SharedString::from(format!("qe-br-{b}")))
                            .px_2()
                            .h(row_height(cx))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(move |v, _, _, cx| v.switch_to(name.clone(), cx)))
                            .child(div().truncate().child(b))
                            .into_any_element()
                    })
                    .collect()
            };
            div()
                .id("qe-branches")
                .max_h(px(BRANCH_LIST_HEIGHT))
                .overflow_y_scrollbar()
                .border_1()
                .border_color(color(cx, T::BorderSubtle))
                .children(rows)
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.branch_input)))
                    .child(
                        button("qe-branch", "Create", cx)
                            .on_click(cx.listener(|v, _, w, cx| v.create_branch(w, cx))),
                    )
                    .child(
                        button("qe-switch", if open { "Switch ▴" } else { "Switch ▾" }, cx)
                            .on_click(cx.listener(|v, _, _, cx| v.toggle_switcher(cx))),
                    ),
            )
            .children(list)
    }
}

impl Render for QuickEdit {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("quick-edit")
            .w(px(WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .bg(color(cx, T::BgPanel))
            .border_l_1()
            .border_color(color(cx, T::Border))
            .track_focus(&self.focus)
            // Only Esc from here: ↑ / ↓ belong to the inputs.
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" && v.on_key("escape", cx) {
                    cx.stop_propagation();
                }
            }))
            .child(self.render_header(cx))
            .child(self.render_actions(cx))
            .children(self.render_in_progress(cx))
            .child(section("BRANCH", cx))
            .child(self.render_branch(cx))
            .child(staging::render(self, cx))
            .children(self.status.clone().map(|s| {
                div()
                    .text_xs()
                    .truncate()
                    .text_color(color(cx, T::FgMuted))
                    .child(s)
            }))
    }
}

/// A compact button for the pane's action rows.
fn small(b: Stateful<Div>) -> Stateful<Div> {
    b.py_0().text_xs()
}

impl Staging for QuickEdit {
    fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    fn message(&self) -> &Entity<InputState> {
        &self.message
    }

    fn commit_state(&mut self) -> (&mut bool, &mut Option<String>) {
        (&mut self.amend, &mut self.armed_discard)
    }

    fn run_job(&mut self, label: String, cx: &mut Context<Self>, job: Job) {
        self.run_boxed(label, cx, job);
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
