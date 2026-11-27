//! The refs panel: branches, tags and submodules, virtualized, and the
//! drag and drop of branches onto each other.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use archipel_app::forge::{BranchCiMark, branch_ci_mark};
use archipel_model::refs::{RefKind, RefTip};
use archipel_model::requests::LocalCounterpart;
use archipel_model::submodule::SubmoduleInfo;
use archipel_model::ui_state::UiState;
use archipel_model::{Oid, RepoId};
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, MouseButton, MouseDownEvent, SharedString, Window, div, px,
    uniform_list,
};

use crate::theme::{color, radius, repo_surface, row_height, tone};
use crate::widgets::{SHORT_OID, TextTooltip, scroll_list};

use super::RepoView;
use super::menu::{MenuAction, MenuTarget};
use crate::staging::StagingEvent;

/// One fixed-height row of the refs panel.
pub(super) enum RefRow {
    /// Section title and entry count.
    Header(&'static str, usize),
    /// A branch or tag; `current` when `HEAD` is on it.
    Tip { tip: RefTip, current: bool },
    /// A worktree of the repo (spec §10).
    Worktree(super::worktrees::WorktreeEntry),
}

/// Branches and tags of the refs panel, flattened into rows (a repo may
/// have thousands of tags, so the panel is virtualized too).
#[derive(Default)]
pub(super) struct RefList {
    pub(super) rows: Vec<RefRow>,
    /// Local branches and their upstreams, to check out a remote branch
    /// through its local counterpart when it exists.
    pub(super) locals: HashMap<String, Option<String>>,
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
    /// The local branch named `branch`, if any.
    pub(super) fn local(&self, branch: &str) -> Option<LocalCounterpart> {
        self.refs
            .locals
            .get(branch)
            .map(|upstream| LocalCounterpart {
                upstream: upstream.clone(),
            })
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

    /// The refs pane at its width with its collapse button, or the thin
    /// strip expanding it back when collapsed.
    pub(super) fn render_refs_pane(&mut self, ui: &UiState, cx: &mut Context<Self>) -> AnyElement {
        if ui.refs_collapsed {
            return render_refs_strip(cx);
        }
        div()
            .w(px(ui.refs_width))
            .flex_none()
            .bg(repo_surface(cx))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(render_refs_hide(cx))
            .child(
                // Held to the pane's height, as the graph's list is, so the
                // list's viewport is the visible part and not its rows.
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    .child(self.render_refs(cx)),
            )
            .into_any_element()
    }

    /// Branches and tags: click jumps to the commit, double click checks
    /// out. Then the submodules (the header always, with its add button):
    /// double click opens one. A click on a section header collapses or
    /// expands it.
    fn render_refs(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
        let base = refs.rows.len();
        shown.push(base);
        if !is_collapsed("SUBMODULES") {
            shown.extend(base + 1..=base + subs.len());
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
                                if let RefRow::Worktree(entry) = row {
                                    return Some(super::worktrees::render_worktree_row(
                                        i, entry, cx,
                                    ));
                                }
                                let elsewhere = match row {
                                    RefRow::Tip { tip, .. } if tip.kind == RefKind::LocalBranch => {
                                        this.worktree_branches
                                            .get(&tip.name)
                                            .map(|(_, p)| p.clone())
                                    }
                                    _ => None,
                                };
                                let repo = this.path.id();
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
                                Some(render_ref_row(i, row, folded, ci, elsewhere, repo, cx))
                            }
                            Some(0) => Some(render_ref_row(
                                i,
                                &RefRow::Header("SUBMODULES", subs.len()),
                                is_collapsed("SUBMODULES"),
                                None,
                                None,
                                this.path.id(),
                                cx,
                            )),
                            Some(s) => subs.get(s - 1).map(|sub| render_submodule_row(i, sub, cx)),
                        }
                    })
                    .collect()
            }),
        );
        scroll_list("refs-scroll", list)
            .handle(&self.refs_scroll)
            .into_any_element()
    }

    /// Collapses the whole refs pane to a thin strip, or expands it back.
    fn toggle_refs_pane(&mut self, cx: &mut Context<Self>) {
        self.core
            .update_ui_state(|ui| ui.refs_collapsed = !ui.refs_collapsed);
        cx.notify();
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

/// The top row of the expanded refs pane: a button collapsing it.
fn render_refs_hide(cx: &mut Context<RepoView>) -> AnyElement {
    let hover = color(cx, T::BgHover);
    div()
        .h(row_height(cx))
        .flex_none()
        .flex()
        .justify_end()
        .items_center()
        .px_1()
        .child(
            div()
                .id("refs-hide")
                .px_1p5()
                .rounded(radius(cx))
                .border_1()
                .border_color(color(cx, T::Border))
                .bg(color(cx, T::BgElevated))
                .cursor_pointer()
                .text_sm()
                .line_height(px(14.0))
                .text_color(color(cx, T::FgDefault))
                .hover(move |s| s.bg(hover))
                .child("◂")
                .tooltip(|_, cx| TextTooltip::view(&["Collapse the refs pane".into()], cx))
                .on_click(cx.listener(|v, _, _, cx| v.toggle_refs_pane(cx))),
        )
        .into_any_element()
}

/// The collapsed refs pane: a thin strip, a click expands it.
fn render_refs_strip(cx: &mut Context<RepoView>) -> AnyElement {
    let hover = color(cx, T::BgHover);
    div()
        .id("refs-strip")
        .w(px(22.0))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .bg(repo_surface(cx))
        .border_r_1()
        .border_color(color(cx, T::BorderSubtle))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .text_sm()
        .text_color(color(cx, T::FgDefault))
        .child(div().h(row_height(cx)).flex().items_center().child("▸"))
        .tooltip(|_, cx| TextTooltip::view(&["Show branches, tags and submodules".into()], cx))
        .on_click(cx.listener(|v, _, _, cx| v.toggle_refs_pane(cx)))
        .into_any_element()
}

/// One row of the refs panel; `collapsed` for a header whose section is
/// collapsed.
#[allow(clippy::too_many_arguments)] // one row, everything it shows
fn render_ref_row(
    i: usize,
    row: &RefRow,
    collapsed: bool,
    ci: Option<BranchCiMark>,
    elsewhere: Option<std::path::PathBuf>,
    repo: archipel_model::RepoId,
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
                .when(title == "REMOTE", |d| {
                    d.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                            v.open_menu(MenuTarget::Remotes, e.position, cx);
                        }),
                    )
                })
                .child(
                    div()
                        .w(px(12.0))
                        .flex_none()
                        .child(if collapsed { "▸" } else { "▾" }),
                )
                .child(format!("{title} ({count})"))
                .when(title == "WORKTREES", |d| {
                    d.child(super::worktrees::new_worktree_button(repo, cx))
                })
                .when(title == "SUBMODULES", |d| {
                    d.child(add_submodule_button(repo, cx))
                })
                .into_any_element()
        }
        RefRow::Worktree(entry) => super::worktrees::render_worktree_row(i, entry, cx),
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
                .when_some(elsewhere, |d, path| {
                    // Checked out in another worktree (spec §10).
                    let where_ = format!("Checked out in the worktree at {}", path.display());
                    d.child(
                        div()
                            .id(SharedString::from(format!("ref-wt-{i}")))
                            .flex_none()
                            .ml_1()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .tooltip(move |_, cx| {
                                TextTooltip::view(std::slice::from_ref(&where_), cx)
                            })
                            .child(archipel_app::dashboard::WORKTREE_GLYPH),
                    )
                })
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

/// The `+` of the SUBMODULES header: asks for a submodule to add.
fn add_submodule_button(repo: RepoId, cx: &mut Context<RepoView>) -> AnyElement {
    let hover = color(cx, T::BgHover);
    div()
        .id("submodules-add")
        .ml_auto()
        .px_1()
        .rounded(radius(cx))
        .text_color(color(cx, T::FgMuted))
        .hover(move |s| s.bg(hover))
        .cursor_pointer()
        .tooltip(|_, cx| TextTooltip::view(&["Add submodule…".into()], cx))
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            cx.emit(StagingEvent::AddSubmodule(repo));
        }))
        .child("+")
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
