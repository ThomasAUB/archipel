//! The branches panel (spec §7): local branches merged into the default
//! branch but never deleted, and stale unmerged branches, across the selected
//! repos (all when none), deleted in one undoable batch.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use archipel_app::App as Core;
use archipel_app::dashboard::relative_time;
use archipel_model::{Oid, RepoId, Timestamp};
use archipel_ops::Outcome;
use archipel_ops::batch::BatchTarget;
use archipel_stats::{BranchRow, DEFAULT_STALE_BRANCH_DAYS};
use archipel_theme::ColorToken as T;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, EventEmitter, SharedString, Window, div, px};

use crate::theme::{color, radius};
use crate::widgets::{button, primary_button, section};

/// What the panel reports to the workspace.
pub enum BranchesPanelEvent {
    /// The panel was closed.
    Closed,
    /// A delete run finished, with a one-line summary.
    Finished(String),
}

/// How long the Delete button waits for the confirming second click.
const ARM_TIME: Duration = Duration::from_secs(4);

/// A branch row of one repo: repo and branch name.
type Key = (RepoId, String);

/// The panel.
pub struct BranchesPanel {
    core: Arc<Core>,
    /// Repos looked at (all when empty).
    scope: Vec<RepoId>,
    merged: Vec<BranchRow>,
    stale: Vec<BranchRow>,
    selected: HashSet<Key>,
    busy: bool,
    /// Delete clicked once: the next click deletes.
    armed: bool,
    errors: Vec<String>,
    report: Vec<(String, String, bool)>,
}

impl EventEmitter<BranchesPanelEvent> for BranchesPanel {}

impl BranchesPanel {
    /// Opens the panel on `scope` (every repo when empty) and lists its
    /// branches in the background.
    pub fn new(core: Arc<Core>, scope: Vec<RepoId>, cx: &mut Context<Self>) -> Self {
        let panel = Self {
            core,
            scope,
            merged: Vec::new(),
            stale: Vec::new(),
            selected: HashSet::new(),
            busy: true,
            armed: false,
            errors: Vec::new(),
            report: Vec::new(),
        };
        panel.load(cx);
        panel
    }

    fn load(&self, cx: &mut Context<Self>) {
        let (core, scope) = (self.core.clone(), self.scope.clone());
        let task = cx.background_spawn(async move { core.branch_ages(&scope) });
        cx.spawn(async move |this, cx| {
            let (view, errors) = task.await;
            let _ = this.update(cx, |p, cx| {
                let min_age = Duration::from_secs(DEFAULT_STALE_BRANCH_DAYS * 86_400);
                p.merged = view.merged_not_deleted().cloned().collect();
                p.stale = view.stale(Timestamp::now(), min_age).cloned().collect();
                // Merged branches lose nothing when deleted: picked by default.
                p.selected = p.merged.iter().map(key).collect();
                p.errors = errors;
                p.busy = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn click_delete(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_empty() || self.busy {
            return;
        }
        if !self.armed {
            self.armed = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ARM_TIME).await;
                let _ = this.update(cx, |p, cx| {
                    p.armed = false;
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
            return;
        }
        self.armed = false;
        self.delete(cx);
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let mut branches: HashMap<RepoId, Vec<(String, Oid)>> = HashMap::new();
        for row in self.merged.iter().chain(&self.stale) {
            if self.selected.contains(&key(row)) {
                branches
                    .entry(row.repo_id)
                    .or_default()
                    .push((row.age.name.clone(), row.age.tip));
            }
        }
        let rows = self.core.rows();
        let targets: Vec<BatchTarget> = rows
            .iter()
            .filter(|r| branches.contains_key(&r.id))
            .map(|r| BatchTarget {
                id: r.id,
                path: r.path.clone(),
            })
            .collect();
        let names: HashMap<RepoId, String> = rows.into_iter().map(|r| (r.id, r.name)).collect();
        let batches = self.core.batches().clone();
        let job = self
            .core
            .scheduler()
            .runtime()
            .spawn(async move { batches.delete_branches_all(&targets, branches).await });
        self.busy = true;
        self.report.clear();
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |p, cx| {
                p.busy = false;
                match result {
                    Ok(report) => {
                        let mut total = 0;
                        p.report = report
                            .per_repo
                            .iter()
                            .map(|(id, outcome)| {
                                let name = names.get(id).cloned().unwrap_or_default();
                                match outcome {
                                    Outcome::Done(n) => {
                                        total += n;
                                        (name, format!("{n} deleted"), false)
                                    }
                                    Outcome::Failed(e) => (name, e.to_string(), true),
                                    Outcome::Skipped(r) => (name, format!("skipped: {r}"), false),
                                }
                            })
                            .collect();
                        let (_, failed, _) = report.counts();
                        cx.emit(BranchesPanelEvent::Finished(format!(
                            "Delete branches: {total} deleted, {failed} repo(s) failed"
                        )));
                        p.core.refresh_all();
                        p.busy = true;
                        p.load(cx);
                    }
                    Err(e) => p.errors = vec![e.to_string()],
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Selects or clears every row of a section.
    fn select_all(&mut self, stale: bool, on: bool) {
        let rows = if stale { &self.stale } else { &self.merged };
        for k in rows.iter().map(key) {
            if on {
                self.selected.insert(k);
            } else {
                self.selected.remove(&k);
            }
        }
        self.armed = false;
    }

    fn render_section(
        &self,
        stale: bool,
        title: String,
        empty: &'static str,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let rows = if stale { &self.stale } else { &self.merged };
        let tag = if stale { "stale" } else { "merged" };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(section(title, cx))
            .when(!rows.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            button(SharedString::from(format!("b-{tag}-all")), "All", cx).on_click(
                                cx.listener(move |p, _, _, cx| {
                                    p.select_all(stale, true);
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            button(SharedString::from(format!("b-{tag}-none")), "None", cx)
                                .on_click(cx.listener(move |p, _, _, cx| {
                                    p.select_all(stale, false);
                                    cx.notify();
                                })),
                        ),
                )
            });
        let mut out = vec![header.into_any_element()];
        if rows.is_empty() && !self.busy {
            out.push(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(empty)
                    .into_any_element(),
            );
        }
        let now = Timestamp::now();
        for (i, row) in rows.iter().enumerate() {
            let k = key(row);
            let checked = self.selected.contains(&k);
            out.push(
                div()
                    .id(SharedString::from(format!("b-{tag}-{i}")))
                    .flex()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(radius(cx))
                    .cursor_pointer()
                    .hover(|s| s.bg(color(cx, T::BgHover)))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !p.selected.remove(&k) {
                            p.selected.insert(k.clone());
                        }
                        p.armed = false;
                        cx.notify();
                    }))
                    .child(if checked { "☑" } else { "☐" })
                    .child(
                        div()
                            .text_color(color(cx, T::FgMuted))
                            .child(row.repo.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(row.age.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .child(relative_time(row.age.last_commit, now)),
                    )
                    .into_any_element(),
            );
        }
        out
    }
}

fn key(row: &BranchRow) -> Key {
    (row.repo_id, row.age.name.clone())
}

impl Render for BranchesPanel {
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scope = match self.scope.len() {
            0 => "every repository".to_owned(),
            1 => "the selected repository".to_owned(),
            n => format!("{n} selected repositories"),
        };
        let merged = self.render_section(
            false,
            format!("MERGED, NOT DELETED ({})", self.merged.len()),
            "No merged branch left behind.",
            cx,
        );
        let stale = self.render_section(
            true,
            format!(
                "STALE: NOT MERGED, NO COMMIT FOR {DEFAULT_STALE_BRANCH_DAYS} DAYS ({})",
                self.stale.len()
            ),
            "No stale branch.",
            cx,
        );
        let count = self.selected.len();
        let label = match (self.armed, count) {
            (true, 1) => "Confirm: delete 1 branch".to_owned(),
            (true, n) => format!("Confirm: delete {n} branches"),
            (false, 1) => "Delete 1 branch".to_owned(),
            (false, n) => format!("Delete {n} branches"),
        };
        let delete = if self.armed {
            primary_button("b-delete", label, cx)
        } else {
            button("b-delete", label, cx)
        }
        .when(count == 0 || self.busy, |b| b.opacity(0.5))
        .on_click(cx.listener(|p, _, _, cx| p.click_delete(cx)));
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
        let errors = self.errors.iter().map(|e| {
            div()
                .text_xs()
                .text_color(color(cx, T::StatusError))
                .child(e.clone())
        });
        div()
            .id("branches-panel")
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
                    .child("Clean up branches")
                    .child(
                        button("b-close", "Close", cx).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(BranchesPanelEvent::Closed)),
                        ),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!(
                        "Local branches of {scope}, default and checked-out branches excluded. \
                         Deleting is forced; Undo on a repo restores its branches one at a time."
                    )),
            )
            .when(self.busy, |d| {
                d.child(div().text_color(color(cx, T::FgMuted)).child("Working…"))
            })
            .children(errors)
            .child(
                div()
                    .id("b-rows")
                    .flex_1()
                    .overflow_y_scrollbar()
                    .children(merged)
                    .children(stale),
            )
            .child(div().flex().justify_end().child(delete))
            .children(report)
    }
}
