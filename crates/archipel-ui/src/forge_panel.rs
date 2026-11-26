//! The forge panel (spec §8): accounts (sign in with a personal access
//! token, sign out), open pull requests of the selected repos (assign and
//! merge them),
//! new pull requests for the selected repos (one per repo, linked), open
//! issues of the selected repos (close, reopen), a new issue, and clone:
//! from URLs (no account needed) or from an account / organization / group.
//!
//! Every forge call runs on the app runtime (scheduler network pool); the
//! panel only awaits the results.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use archipel_app::dashboard::relative_time;
use archipel_app::forge::{AUTO_MERGE_MARK, auto_merge_note, ci_label, review_label};
use archipel_app::{
    App as Core, AutoMergeOutcome, CloneOutcome, CloneProtocol, IssueTarget, PullRequestPlan,
    RepoIssues, RepoPullRequests, clone_dir_name, split_clone_urls,
};
use archipel_model::forge::{
    CloseReason, ForgeAccount, ForgeKind, ForgeRepo, Issue, IssueDraft, MergeMethod, MergeState,
    Namespace, PullRequestStatus, RemoteRepo,
};
use archipel_model::{RepoId, Timestamp};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, EventEmitter, PathPromptOptions, SharedString, Subscription, Window, div, px,
};

use crate::armed::Armed;
use crate::theme::{color, radius};
use crate::widgets::{
    ClickIf as _, Slot as _, button, check, check_box, chip, confirm_button, primary_button,
    section, steady_button,
};

/// Which part of the panel is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeTab {
    /// Signed-in hosts, sign-in form.
    Accounts,
    /// Open pull requests of these repos (all when empty), to merge.
    Open(Vec<RepoId>),
    /// New pull requests for these repos (all when empty).
    Create(Vec<RepoId>),
    /// Open issues of these repos (all when empty), to close.
    Issues(Vec<RepoId>),
    /// A new issue on one of these repos (all when empty).
    NewIssue(Vec<RepoId>),
    /// Clone from URLs or from an account.
    Clone,
}

/// What the panel reports to the workspace.
pub enum ForgePanelEvent {
    /// Closed.
    Closed,
    /// A one-line result for the status bar.
    Notice(String),
    /// A one-line result for the status bar, opening `url` when clicked.
    NoticeLink(String, String),
}

impl ForgeTab {
    /// The repos of a pull request or issue tab.
    fn repos(&self) -> Option<&Vec<RepoId>> {
        match self {
            Self::Open(ids) | Self::Create(ids) | Self::Issues(ids) | Self::NewIssue(ids) => {
                Some(ids)
            }
            Self::Accounts | Self::Clone => None,
        }
    }
}

/// Report line: text, link, error.
type Line = (String, Option<String>, bool);

/// The panel.
#[allow(clippy::struct_excessive_bools)] // independent form toggles
pub struct ForgePanel {
    core: Arc<Core>,
    tab: ForgeTab,
    /// Jobs started by [`Self::run`] and not finished: several can run at
    /// once (picking another account while namespaces load).
    running: usize,
    error: Option<String>,
    // Accounts.
    accounts: Vec<ForgeAccount>,
    kind: Option<ForgeKind>,
    host: Entity<InputState>,
    token: Entity<InputState>,
    // Open pull requests.
    open: Vec<RepoPullRequests>,
    method: MergeMethod,
    delete_branch: bool,
    /// The Merge button clicked once: `(repo, number)`.
    armed: Armed<(RepoId, u64)>,
    /// The pull request whose assignees are being edited: `(repo, number)`.
    assigning: Option<(RepoId, u64)>,
    /// The assignees picked in that editor.
    assign_picks: Vec<String>,
    /// Who the pull requests of each repo can be assigned to (read once
    /// per listing), or why it could not be read.
    assignable: HashMap<RepoId, Result<Vec<String>, String>>,
    // New pull requests.
    plan: Option<PullRequestPlan>,
    title: Entity<InputState>,
    description: Entity<TextareaState>,
    target: Entity<InputState>,
    draft: bool,
    // Open issues.
    issues: Vec<RepoIssues>,
    close_reason: CloseReason,
    close_comment: Entity<InputState>,
    /// The Close button clicked once: `(repo, number)`.
    armed_close: Armed<(RepoId, u64)>,
    // New issue.
    issue_targets: Vec<IssueTarget>,
    issue_repo: Option<RepoId>,
    issue_title: Entity<InputState>,
    issue_description: Entity<TextareaState>,
    issue_labels: Vec<String>,
    issue_assignees: Vec<String>,
    // Clone.
    /// URLs to clone (one or several).
    url: Entity<InputState>,
    account: Option<ForgeAccount>,
    namespaces: Vec<Namespace>,
    namespace: Option<Namespace>,
    repos: Vec<(RemoteRepo, bool)>,
    filter: Entity<InputState>,
    folder: Option<PathBuf>,
    ssh: bool,
    report: Vec<Line>,
    /// Enter in the URL field clones.
    _url_enter: Subscription,
}

impl EventEmitter<ForgePanelEvent> for ForgePanel {}

fn kind_label(kind: &ForgeKind) -> String {
    match kind.as_str() {
        "github" => "GitHub".into(),
        "gitlab" => "GitLab".into(),
        other => other.to_owned(),
    }
}

/// Adds `item` to `list`, or removes it when there.
fn toggle(list: &mut Vec<String>, item: &str) {
    match list.iter().position(|x| x == item) {
        Some(i) => {
            list.remove(i);
        }
        None => list.push(item.to_owned()),
    }
}

/// `!12` on GitLab, `#12` elsewhere.
fn pr_ref(forge: Option<&ForgeRepo>, number: u64) -> String {
    let gitlab = forge.is_some_and(|f| f.account.kind == ForgeKind::gitlab());
    format!("{}{number}", if gitlab { '!' } else { '#' })
}

impl ForgePanel {
    /// Opens the panel on `tab`.
    pub fn new(
        core: Arc<Core>,
        tab: ForgeTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let host = input(
            "github.com, gitlab.com or a self-hosted GitLab / GitHub Enterprise host",
            window,
            cx,
        );
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Personal access token")
        });
        let title = input("Pull request title", window, cx);
        let target = input(
            "Target branch (empty: each repo's default branch)",
            window,
            cx,
        );
        let filter = input("Filter repositories", window, cx);
        let url = input(
            "https://host/owner/repo.git or git@host:owner/repo.git (several: separate with spaces)",
            window,
            cx,
        );
        let url_enter = cx.subscribe_in(&url, window, |p: &mut Self, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                p.clone_urls(window, cx);
            }
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(5)
                .placeholder("Description (Markdown)")
        });
        let close_comment = input("Closing comment (optional, Markdown)", window, cx);
        let issue_title = input("Issue title", window, cx);
        let issue_description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Description (Markdown)")
        });
        let mut panel = Self {
            accounts: core.forge_accounts(),
            core,
            tab: ForgeTab::Accounts,
            running: 0,
            error: None,
            kind: None,
            host,
            token,
            open: Vec::new(),
            method: MergeMethod::default(),
            delete_branch: true,
            armed: Armed::default(),
            assigning: None,
            assign_picks: Vec::new(),
            assignable: HashMap::new(),
            plan: None,
            title,
            description,
            target,
            draft: false,
            issues: Vec::new(),
            close_reason: CloseReason::default(),
            close_comment,
            armed_close: Armed::default(),
            issue_targets: Vec::new(),
            issue_repo: None,
            issue_title,
            issue_description,
            issue_labels: Vec::new(),
            issue_assignees: Vec::new(),
            url,
            account: None,
            namespaces: Vec::new(),
            namespace: None,
            repos: Vec::new(),
            filter,
            folder: None,
            ssh: false,
            report: Vec::new(),
            _url_enter: url_enter,
        };
        panel.show(tab, window, cx);
        panel
    }

    /// Switches to a tab and loads what it needs.
    pub fn show(&mut self, tab: ForgeTab, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.report.clear();
        self.accounts = self.core.forge_accounts();
        match &tab {
            ForgeTab::Accounts => {
                let focus = if self.accounts.is_empty() {
                    &self.host
                } else {
                    &self.token
                };
                focus.update(cx, |i, cx| i.focus(window, cx));
            }
            ForgeTab::Open(ids) => self.list_open(ids.clone(), window, cx),
            ForgeTab::Create(ids) => self.prepare(ids.clone(), window, cx),
            ForgeTab::Issues(ids) => self.list_issues(ids.clone(), window, cx),
            ForgeTab::NewIssue(ids) => self.prepare_issue(ids.clone(), window, cx),
            ForgeTab::Clone => {
                if self.folder.is_none() {
                    self.folder = self.core.default_clone_folder();
                }
                self.url.update(cx, |i, cx| i.focus(window, cx));
                if self.account.is_none()
                    && let Some(first) = self.accounts.first().cloned()
                {
                    self.pick_account(first, window, cx);
                }
            }
        }
        self.tab = tab;
        cx.notify();
    }

    /// Whether a job started by [`Self::run`] is still running.
    fn busy(&self) -> bool {
        self.running > 0
    }

    /// Runs `job` on the app runtime, then `done` on the UI thread.
    fn run<T, F>(
        &mut self,
        job: F,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        self.running += 1;
        self.error = None;
        let handle = self.core.scheduler().runtime().spawn(job);
        cx.spawn_in(window, async move |this, cx| {
            let result = handle.await;
            let _ = this.update_in(cx, |p, window, cx| {
                p.running -= 1;
                match result {
                    Ok(value) => done(p, value, window, cx),
                    Err(e) => p.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ----- accounts -----------------------------------------------------

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.host.read(cx).value().to_string();
        let token = self.token.read(cx).value().to_string();
        let kind = self.kind.clone();
        let core = self.core.clone();
        self.run(
            async move { core.sign_in(&host, kind, &token).await },
            window,
            cx,
            |p, result, window, cx| match result {
                Ok(account) => {
                    p.token.update(cx, |t, cx| t.set_value("", window, cx));
                    p.host.update(cx, |t, cx| t.set_value("", window, cx));
                    p.accounts = p.core.forge_accounts();
                    cx.emit(ForgePanelEvent::Notice(format!(
                        "Signed in to {} as {} ({})",
                        account.host,
                        account.user,
                        kind_label(&account.kind)
                    )));
                }
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    fn sign_out(&mut self, host: String, window: &mut Window, cx: &mut Context<Self>) {
        let core = self.core.clone();
        self.run(
            async move { core.sign_out(&host).await },
            window,
            cx,
            |p, result, _, _| {
                if let Err(e) = result {
                    p.error = Some(e.to_string());
                }
                p.accounts = p.core.forge_accounts();
                if p.account.as_ref().is_some_and(|a| !p.accounts.contains(a)) {
                    p.account = None;
                    p.namespaces.clear();
                    p.repos.clear();
                }
            },
        );
    }

    // ----- open pull requests ---------------------------------------------

    fn list_open(&mut self, ids: Vec<RepoId>, window: &mut Window, cx: &mut Context<Self>) {
        self.armed.disarm();
        self.assigning = None;
        self.assignable.clear();
        let core = self.core.clone();
        self.run(
            async move { core.open_pull_requests(&ids).await },
            window,
            cx,
            |p, result, _, _| match result {
                Ok(open) => p.open = open,
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    /// The Merge button: the first click arms it, a second one before
    /// [`ARM_TIME`](crate::armed::ARM_TIME) merges.
    fn click_merge(
        &mut self,
        repo: RepoId,
        number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.armed.click((repo, number), cx, |p| &mut p.armed) {
            return;
        }
        let found = self.open.iter().find(|r| r.repo == repo).and_then(|r| {
            let pr = r
                .result
                .as_ref()
                .ok()?
                .iter()
                .find(|p| p.number == number)?;
            Some((r.forge.clone()?, pr.clone()))
        });
        let Some((forge, pr)) = found else { return };
        let label = format!("{} {}", forge.path, pr_ref(Some(&forge), number));
        let url = pr.url.clone();
        let (method, delete) = (self.method, self.delete_branch);
        let core = self.core.clone();
        self.run(
            async move {
                core.merge_pull_request(repo, &forge, &pr, method, delete)
                    .await
            },
            window,
            cx,
            move |p, result, _, cx| match result {
                Ok(report) => {
                    for r in p.open.iter_mut().filter(|r| r.repo == repo) {
                        if let Ok(prs) = &mut r.result {
                            prs.retain(|pr| pr.number != number);
                        }
                    }
                    let text = match report.warning {
                        Some(w) => format!("{label} merged ({w})"),
                        None => format!("{label} merged"),
                    };
                    p.report = vec![(text.clone(), Some(url), false)];
                    cx.emit(ForgePanelEvent::Notice(text));
                }
                Err(e) => p.error = Some(format!("{label}: {e}")),
            },
        );
    }

    /// The listed pull request `number` of `repo`, with its forge repo.
    fn listed(&self, repo: RepoId, number: u64) -> Option<(ForgeRepo, PullRequestStatus)> {
        let r = self.open.iter().find(|r| r.repo == repo)?;
        let pr = r
            .result
            .as_ref()
            .ok()?
            .iter()
            .find(|p| p.number == number)?;
        Some((r.forge.clone()?, pr.clone()))
    }

    /// Whether the listed pull request has an auto-merge armed (on the
    /// forge or watched by Archipel).
    fn auto_merge_armed(&self, repo: RepoId, pr: &PullRequestStatus) -> bool {
        pr.auto_merge || self.core.auto_merge_watch(repo, pr.number).is_some()
    }

    /// The listed pull requests from branch `source`, one per repo, that
    /// can be armed: the linked pull requests of a multi-repo change.
    fn linked(&self, source: &str) -> Vec<(RepoId, u64)> {
        self.open
            .iter()
            .filter_map(|r| {
                let pr = r.result.as_ref().ok()?.iter().find(|p| {
                    p.source == source
                        && !p.draft
                        && !p.from_fork
                        && !self.auto_merge_armed(r.repo, p)
                })?;
                r.forge.as_ref()?;
                Some((r.repo, pr.number))
            })
            .collect()
    }

    /// "When CI passes": arms the auto-merge of each of `prs` with the
    /// chosen method and branch deletion.
    fn arm_auto_merge(
        &mut self,
        prs: Vec<(RepoId, u64)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found: Vec<_> = prs
            .into_iter()
            .filter_map(|(repo, number)| {
                let (forge, pr) = self.listed(repo, number)?;
                Some((repo, forge, pr))
            })
            .collect();
        if found.is_empty() {
            return;
        }
        let (method, delete) = (self.method, self.delete_branch);
        let core = self.core.clone();
        self.run(
            async move {
                let mut out = Vec::new();
                for (repo, forge, pr) in found {
                    let label = format!("{} {}", forge.path, pr_ref(Some(&forge), pr.number));
                    let result = core.arm_auto_merge(repo, &forge, &pr, method, delete).await;
                    out.push((repo, pr.number, pr.url, label, result));
                }
                out
            },
            window,
            cx,
            |p, results, _, cx| {
                p.report.clear();
                for (repo, number, url, label, result) in results {
                    let (text, error) = match result {
                        Ok(AutoMergeOutcome::Forge { warning }) => {
                            p.set_auto_merge(repo, number, true);
                            match warning {
                                Some(w) => (format!("{label}: auto-merge armed ({w})"), false),
                                None => (format!("{label}: auto-merge armed on the forge"), false),
                            }
                        }
                        Ok(AutoMergeOutcome::Merged) => {
                            p.drop_listed(repo, number);
                            (format!("{label} merged: nothing left to wait for"), false)
                        }
                        Ok(AutoMergeOutcome::Watched { refusal }) => (
                            format!(
                                "{label}: the forge refused its auto-merge ({refusal}); Archipel \
                                 merges it once its CI passes, only while it runs"
                            ),
                            false,
                        ),
                        Err(e) => (format!("{label}: {e}"), true),
                    };
                    p.report.push((text, Some(url), error));
                }
                if let [(text, _, false)] = p.report.as_slice() {
                    cx.emit(ForgePanelEvent::Notice(text.clone()));
                }
            },
        );
    }

    /// "Cancel auto-merge".
    fn cancel_auto_merge(
        &mut self,
        repo: RepoId,
        number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((forge, pr)) = self.listed(repo, number) else {
            return;
        };
        let label = format!("{} {}", forge.path, pr_ref(Some(&forge), number));
        let core = self.core.clone();
        self.run(
            async move { core.cancel_auto_merge(repo, &forge, &pr).await },
            window,
            cx,
            move |p, result, _, cx| match result {
                Ok(()) => {
                    p.set_auto_merge(repo, number, false);
                    let text = format!("{label}: auto-merge cancelled");
                    p.report = vec![(text.clone(), None, false)];
                    cx.emit(ForgePanelEvent::Notice(text));
                }
                Err(e) => p.error = Some(format!("{label}: {e}")),
            },
        );
    }

    /// Notes the forge's auto-merge state of a listed pull request.
    fn set_auto_merge(&mut self, repo: RepoId, number: u64, armed: bool) {
        for r in self.open.iter_mut().filter(|r| r.repo == repo) {
            if let Ok(prs) = &mut r.result {
                for pr in prs.iter_mut().filter(|pr| pr.number == number) {
                    pr.auto_merge = armed;
                }
            }
        }
    }

    /// Opens the assignee editor of a listed pull request, or closes it
    /// when it is the one open. Who can be assigned is read on first use.
    fn edit_assignees(
        &mut self,
        repo: RepoId,
        number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.assigning == Some((repo, number)) {
            self.assigning = None;
            cx.notify();
            return;
        }
        let Some((forge, pr)) = self.listed(repo, number) else {
            return;
        };
        self.assigning = Some((repo, number));
        self.assign_picks = pr.assignees;
        if !self.assignable.contains_key(&repo) {
            let core = self.core.clone();
            self.run(
                async move { core.assignable_users(repo, &forge).await },
                window,
                cx,
                move |p, result, _, _| {
                    p.assignable.insert(repo, result.map_err(|e| e.to_string()));
                },
            );
        }
        cx.notify();
    }

    /// Sends the picked assignees of the pull request being edited.
    fn save_assignees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((repo, number)) = self.assigning else {
            return;
        };
        let Some((forge, _)) = self.listed(repo, number) else {
            return;
        };
        let label = format!("{} {}", forge.path, pr_ref(Some(&forge), number));
        let asked = self.assign_picks.clone();
        let wanted = asked.clone();
        let core = self.core.clone();
        self.run(
            async move {
                core.set_pull_request_assignees(repo, &forge, number, wanted)
                    .await
            },
            window,
            cx,
            move |p, result, _, cx| match result {
                Ok(kept) => {
                    for r in p.open.iter_mut().filter(|r| r.repo == repo) {
                        if let Ok(prs) = &mut r.result {
                            for pr in prs.iter_mut().filter(|pr| pr.number == number) {
                                pr.assignees.clone_from(&kept);
                            }
                        }
                    }
                    p.assigning = None;
                    let dropped: Vec<&str> = asked
                        .iter()
                        .filter(|a| !kept.contains(a))
                        .map(String::as_str)
                        .collect();
                    let mut text = if kept.is_empty() {
                        format!("{label}: no one assigned")
                    } else {
                        format!("{label} assigned to {}", kept.join(", "))
                    };
                    if !dropped.is_empty() {
                        text = format!("{text} ({} not kept by the forge)", dropped.join(", "));
                    }
                    p.report = vec![(text.clone(), None, !dropped.is_empty())];
                    cx.emit(ForgePanelEvent::Notice(text));
                }
                Err(e) => p.error = Some(format!("{label}: {e}")),
            },
        );
    }

    /// The assignee editor under a pull request row: a chip per user who
    /// can be assigned (and per current assignee), Save and Cancel.
    fn assign_editor(
        &self,
        id: &str,
        repo: RepoId,
        current: &[String],
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let known = self.assignable.get(&repo);
        let mut users: Vec<String> = match known {
            Some(Ok(list)) => list.clone(),
            _ => Vec::new(),
        };
        for login in current.iter().chain(&self.assign_picks) {
            if !users.contains(login) {
                users.push(login.clone());
            }
        }
        let (note, tone) = match known {
            None => (Some("Reading who can be assigned…".to_owned()), T::FgSubtle),
            Some(Err(e)) => (Some(e.clone()), T::StatusError),
            Some(Ok(list)) if list.is_empty() => (
                Some(
                    "No one to choose from: listing who can be assigned needs write access \
                     to the repository"
                        .to_owned(),
                ),
                T::FgSubtle,
            ),
            Some(Ok(_)) => (None, T::FgSubtle),
        };
        let chips = users.into_iter().enumerate().map(|(i, login)| {
            let on = self.assign_picks.contains(&login);
            chip(
                SharedString::from(format!("{id}-user-{i}")),
                login.clone(),
                on,
                cx,
            )
            .on_click(cx.listener(move |p, _, _, cx| {
                toggle(&mut p.assign_picks, &login);
                cx.notify();
            }))
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .ml_4()
            .mr_2()
            .mb_1()
            .px_2()
            .py_1()
            .rounded(radius(cx))
            .border_1()
            .border_color(color(cx, T::Border))
            .child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::FgSubtle))
                    .child("ASSIGNEES (click to pick or drop)"),
            )
            .child(div().flex().flex_wrap().gap_1().children(chips))
            .children(note.map(|note| div().text_xs().text_color(color(cx, tone)).child(note)))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .justify_end()
                    .child(
                        button(SharedString::from(format!("{id}-cancel")), "Cancel", cx).on_click(
                            cx.listener(|p, _, _, cx| {
                                p.assigning = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        primary_button(SharedString::from(format!("{id}-save")), "Save", cx)
                            .on_click(cx.listener(|p, _, window, cx| p.save_assignees(window, cx))),
                    ),
            )
            .into_any_element()
    }

    /// Removes a merged pull request from the list.
    fn drop_listed(&mut self, repo: RepoId, number: u64) {
        for r in self.open.iter_mut().filter(|r| r.repo == repo) {
            if let Ok(prs) = &mut r.result {
                prs.retain(|pr| pr.number != number);
            }
        }
    }

    // ----- new pull requests ----------------------------------------------

    fn prepare(&mut self, ids: Vec<RepoId>, window: &mut Window, cx: &mut Context<Self>) {
        self.plan = None;
        let core = self.core.clone();
        self.run(
            async move { core.prepare_pull_requests(&ids).await },
            window,
            cx,
            |p, result, window, cx| match result {
                Ok(plan) => {
                    let title = plan.title.clone();
                    let target = plan.common_target().unwrap_or_default();
                    p.title.update(cx, |t, cx| t.set_value(title, window, cx));
                    p.target.update(cx, |t, cx| t.set_value(target, window, cx));
                    p.title.update(cx, |t, cx| t.focus(window, cx));
                    p.plan = Some(plan);
                }
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let title = self.title.read(cx).value().to_string();
        let description = self.description.read(cx).value().to_string();
        let target = self.target.read(cx).value().trim().to_owned();
        let draft = self.draft;
        let core = self.core.clone();
        self.run(
            async move {
                let target = (!target.is_empty()).then_some(target);
                core.create_pull_requests(&plan, &title, &description, target.as_deref(), draft)
                    .await
            },
            window,
            cx,
            |p, result, _, cx| match result {
                Ok(outcomes) => {
                    // Opened ones are now "already open": keep only failures
                    // selected, for a retry.
                    let opened: Vec<RepoId> = outcomes
                        .iter()
                        .filter(|o| o.result.is_ok())
                        .map(|o| o.repo)
                        .collect();
                    if let Some(plan) = p.plan.as_mut() {
                        for id in &opened {
                            plan.select(*id, false);
                        }
                    }
                    p.report = outcomes
                        .into_iter()
                        .map(|o| match o.result {
                            Ok(pr) => {
                                let link = o
                                    .link_error
                                    .map_or_else(String::new, |e| format!(" (not linked: {e})"));
                                let text = format!("{}: #{} {}{link}", o.name, pr.number, pr.url);
                                (text, Some(pr.url), false)
                            }
                            Err(e) => (format!("{}: {e}", o.name), None, true),
                        })
                        .collect();
                    let ok = opened.len();
                    cx.emit(ForgePanelEvent::Notice(format!(
                        "Pull requests: {ok} opened, {} failed",
                        p.report.len() - ok
                    )));
                }
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    // ----- issues -----------------------------------------------------------

    fn list_issues(&mut self, ids: Vec<RepoId>, window: &mut Window, cx: &mut Context<Self>) {
        self.armed_close.disarm();
        let core = self.core.clone();
        self.run(
            async move { core.open_issues(&ids).await },
            window,
            cx,
            |p, result, _, _| match result {
                Ok(issues) => p.issues = issues,
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    /// The Close button: the first click arms it, a second one before
    /// [`ARM_TIME`](crate::armed::ARM_TIME) closes with the chosen reason
    /// and comment.
    fn click_close(
        &mut self,
        repo: RepoId,
        number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .armed_close
            .click((repo, number), cx, |p| &mut p.armed_close)
        {
            return;
        }
        let found = self.issues.iter().find(|r| r.repo == repo).and_then(|r| {
            let issue = r
                .result
                .as_ref()
                .ok()?
                .iter()
                .find(|i| i.number == number)?;
            Some((r.forge.clone()?, issue.clone()))
        });
        let Some((forge, issue)) = found else { return };
        let label = format!("{} #{number}", forge.path);
        let url = issue.url.clone();
        let comment = self.close_comment.read(cx).value().to_string();
        let reason = self.close_reason;
        let core = self.core.clone();
        self.run(
            async move {
                core.close_issue(repo, &forge, &issue, reason, Some(comment))
                    .await
            },
            window,
            cx,
            move |p, result, window, cx| match result {
                Ok(()) => {
                    for r in p.issues.iter_mut().filter(|r| r.repo == repo) {
                        if let Ok(issues) = &mut r.result {
                            issues.retain(|i| i.number != number);
                        }
                    }
                    p.close_comment
                        .update(cx, |c, cx| c.set_value("", window, cx));
                    let text = format!("{label} closed");
                    p.report = vec![(text.clone(), Some(url), false)];
                    cx.emit(ForgePanelEvent::Notice(text));
                }
                Err(e) => p.error = Some(format!("{label}: {e}")),
            },
        );
    }

    /// Reopens an issue closed from Archipel in this session.
    fn reopen(
        &mut self,
        repo: RepoId,
        forge: ForgeRepo,
        issue: Issue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = format!("{} #{}", forge.path, issue.number);
        let core = self.core.clone();
        let target = forge.clone();
        let number = issue.number;
        self.run(
            async move { core.reopen_issue(repo, &target, number).await },
            window,
            cx,
            move |p, result, _, cx| match result {
                Ok(()) => {
                    for r in p
                        .issues
                        .iter_mut()
                        .filter(|r| r.forge.as_ref() == Some(&forge))
                    {
                        if let Ok(issues) = &mut r.result {
                            issues.push(issue.clone());
                            issues.sort_by_key(|i| i.number);
                        }
                    }
                    let text = format!("{label} reopened");
                    p.report = vec![(text.clone(), Some(issue.url.clone()), false)];
                    cx.emit(ForgePanelEvent::Notice(text));
                }
                Err(e) => p.error = Some(format!("{label}: {e}")),
            },
        );
    }

    /// Reads the repos a new issue can go to; the first of `ids` (the
    /// repo whose menu was opened) is picked when it takes issues.
    fn prepare_issue(&mut self, ids: Vec<RepoId>, window: &mut Window, cx: &mut Context<Self>) {
        self.issue_targets.clear();
        let preferred = ids.first().copied();
        let core = self.core.clone();
        self.run(
            async move { core.issue_targets(&ids).await },
            window,
            cx,
            move |p, result, window, cx| match result {
                Ok(targets) => {
                    // The preferred repo, else the one picked before, else
                    // the first that takes issues.
                    let usable = |id: &RepoId| {
                        targets
                            .iter()
                            .any(|t| t.repo == *id && t.choices.is_ok() && t.forge.is_some())
                    };
                    p.issue_repo = preferred
                        .filter(usable)
                        .or(p.issue_repo.filter(usable))
                        .or_else(|| {
                            targets
                                .iter()
                                .find(|t| t.choices.is_ok() && t.forge.is_some())
                                .map(|t| t.repo)
                        });
                    p.issue_targets = targets;
                    p.keep_issue_choices();
                    p.issue_title.update(cx, |t, cx| t.focus(window, cx));
                }
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    /// The repo picked for the new issue, when it can take one.
    fn issue_target(&self) -> Option<&IssueTarget> {
        let id = self.issue_repo?;
        self.issue_targets
            .iter()
            .find(|t| t.repo == id && t.forge.is_some() && t.choices.is_ok())
    }

    /// Drops the picked labels and assignees the picked repo does not have.
    fn keep_issue_choices(&mut self) {
        let choices = self
            .issue_target()
            .and_then(|t| t.choices.as_ref().ok())
            .cloned()
            .unwrap_or_default();
        self.issue_labels.retain(|l| choices.labels.contains(l));
        self.issue_assignees
            .retain(|a| choices.assignees.contains(a));
    }

    fn create_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.issue_target() else {
            self.error = Some("Pick a repository that takes issues".into());
            cx.notify();
            return;
        };
        let (repo, Some(forge)) = (target.repo, target.forge.clone()) else {
            return;
        };
        let draft = IssueDraft {
            title: self.issue_title.read(cx).value().to_string(),
            description: self.issue_description.read(cx).value().to_string(),
            labels: self.issue_labels.clone(),
            assignees: self.issue_assignees.clone(),
        };
        let core = self.core.clone();
        let path = forge.path.clone();
        self.run(
            async move { core.create_issue(repo, &forge, draft).await },
            window,
            cx,
            move |p, result, window, cx| match result {
                Ok(created) => {
                    p.issue_title
                        .update(cx, |t, cx| t.set_value("", window, cx));
                    p.issue_description
                        .update(cx, |t, cx| t.set_value("", window, cx));
                    p.issue_labels.clear();
                    p.issue_assignees.clear();
                    let text = format!("Created {path} #{}", created.number);
                    p.report = vec![(
                        format!("{text}: {}", created.url),
                        Some(created.url.clone()),
                        false,
                    )];
                    cx.emit(ForgePanelEvent::NoticeLink(
                        format!("{text} — click to open it on the forge"),
                        created.url,
                    ));
                }
                Err(e) => p.error = Some(format!("{path}: {e}")),
            },
        );
    }

    // ----- clone ----------------------------------------------------------

    fn pick_account(&mut self, account: ForgeAccount, window: &mut Window, cx: &mut Context<Self>) {
        self.account = Some(account.clone());
        self.namespaces.clear();
        self.namespace = None;
        self.repos.clear();
        let core = self.core.clone();
        let picked = account.clone();
        self.run(
            async move { core.forge_namespaces(&account).await },
            window,
            cx,
            move |p, result, window, cx| match result {
                // Another account was picked meanwhile.
                _ if p.account.as_ref() != Some(&picked) => {}
                Ok(namespaces) => {
                    let first = namespaces.first().cloned();
                    p.namespaces = namespaces;
                    if let Some(ns) = first {
                        p.pick_namespace(ns, window, cx);
                    }
                }
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    fn pick_namespace(
        &mut self,
        namespace: Namespace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(account) = self.account.clone() else {
            return;
        };
        self.namespace = Some(namespace.clone());
        self.repos.clear();
        let core = self.core.clone();
        let picked = (account.clone(), namespace.clone());
        self.run(
            async move { core.forge_repositories(&account, &namespace).await },
            window,
            cx,
            move |p, result, _, _| match result {
                // Another account or namespace was picked meanwhile.
                _ if p.account.as_ref() != Some(&picked.0)
                    || p.namespace.as_ref() != Some(&picked.1) => {}
                Ok(repos) => p.repos = repos.into_iter().map(|r| (r, false)).collect(),
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    fn pick_folder(cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |p, cx| {
                    p.folder = paths.into_iter().next();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn clone_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repos: Vec<RemoteRepo> = self
            .repos
            .iter()
            .filter(|(_, on)| *on)
            .map(|(r, _)| r.clone())
            .collect();
        let Some(folder) = self.folder.clone() else {
            self.error = Some("Choose the folder to clone into".into());
            cx.notify();
            return;
        };
        let protocol = if self.ssh {
            CloneProtocol::Ssh
        } else {
            CloneProtocol::Https
        };
        let core = self.core.clone();
        self.run(
            async move { core.clone_into_project(&repos, &folder, protocol).await },
            window,
            cx,
            |p, result, _, cx| match result {
                Ok(outcomes) => {
                    for (_, on) in &mut p.repos {
                        *on = false;
                    }
                    p.report_clones(outcomes, cx);
                }
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    /// Clones the URLs typed in the URL field.
    fn clone_urls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let urls = split_clone_urls(&self.url.read(cx).value());
        if urls.is_empty() {
            self.error = Some("Enter the URL of the repository to clone".into());
            cx.notify();
            return;
        }
        let Some(folder) = self.folder.clone() else {
            self.error = Some("Choose the folder to clone into".into());
            cx.notify();
            return;
        };
        let core = self.core.clone();
        self.run(
            async move { core.clone_urls_into_project(&urls, &folder).await },
            window,
            cx,
            |p, result, window, cx| match result {
                Ok(outcomes) => {
                    // Keep only the URLs that failed, for a retry.
                    let failed: Vec<&str> = outcomes
                        .iter()
                        .filter(|o| o.result.is_err())
                        .map(|o| o.repo.as_str())
                        .collect();
                    let left = failed.join(" ");
                    p.url.update(cx, |i, cx| i.set_value(left, window, cx));
                    p.report_clones(outcomes, cx);
                }
                Err(e) => p.error = Some(e.to_string()),
            },
        );
    }

    /// Shows the per-repo result of a clone and reports it.
    fn report_clones(&mut self, outcomes: Vec<CloneOutcome>, cx: &mut Context<Self>) {
        self.report = outcomes
            .into_iter()
            .map(|o| match o.result {
                Ok(path) => (
                    format!("{}: {}", o.repo, path.as_path().display()),
                    None,
                    false,
                ),
                Err(e) => (format!("{}: {e}", o.repo), None, true),
            })
            .collect();
        let ok = self.report.iter().filter(|l| !l.2).count();
        cx.emit(ForgePanelEvent::Notice(format!(
            "Clone: {ok} added to the project, {} failed",
            self.report.len() - ok
        )));
    }

    // ----- rendering --------------------------------------------------------

    fn render_accounts(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let accounts = self.accounts.iter().enumerate().map(|(i, a)| {
            let host = a.host.clone();
            div()
                .flex()
                .justify_between()
                .items_center()
                .px_2()
                .py_1()
                .child(format!("{} — {} ({})", a.host, a.user, kind_label(&a.kind)))
                .child(
                    button(SharedString::from(format!("f-out-{i}")), "Sign out", cx).on_click(
                        cx.listener(move |p, _, window, cx| p.sign_out(host.clone(), window, cx)),
                    ),
                )
        });
        let mut kinds: Vec<Option<ForgeKind>> = vec![None];
        kinds.extend(self.core.forge_kinds().into_iter().map(Some));
        let kind_chips = kinds.into_iter().enumerate().map(|(i, k)| {
            let label = k.as_ref().map_or_else(|| "Detect".to_owned(), kind_label);
            let active = self.kind == k;
            chip(SharedString::from(format!("f-kind-{i}")), label, active, cx).on_click(
                cx.listener(move |p, _, _, cx| {
                    p.kind.clone_from(&k);
                    cx.notify();
                }),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section("SIGNED IN", cx))
            .when(self.accounts.is_empty(), |d| {
                d.child(
                    div()
                        .px_2()
                        .text_color(color(cx, T::FgMuted))
                        .child("No account yet."),
                )
            })
            .children(accounts)
            .child(section("SIGN IN WITH A PERSONAL ACCESS TOKEN", cx))
            .child(Input::new(&self.host))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .items_center()
                    .child("Forge:")
                    .children(kind_chips),
            )
            .child(Input::new(&self.token))
            .child(div().text_xs().text_color(color(cx, T::FgSubtle)).child(
                "GitHub: classic token with the repo and read:org scopes (or a fine-grained \
                         token with pull requests, checks, contents and issues, and \
                         administration to publish new repositories). GitLab: api scope. The token goes to the OS \
                         keychain, never to archipel.toml.",
            ))
            .child(
                div().flex().justify_end().child(
                    primary_button("f-sign-in", "Sign in", cx)
                        .on_click(cx.listener(|p, _, window, cx| p.sign_in(window, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_open(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let ids = match &self.tab {
            ForgeTab::Open(ids) => ids.clone(),
            _ => Vec::new(),
        };
        let methods = [
            (MergeMethod::Merge, "Merge commit"),
            (MergeMethod::Squash, "Squash"),
            (MergeMethod::Rebase, "Rebase"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (m, label))| {
            chip(
                SharedString::from(format!("f-method-{i}")),
                label,
                self.method == m,
                cx,
            )
            .on_click(cx.listener(move |p, _, _, cx| {
                p.method = m;
                cx.notify();
            }))
        });
        let delete = self.delete_branch;
        let options = div()
            .flex()
            .gap_1()
            .items_center()
            .children(methods)
            .child(
                check("f-delete-branch", "Delete source branch", delete, cx)
                    .ml_2()
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.delete_branch = !p.delete_branch;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(button("f-open-refresh", "Refresh", cx).on_click(
                cx.listener(move |p, _, window, cx| p.list_open(ids.clone(), window, cx)),
            ));
        let mut list = div().flex().flex_col().gap_1();
        for (ri, r) in self.open.iter().enumerate() {
            list = list.child(section(r.name.clone(), cx));
            let prs = match &r.result {
                Err(e) => {
                    list = list.child(
                        div()
                            .px_2()
                            .text_color(color(cx, T::FgSubtle))
                            .child(e.clone()),
                    );
                    continue;
                }
                Ok(prs) if prs.is_empty() => {
                    list = list.child(
                        div()
                            .px_2()
                            .text_color(color(cx, T::FgSubtle))
                            .child("No open pull request"),
                    );
                    continue;
                }
                Ok(prs) => prs,
            };
            for pr in prs {
                list = list.child(self.open_row(ri, r, pr, cx));
                if self.assigning == Some((r.repo, pr.number)) {
                    let id = format!("f-assign-{ri}-{}", pr.number);
                    list = list.child(self.assign_editor(&id, r.repo, &pr.assignees, cx));
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(options)
            .child(div().text_xs().text_color(color(cx, T::FgSubtle)).child(
                "Click a title to open it on the forge. Assign picks who it is assigned to. \
                 Merge asks for a second click, and is \
                 refused if the branch moved since this list was read. When CI passes arms the \
                 forge's auto-merge with the same choices (merged even with Archipel closed); \
                 when the forge refuses it, Archipel merges once the CI passes, only while it \
                 runs.",
            ))
            .child(list)
            .into_any_element()
    }

    /// "When CI passes" (and "… in all N repos" for linked pull requests),
    /// or "Cancel auto-merge" once armed.
    fn auto_merge_controls(
        &self,
        id: &str,
        repo: RepoId,
        pr: &PullRequestStatus,
        armed: bool,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        const WHEN: &str = "When CI passes";
        const CANCEL: &str = "Cancel auto-merge";
        let number = pr.number;
        let linked = if pr.from_fork {
            Vec::new()
        } else {
            self.linked(&pr.source)
        };
        // Cancel takes the place of "When CI passes", as wide, and the
        // linked button keeps its room while armed: arming moves nothing.
        let first = if armed {
            steady_button(
                SharedString::from(format!("{id}-auto-cancel")),
                CANCEL,
                &[WHEN],
                cx,
            )
            .on_click(cx.listener(move |p, _, window, cx| {
                p.cancel_auto_merge(repo, number, window, cx);
            }))
        } else {
            steady_button(
                SharedString::from(format!("{id}-auto")),
                WHEN,
                &[CANCEL],
                cx,
            )
            .on_click(cx.listener(move |p, _, window, cx| {
                p.arm_auto_merge(vec![(repo, number)], window, cx);
            }))
        };
        div()
            .flex()
            .gap_1()
            .child(first)
            .when(linked.len() > 1, |d| {
                let n = linked.len();
                d.child(
                    button(
                        SharedString::from(format!("{id}-auto-linked")),
                        format!("… in all {n} repos"),
                        cx,
                    )
                    .shown_if(!armed)
                    .on_click(cx.listener(move |p, _, window, cx| {
                        p.arm_auto_merge(linked.clone(), window, cx);
                    })),
                )
            })
            .into_any_element()
    }

    /// One open pull request: title (opens it), details, merge state and
    /// the Merge button (none on a draft).
    fn open_row(
        &self,
        ri: usize,
        r: &RepoPullRequests,
        pr: &PullRequestStatus,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (repo, number, url) = (r.repo, pr.number, pr.url.clone());
        let mut detail = format!(
            "{} → {} · {} · {} · {}",
            pr.source,
            pr.target,
            pr.author,
            ci_label(pr.ci),
            review_label(pr.review)
        );
        if pr.draft {
            detail.push_str(" · draft");
        }
        if !pr.assignees.is_empty() {
            detail = format!("{detail} · → {}", pr.assignees.join(", "));
        }
        let (state, tone) = match &pr.merge {
            MergeState::Unknown => (String::new(), T::FgMuted),
            MergeState::Mergeable => ("mergeable".to_owned(), T::StatusSuccess),
            MergeState::Conflicts => ("conflicts".to_owned(), T::StatusError),
            MergeState::Blocked(why) => (why.clone(), T::StatusWarning),
        };
        let id = format!("f-open-{ri}-{number}");
        let watch = self.core.auto_merge_watch(repo, number);
        let note = auto_merge_note(pr, watch.as_ref());
        let (state, tone) = match (&note, &watch) {
            (Some(_), Some(w)) if w.waiting.is_some() => {
                (format!("{AUTO_MERGE_MARK} (Archipel)"), T::StatusWarning)
            }
            (Some(_), Some(_)) => (format!("{AUTO_MERGE_MARK} (Archipel)"), T::Accent),
            (Some(_), None) => (AUTO_MERGE_MARK.to_owned(), T::Accent),
            (None, _) => (state, tone),
        };
        let auto = self.auto_merge_controls(&id, repo, pr, note.is_some(), cx);
        let assign = chip(
            SharedString::from(format!("{id}-assign")),
            "Assign",
            self.assigning == Some((repo, number)),
            cx,
        )
        .on_click(cx.listener(move |p, _, window, cx| {
            p.edit_assignees(repo, number, window, cx);
        }));
        let merge = confirm_button(
            SharedString::from(format!("{id}-merge")),
            "Merge",
            "Confirm merge",
            self.armed.is(&(repo, number)),
            cx,
        )
        .on_click(cx.listener(move |p, _, window, cx| {
            p.click_merge(repo, number, window, cx);
        }));
        div()
            .flex()
            .gap_2()
            .items_center()
            .px_2()
            .py_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(SharedString::from(id))
                            .truncate()
                            .cursor_pointer()
                            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                            .child(format!("{} {}", pr_ref(r.forge.as_ref(), number), pr.title)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgMuted))
                            .child(detail),
                    )
                    .children(note.map(|note| {
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .child(note)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, tone))
                    .child(state),
            )
            .child(div().flex_none().child(assign))
            // A draft keeps their room, unpainted, so the buttons line up
            // from row to row.
            .child(div().flex_none().child(auto).shown_if(!pr.draft))
            .child(merge.shown_if(!pr.draft))
    }

    fn render_create(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let Some(plan) = &self.plan else {
            return div().into_any_element();
        };
        let mismatch = plan.branch_mismatch();
        let count = plan.selected().count();
        let rows = plan.targets.iter().enumerate().map(|(i, t)| {
            let id = t.repo;
            let selected = t.selected && t.problem.is_none();
            let detail = match &t.problem {
                Some(p) => p.clone(),
                None => format!(
                    "{} → {}{}",
                    t.branch.clone().unwrap_or_default(),
                    t.target,
                    if t.needs_push {
                        " · will push the branch first"
                    } else {
                        ""
                    }
                ),
            };
            let tone = if t.problem.is_some() {
                T::FgSubtle
            } else {
                T::FgMuted
            };
            div()
                .id(SharedString::from(format!("f-pr-{i}")))
                .flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(radius(cx))
                .cursor_pointer()
                // A repo with a problem cannot be part of the plan.
                .on_click_if(
                    t.problem.is_none(),
                    cx.listener(move |p, _, _, cx| {
                        if let Some(plan) = p.plan.as_mut() {
                            plan.select(id, !selected);
                        }
                        cx.notify();
                    }),
                )
                .child(check_box(selected, cx))
                .child(t.name.clone())
                .child(div().text_xs().text_color(color(cx, tone)).child(detail))
        });
        let draft = self.draft;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section("REPOSITORIES (click to select)", cx))
            .children(rows)
            .when(mismatch, |d| {
                d.child(
                    div()
                        .text_color(color(cx, T::StatusWarning))
                        .child("⚠ The selected repositories are on different branches"),
                )
            })
            .child(Input::new(&self.title))
            .child(Textarea::new(&self.description))
            .child(Input::new(&self.target))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(check("f-draft", "Draft", draft, cx).on_click(cx.listener(
                        |p, _, _, cx| {
                            p.draft = !p.draft;
                            cx.notify();
                        },
                    )))
                    .child(
                        primary_button(
                            "f-create",
                            match count {
                                1 => "Open 1 pull request".to_owned(),
                                n => format!("Open {n} linked pull requests"),
                            },
                            cx,
                        )
                        .on_click(cx.listener(|p, _, window, cx| p.create(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_issues(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let ids = match &self.tab {
            ForgeTab::Issues(ids) => ids.clone(),
            _ => Vec::new(),
        };
        let reasons = [
            (CloseReason::Completed, "Completed"),
            (CloseReason::NotPlanned, "Not planned"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (r, label))| {
            chip(
                SharedString::from(format!("f-reason-{i}")),
                label,
                self.close_reason == r,
                cx,
            )
            .on_click(cx.listener(move |p, _, _, cx| {
                p.close_reason = r;
                cx.notify();
            }))
        });
        let options = div()
            .flex()
            .gap_1()
            .items_center()
            .child("Close as:")
            .children(reasons)
            .child(div().flex_1())
            .child(button("f-issues-refresh", "Refresh", cx).on_click(
                cx.listener(move |p, _, window, cx| p.list_issues(ids.clone(), window, cx)),
            ));
        let closed = self.core.closed_issues();
        let now = Timestamp::now();
        let mut list = div().flex().flex_col().gap_1();
        for (ri, r) in self.issues.iter().enumerate() {
            list = list.child(section(r.name.clone(), cx));
            match &r.result {
                Err(e) => {
                    list = list.child(
                        div()
                            .px_2()
                            .text_color(color(cx, T::FgSubtle))
                            .child(e.clone()),
                    );
                }
                Ok(issues) if issues.is_empty() => {
                    list = list.child(
                        div()
                            .px_2()
                            .text_color(color(cx, T::FgSubtle))
                            .child("No open issue"),
                    );
                }
                Ok(issues) => {
                    for issue in issues {
                        list = list.child(self.issue_row(ri, r.repo, issue, now, cx));
                    }
                }
            }
            let reopenable = closed
                .iter()
                .filter(|c| r.forge.as_ref() == Some(&c.forge))
                .filter(|c| {
                    // Not listed as open again (reopened on the forge).
                    !r.result
                        .as_ref()
                        .is_ok_and(|open| open.iter().any(|i| i.number == c.issue.number))
                });
            for c in reopenable {
                list = list.child(Self::closed_row(ri, c.repo, &c.forge, &c.issue, cx));
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(options)
            .child(Input::new(&self.close_comment))
            .child(div().text_xs().text_color(color(cx, T::FgSubtle)).child(
                "Click a title to open it on the forge. Close asks for a second click; the \
                 comment, when given, is posted first. GitLab has no close reason. An issue \
                 closed here can be reopened for the rest of the session.",
            ))
            .child(list)
            .into_any_element()
    }

    /// One open issue: title (opens it), author, labels, assignees, age,
    /// and the Close button.
    fn issue_row(
        &self,
        ri: usize,
        repo: RepoId,
        issue: &Issue,
        now: Timestamp,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (number, url) = (issue.number, issue.url.clone());
        let mut detail = vec![issue.author.clone()];
        if !issue.labels.is_empty() {
            detail.push(issue.labels.join(", "));
        }
        if !issue.assignees.is_empty() {
            detail.push(format!("→ {}", issue.assignees.join(", ")));
        }
        if let Some(at) = issue.created_at {
            detail.push(relative_time(at, now));
        }
        let id = format!("f-issue-{ri}-{number}");
        let close = confirm_button(
            SharedString::from(format!("{id}-close")),
            "Close",
            "Confirm close",
            self.armed_close.is(&(repo, number)),
            cx,
        )
        .on_click(cx.listener(move |p, _, window, cx| {
            p.click_close(repo, number, window, cx);
        }));
        let open_url = url.clone();
        div()
            .flex()
            .gap_2()
            .items_center()
            .px_2()
            .py_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id(SharedString::from(id.clone()))
                            .truncate()
                            .cursor_pointer()
                            .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                            .child(format!("#{number} {}", issue.title)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgMuted))
                            .child(detail.join(" · ")),
                    ),
            )
            .child(
                button(
                    SharedString::from(format!("{id}-open")),
                    "Open on forge",
                    cx,
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&open_url))),
            )
            .child(close)
    }

    /// An issue closed from here in this session, with Reopen.
    fn closed_row(
        ri: usize,
        repo: RepoId,
        forge: &ForgeRepo,
        issue: &Issue,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = format!("f-closed-{ri}-{}", issue.number);
        let (forge, reopened) = (forge.clone(), issue.clone());
        div()
            .flex()
            .gap_2()
            .items_center()
            .px_2()
            .py_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(color(cx, T::FgSubtle))
                    .child(format!("#{} {} · closed", issue.number, issue.title)),
            )
            .child(
                button(SharedString::from(format!("{id}-reopen")), "Reopen", cx).on_click(
                    cx.listener(move |p, _, window, cx| {
                        p.reopen(repo, forge.clone(), reopened.clone(), window, cx);
                    }),
                ),
            )
    }

    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render_new_issue(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        if self.issue_targets.is_empty() {
            return div().into_any_element();
        }
        let picked = self.issue_repo;
        let repos = self.issue_targets.iter().enumerate().map(|(i, t)| {
            let id = t.repo;
            let usable = t.forge.is_some() && t.choices.is_ok();
            let (detail, tone) = match (&t.forge, &t.choices) {
                (Some(f), Ok(_)) => (f.path.clone(), T::FgMuted),
                (_, Err(e)) => (e.clone(), T::FgSubtle),
                (None, Ok(_)) => ("no forge repository".to_owned(), T::FgSubtle),
            };
            div()
                .id(SharedString::from(format!("f-issue-repo-{i}")))
                .flex()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(radius(cx))
                .cursor_pointer()
                .on_click_if(
                    usable,
                    cx.listener(move |p, _, _, cx| {
                        p.issue_repo = Some(id);
                        p.keep_issue_choices();
                        cx.notify();
                    }),
                )
                .child(check_box(usable && picked == Some(id), cx))
                .child(t.name.clone())
                .child(div().text_xs().text_color(color(cx, tone)).child(detail))
        });
        let choices = self
            .issue_target()
            .and_then(|t| t.choices.as_ref().ok())
            .cloned()
            .unwrap_or_default();
        let labels = choices.labels.into_iter().enumerate().map(|(i, label)| {
            let on = self.issue_labels.contains(&label);
            chip(
                SharedString::from(format!("f-label-{i}")),
                label.clone(),
                on,
                cx,
            )
            .on_click(cx.listener(move |p, _, _, cx| {
                toggle(&mut p.issue_labels, &label);
                cx.notify();
            }))
        });
        let assignees = choices.assignees.into_iter().enumerate().map(|(i, login)| {
            let on = self.issue_assignees.contains(&login);
            chip(
                SharedString::from(format!("f-assignee-{i}")),
                login.clone(),
                on,
                cx,
            )
            .on_click(cx.listener(move |p, _, _, cx| {
                toggle(&mut p.issue_assignees, &login);
                cx.notify();
            }))
        });
        let (labels, assignees): (Vec<_>, Vec<_>) = (labels.collect(), assignees.collect());
        let usable = self.issue_target().is_some();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section("REPOSITORY (click to pick)", cx))
            .children(repos)
            .when(!usable, |d| {
                d.child(
                    div()
                        .px_2()
                        .text_color(color(cx, T::StatusWarning))
                        .child("None of these repositories takes issues here"),
                )
            })
            .when(usable, |d| {
                d.child(Input::new(&self.issue_title))
                    .child(Textarea::new(&self.issue_description))
                    .when(!labels.is_empty(), |d| {
                        d.child(section("LABELS", cx))
                            .child(div().flex().flex_wrap().gap_1().children(labels))
                    })
                    .when(!assignees.is_empty(), |d| {
                        d.child(section("ASSIGNEES", cx))
                            .child(div().flex().flex_wrap().gap_1().children(assignees))
                    })
                    .child(
                        div().flex().justify_end().child(
                            primary_button("f-create-issue", "Create issue", cx).on_click(
                                cx.listener(|p, _, window, cx| p.create_issue(window, cx)),
                            ),
                        ),
                    )
            })
            .into_any_element()
    }

    /// Clone tab: the destination folder, then the two sources (URLs, which
    /// need no account, and an account's repositories).
    fn render_clone(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let folder = self.folder.as_ref().map_or_else(
            || "No folder chosen".to_owned(),
            |f| f.display().to_string(),
        );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .child(section("CLONE INTO", cx))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        button("f-folder", "Choose folder…", cx)
                            .on_click(cx.listener(|_, _, _, cx| Self::pick_folder(cx))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgMuted))
                            .child(folder),
                    ),
            )
            .child(self.render_clone_url(cx))
            .child(section("FROM AN ACCOUNT", cx))
            .child(self.render_clone_account(cx))
            .into_any_element()
    }

    /// Clone from URLs: the field, where each one lands, the button.
    fn render_clone_url(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let urls = split_clone_urls(&self.url.read(cx).value());
        let names: Vec<String> = urls
            .iter()
            .map(|u| clone_dir_name(u).unwrap_or_else(|| format!("? ({u})")))
            .collect();
        let (hint, tone) = if names.is_empty() {
            (
                "Any URL git clone accepts (HTTPS, SSH, a local path). Credentials come from \
                 your Git setup: no forge account needed."
                    .to_owned(),
                T::FgSubtle,
            )
        } else if names.iter().any(|n| n.starts_with('?')) {
            (
                format!("Cannot name a folder after: {}", names.join(", ")),
                T::StatusWarning,
            )
        } else {
            (format!("Creates {}", names.join(", ")), T::FgMuted)
        };
        let label = match urls.len() {
            0 | 1 => "Clone into the project".to_owned(),
            n => format!("Clone {n} into the project"),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section("FROM A URL", cx))
            .child(Input::new(&self.url))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(color(cx, tone))
                            .child(hint),
                    )
                    .child(
                        primary_button("f-clone-url", label, cx)
                            .on_click(cx.listener(|p, _, window, cx| p.clone_urls(window, cx))),
                    ),
            )
    }

    /// Clone from an account: account, namespace, repositories.
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render_clone_account(&mut self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        if self.accounts.is_empty() {
            return div()
                .flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .text_color(color(cx, T::FgMuted))
                        .child("Sign in to a forge to browse an account's repositories."),
                )
                .child(button("f-to-accounts", "Accounts", cx).on_click(
                    cx.listener(|p, _, window, cx| p.show(ForgeTab::Accounts, window, cx)),
                ))
                .into_any_element();
        }
        let account_chips = self.accounts.clone().into_iter().enumerate().map(|(i, a)| {
            let active = self.account.as_ref() == Some(&a);
            chip(
                SharedString::from(format!("f-acc-{i}")),
                a.host.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |p, _, window, cx| p.pick_account(a.clone(), window, cx)))
        });
        let ns_chips = self
            .namespaces
            .clone()
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                let active = self.namespace.as_ref() == Some(&n);
                chip(
                    SharedString::from(format!("f-ns-{i}")),
                    n.path.clone(),
                    active,
                    cx,
                )
                .on_click(
                    cx.listener(move |p, _, window, cx| p.pick_namespace(n.clone(), window, cx)),
                )
            });
        let query = self.filter.read(cx).value().to_lowercase();
        let repos = self
            .repos
            .iter()
            .enumerate()
            .filter(|(_, (r, _))| query.is_empty() || r.path.to_lowercase().contains(&query))
            .map(|(i, (r, on))| {
                let detail = match (&r.description, r.archived) {
                    (_, true) => "archived".to_owned(),
                    (Some(d), _) => d.clone(),
                    (None, _) => String::new(),
                };
                div()
                    .id(SharedString::from(format!("f-repo-{i}")))
                    .flex()
                    .gap_2()
                    .px_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(entry) = p.repos.get_mut(i) {
                            entry.1 = !entry.1;
                        }
                        cx.notify();
                    }))
                    .child(check_box(*on, cx))
                    .child(div().flex_none().child(r.path.clone()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgMuted))
                            .child(detail),
                    )
            });
        let chosen = self.repos.iter().filter(|(_, on)| *on).count();
        let ssh = self.ssh;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .min_h_0()
            .child(div().flex().flex_wrap().gap_1().children(account_chips))
            .child(section("USER, ORGANIZATION OR GROUP", cx))
            .child(div().flex().flex_wrap().gap_1().children(ns_chips))
            .child(section("REPOSITORIES (click to select)", cx))
            .child(Input::new(&self.filter))
            .child(
                div()
                    .id("f-repos")
                    .flex_1()
                    .min_h(px(120.0))
                    .overflow_y_scrollbar()
                    .children(repos),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        check("f-ssh", "Clone over SSH", ssh, cx).on_click(cx.listener(
                            |p, _, _, cx| {
                                p.ssh = !p.ssh;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        primary_button("f-clone", format!("Clone {chosen} into the project"), cx)
                            .on_click(cx.listener(|p, _, window, cx| p.clone_selected(window, cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ForgePanel {
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = [
            ("f-tab-accounts", "Accounts", ForgeTab::Accounts),
            ("f-tab-open", "Pull requests", ForgeTab::Open(Vec::new())),
            (
                "f-tab-create",
                "New pull request",
                ForgeTab::Create(Vec::new()),
            ),
            ("f-tab-issues", "Issues", ForgeTab::Issues(Vec::new())),
            (
                "f-tab-new-issue",
                "New issue",
                ForgeTab::NewIssue(Vec::new()),
            ),
            ("f-tab-clone", "Clone", ForgeTab::Clone),
        ]
        .into_iter()
        .map(|(id, label, tab)| {
            let active = std::mem::discriminant(&tab) == std::mem::discriminant(&self.tab);
            chip(id, label, active, cx).on_click(cx.listener(move |p, _, window, cx| {
                // The pull request and issue tabs keep the repos the panel
                // was opened for.
                let ids = p.tab.repos().cloned().unwrap_or_default();
                let tab = match &tab {
                    ForgeTab::Open(_) => ForgeTab::Open(ids),
                    ForgeTab::Create(_) => ForgeTab::Create(ids),
                    ForgeTab::Issues(_) => ForgeTab::Issues(ids),
                    ForgeTab::NewIssue(_) => ForgeTab::NewIssue(ids),
                    ForgeTab::Accounts | ForgeTab::Clone => tab.clone(),
                };
                p.show(tab, window, cx);
            }))
        })
        .collect::<Vec<_>>();
        let body = match self.tab {
            ForgeTab::Accounts => self.render_accounts(cx),
            ForgeTab::Open(_) => self.render_open(cx),
            ForgeTab::Create(_) => self.render_create(cx),
            ForgeTab::Issues(_) => self.render_issues(cx),
            ForgeTab::NewIssue(_) => self.render_new_issue(cx),
            ForgeTab::Clone => self.render_clone(cx),
        };
        let report = self
            .report
            .iter()
            .enumerate()
            .map(|(i, (text, link, failed))| {
                let link = link.clone();
                div()
                    .id(SharedString::from(format!("f-report-{i}")))
                    .text_xs()
                    .text_color(color(
                        cx,
                        if *failed {
                            T::StatusError
                        } else {
                            T::StatusSuccess
                        },
                    ))
                    .when(link.is_some(), Styled::cursor_pointer)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(url) = &link {
                            cx.open_url(url);
                        }
                    }))
                    .child(text.clone())
            });
        div()
            .id("forge-panel")
            .occlude()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(560.0))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(color(cx, T::BgElevated))
            .border_l_1()
            .border_color(color(cx, T::Border))
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(div().flex().flex_wrap().gap_1().children(tabs))
                    .child(
                        button("f-close", "Close", cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ForgePanelEvent::Closed))),
                    ),
            )
            .when(self.busy(), |d| {
                d.child(div().text_color(color(cx, T::FgMuted)).child("Working…"))
            })
            .child(
                div()
                    .id("f-body")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(color(cx, T::StatusError)).child(e)),
            )
            .children(report)
    }
}
