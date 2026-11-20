//! The dependency tree view of the dashboard (spec §9): repos, their linked
//! worktrees and their submodules, with every dashboard column on the
//! project's repos and worktrees, the submodule state and update actions,
//! and worktree actions on hover.

use std::collections::HashMap;

use archipel_app::Command;
use archipel_app::dashboard::{DashboardRow, RepoStatus, WORKTREE_GLYPH};
use archipel_app::deptree::TreeRow;
use archipel_model::ui_state::UiState;
use archipel_model::{RepoId, RepoPath, Timestamp};
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, SharedString, div, px};

use crate::theme::{color, row_height};
use crate::widgets::{SHORT_OID, TextTooltip, button};
use crate::worktree_dialog::WorktreeStart;

use super::Workspace;
use super::dashboard::{WIDTHS, nested_cell, nesting, row_cells};
use super::worktrees::HandOff;

impl Workspace {
    /// Opens a submodule in a tab off the UI thread, showing it as opening
    /// meanwhile.
    fn open_submodule(&mut self, path: RepoPath, cx: &mut Context<Self>) {
        if self.opening.is_some() {
            return;
        }
        self.opening = Some(path.clone());
        cx.notify();
        let core = self.core.clone();
        let task = cx.background_spawn(async move { core.dispatch(Command::OpenSubmodule(path)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.opening = None;
                if let Err(e) = result {
                    this.notice = Some(e.to_string());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The dependency tree (spec §9): project repos keep their dashboard
    /// columns, with their update buttons on hover when they are
    /// submodules; other submodules show their state and actions.
    #[allow(clippy::too_many_lines)] // declarative element tree
    pub(super) fn render_tree(
        &self,
        tree: &[TreeRow],
        rows: &[DashboardRow],
        selected: &[RepoId],
        ui: &UiState,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let ext = self.core.extensions().clone();
        let now = Timestamp::now();
        let by_id: HashMap<RepoId, &DashboardRow> = rows.iter().map(|r| (r.id, r)).collect();
        let rest: f32 = WIDTHS[1..].iter().sum();
        let opening = self.opening.as_ref();
        let all_worktrees = ui
            .filters
            .iter()
            .any(|f| f == archipel_app::dashboard::WORKTREES_FILTER);
        let workspace = cx.weak_entity();
        tree.iter()
            .enumerate()
            .map(|(i, node)| {
                let key = node.key.clone();
                let arrow = match (node.has_children, node.collapsed) {
                    (false, _) => " ",
                    (true, true) => "▸",
                    (true, false) => "▾",
                };
                let out_of_sync = node.state.as_ref().is_some_and(|s| s.out_of_sync);
                let row = node.repo.and_then(|id| by_id.get(&id).copied());
                // A project repo or worktree shows its Repo cell (path line,
                // tooltip, Worktree Count); a plain submodule its name.
                let label = match row {
                    Some(row) if node.worktree => Self::worktree_label(node, row, cx),
                    Some(row) => {
                        let cell = row_cells(&ext, &self.core, row, now, 0, cx)
                            .into_iter()
                            .next()
                            .unwrap_or_else(|| div().into_any_element());
                        let id = row.id;
                        let toggle = workspace.clone();
                        nested_cell(
                            cell,
                            nesting(row, None, ui, all_worktrees),
                            id,
                            cx,
                            move |cx| {
                                let _ = toggle.update(cx, |this, cx| {
                                    this.dispatch(Command::ToggleWorktrees(id), cx);
                                });
                            },
                        )
                    }
                    None => div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(node.name.clone())
                        .into_any_element(),
                };
                let name = div()
                    .flex_grow(WIDTHS[0])
                    .flex_basis(px(0.0))
                    .min_w_0()
                    .overflow_hidden()
                    .flex()
                    .items_start()
                    .child(
                        div()
                            .flex_none()
                            .font_family(crate::theme::mono_font(cx))
                            .text_color(color(cx, T::FgSubtle))
                            .child(node.prefix.clone()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("tree-toggle-{i}")))
                            .flex_none()
                            .w(px(16.0))
                            .text_color(color(cx, T::FgMuted))
                            .when(node.has_children, gpui_kit::Styled::cursor_pointer)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.dispatch(Command::ToggleTreeNode(key.clone()), cx);
                            }))
                            .child(arrow),
                    )
                    .child(label)
                    .when(out_of_sync, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .ml_2()
                                .text_xs()
                                .text_color(color(cx, T::StatusConflict))
                                .child("≠ pinned"),
                        )
                    });
                match row {
                    Some(row) => {
                        let cells = row_cells(&ext, &self.core, row, now, 1, cx);
                        let mut actions = Self::row_actions(i, row, cx);
                        actions.extend(Self::node_actions(i, node, true, opening, cx));
                        let group = SharedString::from(format!("tree-row-{i}"));
                        let hover = color(cx, T::BgHover);
                        Self::row_shell(row, selected, cx)
                            .when(row.context_only, |d| d.opacity(0.5))
                            .relative()
                            .group(group.clone())
                            .child(name)
                            .children(cells)
                            .when(!actions.is_empty(), |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .right_0()
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .bg(hover)
                                        .opacity(0.0)
                                        .group_hover(group, |s| s.opacity(1.0))
                                        .children(actions),
                                )
                            })
                            .into_any_element()
                    }
                    None => div()
                        .id(SharedString::from(format!("tree-{i}")))
                        .flex()
                        .items_center()
                        .px_3()
                        .py_1()
                        .min_h(row_height(cx))
                        .border_b_1()
                        .border_color(color(cx, T::BorderSubtle))
                        .child(name)
                        .child(
                            div()
                                .flex_grow(rest)
                                .flex_basis(px(0.0))
                                .flex()
                                .items_center()
                                .gap_2()
                                .overflow_hidden()
                                .child(Self::node_state(node, cx))
                                .children(Self::node_actions(i, node, false, opening, cx)),
                        )
                        .into_any_element(),
                }
            })
            .collect()
    }

    /// The name of a linked worktree node: Worktree Glyph, branch, lock,
    /// and its folder below; dimmed when its folder is gone.
    fn worktree_label(node: &TreeRow, row: &DashboardRow, cx: &mut Context<Self>) -> AnyElement {
        let info = row.worktree.clone();
        let locked = info.as_ref().and_then(|w| w.locked.clone());
        let missing = info.as_ref().is_some_and(|w| w.missing);
        let path = row.path.as_path().display().to_string();
        let token = if missing {
            T::FgMuted
        } else if matches!(row.status, RepoStatus::Missing | RepoStatus::Error(_)) {
            T::StatusConflict
        } else {
            T::FgDefault
        };
        let mut tooltip = vec![
            match &info {
                Some(w) => format!("Worktree of {} on {}", w.main_name, w.label),
                None => row.name.clone(),
            },
            path.clone(),
        ];
        match locked.as_deref() {
            Some("") => tooltip.push("Locked".into()),
            Some(reason) => tooltip.push(format!("Locked: {reason}")),
            None => {}
        }
        if missing {
            tooltip.push("Its folder is gone: prune it, or restore the folder".into());
        }
        let detail = match &info {
            Some(w) if w.parent.is_none() => format!("worktree of {} · {path}", w.main_name),
            _ if missing => format!("missing · {path}"),
            _ => path,
        };
        div()
            .id(SharedString::from(format!("tree-wt-{}", row.id)))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .text_color(color(cx, token))
                    .child(
                        div()
                            .flex_none()
                            .text_color(color(cx, T::FgSubtle))
                            .child(WORKTREE_GLYPH),
                    )
                    .child(div().truncate().child(node.name.clone()))
                    .when(locked.is_some(), |d| d.child(div().flex_none().child("🔒"))),
            )
            .child(
                div()
                    .truncate()
                    .text_xs()
                    .text_color(color(cx, T::FgSubtle))
                    .child(detail),
            )
            .tooltip(move |_, cx| TextTooltip::view(&tooltip, cx))
            .into_any_element()
    }

    /// Hover buttons of a project repo or worktree node: statistics and a
    /// new worktree on a repo; open, terminal, lock and remove (or prune) on
    /// a worktree.
    fn row_actions(i: usize, row: &DashboardRow, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let id = row.id;
        let Some(info) = row.worktree.clone() else {
            return vec![
                button(
                    SharedString::from(format!("tree-stats-{i}")),
                    "Statistics",
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_stats(vec![id], Some(id), window, cx);
                }))
                .into_any_element(),
                button(
                    SharedString::from(format!("tree-new-wt-{i}")),
                    "+ Worktree",
                    cx,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_new_worktree(id, &WorktreeStart::default(), window, cx);
                }))
                .into_any_element(),
            ];
        };
        if info.missing {
            return vec![
                button(SharedString::from(format!("tree-prune-{i}")), "Prune", cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.dispatch(Command::PruneWorktrees(vec![id]), cx);
                    }))
                    .into_any_element(),
            ];
        }
        let mut out = vec![
            button(SharedString::from(format!("tree-open-wt-{i}")), "Open", cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.dispatch(Command::OpenRepoTab(id), cx);
                }))
                .into_any_element(),
            button(
                SharedString::from(format!("tree-term-wt-{i}")),
                "Terminal",
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.hand_off(id, HandOff::Terminal, cx);
            }))
            .into_any_element(),
        ];
        let lock = if info.locked.is_some() {
            "Unlock"
        } else {
            "Lock…"
        };
        out.push(
            button(SharedString::from(format!("tree-lock-wt-{i}")), lock, cx)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.lock_or_unlock(id, window, cx);
                }))
                .into_any_element(),
        );
        if info.parent.is_some() && info.locked.is_none() {
            out.push(
                button(SharedString::from(format!("tree-rm-wt-{i}")), "Remove…", cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_remove_worktree(id, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        out
    }

    fn node_state(node: &TreeRow, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = &node.state else {
            return div().into_any_element();
        };
        let short =
            |o: Option<archipel_model::Oid>| o.map_or_else(|| "?".into(), |o| o.short(SHORT_OID));
        let (text, token) = if !state.initialized {
            (
                format!("not initialized · pins {}", short(state.pinned)),
                T::FgMuted,
            )
        } else if state.out_of_sync {
            (
                format!(
                    "checked out {} ≠ pinned {}",
                    short(state.checked_out),
                    short(state.pinned)
                ),
                T::StatusConflict,
            )
        } else {
            (format!("at {}", short(state.checked_out)), T::FgMuted)
        };
        div()
            .flex()
            .gap_2()
            .text_xs()
            .child(div().truncate().text_color(color(cx, token)).child(text))
            .when(state.dirty, |d| {
                d.child(div().text_color(color(cx, T::StatusDirty)).child("dirty"))
            })
            .into_any_element()
    }

    /// A submodule node's buttons; a project repo (`in_project`) has its
    /// row's own open and menu, so it only gets the update buttons.
    fn node_actions(
        i: usize,
        node: &TreeRow,
        in_project: bool,
        opening: Option<&RepoPath>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(state) = node.state.clone() else {
            return Vec::new();
        };
        let update = |label: &'static str, init: bool, recursive: bool, cx: &mut Context<Self>| {
            let state = state.clone();
            button(SharedString::from(format!("tree-{label}-{i}")), label, cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.dispatch(
                        Command::UpdateSubmodules {
                            parent: state.parent.clone(),
                            paths: vec![state.rel_path.clone()],
                            init,
                            recursive,
                        },
                        cx,
                    );
                }))
                .into_any_element()
        };
        let mut out = Vec::new();
        if state.initialized {
            out.push(update("Update", false, false, cx));
            out.push(update("Update all", false, true, cx));
            if in_project {
                return out;
            }
            let path = node.path.clone();
            out.push(if opening == Some(&path) {
                div()
                    .text_color(color(cx, T::FgMuted))
                    .child("Opening…")
                    .into_any_element()
            } else {
                button(SharedString::from(format!("tree-open-{i}")), "Open", cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_submodule(path.clone(), cx);
                    }))
                    .into_any_element()
            });
            let path = node.path.clone();
            out.push(
                button(
                    SharedString::from(format!("tree-add-{i}")),
                    "Add to project",
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dispatch(Command::AddRepos(vec![path.clone()]), cx);
                }))
                .into_any_element(),
            );
        } else {
            out.push(update("Init", true, false, cx));
            out.push(update("Init all", true, true, cx));
        }
        out
    }
}
