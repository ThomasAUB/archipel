//! The staging panel shared by the repo tab and the dashboard's quick edit
//! pane: unstaged files, staged files (the "ready to commit" half, in the
//! accent color), then the commit box.

use std::time::Duration;

use archipel_app::forge::pick_remote;
use archipel_model::changes::{FileChange, FileState};
use archipel_model::ports::DiffSide;
use archipel_model::requests::{CommitMode, CommitRequest, PushRequest};
use archipel_model::{CancelToken, NoProgress, RepoPath};
use archipel_ops::{OpError, RepoActions};
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, SharedString, Window, div, px};

use crate::theme::{color, row_height};
use crate::widgets::{button, icon_button, primary_button, section};

/// How long a Discard button waits for the confirming second click.
const DISCARD_ARM_TIME: Duration = Duration::from_secs(3);

/// The armed-discard key of the "Discard all" button (no file is named so).
const ALL_FILES: &str = "\0all";

/// A write on the repo, run on a background thread.
pub type Job = Box<dyn FnOnce(&RepoActions, &RepoPath) -> Result<(), OpError> + Send>;

/// A view showing the staging panel.
pub trait Staging: Sized + 'static {
    /// The working copy changes.
    fn changes(&self) -> &[FileChange];
    /// The commit message input.
    fn message(&self) -> &Entity<InputState>;
    /// Whether the next commit amends, and the file whose Discard button
    /// was clicked once.
    fn commit_state(&mut self) -> (&mut bool, &mut Option<String>);
    /// Runs a write on the repo, then reloads.
    fn run_job(&mut self, label: String, cx: &mut Context<Self>, job: Job);
    /// Reports a refusal (nothing written).
    fn report(&mut self, text: String, cx: &mut Context<Self>);
    /// The file whose diff is open, highlighted.
    fn open_file(&self) -> Option<(&str, DiffSide)>;
    /// A click on a file: opens its diff, or closes it when open.
    fn click_file(
        &mut self,
        file: String,
        side: DiffSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
    /// The "Discard all" button: by default, a second click within
    /// [`DISCARD_ARM_TIME`] discards.
    fn discard_all(&mut self, files: Vec<String>, _window: &mut Window, cx: &mut Context<Self>) {
        arm_discard(self, ALL_FILES.to_owned(), cx, move |a, p| {
            a.discard(p, &files)
        });
    }
}

/// A file's Discard button: the first click arms it, a second one before
/// [`DISCARD_ARM_TIME`] discards.
fn click_discard<V: Staging>(v: &mut V, file: String, cx: &mut Context<V>) {
    let files = vec![file.clone()];
    arm_discard(v, file, cx, move |a, p| a.discard(p, &files));
}

fn arm_discard<V: Staging>(
    v: &mut V,
    key: String,
    cx: &mut Context<V>,
    job: impl FnOnce(&RepoActions, &RepoPath) -> Result<(), OpError> + Send + 'static,
) {
    let armed = v.commit_state().1;
    if armed.as_ref() == Some(&key) {
        *armed = None;
        v.run_job("Discard".into(), cx, Box::new(job));
        return;
    }
    *armed = Some(key.clone());
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(DISCARD_ARM_TIME).await;
        let _ = this.update(cx, |v, cx| {
            let armed = v.commit_state().1;
            if armed.as_ref() == Some(&key) {
                *armed = None;
                cx.notify();
            }
        });
    })
    .detach();
    cx.notify();
}

/// Commits the staged files (or amends), then pushes when `push`.
pub fn commit<V: Staging>(v: &mut V, push: bool, window: &mut Window, cx: &mut Context<V>) {
    let message = v.message().read(cx).value().to_string();
    if message.trim().is_empty() {
        v.report("Write a commit message first".into(), cx);
        return;
    }
    let staged = v.changes().iter().any(FileChange::is_staged);
    let amend = std::mem::take(v.commit_state().0);
    if !amend && !staged {
        v.report("Stage files first".into(), cx);
        return;
    }
    let mode = if amend {
        CommitMode::Amend
    } else {
        CommitMode::New
    };
    v.message()
        .clone()
        .update(cx, |m, cx| m.set_value("", window, cx));
    let label = if push { "Commit and push" } else { "Commit" };
    v.run_job(
        label.into(),
        cx,
        Box::new(move |a, p| {
            a.commit(p, &CommitRequest { message, mode }, None)?;
            if push { push_current(a, p) } else { Ok(()) }
        }),
    );
}

/// Pushes the current branch; a branch without upstream is pushed to the
/// default remote (`origin` first) and tracks it from then on.
pub fn push_current(a: &RepoActions, p: &RepoPath) -> Result<(), OpError> {
    let summary = a.ports().status.summary(p)?;
    let request = match (summary.head.branch(), &summary.upstream) {
        (Some(branch), None) => {
            let remotes = a.ports().remotes.remotes(p)?;
            let remote = pick_remote(&remotes, None)
                .ok_or_else(|| OpError::Refused("the repository has no remote".into()))?;
            PushRequest {
                remote: Some(remote.name.clone()),
                refspec: Some(branch.to_owned()),
                set_upstream: true,
                ..PushRequest::default()
            }
        }
        _ => PushRequest::default(),
    };
    a.push(p, &request, &NoProgress, &CancelToken::new(), None)
}

/// The panel: unstaged files, staged files, commit box.
pub fn render<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    for c in v.changes() {
        if c.is_staged() {
            staged.push(c.clone());
        }
        if c.is_unstaged() {
            unstaged.push(c.clone());
        }
    }
    let open = v.open_file().map(|(f, s)| (f.to_owned(), s));
    let (amend, armed) = v.commit_state();
    let (amend, armed) = (*amend, armed.clone());
    let staged_rows: Vec<_> = staged
        .iter()
        .map(|c| file_row(c, DiffSide::Staged, open.as_ref(), armed.as_deref(), cx))
        .collect();
    let unstaged_rows: Vec<_> = unstaged
        .iter()
        .map(|c| file_row(c, DiffSide::Unstaged, open.as_ref(), armed.as_deref(), cx))
        .collect();
    let unstaged_paths: Vec<String> = unstaged.iter().map(|c| c.path.clone()).collect();
    let staged_paths: Vec<String> = staged.iter().map(|c| c.path.clone()).collect();
    let discard_paths = unstaged_paths.clone();
    let danger = color(cx, T::StatusError);
    let unstaged_header = div()
        .flex()
        .items_center()
        .justify_between()
        .pr_2()
        .child(section(format!("UNSTAGED ({})", unstaged.len()), cx))
        .when(!unstaged_paths.is_empty(), |d| {
            let discard = if armed.as_deref() == Some(ALL_FILES) {
                button("discard-all", "Discard all?", cx)
                    .text_color(danger)
                    .border_color(danger)
            } else {
                button("discard-all", "Discard all", cx)
            };
            d.child(
                div()
                    .flex()
                    .gap_1()
                    .pt_2()
                    .child(
                        discard.on_click(cx.listener(move |v: &mut V, _, window, cx| {
                            v.discard_all(discard_paths.clone(), window, cx);
                        })),
                    )
                    .child(button("stage-all", "Stage all", cx).on_click(cx.listener(
                        move |v: &mut V, _, _, cx| {
                            let files = unstaged_paths.clone();
                            v.run_job(
                                "Stage all".into(),
                                cx,
                                Box::new(move |a, p| a.stage(p, &files)),
                            );
                        },
                    ))),
            )
        });
    // The staged half reads as "ready to commit": a hard line above it,
    // an accent-tinted header and an accent stripe along its files.
    let staged_header = div()
        .flex()
        .items_center()
        .justify_between()
        .pr_2()
        .pb_1()
        .border_t_1()
        .border_color(color(cx, T::Border))
        .bg(color(cx, T::AccentSubtle))
        .child(section(format!("STAGED ({})", staged.len()), cx).text_color(color(cx, T::Accent)))
        .when(!staged_paths.is_empty(), |d| {
            d.child(
                div().pt_2().child(
                    button("unstage-all", "Unstage all", cx).on_click(cx.listener(
                        move |v: &mut V, _, _, cx| {
                            let files = staged_paths.clone();
                            v.run_job(
                                "Unstage all".into(),
                                cx,
                                Box::new(move |a, p| a.unstage(p, &files)),
                            );
                        },
                    )),
                ),
            )
        });
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(unstaged_header)
        .child(
            div()
                .id("unstaged")
                .flex_1()
                .overflow_y_scrollbar()
                .children(unstaged_rows),
        )
        .child(staged_header)
        .child(
            div()
                .id("staged")
                .flex_1()
                .overflow_y_scrollbar()
                .border_l_2()
                .border_color(color(cx, T::Accent))
                .children(staged_rows),
        )
        .child(commit_box(v.message(), amend, cx))
        .into_any_element()
}

fn file_row<V: Staging>(
    c: &FileChange,
    side: DiffSide,
    open: Option<&(String, DiffSide)>,
    armed: Option<&str>,
    cx: &mut Context<V>,
) -> AnyElement {
    let hover = color(cx, T::BgHover);
    let path = c.path.clone();
    let (label, token) = match c.state {
        FileState::Untracked => ("+", T::StatusAhead),
        FileState::Conflicted => ("!", T::StatusConflict),
        FileState::Tracked { .. } => ("✎", T::StatusDirty),
    };
    let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
    let is_open = open.is_some_and(|(f, s)| *f == path && *s == side);
    let (action, icon) = if side == DiffSide::Staged {
        ("Unstage", IconName::Minus)
    } else {
        ("Stage", IconName::Plus)
    };
    let danger = color(cx, T::StatusError);
    div()
        .id(SharedString::from(format!("f-{side:?}-{path}")))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .h(row_height(cx))
        .cursor_pointer()
        .on_click(cx.listener(move |v: &mut V, _, window, cx| {
            v.click_file(p1.clone(), side, window, cx);
        }))
        .when(is_open, |d| d.bg(color(cx, T::BgSelected)))
        .hover(move |s| s.bg(hover))
        .child(
            div()
                .w(px(14.0))
                .flex_none()
                .text_color(color(cx, token))
                .child(label),
        )
        .child(div().flex_1().min_w_0().truncate().child(path))
        .when(side == DiffSide::Unstaged, |d| {
            let id = SharedString::from(format!("discard-{p3}"));
            let discard = if armed == Some(p3.as_str()) {
                button(id, "Discard?", cx)
                    .py_0()
                    .text_color(danger)
                    .border_color(danger)
            } else {
                icon_button(id, IconName::Undo2, "Discard (click twice)", cx)
            };
            // Kept apart from Stage so a slip does not hit it.
            d.child(
                discard
                    .mr_3()
                    .on_click(cx.listener(move |v: &mut V, _, _, cx| {
                        cx.stop_propagation();
                        click_discard(v, p3.clone(), cx);
                    })),
            )
        })
        .child(
            icon_button(
                SharedString::from(format!("stage-{side:?}-{p2}")),
                icon,
                action,
                cx,
            )
            .on_click(cx.listener(move |v: &mut V, _, _, cx| {
                cx.stop_propagation();
                let files = vec![p2.clone()];
                let job: Job = if side == DiffSide::Staged {
                    Box::new(move |a, p| a.unstage(p, &files))
                } else {
                    Box::new(move |a, p| a.stage(p, &files))
                };
                v.run_job(action.into(), cx, job);
            })),
        )
        .into_any_element()
}

fn commit_box<V: Staging>(
    message: &Entity<InputState>,
    amend: bool,
    cx: &mut Context<V>,
) -> AnyElement {
    div()
        .p_2()
        .flex()
        .flex_col()
        .gap_2()
        .border_t_1()
        .border_color(color(cx, T::BorderSubtle))
        .child(Input::new(message))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .id("amend")
                        .cursor_pointer()
                        .text_color(color(cx, if amend { T::Accent } else { T::FgMuted }))
                        .on_click(cx.listener(|v: &mut V, _, _, cx| {
                            let amend = v.commit_state().0;
                            *amend = !*amend;
                            cx.notify();
                        }))
                        .child(if amend {
                            "☑ Amend last commit"
                        } else {
                            "☐ Amend last commit"
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            button("commit-push", "Commit & push", cx).on_click(
                                cx.listener(|v: &mut V, _, w, cx| commit(v, true, w, cx)),
                            ),
                        )
                        .child(
                            primary_button("commit", "Commit", cx).on_click(
                                cx.listener(|v: &mut V, _, w, cx| commit(v, false, w, cx)),
                            ),
                        ),
                ),
        )
        .into_any_element()
}
