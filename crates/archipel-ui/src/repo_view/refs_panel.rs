//! The refs panel: branches, tags and submodules, virtualized, and the
//! drag and drop of branches onto each other.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use archipel_app::forge::{BranchCiMark, branch_ci_mark};
use archipel_model::Oid;
use archipel_model::refs::{RefKind, RefTip};
use archipel_model::submodule::SubmoduleInfo;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, MouseButton, MouseDownEvent, SharedString, Window, div, px,
    uniform_list,
};

use crate::theme::{color, radius, row_height, tone};
use crate::widgets::{SHORT_OID, TextTooltip, scroll_list};

use super::RepoView;
use super::menu::{MenuAction, MenuTarget};

/// One fixed-height row of the refs panel.
pub(super) enum RefRow {
    /// Section title and entry count.
    Header(&'static str, usize),
    /// A branch or tag; `current` when `HEAD` is on it.
    Tip { tip: RefTip, current: bool },
}

/// Branches and tags of the refs panel, flattened into rows (a repo may
/// have thousands of tags, so the panel is virtualized too).
#[derive(Default)]
pub(super) struct RefList {
    pub(super) rows: Vec<RefRow>,
    /// Local branch names, to check out a remote branch through its local
    /// counterpart when it exists.
    pub(super) locals: HashSet<String>,
}

/// Drag payload of a branch badge or a refs panel branch row.
struct BranchDrag(RefTip);

/// Whether `tip` can be dragged, and dropped on (local or remote branch).
pub(super) fn is_branch(tip: &RefTip) -> bool {
    matches!(tip.kind, RefKind::LocalBranch | RefKind::RemoteBranch)
}

/// The badge following the pointer while a branch is dragged.
struct BranchGhost(SharedString);

impl Render for BranchGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_1()
            .rounded(radius(cx))
            .text_xs()
            .whitespace_nowrap()
            .bg(color(cx, T::Accent))
            .text_color(color(cx, T::AccentFg))
            .shadow_md()
            .child(self.0.clone())
    }
}

impl RepoView {
    pub(super) fn has_local(&self, branch: &str) -> bool {
        self.refs.locals.contains(branch)
    }

    /// The checked-out branch, from the refs panel.
    pub(super) fn current_branch(&self) -> Option<&str> {
        self.refs.rows.iter().find_map(|r| match r {
            RefRow::Tip { tip, current: true } if tip.kind == RefKind::LocalBranch => {
                Some(tip.name.as_str())
            }
            _ => None,
        })
    }

    /// Branches and tags: click jumps to the commit, double click checks
    /// out. Then the submodules, when any: double click opens one. A click
    /// on a section header collapses or expands it.
    pub(super) fn render_refs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let refs = Arc::clone(&self.refs);
        let subs = Arc::clone(&self.submodules);
        let collapsed = self
            .core
            .ui_state()
            .unwrap_or_default()
            .collapsed_ref_sections;
        let is_collapsed = move |title: &str| collapsed.iter().any(|c| c == title);
        // Indices of the shown rows: the refs rows, then the submodules
        // header and the submodules.
        let mut shown = Vec::with_capacity(refs.rows.len());
        let mut open = true;
        for (i, row) in refs.rows.iter().enumerate() {
            if let RefRow::Header(title, _) = row {
                open = !is_collapsed(title);
                shown.push(i);
            } else if open {
                shown.push(i);
            }
        }
        if !subs.is_empty() {
            let base = refs.rows.len();
            shown.push(base);
            if !is_collapsed("SUBMODULES") {
                shown.extend(base + 1..=base + subs.len());
            }
        }
        let list = uniform_list(
            "refs",
            shown.len(),
            cx.processor(move |this: &mut Self, range: Range<usize>, _window, cx| {
                range
                    .filter_map(|r| {
                        let i = shown[r];
                        match i.checked_sub(refs.rows.len()) {
                            None => {
                                let row = &refs.rows[i];
                                let folded = matches!(row, RefRow::Header(t, _) if is_collapsed(t));
                                let ci = match row {
                                    RefRow::Tip { tip, current } if is_branch(tip) => {
                                        branch_ci_mark(
                                            &tip.name,
                                            tip.kind == RefKind::RemoteBranch,
                                            *current,
                                            &this.ci,
                                            &this.pull_requests,
                                        )
                                    }
                                    _ => None,
                                };
                                Some(render_ref_row(i, row, folded, ci, cx))
                            }
                            Some(0) => Some(render_ref_row(
                                i,
                                &RefRow::Header("SUBMODULES", subs.len()),
                                is_collapsed("SUBMODULES"),
                                None,
                                cx,
                            )),
                            Some(s) => subs.get(s - 1).map(|sub| render_submodule_row(i, sub, cx)),
                        }
                    })
                    .collect()
            }),
        );
        scroll_list("refs-scroll", list).into_any_element()
    }

    /// Collapses or expands the refs pane section `title`.
    fn toggle_ref_section(&mut self, title: &'static str, cx: &mut Context<Self>) {
        self.core.update_ui_state(|ui| {
            let sections = &mut ui.collapsed_ref_sections;
            if let Some(i) = sections.iter().position(|c| c == title) {
                sections.remove(i);
            } else {
                sections.push(title.to_owned());
            }
        });
        cx.notify();
    }
}

/// Makes `element`, showing branch `tip`, draggable onto other branches
/// and a drop target for them: dropping opens the merge / rebase menu.
pub(super) fn branch_drag_and_drop<E: StatefulInteractiveElement>(
    element: E,
    tip: &RefTip,
    cx: &Context<RepoView>,
) -> E {
    let target = tip.clone();
    let name = SharedString::from(tip.name.clone());
    element
        .on_drag(BranchDrag(tip.clone()), move |_, _, _, cx| {
            cx.new(|_| BranchGhost(name.clone()))
        })
        .drag_over::<BranchDrag>(|s, _, _, cx| {
            s.bg(color(cx, T::Accent))
                .text_color(color(cx, T::AccentFg))
        })
        .on_drop(cx.listener(move |v, d: &BranchDrag, window, cx| {
            cx.stop_propagation();
            if d.0 != target {
                let position = window.mouse_position();
                v.open_menu(
                    MenuTarget::Drop {
                        source: d.0.clone(),
                        target: target.clone(),
                    },
                    position,
                    cx,
                );
            }
        }))
}

/// One row of the refs panel; `collapsed` for a header whose section is
/// collapsed.
fn render_ref_row(
    i: usize,
    row: &RefRow,
    collapsed: bool,
    ci: Option<BranchCiMark>,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let base = div()
        .h(row_height(cx))
        .flex()
        .items_center()
        .px_2()
        .whitespace_nowrap()
        .overflow_hidden();
    match row {
        RefRow::Header(title, count) => {
            let hover = color(cx, T::BgHover);
            let title = *title;
            base.id(SharedString::from(format!("ref-{i}")))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .on_click(cx.listener(move |v, _, _, cx| v.toggle_ref_section(title, cx)))
                .child(
                    div()
                        .w(px(12.0))
                        .flex_none()
                        .child(if collapsed { "▸" } else { "▾" }),
                )
                .child(format!("{title} ({count})"))
                .into_any_element()
        }
        RefRow::Tip { tip, current } => {
            let hover = color(cx, T::BgHover);
            let name = tip.name.clone();
            let tip = tip.clone();
            let menu_tip = tip.clone();
            let drag_tip = tip.clone();
            base.id(SharedString::from(format!("ref-{i}")))
                .pl_4()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .when(*current, |d| {
                    d.text_color(color(cx, T::AccentText))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                })
                .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                    if e.click_count() > 1 {
                        v.checkout_ref(&tip, cx);
                    } else {
                        v.focus.focus(window, cx);
                        v.jump_to(tip.target, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                        v.open_menu(MenuTarget::Ref(menu_tip.clone()), e.position, cx);
                    }),
                )
                .when(is_branch(&drag_tip), |d| {
                    branch_drag_and_drop(d, &drag_tip, cx)
                })
                .child(div().min_w_0().truncate().child(if *current {
                    format!("● {name}")
                } else {
                    name
                }))
                .when_some(ci, |d, ci| d.child(render_ci_mark(i, ci, cx)))
                .into_any_element()
        }
    }
}

/// The CI mark after a branch name: its run or pull request on hover,
/// opened on click.
fn render_ci_mark(i: usize, ci: BranchCiMark, cx: &mut Context<RepoView>) -> AnyElement {
    let lines = ci.tooltip;
    div()
        .id(SharedString::from(format!("ref-ci-{i}")))
        .flex_none()
        .ml_1()
        .text_xs()
        .text_color(tone(cx, ci.tone))
        .child(ci.mark)
        .tooltip(move |_, cx| TextTooltip::view(&lines, cx))
        .when_some(ci.url, |d, url| {
            d.cursor_pointer().on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.open_url(&url);
            })
        })
        .into_any_element()
}

/// One submodule of the refs panel: dimmed when not initialized, in the
/// warning color when checked out at another commit than the pinned one.
fn render_submodule_row(i: usize, sub: &SubmoduleInfo, cx: &mut Context<RepoView>) -> AnyElement {
    let hover = color(cx, T::BgHover);
    let (fg, state) = if !sub.initialized {
        (T::FgSubtle, "Not initialized")
    } else if sub.is_out_of_sync() {
        (
            T::StatusWarning,
            "Checked out at another commit than the pinned one",
        )
    } else {
        (T::FgDefault, "At the pinned commit")
    };
    let short = |oid: Option<Oid>| oid.map_or_else(|| "—".to_owned(), |o| o.short(SHORT_OID));
    let tooltip = vec![
        sub.name.clone(),
        state.to_owned(),
        format!("Pinned: {}", short(sub.pinned)),
        format!("Checked out: {}", short(sub.checked_out)),
    ];
    let path = sub.path.clone();
    let initialized = sub.initialized;
    let menu_sub = sub.clone();
    div()
        .id(SharedString::from(format!("ref-{i}")))
        .h(row_height(cx))
        .flex()
        .items_center()
        .px_2()
        .pl_4()
        .whitespace_nowrap()
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .text_color(color(cx, fg))
        .tooltip(move |_, cx| TextTooltip::view(&tooltip, cx))
        .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
            if e.click_count() > 1 && initialized {
                v.choose(MenuAction::OpenSubmodule(path.clone()), window, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                v.open_menu(MenuTarget::Submodule(menu_sub.clone()), e.position, cx);
            }),
        )
        .child(div().truncate().child(sub.name.clone()))
        .into_any_element()
}
