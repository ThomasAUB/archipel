//! The dependency tree view of the dashboard (spec §9): repos and their
//! submodules, with the submodule state and update actions.

use std::collections::HashMap;

use archipel_app::Command;
use archipel_app::dashboard::DashboardRow;
use archipel_app::deptree::TreeRow;
use archipel_model::{RepoId, RepoPath, Timestamp};
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, SharedString, div, px};

use crate::theme::{color, row_height};
use crate::widgets::{SHORT_OID, button};

use super::Workspace;
use super::dashboard::{WIDTHS, row_cells};

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
    pub(super) fn render_tree(
        &self,
        tree: &[TreeRow],
        rows: &[DashboardRow],
        selected: &[RepoId],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let ext = self.core.extensions().clone();
        let now = Timestamp::now();
        let by_id: HashMap<RepoId, &DashboardRow> = rows.iter().map(|r| (r.id, r)).collect();
        let rest: f32 = WIDTHS[1..].iter().sum();
        let opening = self.opening.as_ref();
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
                let name = div()
                    .flex_grow(WIDTHS[0])
                    .flex_basis(px(0.0))
                    .overflow_hidden()
                    .flex()
                    .items_center()
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
                    .child(div().truncate().child(node.name.clone()))
                    .when(out_of_sync, |d| {
                        d.child(
                            div()
                                .ml_2()
                                .text_xs()
                                .text_color(color(cx, T::StatusConflict))
                                .child("≠ pinned"),
                        )
                    });
                match node.repo.and_then(|id| by_id.get(&id)) {
                    Some(row) => {
                        let cells = row_cells(&ext, row, now, 1, cx);
                        let actions = Self::node_actions(i, node, true, opening, cx);
                        let group = SharedString::from(format!("tree-row-{i}"));
                        let hover = color(cx, T::BgHover);
                        Self::row_shell(row, selected, cx)
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
