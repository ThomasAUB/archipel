//! The multi-repo commit panel (spec §4): repos pre-checked when they have
//! staged changes, staged files preview, branch mismatch warning, one
//! message (with ticket prefix), optional push, per-repo report and retry.

use std::sync::Arc;

use archipel_app::App as Core;
use archipel_app::message::{extractor_for, with_ticket};
use archipel_model::RepoPath;
use archipel_ops::{CommitOptions, CommitPlan, Outcome};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, EventEmitter, SharedString, Window, div, px};

use crate::theme::{color, radius};
use crate::widgets::{SHORT_OID, button, primary_button, section};

/// What the panel reports to the workspace.
pub enum CommitPanelEvent {
    /// The panel was closed.
    Closed,
    /// A commit run finished, with a one-line summary.
    Finished(String),
}

/// Staged files shown per repo before "and N more".
const PREVIEW_FILES: usize = 6;

/// The panel.
pub struct CommitPanel {
    core: Arc<Core>,
    plan: Option<CommitPlan>,
    message: Entity<InputState>,
    push_after: bool,
    busy: bool,
    report: Vec<(String, String, bool)>,
    error: Option<String>,
}

impl EventEmitter<CommitPanelEvent> for CommitPanel {}

impl CommitPanel {
    /// Opens the panel and reads every repo of the project in the background.
    pub fn new(core: Arc<Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Commit message for every selected repo")
        });
        message.update(cx, |m, cx| m.focus(window, cx));
        let panel = Self {
            core,
            plan: None,
            message,
            push_after: false,
            busy: true,
            report: Vec::new(),
            error: None,
        };
        panel.prepare(window, cx);
        panel
    }

    fn prepare(&self, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<RepoPath> = self
            .core
            .rows()
            .into_iter()
            .filter(|r| r.summary().is_some())
            .map(|r| r.path)
            .collect();
        let commit = self.core.multi_commit().clone();
        let job = self
            .core
            .scheduler()
            .runtime()
            .spawn(async move { commit.prepare(&paths).await });
        let settings = self
            .core
            .project()
            .map(|(_, def)| def.settings)
            .unwrap_or_default();
        cx.spawn_in(window, async move |this, cx| {
            let plan = job.await;
            let _ = this.update_in(cx, |p, window, cx| {
                p.busy = false;
                match plan {
                    Ok(plan) => {
                        let branch = plan
                            .targets
                            .iter()
                            .find(|t| t.selected)
                            .and_then(|t| t.branch.clone());
                        let ticket =
                            extractor_for(&settings).and_then(|x| x.ticket(branch.as_deref()));
                        if let Some(t) = ticket {
                            p.message
                                .update(cx, |m, cx| m.set_value(format!("{t} "), window, cx));
                        }
                        p.plan = Some(plan);
                    }
                    Err(e) => p.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let settings = self
            .core
            .project()
            .map(|(_, def)| def.settings)
            .unwrap_or_default();
        let branch = plan
            .targets
            .iter()
            .find(|t| t.selected)
            .and_then(|t| t.branch.clone());
        let ticket = extractor_for(&settings).and_then(|x| x.ticket(branch.as_deref()));
        let message = with_ticket(&self.message.read(cx).value(), ticket.as_deref());
        let options = CommitOptions {
            push_after: self.push_after,
            require_same_branch: settings.require_same_branch,
            ..CommitOptions::default()
        };
        let names: Vec<(archipel_model::RepoId, String)> =
            plan.targets.iter().map(|t| (t.id, t.path.name())).collect();
        let commit = self.core.multi_commit().clone();
        let job = self
            .core
            .scheduler()
            .runtime()
            .spawn(async move { commit.run(&plan, &message, &options).await });
        self.busy = true;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match result {
                    Ok(Ok(report)) => {
                        p.report = report
                            .per_repo
                            .iter()
                            .map(|(id, outcome)| {
                                let name = names
                                    .iter()
                                    .find(|(i, _)| i == id)
                                    .map_or_else(String::new, |(_, n)| n.clone());
                                match outcome {
                                    Outcome::Done(c) => {
                                        let push = match &c.push {
                                            None => String::new(),
                                            Some(Ok(())) => ", pushed".into(),
                                            Some(Err(e)) => format!(", push failed: {e}"),
                                        };
                                        (
                                            name,
                                            format!("committed {}{push}", c.oid.short(SHORT_OID)),
                                            false,
                                        )
                                    }
                                    Outcome::Failed(e) => (name, e.to_string(), true),
                                    Outcome::Skipped(r) => (name, format!("skipped: {r}"), false),
                                }
                            })
                            .collect();
                        let (done, failed, _) = report.counts();
                        // Keep only failed repos selected, for "retry".
                        if let Some(plan) = p.plan.as_mut() {
                            let failed_ids = report.failed();
                            for t in &mut plan.targets {
                                t.selected = failed_ids.contains(&t.id);
                            }
                        }
                        cx.emit(CommitPanelEvent::Finished(format!(
                            "Commit: {done} committed, {failed} failed"
                        )));
                        p.core.refresh_all();
                    }
                    Ok(Err(e)) => p.error = Some(e.to_string()),
                    Err(e) => p.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for CommitPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut repos = Vec::new();
        let mut mismatch = false;
        if let Some(plan) = &self.plan {
            mismatch = plan.branch_mismatch();
            for (i, t) in plan.targets.iter().enumerate() {
                let id = t.id;
                let mut files: Vec<String> = t
                    .staged
                    .iter()
                    .take(PREVIEW_FILES)
                    .map(|f| f.path.clone())
                    .collect();
                if t.staged.len() > PREVIEW_FILES {
                    files.push(format!("… and {} more", t.staged.len() - PREVIEW_FILES));
                }
                repos.push(
                    div()
                        .id(SharedString::from(format!("c-repo-{i}")))
                        .flex()
                        .flex_col()
                        .px_2()
                        .py_1()
                        .rounded(radius(cx))
                        .cursor_pointer()
                        .on_click(cx.listener(move |p, _, _, cx| {
                            if let Some(plan) = p.plan.as_mut() {
                                let selected =
                                    plan.targets.iter().any(|t| t.id == id && t.selected);
                                plan.select(id, !selected);
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(if t.selected { "☑" } else { "☐" })
                                .child(t.path.name())
                                .child(
                                    div().text_color(color(cx, T::FgMuted)).child(
                                        t.branch.clone().unwrap_or_else(|| "detached".into()),
                                    ),
                                )
                                .child(div().text_xs().text_color(color(cx, T::FgSubtle)).child(
                                    format!("{} staged, {} unstaged", t.staged.len(), t.unstaged),
                                )),
                        )
                        .children(files.into_iter().map(|f| {
                            div()
                                .pl_6()
                                .text_xs()
                                .text_color(color(cx, T::FgMuted))
                                .child(f)
                        })),
                );
            }
        }
        let report = self.report.iter().map(|(name, text, failed)| {
            div()
                .text_xs()
                .text_color(color(
                    cx,
                    if *failed {
                        T::StatusError
                    } else {
                        T::StatusSuccess
                    },
                ))
                .child(format!("{name}: {text}"))
        });
        let push = self.push_after;
        div()
            .id("commit-panel")
            .occlude()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(520.0))
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
                    .child("Commit on several repositories")
                    .child(
                        button("c-close", "Close", cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CommitPanelEvent::Closed))),
                    ),
            )
            .when(self.busy, |d| {
                d.child(div().text_color(color(cx, T::FgMuted)).child("Working…"))
            })
            .when(mismatch, |d| {
                d.child(
                    div()
                        .text_color(color(cx, T::StatusWarning))
                        .child("⚠ The selected repositories are on different branches"),
                )
            })
            .child(section("REPOSITORIES (click to select)", cx))
            .child(
                div()
                    .id("c-repos")
                    .flex_1()
                    .overflow_y_scrollbar()
                    .children(repos),
            )
            .child(Input::new(&self.message))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .id("c-push")
                            .cursor_pointer()
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.push_after = !p.push_after;
                                cx.notify();
                            }))
                            .child(if push {
                                "☑ Push after commit"
                            } else {
                                "☐ Push after commit"
                            }),
                    )
                    .child(
                        primary_button(
                            "c-run",
                            if self.report.iter().any(|r| r.2) {
                                "Retry failed"
                            } else {
                                "Commit"
                            },
                            cx,
                        )
                        .on_click(cx.listener(|p, _, _, cx| p.run(cx))),
                    ),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(color(cx, T::StatusError)).child(e)),
            )
            .children(report)
    }
}
