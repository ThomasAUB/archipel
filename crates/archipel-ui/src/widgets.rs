//! Small styled building blocks, all colored through theme tokens.

use archipel_model::ui_state::UiState;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, AppContext, Bounds, Div, ElementId, Empty, Hsla, Pixels, SharedString, Stateful, div, px,
};

use crate::theme::{color, radius};

/// A clickable text button.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let hover = color(cx, T::BgHover);
    div()
        .id(id)
        .px_2()
        .py(px(3.0))
        .rounded(radius(cx))
        .border_1()
        .border_color(color(cx, T::Border))
        .bg(color(cx, T::BgElevated))
        .text_color(color(cx, T::FgDefault))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

/// A primary (accent) button.
pub fn primary_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    cx: &App,
) -> Stateful<Div> {
    let hover = color(cx, T::AccentHover);
    div()
        .id(id)
        .px_3()
        .py(px(3.0))
        .rounded(radius(cx))
        .bg(color(cx, T::Accent))
        .text_color(color(cx, T::AccentFg))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

/// A filter chip, highlighted when active.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let (bg, fg) = if active {
        (color(cx, T::AccentSubtle), color(cx, T::Accent))
    } else {
        (color(cx, T::BgPanel), color(cx, T::FgMuted))
    };
    let hover = color(cx, T::BgHover);
    div()
        .id(id)
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .border_1()
        .border_color(if active {
            color(cx, T::Accent)
        } else {
            color(cx, T::BorderSubtle)
        })
        .bg(bg)
        .text_color(fg)
        .text_xs()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

/// A small round status dot.
pub fn dot(fill: Hsla) -> Div {
    div().size(px(8.0)).rounded_full().bg(fill).flex_none()
}

/// A section title in panels.
pub fn section(label: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px_2()
        .pt_3()
        .pb_1()
        .text_xs()
        .text_color(color(cx, T::FgSubtle))
        .child(label.into())
}

/// A pane whose width a [`splitter`] drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    /// The workspace sidebar (left of the main area).
    Sidebar,
    /// A repo view's refs (left of the graph).
    Refs,
    /// A repo view's commit details (right of the graph).
    Details,
    /// A repo view's changed files (left of the file diff).
    Changes,
}

/// Drag payload of a [`splitter`]: the row holding the pane listens for
/// `DragMoveEvent<PaneDrag>` and calls [`resize_pane`]. gpui hands a drag
/// to every such listener in the window, wherever the pointer is, so each
/// row must ignore the panes it does not hold.
pub struct PaneDrag(pub Pane);

/// Narrowest a pane, or the space left beside it, gets.
const MIN_PANE: f32 = 120.0;

/// The separator between two panes, a grab area dragging `pane`'s width.
pub fn splitter(pane: Pane, cx: &App) -> Stateful<Div> {
    let group = SharedString::from(format!("split-{pane:?}"));
    let accent = color(cx, T::Accent);
    div()
        .id(group.clone())
        .group(group.clone())
        .w(px(5.0))
        .h_full()
        .flex_none()
        .flex()
        .justify_center()
        .cursor_col_resize()
        .on_drag(PaneDrag(pane), |_, _, _, cx| cx.new(|_| Empty))
        .child(
            div()
                .w(px(1.0))
                .h_full()
                .bg(color(cx, T::BorderSubtle))
                .group_hover(group, move |s| s.bg(accent)),
        )
}

/// Sets `pane`'s width from the pointer at `x`, within the row `bounds`
/// holding it, keeping it and its neighbors at least [`MIN_PANE`] wide.
pub fn resize_pane(ui: &mut UiState, pane: Pane, x: Pixels, bounds: Bounds<Pixels>) {
    let (width, other) = match pane {
        Pane::Sidebar | Pane::Changes => (x - bounds.left(), 0.0),
        Pane::Refs => (x - bounds.left(), ui.details_width),
        Pane::Details => (bounds.right() - x, ui.refs_width),
    };
    let max = (f32::from(bounds.size.width) - other - MIN_PANE).max(MIN_PANE);
    let width = f32::from(width).clamp(MIN_PANE, max);
    *match pane {
        Pane::Sidebar => &mut ui.sidebar_width,
        Pane::Refs => &mut ui.refs_width,
        Pane::Details => &mut ui.details_width,
        Pane::Changes => &mut ui.changes_width,
    } = width;
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, size};

    use super::*;

    fn row() -> Bounds<Pixels> {
        Bounds::new(point(px(100.0), px(0.0)), size(px(1000.0), px(600.0)))
    }

    #[test]
    fn left_panes_follow_the_pointer_from_the_row_start() {
        let mut ui = UiState::default();
        resize_pane(&mut ui, Pane::Refs, px(400.0), row());
        assert!((ui.refs_width - 300.0).abs() < f32::EPSILON);
    }

    #[test]
    fn right_panes_follow_the_pointer_from_the_row_end() {
        let mut ui = UiState::default();
        resize_pane(&mut ui, Pane::Details, px(800.0), row());
        assert!((ui.details_width - 300.0).abs() < f32::EPSILON);
    }

    #[test]
    fn widths_keep_room_for_the_neighbors() {
        let mut ui = UiState::default();
        resize_pane(&mut ui, Pane::Refs, px(0.0), row());
        assert!((ui.refs_width - MIN_PANE).abs() < f32::EPSILON);
        resize_pane(&mut ui, Pane::Details, px(100.0), row());
        assert!((ui.details_width - (1000.0 - MIN_PANE - MIN_PANE)).abs() < f32::EPSILON);
    }
}
