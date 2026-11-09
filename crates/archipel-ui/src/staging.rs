//! The staging panel shared by the repo tab and the dashboard's quick edit
//! pane: the repo actions (fetch, pull, push, stash, undo, …), the branch
//! section, unstaged files, staged files (the "ready to commit" half, in
//! the accent color), then the commit box. The pull request button asks
//! the workspace for the forge panel ([`StagingEvent`]). Its state ([`Panel`]) and
//! behavior live here, so both places show and do exactly the same.

use std::sync::Arc;
use std::time::Duration;

use archipel_app::forge::{ForgeStatus, pick_remote};
use archipel_app::{App as Core, Command};
use archipel_model::changes::{ChangeKind, FileChange, FileState};
use archipel_model::ports::DiffSide;
use archipel_model::requests::{
    CheckoutTarget, CommitMode, CommitRequest, FetchRequest, ForceMode, PullMode, PushRequest,
    SequenceAction, StashApplyMode, StashRequest,
};
use archipel_model::{Oid, RepoOp, RepoPath, RepoSummary, Upstream};
use archipel_ops::actions::split_remote_ref;
use archipel_ops::{OpClass, OpContext, OpError, RepoActions};
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Div, Entity, EventEmitter, SharedString, Stateful, Subscription, Window,
    div, px,
};

use crate::file_edit::FileEdit;
use crate::forge_panel::ForgeTab;
use crate::ignore::{self, IgnoreDialog};
use crate::theme::{color, row_height};
use crate::widgets::{TextTooltip, button, icon_button, primary_button, section};

/// Height of the branch switcher list before it scrolls.
const BRANCH_LIST_HEIGHT: f32 = 160.0;

/// How long a Discard (or Force push) button waits for the confirming
/// second click.
const DISCARD_ARM_TIME: Duration = Duration::from_secs(3);

/// The armed-discard key of the "Discard all" button (no file is named so).
const ALL_FILES: &str = "\0all";

/// A job on the repo, run by the scheduler; the context reports progress
/// and carries the cancellation token.
pub type Job = Box<dyn FnOnce(&RepoActions, &RepoPath, &OpContext) -> Result<(), OpError> + Send>;

/// A [`Job`] whose success may carry a note for the status line.
pub type NotedJob =
    Box<dyn FnOnce(&RepoActions, &RepoPath, &OpContext) -> Result<Option<String>, OpError> + Send>;

/// `job` as a [`NotedJob`] without note.
pub fn noted(job: Job) -> NotedJob {
    Box::new(move |a, p, op| job(a, p, op).map(|()| None))
}

/// Submits `job` through the core's scheduler (operations drawer, per-repo
/// serialization) and hands its status line to `done` on the view.
pub(crate) fn submit_job<V: 'static>(
    core: &Arc<Core>,
    path: &RepoPath,
    class: OpClass,
    label: String,
    job: NotedJob,
    cx: &mut Context<V>,
    done: impl FnOnce(&mut V, String, &mut Context<V>) + 'static,
) {
    let handle = core.submit_repo_job(path, class, label.clone(), job);
    cx.spawn(async move |this, cx| {
        let status = match handle.wait().await {
            Ok(None) => format!("{label}: done"),
            Ok(Some(note)) => format!("{label}: {note}"),
            Err(e) => format!("{label} failed: {e}"),
        };
        // The view may have been closed meanwhile.
        let _ = this.update(cx, |v, cx| done(v, status, cx));
    })
    .detach();
}

/// The panel's own state, held by each view showing it.
pub struct Panel {
    /// The commit message; Enter commits.
    message: Entity<InputState>,
    /// Whether the next commit amends.
    amend: bool,
    /// The Discard button clicked once ([`ALL_FILES`] for "Discard all").
    armed: Option<String>,
    /// The last summary of the repo (head, upstream, stashes, …).
    summary: Option<RepoSummary>,
    /// The new branch name; Enter creates it.
    branch_input: Entity<InputState>,
    /// Local branches other than the current one, while the switcher is
    /// open (`None`: closed).
    branches: Option<Vec<String>>,
    /// The working-copy file being edited in place of its diff.
    pub(crate) edit: Option<FileEdit>,
    /// The dialog adding a file to the `.gitignore`, while open.
    pub(crate) ignore: Option<IgnoreDialog>,
    /// The force push armed by Push on a diverged branch: the button turns
    /// red, and a second click before [`DISCARD_ARM_TIME`] force pushes.
    force_push: Option<ForcePush>,
    _subscriptions: [Subscription; 2],
}

impl Panel {
    pub fn new<V: Staging>(
        summary: Option<RepoSummary>,
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Self {
        let message = cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        let branch_input = cx.new(|cx| InputState::new(window, cx).placeholder("New branch name"));
        let subscriptions = [
            cx.subscribe_in(
                &message,
                window,
                |v: &mut V, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        commit(v, false, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &branch_input,
                window,
                |v: &mut V, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        create_branch(v, window, cx);
                    }
                },
            ),
        ];
        Self {
            message,
            amend: false,
            armed: None,
            summary,
            branch_input,
            branches: None,
            edit: None,
            ignore: None,
            force_push: None,
            _subscriptions: subscriptions,
        }
    }

    /// The Discard button clicked once, waiting for its second click.
    pub fn armed(&self) -> Option<&str> {
        self.armed.as_deref()
    }

    /// The last summary of the repo.
    pub fn summary(&self) -> Option<&RepoSummary> {
        self.summary.as_ref()
    }
}

/// The repo was refreshed: keeps its summary, reloads the open switcher.
pub fn on_summary<V: Staging>(v: &mut V, summary: Option<&RepoSummary>, cx: &mut Context<V>) {
    if let Some(s) = summary {
        v.panel().summary = Some(s.clone());
    }
    if v.panel().branches.is_some() {
        load_branches(v, cx);
    }
}

/// A view showing the staging panel.
/// What the panel asks of the workspace.
pub enum StagingEvent {
    /// Open the forge panel on this tab.
    OpenForge(ForgeTab),
}

pub trait Staging: Sized + EventEmitter<StagingEvent> + 'static {
    /// The app (undo / redo journal, ports).
    fn core(&self) -> &Arc<Core>;
    /// The repo shown.
    fn repo_path(&self) -> &RepoPath;
    /// The working copy changes.
    fn changes(&self) -> &[FileChange];
    /// The panel's state.
    fn panel(&mut self) -> &mut Panel;
    /// Runs a job of `class` on the repo, then reloads; its note, if
    /// any, replaces "done" on the status line.
    fn run_noted_job(
        &mut self,
        class: OpClass,
        label: String,
        cx: &mut Context<Self>,
        job: NotedJob,
    );
    /// Runs a job of `class` on the repo, then reloads.
    fn run_job_as(&mut self, class: OpClass, label: String, cx: &mut Context<Self>, job: Job) {
        self.run_noted_job(class, label, cx, noted(job));
    }
    /// Runs a local write on the repo, then reloads.
    fn run_job(&mut self, label: String, cx: &mut Context<Self>, job: Job) {
        self.run_job_as(OpClass::Write, label, cx, job);
    }
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
}

/// A file's Discard button: the first click arms it, a second one before
/// [`DISCARD_ARM_TIME`] discards.
fn click_discard<V: Staging>(v: &mut V, file: String, cx: &mut Context<V>) {
    let files = vec![file.clone()];
    arm_discard(v, file, cx, move |a, p, _| a.discard(p, &files));
}

/// A Discard button: the first click arms `key`, a second one on the same
/// key before [`DISCARD_ARM_TIME`] runs `job`.
pub(crate) fn arm_discard<V: Staging>(
    v: &mut V,
    key: String,
    cx: &mut Context<V>,
    job: impl FnOnce(&RepoActions, &RepoPath, &OpContext) -> Result<(), OpError> + Send + 'static,
) {
    let armed = &mut v.panel().armed;
    if armed.as_ref() == Some(&key) {
        *armed = None;
        v.run_job("Discard".into(), cx, Box::new(job));
        return;
    }
    *armed = Some(key.clone());
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(DISCARD_ARM_TIME).await;
        let _ = this.update(cx, |v, cx| {
            let armed = &mut v.panel().armed;
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
    let message = v.panel().message.read(cx).value().to_string();
    if message.trim().is_empty() {
        v.report("Write a commit message first".into(), cx);
        return;
    }
    let staged = v.changes().iter().any(FileChange::is_staged);
    let amend = std::mem::take(&mut v.panel().amend);
    if !amend && !staged {
        v.report("Stage files first".into(), cx);
        return;
    }
    let mode = if amend {
        CommitMode::Amend
    } else {
        CommitMode::New
    };
    v.panel()
        .message
        .clone()
        .update(cx, |m, cx| m.set_value("", window, cx));
    let (label, class) = if push {
        ("Commit and push", OpClass::Network)
    } else {
        ("Commit", OpClass::Write)
    };
    v.run_job_as(
        class,
        label.into(),
        cx,
        Box::new(move |a, p, op| {
            a.commit(p, &CommitRequest { message, mode }, None)?;
            if push { push_current(a, p, op) } else { Ok(()) }
        }),
    );
}

/// Pushes the current branch; a branch without upstream is pushed to the
/// default remote (`origin` first) and tracks it from then on.
pub fn push_current(a: &RepoActions, p: &RepoPath, op: &OpContext) -> Result<(), OpError> {
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
    a.push(p, &request, op, op.cancel(), None)
}

/// A force push waiting for confirmation: the branch and its upstream
/// diverged (rebased, amended), so a regular push would be rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ForcePush {
    branch: String,
    remote: String,
    remote_branch: String,
    /// The upstream tip when the dialog opened: the push is refused if the
    /// remote moved since (`--force-with-lease=<branch>:<oid>`), even when
    /// a background fetch updated the tracking ref meanwhile.
    expected: Oid,
    ahead: u32,
    behind: u32,
}

impl ForcePush {
    /// The explanation shown over the armed Force push button.
    fn explanation(&self) -> Vec<String> {
        let commits = |n: u32| {
            if n == 1 {
                "1 commit".to_owned()
            } else {
                format!("{n} commits")
            }
        };
        let upstream = format!("{}/{}", self.remote, self.remote_branch);
        vec![
            format!(
                "{} has diverged from {upstream} ({} ahead, {} behind): a regular push would be \
                 rejected.",
                self.branch,
                commits(self.ahead),
                commits(self.behind),
            ),
            format!(
                "Click again to force push: the {} only on {upstream} will be replaced.",
                commits(self.behind)
            ),
            format!("The push is refused if {upstream} moved since the last fetch."),
        ]
    }

    fn request(&self) -> PushRequest {
        PushRequest {
            remote: Some(self.remote.clone()),
            refspec: Some(format!("{}:{}", self.branch, self.remote_branch)),
            force: ForceMode::WithLease(Some(self.expected)),
            ..PushRequest::default()
        }
    }
}

/// The current branch and its upstream when they diverged: a regular push
/// would be rejected as non fast-forward.
fn diverged(summary: &RepoSummary) -> Option<(&str, &Upstream)> {
    let upstream = summary.upstream.as_ref()?;
    let diverged = upstream.ahead > 0 && upstream.behind > 0 && !upstream.gone;
    diverged.then_some((summary.head.branch()?, upstream))
}

/// Push: a regular push, or on a diverged branch a Force push button
/// waiting for its confirming second click.
fn push<V: Staging>(v: &mut V, window: &mut Window, cx: &mut Context<V>) {
    if let Some(force) = v.panel().force_push.take() {
        force_push(v, &force, cx);
        return;
    }
    let Some((branch, upstream)) = v
        .panel()
        .summary
        .as_ref()
        .and_then(diverged)
        .map(|(b, u)| (b.to_owned(), u.clone()))
    else {
        v.run_job_as(OpClass::Network, "Push".into(), cx, Box::new(push_current));
        return;
    };
    let ports = v.core().repo_actions().ports();
    let (refs, remotes) = (ports.refs.clone(), ports.remotes.clone());
    let path = v.repo_path().clone();
    let task = cx.background_spawn(async move {
        let names = remotes.remotes(&path).map_err(|e| e.to_string())?;
        let names: Vec<&str> = names.iter().map(|r| r.name.as_str()).collect();
        let (remote, remote_branch) = split_remote_ref(&upstream.name, &names);
        let expected = refs
            .resolve(&path, &format!("refs/remotes/{}", upstream.name))
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("{} not found", upstream.name))?;
        Ok::<_, String>(ForcePush {
            branch,
            remote: remote.to_owned(),
            remote_branch: remote_branch.to_owned(),
            expected,
            ahead: upstream.ahead,
            behind: upstream.behind,
        })
    });
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |v: &mut V, cx| match result {
            Ok(force) => arm_force_push(v, force, cx),
            Err(e) => v.report(format!("Push failed: {e}"), cx),
        });
    })
    .detach();
}

/// Turns Push into a red Force push button until a second click or
/// [`DISCARD_ARM_TIME`].
fn arm_force_push<V: Staging>(v: &mut V, force: ForcePush, cx: &mut Context<V>) {
    v.panel().force_push = Some(force.clone());
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(DISCARD_ARM_TIME).await;
        let _ = this.update(cx, |v, cx| {
            let armed = &mut v.panel().force_push;
            if armed.as_ref() == Some(&force) {
                *armed = None;
                cx.notify();
            }
        });
    })
    .detach();
    cx.notify();
}

/// Runs the armed force push, leased on the upstream tip.
fn force_push<V: Staging>(v: &mut V, force: &ForcePush, cx: &mut Context<V>) {
    let request = force.request();
    v.run_job_as(
        OpClass::Network,
        format!("Force push {}", force.branch),
        cx,
        Box::new(move |a, p, op| a.push(p, &request, op, op.cancel(), None)),
    );
    cx.notify();
}

/// Reads the local branches into the switcher (opening it).
fn load_branches<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let refs = v.core().repo_actions().ports().refs.clone();
    let path = v.repo_path().clone();
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
        let _ = this.update(cx, |v, cx| match result {
            Ok(b) => {
                v.panel().branches = Some(b);
                cx.notify();
            }
            Err(e) => v.report(e, cx),
        });
    })
    .detach();
}

fn toggle_switcher<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    if v.panel().branches.take().is_none() {
        load_branches(v, cx);
    }
    cx.notify();
}

/// Creates the branch of the name field on `HEAD` and checks it out.
fn create_branch<V: Staging>(v: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let input = v.panel().branch_input.clone();
    let name = input.read(cx).value().trim().to_owned();
    if name.is_empty() {
        v.report("Write a branch name first".into(), cx);
        return;
    }
    input.update(cx, |b, cx| b.set_value("", window, cx));
    v.run_job(
        format!("Create branch {name}"),
        cx,
        Box::new(move |a, p, _| {
            a.create_branch(p, &name, None, None)?;
            a.checkout(p, &CheckoutTarget::Branch(name), None)
        }),
    );
}

fn switch_to<V: Staging>(v: &mut V, branch: String, cx: &mut Context<V>) {
    v.panel().branches = None;
    v.run_noted_job(
        OpClass::Write,
        format!("Checkout {branch}"),
        cx,
        Box::new(move |a, p, _| {
            a.checkout_autostash(p, &CheckoutTarget::Branch(branch))
                .map(|o| o.note())
        }),
    );
}

/// Undo or redo through the app (it owns the journal), reporting a
/// refusal.
fn history<V: Staging>(v: &mut V, command: Command, cx: &mut Context<V>) {
    let id = v.repo_path().id();
    match v.core().dispatch(command) {
        Ok(()) => v.core().refresh_repo(id),
        Err(e) => v.report(e.to_string(), cx),
    }
}

/// A compact button for the action rows.
pub fn small(b: Stateful<Div>) -> Stateful<Div> {
    b.py_0().text_xs()
}

/// Push, or once armed on a diverged branch a red "Force push?" like the
/// armed Discard buttons, explained by its tooltip.
fn push_button<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Stateful<Div> {
    let label = match v.panel().force_push.as_ref() {
        Some(force) => {
            let danger = color(cx, T::StatusError);
            let explanation = force.explanation();
            button("st-push", "Force push?", cx)
                .text_color(danger)
                .border_color(danger)
                .tooltip(move |_, cx| TextTooltip::view(&explanation, cx))
        }
        None => button("st-push", "Push", cx),
    };
    small(label).on_click(cx.listener(|v: &mut V, _, window, cx| push(v, window, cx)))
}

/// The pull request button: the open one of the branch (to review or
/// merge it), else "Create PR…"; none when the repo is not on a forge.
fn pull_request_button<V: Staging>(v: &V, cx: &mut Context<V>) -> Option<Stateful<Div>> {
    let id = v.repo_path().id();
    let forge = v.core().rows().into_iter().find(|r| r.id == id)?.forge;
    let (label, tab) = match &forge {
        ForgeStatus::NotOnForge => return None,
        ForgeStatus::Open(pr) => (format!("PR #{}", pr.number), ForgeTab::Open(vec![id])),
        _ => ("Create PR…".to_owned(), ForgeTab::Create(vec![id])),
    };
    Some(
        small(button("st-pr", label, cx)).on_click(cx.listener(move |_: &mut V, _, _, cx| {
            cx.emit(StagingEvent::OpenForge(tab.clone()));
        })),
    )
}

/// Fetch, pull, push, pull request, stash, pop, undo, redo, after `lead`
/// (a button of the view's own).
pub fn render_actions<V: Staging>(
    v: &mut V,
    lead: Option<AnyElement>,
    cx: &mut Context<V>,
) -> AnyElement {
    let id = v.repo_path().id();
    let stashes = v.panel().summary.as_ref().map_or(0, |s| s.stash_count);
    let job = |eid: &'static str,
               label: &'static str,
               class: OpClass,
               cx: &mut Context<V>,
               job: fn(&RepoActions, &RepoPath, &OpContext) -> Result<(), OpError>| {
        small(button(eid, label, cx)).on_click(cx.listener(move |v: &mut V, _, _, cx| {
            v.run_job_as(class, label.into(), cx, Box::new(job));
        }))
    };
    let (write, network) = (OpClass::Write, OpClass::Network);
    div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(lead)
        .child(job("st-fetch", "Fetch", network, cx, |a, p, op| {
            a.fetch(p, &FetchRequest::default(), op, op.cancel())
        }))
        .child(job("st-pull", "Pull", network, cx, |a, p, op| {
            a.pull(p, PullMode::FastForwardOnly, op, op.cancel(), None)
        }))
        .child(push_button(v, cx))
        .children(pull_request_button(v, cx))
        .child(job("st-stash", "Stash", write, cx, |a, p, _| {
            a.stash_push(p, &StashRequest::default()).map(drop)
        }))
        .when(stashes > 0, |d| {
            d.child(job("st-pop", "Pop", write, cx, |a, p, _| {
                a.stash_apply(p, 0, StashApplyMode::Pop)
            }))
        })
        .child(
            small(button("st-undo", "Undo", cx)).on_click(
                cx.listener(move |v: &mut V, _, _, cx| history(v, Command::Undo(id), cx)),
            ),
        )
        .child(
            small(button("st-redo", "Redo", cx)).on_click(
                cx.listener(move |v: &mut V, _, _, cx| history(v, Command::Redo(id), cx)),
            ),
        )
        .into_any_element()
}

/// The merge / rebase / … in progress, with Continue and Abort.
fn render_in_progress<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Option<AnyElement> {
    let op = v.panel().summary.as_ref()?.in_progress?;
    let what = match op {
        RepoOp::Merge => "Merge",
        RepoOp::Rebase => "Rebase",
        RepoOp::CherryPick => "Cherry-pick",
        RepoOp::Revert => "Revert",
        // Not continued from here: a terminal.
        RepoOp::Bisect | RepoOp::ApplyMailbox => return None,
    };
    let sequence = |eid: &'static str, label: &'static str, action, cx: &mut Context<V>| {
        small(button(eid, label, cx)).on_click(cx.listener(move |v: &mut V, _, _, cx| {
            v.run_job(
                format!("{label} {}", what.to_lowercase()),
                cx,
                Box::new(move |a, p, _| a.sequence(p, action)),
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
                        "st-continue",
                        "Continue",
                        SequenceAction::Continue,
                        cx,
                    ))
                    .child(sequence("st-abort", "Abort", SequenceAction::Abort, cx)),
            )
            .into_any_element(),
    )
}

/// The new branch field with Create, and the branch switcher.
fn render_branch<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    let hover = color(cx, T::BgHover);
    let open = v.panel().branches.is_some();
    let list = v.panel().branches.clone().map(|branches| {
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
                        .id(SharedString::from(format!("st-br-{b}")))
                        .px_2()
                        .h(row_height(cx))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |v: &mut V, _, _, cx| {
                            switch_to(v, name.clone(), cx);
                        }))
                        .child(div().truncate().child(b))
                        .into_any_element()
                })
                .collect()
        };
        div()
            .id("st-branches")
            .max_h(px(BRANCH_LIST_HEIGHT))
            .overflow_y_scrollbar()
            .border_1()
            .border_color(color(cx, T::BorderSubtle))
            .children(rows)
    });
    let input = v.panel().branch_input.clone();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().child(Input::new(&input)))
                .child(
                    button("st-branch", "Create", cx)
                        .on_click(cx.listener(|v: &mut V, _, w, cx| create_branch(v, w, cx))),
                )
                .child(
                    button("st-switch", if open { "Switch ▴" } else { "Switch ▾" }, cx)
                        .on_click(cx.listener(|v: &mut V, _, _, cx| toggle_switcher(v, cx))),
                ),
        )
        .children(list)
        .into_any_element()
}

/// The panel: actions, branch, unstaged files, staged files, commit box
/// (the quick edit pane).
pub fn render<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    let actions = render_actions(v, None, cx);
    render_panel(v, Some(actions), cx)
}

/// The panel without its Repo Actions Bar, which the repo tab shows in its
/// toolbar ([`render_actions`]).
pub fn render_body<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    render_panel(v, None, cx)
}

/// `actions` (if any), the operation banner, branch, files, commit box.
fn render_panel<V: Staging>(
    v: &mut V,
    actions: Option<AnyElement>,
    cx: &mut Context<V>,
) -> AnyElement {
    let top = div()
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .children(actions)
        .children(render_in_progress(v, cx))
        .child(section("BRANCH", cx))
        .child(render_branch(v, cx));
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(top)
        .child(render_files(v, cx))
        .into_any_element()
}

/// Unstaged files, staged files, commit box.
#[allow(clippy::too_many_lines)] // declarative element tree
fn render_files<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
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
    let (amend, armed) = (v.panel().amend, v.panel().armed.clone());
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
                    .child(discard.on_click(cx.listener(move |v: &mut V, _, _, cx| {
                        let files = discard_paths.clone();
                        arm_discard(v, ALL_FILES.to_owned(), cx, move |a, p, _| {
                            a.discard(p, &files)
                        });
                    })))
                    .child(button("stage-all", "Stage all", cx).on_click(cx.listener(
                        move |v: &mut V, _, _, cx| {
                            let files = unstaged_paths.clone();
                            v.run_job(
                                "Stage all".into(),
                                cx,
                                Box::new(move |a, p, _| a.stage(p, &files)),
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
        .child(
            section(format!("STAGED ({})", staged.len()), cx).text_color(color(cx, T::AccentText)),
        )
        .when(!staged_paths.is_empty(), |d| {
            d.child(
                div().pt_2().child(
                    button("unstage-all", "Unstage all", cx).on_click(cx.listener(
                        move |v: &mut V, _, _, cx| {
                            let files = staged_paths.clone();
                            v.run_job(
                                "Unstage all".into(),
                                cx,
                                Box::new(move |a, p, _| a.unstage(p, &files)),
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
        .child(commit_box(&v.panel().message.clone(), amend, cx))
        .into_any_element()
}

#[allow(clippy::too_many_lines)] // declarative element tree
fn file_row<V: Staging>(
    c: &FileChange,
    side: DiffSide,
    open: Option<&(String, DiffSide)>,
    armed: Option<&str>,
    cx: &mut Context<V>,
) -> AnyElement {
    let hover = color(cx, T::BgHover);
    let path = c.path.clone();
    // The marker follows the change on the side shown: a file deleted in
    // the worktree reads as deleted, not edited.
    let kind = match c.state {
        FileState::Tracked { staged, .. } if side == DiffSide::Staged => Some(staged),
        FileState::Tracked { unstaged, .. } => Some(unstaged),
        FileState::Untracked | FileState::Conflicted => None,
    };
    let deleted = kind == Some(ChangeKind::Deleted);
    let (label, token) = match (c.state, kind) {
        (FileState::Untracked, _) => ("+", T::StatusAhead),
        (FileState::Conflicted, _) => ("!", T::StatusConflict),
        (_, Some(ChangeKind::Deleted)) => ("−", T::DiffDelFg),
        (_, Some(ChangeKind::Added | ChangeKind::Copied)) => ("+", T::DiffAddFg),
        _ => ("✎", T::StatusDirty),
    };
    let (p1, p2, p3, p4) = (path.clone(), path.clone(), path.clone(), path.clone());
    let tracked = matches!(c.state, FileState::Tracked { .. });
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
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(deleted, |d| {
                    d.line_through().text_color(color(cx, T::FgMuted))
                })
                .child(path),
        )
        .when(side == DiffSide::Unstaged, |d| {
            d.child(
                icon_button(
                    SharedString::from(format!("ignore-{p4}")),
                    IconName::EyeOff,
                    "Add to .gitignore…",
                    cx,
                )
                .on_click(cx.listener(move |v: &mut V, _, window, cx| {
                    cx.stop_propagation();
                    ignore::open(v, p4.clone(), tracked, window, cx);
                })),
            )
        })
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
                    Box::new(move |a, p, _| a.unstage(p, &files))
                } else {
                    Box::new(move |a, p, _| a.stage(p, &files))
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
                            let amend = &mut v.panel().amend;
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

#[cfg(test)]
mod tests {
    use archipel_model::{ChangeCounts, ChangedFiles, HeadState, Timestamp};

    use super::*;

    fn summary(head: HeadState, ahead: u32, behind: u32, gone: bool) -> RepoSummary {
        RepoSummary {
            head,
            head_oid: None,
            upstream: Some(Upstream {
                name: "origin/feat".into(),
                ahead,
                behind,
                gone,
            }),
            changes: ChangeCounts::default(),
            changed_files: ChangedFiles::default(),
            containing_branches: Vec::new(),
            stash_count: 0,
            describe: None,
            last_fetch: None,
            in_progress: None,
            has_submodules: false,
            computed_at: Timestamp(0),
        }
    }

    #[test]
    fn only_a_diverged_branch_offers_a_force_push() {
        let branch = || HeadState::Branch("feat".into());
        let rebased = summary(branch(), 4, 1, false);
        assert_eq!(diverged(&rebased).map(|(b, _)| b), Some("feat"));
        // Ahead only (fast-forward), behind only, upstream gone, detached.
        assert!(diverged(&summary(branch(), 4, 0, false)).is_none());
        assert!(diverged(&summary(branch(), 0, 1, false)).is_none());
        assert!(diverged(&summary(branch(), 4, 1, true)).is_none());
        let detached = HeadState::Detached(Oid::from_hex(&"a".repeat(40)).unwrap());
        assert!(diverged(&summary(detached, 4, 1, false)).is_none());
    }

    #[test]
    fn the_force_push_leases_the_upstream_tip() {
        let expected = Oid::from_hex(&"5d000c8".repeat(6)[..40]).unwrap();
        let force = ForcePush {
            branch: "feat".into(),
            remote: "origin".into(),
            remote_branch: "feat/remote".into(),
            expected,
            ahead: 4,
            behind: 1,
        };
        let request = force.request();
        assert_eq!(request.remote.as_deref(), Some("origin"));
        assert_eq!(request.refspec.as_deref(), Some("feat:feat/remote"));
        assert_eq!(request.force, ForceMode::WithLease(Some(expected)));
        let explanation = force.explanation().join("\n");
        assert!(
            explanation.contains("4 commits ahead, 1 commit behind"),
            "{explanation}"
        );
        assert!(explanation.contains("origin/feat/remote"), "{explanation}");
    }
}
