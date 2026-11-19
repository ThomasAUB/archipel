//! The Publish dialog, opened by Push on a repository without remote (one
//! made with New…): the repository is created on a signed-in forge account
//! (owner, name, description, visibility), or the URL of an existing empty
//! one is given; it becomes `origin` and the branch is pushed there and
//! tracked.

use std::path::Path;
use std::sync::Arc;

use archipel_app::{App as Core, CloneProtocol, PublishTarget, publish_name};
use archipel_model::RepoId;
use archipel_model::forge::{ForgeAccount, Namespace, NewRemoteRepo};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, EventEmitter, KeyDownEvent, SharedString, Subscription, Window, div,
};

use crate::theme::color;
use crate::widgets::{button, check, chip, dialog_frame, field, primary_button};

/// What the dialog reports to the workspace.
pub enum PublishEvent {
    /// Closed (cancelled or published).
    Closed,
    /// Open the forge panel's accounts, to sign in.
    SignIn,
}

/// The Publish dialog.
pub struct PublishDialog {
    core: Arc<Core>,
    repo: RepoId,
    branch: String,
    accounts: Vec<ForgeAccount>,
    account: Option<ForgeAccount>,
    /// The account's namespaces; `None` while they load.
    namespaces: Option<Vec<Namespace>>,
    namespace: Option<Namespace>,
    name: Entity<InputState>,
    description: Entity<InputState>,
    url: Entity<InputState>,
    private: bool,
    ssh: bool,
    /// Publishing: the buttons wait.
    busy: bool,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PublishEvent> for PublishDialog {}

impl PublishDialog {
    /// The dialog for `branch` of repo row `repo` at `dir`, on the first
    /// signed-in account.
    pub fn new(
        core: Arc<Core>,
        repo: RepoId,
        dir: &Path,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Repository name")
                .default_value(publish_name(dir))
        });
        let description =
            cx.new(|cx| InputState::new(window, cx).placeholder("Description (optional)"));
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Or the URL of an existing empty repository, instead of creating one")
        });
        let on_input = |this: &mut Self,
                        _: &Entity<InputState>,
                        e: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<Self>| {
            match e {
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.publish(window, cx),
                _ => {}
            }
        };
        let subs = vec![
            cx.subscribe_in(&name, window, on_input),
            cx.subscribe_in(&description, window, on_input),
            cx.subscribe_in(&url, window, on_input),
        ];
        let accounts = core.forge_accounts();
        let mut dialog = Self {
            core,
            repo,
            branch,
            accounts,
            account: None,
            namespaces: None,
            namespace: None,
            name,
            description,
            url,
            private: true,
            ssh: false,
            busy: false,
            error: None,
            _subs: subs,
        };
        match dialog.accounts.first().cloned() {
            Some(account) => {
                dialog.name.update(cx, |n, cx| n.focus(window, cx));
                dialog.pick_account(account, window, cx);
            }
            None => dialog.url.update(cx, |u, cx| u.focus(window, cx)),
        }
        dialog
    }

    /// Picks `account` and loads its namespaces, the user's own picked.
    fn pick_account(&mut self, account: ForgeAccount, window: &mut Window, cx: &mut Context<Self>) {
        self.account = Some(account.clone());
        self.namespaces = None;
        self.namespace = None;
        let core = self.core.clone();
        let picked = account.clone();
        let handle = self
            .core
            .scheduler()
            .runtime()
            .spawn(async move { core.forge_namespaces(&account).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = handle.await;
            let _ = this.update(cx, |d, cx| {
                // Another account was picked meanwhile.
                if d.account.as_ref() != Some(&picked) {
                    return;
                }
                match result {
                    Ok(Ok(namespaces)) => {
                        d.namespace = namespaces.first().cloned();
                        d.namespaces = Some(namespaces);
                    }
                    Ok(Err(e)) => {
                        d.namespaces = Some(Vec::new());
                        d.error = Some(e.to_string());
                    }
                    Err(e) => d.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// What Publish does: the URL typed, else a new repository on the
    /// account picked.
    fn target(&self, cx: &gpui_kit::App) -> Result<PublishTarget, String> {
        let url = self.url.read(cx).value().trim().to_owned();
        if !url.is_empty() {
            return Ok(PublishTarget::Url(url));
        }
        let Some(account) = self.account.clone() else {
            return Err("Sign in to a forge, or enter the URL of an existing repository".into());
        };
        let Some(namespace) = self.namespace.clone() else {
            return Err("Pick the owner of the repository".into());
        };
        let description = self.description.read(cx).value().trim().to_owned();
        Ok(PublishTarget::Forge {
            account,
            namespace,
            repo: NewRemoteRepo {
                name: self.name.read(cx).value().trim().to_owned(),
                description: (!description.is_empty()).then_some(description),
                private: self.private,
            },
            protocol: if self.ssh {
                CloneProtocol::Ssh
            } else {
                CloneProtocol::Https
            },
        })
    }

    fn publish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let target = match self.target(cx) {
            Ok(target) => target,
            Err(why) => {
                self.error = Some(why);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let (core, repo) = (self.core.clone(), self.repo);
        let handle = self
            .core
            .scheduler()
            .runtime()
            .spawn(async move { core.publish_repo(repo, target).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = handle.await;
            let _ = this.update(cx, |d, cx| {
                d.busy = false;
                match result {
                    Ok(Ok(_)) => cx.emit(PublishEvent::Closed),
                    Ok(Err(e)) => d.error = Some(e.to_string()),
                    Err(e) => d.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Account, owner, name, description, visibility and protocol; or how
    /// to sign in when no account is.
    fn render_forge(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        if self.accounts.is_empty() {
            return div()
                .flex()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .text_color(color(cx, T::FgMuted))
                        .child("Sign in to GitHub or GitLab to create the repository from here."),
                )
                .child(
                    button("publish-sign-in", "Sign in…", cx)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(PublishEvent::SignIn))),
                )
                .into_any_element();
        }
        let accounts = self.accounts.clone().into_iter().enumerate().map(|(i, a)| {
            let active = self.account.as_ref() == Some(&a);
            chip(
                SharedString::from(format!("publish-acc-{i}")),
                format!("{} ({})", a.host, a.user),
                active,
                cx,
            )
            .on_click(cx.listener(move |d, _, window, cx| d.pick_account(a.clone(), window, cx)))
        });
        let owners: gpui_kit::AnyElement = match &self.namespaces {
            None => div()
                .text_color(color(cx, T::FgMuted))
                .child("Loading…")
                .into_any_element(),
            Some(namespaces) => div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(namespaces.clone().into_iter().enumerate().map(|(i, n)| {
                    let active = self.namespace.as_ref() == Some(&n);
                    chip(
                        SharedString::from(format!("publish-ns-{i}")),
                        n.path.clone(),
                        active,
                        cx,
                    )
                    .on_click(cx.listener(move |d, _, _, cx| {
                        d.namespace = Some(n.clone());
                        cx.notify();
                    }))
                }))
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(field(
                "Account",
                div().flex().flex_wrap().gap_1().children(accounts),
                cx,
            ))
            .child(field("Owner", owners, cx))
            .child(field("Name", Input::new(&self.name), cx))
            .child(field("Description", Input::new(&self.description), cx))
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(
                        check("publish-private", "Private", self.private, cx).on_click(
                            cx.listener(|d, _, _, cx| {
                                d.private = !d.private;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        check("publish-ssh", "Push over SSH (else HTTPS)", self.ssh, cx).on_click(
                            cx.listener(|d, _, _, cx| {
                                d.ssh = !d.ssh;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for PublishDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let by_url = !self.url.read(cx).value().trim().is_empty();
        let label = match (self.busy, by_url) {
            (true, _) => "Publishing…",
            (false, true) => "Add as origin and push",
            (false, false) => "Create and push",
        };
        dialog_frame("publish", format!("Publish {}", self.branch), cx)
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(PublishEvent::Closed);
                }
            }))
            .child(div().text_color(color(cx, T::FgMuted)).child(
                "The repository has no remote yet. Create it on a forge, then push \
                            the branch there and track it.",
            ))
            .child(self.render_forge(cx))
            .child(field("URL", Input::new(&self.url), cx))
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(color(cx, T::StatusError)).child(e)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("publish-cancel", "Cancel", cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PublishEvent::Closed))),
                    )
                    .child(
                        primary_button("publish-go", label, cx)
                            .on_click(cx.listener(|d, _, window, cx| d.publish(window, cx))),
                    ),
            )
    }
}
