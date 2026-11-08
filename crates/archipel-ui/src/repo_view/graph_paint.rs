//! The commit graph list: the uncommitted-changes row, commit rows and
//! stash rows, and the lanes and nodes painted in their graph column.

use std::ops::Range;

use archipel_graph::{Edge, EdgeKind, GraphRow};
use archipel_model::Timestamp;
use archipel_model::refs::RefKind;
use archipel_model::requests::StashApplyMode;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, Hsla, MouseButton, MouseDownEvent, PathBuilder,
    Pixels, Point, SharedString, Window, canvas, div, point, px, uniform_list,
};

use crate::theme::{color, lane, mono_font, radius, row_height};
use crate::widgets::{SHORT_OID, scroll_list};

use super::details::stash_title;
use super::menu::MenuTarget;
use super::refs_panel::{branch_drag_and_drop, is_branch};
use super::{Item, RepoView, lock};

/// Width of one graph lane.
const LANE_WIDTH: f32 = 14.0;
/// Radius of a node in the graph column.
const NODE_RADIUS: f32 = 4.0;
/// Radius of the checked-out commit's node, drawn larger to stand out.
const HEAD_NODE_RADIUS: f32 = 6.0;
/// Width of the author column of the commit rows.
const AUTHOR_WIDTH: f32 = 140.0;
/// Width of the date column of the commit and stash rows.
const DATE_WIDTH: f32 = 80.0;
/// Width of the commit id column of the commit and stash rows.
const OID_WIDTH: f32 = 70.0;

impl RepoView {
    pub(super) fn render_graph(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(e) = &self.graph_error {
            return div()
                .p_3()
                .text_color(color(cx, T::StatusError))
                .child(e.clone())
                .into_any_element();
        }
        if self.graph.is_none() {
            return div()
                .p_3()
                .text_color(color(cx, T::FgMuted))
                .child("Loading history…")
                .into_any_element();
        }
        let count = usize::try_from(self.item_count()).unwrap_or(0);
        let list = uniform_list(
            "graph",
            count,
            cx.processor(|this: &mut Self, range: Range<usize>, _window, cx| {
                let items: Vec<Item> = (u32::try_from(range.start).unwrap_or(0)
                    ..u32::try_from(range.end).unwrap_or(0))
                    .map(|i| this.item_at(i))
                    .collect();
                // Graph rows to lay out: the commits shown, and the base
                // commits of the stashes shown (their lanes run through the
                // stash rows).
                let wanted = items.iter().filter_map(|item| match *item {
                    Item::Wip => None,
                    Item::Stash(k) => Some(this.placed()[k].row),
                    Item::Commit(r) => Some(r),
                });
                let start = wanted.clone().min().unwrap_or(0);
                let end = wanted.max().map_or(start, |r| r + 1);
                let rows = this
                    .graph
                    .as_ref()
                    .and_then(|g| lock(g).rows(start..end).ok())
                    .unwrap_or_default();
                if (start..end).any(|r| !this.meta.contains_key(&r)) {
                    let overscan =
                        u32::try_from(this.core.settings().graph_overscan).unwrap_or(u32::MAX / 4);
                    this.fetch_meta(start.saturating_sub(overscan)..end + overscan, cx);
                }
                let row_at = |r: u32| rows.get(usize::try_from(r - start).ok()?);
                let wip_lane = this.wip_lane();
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Item::Wip => {
                            // Linked down to HEAD once it is found; until
                            // then, above HEAD's lane when HEAD is on screen.
                            let column = wip_lane.unwrap_or_else(|| {
                                rows.iter()
                                    .find(|r| r.refs.iter().any(|t| t.kind == RefKind::Head))
                                    .map_or(0, |r| r.column)
                            });
                            out.push(this.render_wip_row(column, wip_lane.is_some(), cx));
                        }
                        Item::Stash(k) => {
                            if let Some(base) = row_at(this.placed()[k].row) {
                                out.push(this.render_stash_row(k, base, cx));
                            }
                        }
                        Item::Commit(r) => {
                            if let Some(row) = row_at(r) {
                                let lane =
                                    this.placed().iter().find(|s| s.row == r).map(|s| s.lane);
                                let wip = this.wip_lane_at(r);
                                out.push(this.render_row(row, lane, wip, cx));
                            }
                        }
                    }
                }
                out
            }),
        );
        scroll_list("graph-scroll", list)
            .handle(&self.scroll)
            .into_any_element()
    }

    /// The uncommitted-changes row: a hollow node on lane `column`, with the
    /// lane going down to the HEAD commit when `linked`, and the staged /
    /// unstaged counts. Click shows the staging lists, double click also
    /// opens the first file's diff.
    fn render_wip_row(&mut self, column: u32, linked: bool, cx: &mut Context<Self>) -> AnyElement {
        let staged = self.changes.iter().filter(|c| c.is_staged()).count();
        let unstaged = self.changes.iter().filter(|c| c.is_unstaged()).count();
        let counts = match (staged, unstaged) {
            (0, u) => format!("{u} unstaged"),
            (s, 0) => format!("{s} staged"),
            (s, u) => format!("{s} staged, {u} unstaged"),
        };
        let ring = color(cx, T::StatusDirty);
        let node = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, _| {
                let scale = window.scale_factor();
                let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
                let r = px(NODE_RADIUS);
                let center = point(
                    snap(bounds.origin.x + px(lane_x(column))),
                    snap(bounds.origin.y + bounds.size.height / 2.0),
                );
                if linked {
                    let bottom = bounds.origin.y + bounds.size.height;
                    stroke(
                        window,
                        point(center.x, center.y + r),
                        point(center.x, bottom),
                        ring,
                        true,
                    );
                }
                window.paint_quad(
                    gpui_kit::fill(
                        Bounds::new(
                            point(center.x - r, center.y - r),
                            gpui_kit::size(r * 2.0, r * 2.0),
                        ),
                        gpui_kit::transparent_black(),
                    )
                    .corner_radii(r)
                    .border_widths(px(1.5))
                    .border_color(ring),
                );
            },
        )
        .size_full();
        let hover = color(cx, T::BgHover);
        div()
            .id("g-wip")
            .w_full()
            .h(row_height(cx))
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .cursor_pointer()
            .when(self.wip_selected, |d| d.bg(color(cx, T::BgSelected)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                v.focus.focus(window, cx);
                v.select_wip(cx);
                if e.click_count() > 1 {
                    v.open_first_change(cx);
                }
            }))
            .child(
                div()
                    .w(px(lane_x(self.lane_count())))
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(node),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .italic()
                    .text_color(color(cx, T::FgMuted))
                    .child("Uncommitted changes"),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, T::StatusDirty))
                    .child(counts),
            )
            .into_any_element()
    }

    /// A commit row. `stash_lane`: the lane of the stashes drawn above it,
    /// which joins its node; `wip_lane`: the uncommitted lane, passing by or
    /// joining the HEAD commit.
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render_row(
        &mut self,
        row: &GraphRow,
        stash_lane: Option<u32>,
        wip_lane: Option<u32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let meta = self.meta.get(&row.row).cloned();
        let selected = self.selected.is_some_and(|(r, _)| r == row.row);
        // Same width on every row so the text columns line up.
        let width = px(lane_x(self.lane_count()));
        let lanes = graph_cell(row, stash_lane, wip_lane, cx);
        let (summary, author, date) = meta.map_or_else(
            || (String::new(), String::new(), String::new()),
            |m| {
                (
                    m.summary.clone(),
                    m.author_name.clone(),
                    archipel_app::dashboard::relative_time(m.commit_time, Timestamp::now()),
                )
            },
        );
        let oid = row.oid;
        let index = row.row;
        let refs = row.refs.clone();
        let is_head = row.refs.iter().any(|r| r.kind == RefKind::Head);
        // On the checked-out commit, the current branch stands for HEAD;
        // a detached HEAD keeps its own badge.
        let current = self.current_branch().filter(|name| {
            row.refs
                .iter()
                .any(|r| r.kind == RefKind::LocalBranch && r.name == *name)
        });
        let badges = row.refs.iter().enumerate().filter_map(|(i, r)| {
            if r.kind == RefKind::Head && current.is_some() {
                return None;
            }
            let tip = r.clone();
            let checked_out = is_head
                && match r.kind {
                    RefKind::Head => true,
                    RefKind::LocalBranch => current == Some(r.name.as_str()),
                    _ => false,
                };
            Some(
                div()
                    .id(SharedString::from(format!("badge-{index}-{i}")))
                    .px_1()
                    .rounded(radius(cx))
                    .text_xs()
                    .map(|d| {
                        if checked_out {
                            d.bg(color(cx, T::Accent))
                                .text_color(color(cx, T::AccentFg))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        } else {
                            d.bg(color(cx, T::AccentSubtle))
                                .text_color(color(cx, T::AccentText))
                        }
                    })
                    // A single click bubbles to the row (select); a double click
                    // checks out this ref rather than the row's preferred one.
                    .on_click({
                        let tip = tip.clone();
                        cx.listener(move |v, e: &ClickEvent, _, cx| {
                            if e.click_count() > 1 {
                                cx.stop_propagation();
                                v.checkout_ref(&tip, cx);
                            }
                        })
                    })
                    // HEAD and stash badges leave the right click to the row.
                    .when(
                        matches!(
                            r.kind,
                            RefKind::LocalBranch | RefKind::RemoteBranch | RefKind::Tag
                        ),
                        |d| {
                            let menu_tip = tip.clone();
                            d.on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    v.open_menu(MenuTarget::Ref(menu_tip.clone()), e.position, cx);
                                }),
                            )
                        },
                    )
                    // Drag onto another branch to merge or rebase.
                    .when(is_branch(r), |d| branch_drag_and_drop(d, &tip, cx))
                    .child(r.name.clone()),
            )
        });
        let hover = color(cx, T::BgHover);
        div()
            .id(SharedString::from(format!("g-{index}")))
            .relative()
            .w_full()
            .h(row_height(cx))
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .cursor_pointer()
            // The checked-out commit: tinted, with an accent bar on its edge.
            .when(is_head, |d| {
                d.bg(color(cx, T::AccentSubtle).opacity(0.5)).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .h_full()
                        .w(px(3.0))
                        .bg(color(cx, T::Accent)),
                )
            })
            .when(selected, |d| d.bg(color(cx, T::BgSelected)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                v.focus.focus(window, cx);
                if e.click_count() > 1 {
                    v.checkout_row(index, cx);
                } else {
                    v.select_commit(index, oid, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |v, e: &MouseDownEvent, window, cx| {
                    v.focus.focus(window, cx);
                    if v.selected.is_none_or(|(r, _)| r != index) {
                        v.select_commit(index, oid, cx);
                    }
                    v.open_menu(MenuTarget::Commit(oid, refs.clone()), e.position, cx);
                }),
            )
            .child(
                div()
                    .w(width)
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(lanes),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .children(badges),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .when(is_head, |d| d.font_weight(gpui_kit::FontWeight::SEMIBOLD))
                    .child(summary),
            )
            .child(
                div()
                    .w(px(AUTHOR_WIDTH))
                    .flex_none()
                    .truncate()
                    .text_color(color(cx, T::FgMuted))
                    .child(author),
            )
            .child(
                div()
                    .w(px(DATE_WIDTH))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(color(cx, T::FgSubtle))
                    .child(date),
            )
            .child(
                div()
                    .w(px(OID_WIDTH))
                    .text_xs()
                    .font_family(mono_font(cx))
                    .text_color(color(cx, T::FgSubtle))
                    .child(oid.short(SHORT_OID)),
            )
            .into_any_element()
    }

    /// Stash `k`, in a row of its own above `base`, the commit it was made
    /// on: a hollow square on the stash lane, linked to the stashes of the
    /// same commit and down to it. Click shows its diff, double click
    /// applies it.
    fn render_stash_row(
        &mut self,
        k: usize,
        base: &GraphRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stash = &self.placed()[k];
        let oid = stash.entry.oid;
        // The first stash of its commit starts the stash lane.
        let linked = k > 0 && self.placed()[k - 1].row == stash.row;
        let cell = stash_cell(base, stash.lane, linked, self.wip_lane_at(stash.row), cx);
        let name = format!("stash@{{{}}}", stash.entry.index);
        let title = stash_title(&stash.entry.message).to_owned();
        let date = archipel_app::dashboard::relative_time(stash.entry.time, Timestamp::now());
        let selected = self.stash_selected == Some(oid);
        let tint = color(cx, T::StatusDirty);
        let hover = color(cx, T::BgHover);
        div()
            .id(SharedString::from(format!("g-stash-{oid}")))
            .w_full()
            .h(row_height(cx))
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .cursor_pointer()
            .when(selected, |d| d.bg(color(cx, T::BgSelected)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                v.focus.focus(window, cx);
                if e.click_count() > 1 {
                    v.stash_op(oid, Some(StashApplyMode::Keep), cx);
                } else if let Some(k) = v.stash_at(oid) {
                    v.select_stash(k, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |v, e: &MouseDownEvent, window, cx| {
                    v.focus.focus(window, cx);
                    if v.stash_selected != Some(oid)
                        && let Some(k) = v.stash_at(oid)
                    {
                        v.select_stash(k, cx);
                    }
                    v.open_menu(MenuTarget::Stash(oid), e.position, cx);
                }),
            )
            .child(
                div()
                    .w(px(lane_x(self.lane_count())))
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(cell),
            )
            .child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded(radius(cx))
                    .text_xs()
                    .border_1()
                    .border_color(tint)
                    .text_color(tint)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .italic()
                    .text_color(color(cx, T::FgMuted))
                    .child(title),
            )
            .child(
                div()
                    .w(px(DATE_WIDTH))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(color(cx, T::FgSubtle))
                    .child(date),
            )
            .child(
                div()
                    .w(px(OID_WIDTH))
                    .text_xs()
                    .font_family(mono_font(cx))
                    .text_color(color(cx, T::FgSubtle))
                    .child(oid.short(SHORT_OID)),
            )
            .into_any_element()
    }
}

/// Horizontal center of lane `col` (lanes are a few dozen at most).
#[allow(clippy::cast_precision_loss)]
fn lane_x(col: u32) -> f32 {
    LANE_WIDTH * col as f32 + LANE_WIDTH / 2.0
}

/// Lanes crossing the top edge of `row`; they cross the stash rows drawn
/// above it too.
pub(super) fn lanes_above(row: &GraphRow) -> impl Iterator<Item = &Edge> {
    row.edges.iter().filter(|e| e.kind != EdgeKind::Outgoing)
}

/// Lane of the stashes drawn above `base`: the first one from the base
/// commit's that no lane crosses, so a stash on a branch tip sits right
/// above it.
pub(super) fn stash_lane(base: &GraphRow) -> u32 {
    // n lanes leave a free one among n + 1 columns.
    let n = u32::try_from(base.edges.len()).unwrap_or(u32::MAX);
    (base.column..=base.column.saturating_add(n))
        .find(|c| !lanes_above(base).any(|e| e.from == *c))
        .unwrap_or(base.column)
}

/// Strokes a lane from `a` down to `b`. A lane changing column runs
/// straight along one side and turns with a rounded corner, so it stays
/// vertical where it meets the rows above and below: `vertical_first`
/// drops from `a` then turns toward `b` (a lane ending at a node), else it
/// leaves `a` sideways then turns down to `b` (a lane leaving a node).
fn stroke(
    window: &mut Window,
    a: Point<Pixels>,
    b: Point<Pixels>,
    color: Hsla,
    vertical_first: bool,
) {
    // Bézier handle length approximating a quarter circle.
    const K: f32 = 0.552_285;
    let mut path = PathBuilder::stroke(px(1.6));
    path.move_to(a);
    let dx = f32::from(b.x - a.x);
    if dx.abs() < 0.5 {
        path.line_to(b);
    } else {
        let r = dx.abs().min(f32::from(b.y - a.y)).max(0.0);
        let (rx, ry) = (px(r.copysign(dx)), px(r));
        let (hx, hy) = (rx * K, ry * K);
        if vertical_first {
            let start = point(a.x, b.y - ry);
            let end = point(a.x + rx, b.y);
            path.line_to(start);
            path.cubic_bezier_to(end, point(start.x, start.y + hy), point(end.x - hx, end.y));
        } else {
            let start = point(b.x - rx, a.y);
            let end = point(b.x, a.y + ry);
            path.line_to(start);
            path.cubic_bezier_to(end, point(start.x + hx, start.y), point(end.x, end.y - hy));
        }
        path.line_to(b);
    }
    if let Ok(p) = path.build() {
        window.paint_path(p, color);
    }
}

/// Paints a stash row above `base`: the lanes passing by (the uncommitted
/// lane `wip_lane` too), and the stash node on lane `column`, linked to the
/// stash above it when `linked` and down toward the base commit.
fn stash_cell(
    base: &GraphRow,
    column: u32,
    linked: bool,
    wip_lane: Option<u32>,
    cx: &App,
) -> impl IntoElement {
    let tint = color(cx, T::StatusDirty);
    let lanes: Vec<(u32, Hsla)> = lanes_above(base)
        .map(|e| (e.from, lane(cx, e.color)))
        .chain(wip_lane.map(|l| (l, tint)))
        .collect();
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let scale = window.scale_factor();
            let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
            let x = |col: u32| snap(bounds.origin.x + px(lane_x(col)));
            let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
            let mid = snap(bounds.origin.y + bounds.size.height / 2.0);
            for (col, c) in &lanes {
                stroke(
                    window,
                    point(x(*col), top),
                    point(x(*col), bottom),
                    *c,
                    true,
                );
            }
            // The node is hollow: the stash lane stops at its edge.
            let r = px(NODE_RADIUS);
            let cx = x(column);
            if linked {
                stroke(window, point(cx, top), point(cx, mid - r), tint, true);
            }
            stroke(window, point(cx, mid + r), point(cx, bottom), tint, true);
            window.paint_quad(
                gpui_kit::fill(
                    Bounds::new(point(cx - r, mid - r), gpui_kit::size(r * 2.0, r * 2.0)),
                    gpui_kit::transparent_black(),
                )
                .corner_radii(px(1.5))
                .border_widths(px(1.5))
                .border_color(tint),
            );
        },
    )
    .size_full()
}

/// Paints the lanes and the commit node of one graph row, the end of the
/// stash lane (`stash_lane`) when stashes are drawn above it, and the
/// uncommitted lane (`wip_lane`), ending at the node of the HEAD commit.
fn graph_cell(
    row: &GraphRow,
    stash_lane: Option<u32>,
    wip_lane: Option<u32>,
    cx: &App,
) -> impl IntoElement {
    let mut edges: Vec<(EdgeKind, u32, u32, Hsla)> = row
        .edges
        .iter()
        .map(|e| (e.kind, e.from, e.to, lane(cx, e.color)))
        .collect();
    if let Some(col) = stash_lane {
        edges.push((
            EdgeKind::Incoming,
            col,
            row.column,
            color(cx, T::StatusDirty),
        ));
    }
    let is_head = row.refs.iter().any(|r| r.kind == RefKind::Head);
    if let Some(col) = wip_lane {
        let (kind, to) = if is_head {
            (EdgeKind::Incoming, row.column)
        } else {
            (EdgeKind::Through, col)
        };
        edges.push((kind, col, to, color(cx, T::StatusDirty)));
    }
    let node = (row.column, lane(cx, row.color));
    // The checked-out commit gets a larger node.
    let node_radius = if is_head {
        HEAD_NODE_RADIUS
    } else {
        NODE_RADIUS
    };
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            // Quads snap to device pixels but paths do not: snapping the
            // centers here keeps the node on its lane line.
            let scale = window.scale_factor();
            let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
            let x = |col: u32| snap(bounds.origin.x + px(lane_x(col)));
            let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
            let mid = snap(bounds.origin.y + bounds.size.height / 2.0);
            for (kind, from, to, c) in &edges {
                let (a, b) = match kind {
                    EdgeKind::Through => (point(x(*from), top), point(x(*to), bottom)),
                    EdgeKind::Incoming => (point(x(*from), top), point(x(*to), mid)),
                    EdgeKind::Outgoing => (point(x(*from), mid), point(x(*to), bottom)),
                };
                stroke(window, a, b, *c, *kind != EdgeKind::Outgoing);
            }
            let r = px(node_radius);
            let center = point(x(node.0), mid);
            window.paint_quad(
                gpui_kit::fill(
                    Bounds::new(
                        point(center.x - r, center.y - r),
                        gpui_kit::size(r * 2.0, r * 2.0),
                    ),
                    node.1,
                )
                .corner_radii(r),
            );
        },
    )
    .size_full()
}
