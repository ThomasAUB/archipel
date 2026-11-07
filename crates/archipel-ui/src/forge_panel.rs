//! The forge panel (spec §8): accounts (sign in with a personal access
//! token, sign out), open pull requests of the selected repos (merge them),
//! new pull requests for the selected repos (one per repo, linked), and
//! clone: from URLs (no account needed) or from an account / organization /
//! group.
//!
//! Every forge call runs on the app runtime (scheduler network pool); the
//! panel only awaits the results.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use archipel_app::forge::{ci_label, review_label};
use archipel_app::{
    App as Core, CloneOutcome, CloneProtocol, PullRequestPlan, RepoPullRequests, clone_dir_name,
    split_clone_urls,
};
use archipel_model::RepoId;
use archipel_model::forge::{
    ForgeAccount, ForgeKind, ForgeRepo, MergeMethod, MergeState, Namespace, PullRequestStatus,
    RemoteRepo,
};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, EventEmitter, PathPromptOptions, SharedString, Subscription, Window, div, px,
};

use crate::theme::{color, radius};
use crate::widgets::{button, chip, primary_button, section};

/// Which part of the panel is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeTab {
    /// Signed-in hosts, sign-in form.
    Accounts,
    /// Open pull requests of these repos (all when empty), to merge.
    Open(Vec<RepoId>),
    /// New pull requests for these repos (all when empty).
    Create(Vec<RepoId>),
    /// Clone from URLs or from an account.
    Clone,
}

/// What the panel reports to the workspace.
pub enum ForgePanelEvent {
    /// Closed.
    Closed,
    /// A one-line result for the status bar.
    Notice(String),
}

/// Report line: text, link, error.
type Line = (String, Option<String>, bool);

/// How long a Merge button waits for the confirming second click.
const MERGE_ARM_TIME: Duration = Duration::from_secs(4);

/// The panel.
#[allow(clippy::struct_excessive_bools)] // independent form toggles and the busy flag
pub struct ForgePanel {
    core: Arc<Core>,
    tab: ForgeTab,
    busy: bool,
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
    armed: Option<(RepoId, u64)>,
    // New pull requests.
    plan: Option<PullRequestPlan>,
    title: Entity<InputState>,
    description: Entity<TextareaState>,
    target: Entity<InputState>,
    draft: bool,
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
        let mut panel = Self {
            accounts: core.forge_accounts(),
            core,
            tab: ForgeTab::Accounts,
            busy: false,
            error: None,
            kind: None,
            host,
            token,
            open: Vec::new(),
            method: MergeMethod::default(),
            delete_branch: true,
            armed: None,
            plan: None,
            title,
            description,
            target,
            draft: false,
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
        self.busy = true;
        self.error = None;
        let handle = self.core.scheduler().runtime().spawn(job);
        cx.spawn_in(window, async move |this, cx| {
            let result = handle.await;
            let _ = this.update_in(cx, |p, window, cx| {
                p.busy = false;
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
        self.armed = None;
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
    /// [`MERGE_ARM_TIME`] merges.
    fn click_merge(
        &mut self,
        repo: RepoId,
        number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.armed != Some((repo, number)) {
            self.armed = Some((repo, number));
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(MERGE_ARM_TIME).await;
                let _ = this.update(cx, |p, cx| {
                    if p.armed == Some((repo, number)) {
                        p.armed = None;
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
            return;
        }
        self.armed = None;
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

    // ----- clone ----------------------------------------------------------

    fn pick_account(&mut self, account: ForgeAccount, window: &mut Window, cx: &mut Context<Self>) {
        self.account = Some(account.clone());
        self.namespaces.clear();
        self.namespace = None;
        self.repos.clear();
        let core = self.core.clone();
        self.run(
            async move { core.forge_namespaces(&account).await },
            window,
            cx,
            |p, result, window, cx| match result {
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
        self.run(
            async move { core.forge_repositories(&account, &namespace).await },
            window,
            cx,
            |p, result, _, _| match result {
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
        if self.busy {
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
                         token with pull requests, checks and contents). GitLab: api scope. \
                         The token goes to the OS keychain, never to archipel.toml.",
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
                div()
                    .id("f-delete-branch")
                    .ml_2()
                    .cursor_pointer()
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.delete_branch = !p.delete_branch;
                        cx.notify();
                    }))
                    .child(if delete {
                        "☑ Delete source branch"
                    } else {
                        "☐ Delete source branch"
                    }),
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
            }
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(options)
            .child(div().text_xs().text_color(color(cx, T::FgSubtle)).child(
                "Click a title to open it on the forge. Merge asks for a second click, and is \
                 refused if the branch moved since this list was read.",
            ))
            .child(list)
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
        let (state, tone) = match &pr.merge {
            MergeState::Unknown => (String::new(), T::FgMuted),
            MergeState::Mergeable => ("mergeable".to_owned(), T::StatusSuccess),
            MergeState::Conflicts => ("conflicts".to_owned(), T::StatusError),
            MergeState::Blocked(why) => (why.clone(), T::StatusWarning),
        };
        let id = format!("f-open-{ri}-{number}");
        let merge = if self.armed == Some((repo, number)) {
            primary_button(
                SharedString::from(format!("{id}-merge")),
                "Confirm merge",
                cx,
            )
        } else {
            button(SharedString::from(format!("{id}-merge")), "Merge", cx)
        }
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
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, tone))
                    .child(state),
            )
            .when(!pr.draft, |d| d.child(merge))
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
                .when(t.problem.is_none(), Styled::cursor_pointer)
                .on_click(cx.listener(move |p, _, _, cx| {
                    if let Some(plan) = p.plan.as_mut() {
                        plan.select(id, !selected);
                    }
                    cx.notify();
                }))
                .child(if selected { "☑" } else { "☐" })
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
                    .child(
                        div()
                            .id("f-draft")
                            .cursor_pointer()
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.draft = !p.draft;
                                cx.notify();
                            }))
                            .child(if draft { "☑ Draft" } else { "☐ Draft" }),
                    )
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
                    .child(if *on { "☑" } else { "☐" })
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
                        div()
                            .id("f-ssh")
                            .cursor_pointer()
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.ssh = !p.ssh;
                                cx.notify();
                            }))
                            .child(if ssh {
                                "☑ Clone over SSH"
                            } else {
                                "☐ Clone over SSH"
                            }),
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
            ("f-tab-clone", "Clone", ForgeTab::Clone),
        ]
        .into_iter()
        .map(|(id, label, tab)| {
            let active = std::mem::discriminant(&tab) == std::mem::discriminant(&self.tab);
            chip(id, label, active, cx).on_click(cx.listener(move |p, _, window, cx| {
                // The pull request tabs keep the repos the panel was opened for.
                let tab = match (&tab, &p.tab) {
                    (ForgeTab::Open(_), ForgeTab::Open(ids) | ForgeTab::Create(ids)) => {
                        ForgeTab::Open(ids.clone())
                    }
                    (ForgeTab::Create(_), ForgeTab::Open(ids) | ForgeTab::Create(ids)) => {
                        ForgeTab::Create(ids.clone())
                    }
                    _ => tab.clone(),
                };
                p.show(tab, window, cx);
            }))
        })
        .collect::<Vec<_>>();
        let body = match self.tab {
            ForgeTab::Accounts => self.render_accounts(cx),
            ForgeTab::Open(_) => self.render_open(cx),
            ForgeTab::Create(_) => self.render_create(cx),
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
                    .child(div().flex().gap_1().children(tabs))
                    .child(
                        button("f-close", "Close", cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ForgePanelEvent::Closed))),
                    ),
            )
            .when(self.busy, |d| {
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
