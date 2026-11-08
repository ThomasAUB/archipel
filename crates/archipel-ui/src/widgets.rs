//! Small styled building blocks, all colored through theme tokens.

use std::fmt::Write as _;

use archipel_model::ui_state::UiState;
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext, Bounds, Context, Deferred, Div, ElementId, Empty, Hsla,
    Modifiers, Pixels, Point, SharedString, Stateful, UniformList, UniformListScrollHandle, Window,
    anchored, deferred, div, px, svg,
};

use crate::theme::{color, radius, text_size, ui_font};

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

/// A square icon button, its action named by a hover tooltip.
pub fn icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    tooltip: &'static str,
    cx: &App,
) -> Stateful<Div> {
    let hover = color(cx, T::BgHover);
    div()
        .id(id)
        .flex_none()
        .p(px(3.0))
        .rounded(radius(cx))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .tooltip(move |_, cx| TextTooltip::view(&[tooltip.to_string()], cx))
        .child(
            svg()
                .path(icon.path())
                .size(px(14.0))
                .text_color(color(cx, T::FgMuted)),
        )
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

/// Hex digits shown of a commit id.
pub const SHORT_OID: usize = 8;

/// Whether no modifier (Ctrl, Alt, Cmd / Win, Shift) is held: views only
/// take plain keys for their own navigation.
pub fn unmodified(m: Modifiers) -> bool {
    !(m.control || m.alt || m.platform || m.shift)
}

/// `popup` (a menu or a dropdown) at `position`, kept inside the window
/// and drawn over the rest of it.
pub fn popup_at(position: Point<Pixels>, popup: impl IntoElement) -> Deferred {
    deferred(anchored().position(position).snap_to_window().child(popup)).with_priority(1)
}

/// `dialog` centered over its parent, which it dims and blocks.
pub fn modal(dialog: impl IntoElement, cx: &App) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .bg(color(cx, T::BgApp).opacity(0.6))
        .child(dialog)
        .into_any_element()
}

/// The frame of a right-click menu; place it with [`popup_at`].
pub fn context_menu(cx: &App) -> Div {
    div()
        .occlude()
        .min_w(px(200.0))
        .py_1()
        .flex()
        .flex_col()
        .font_family(ui_font(cx))
        .text_size(text_size(cx))
        .text_color(color(cx, T::FgDefault))
        .whitespace_nowrap()
        .bg(color(cx, T::BgElevated))
        .border_1()
        .border_color(color(cx, T::Border))
        .rounded(radius(cx))
        .shadow_md()
}

/// An entry of a [`context_menu`]; `danger` colors destructive actions.
pub fn menu_item(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    danger: bool,
    cx: &App,
) -> Stateful<Div> {
    let hover = color(cx, T::BgHover);
    div()
        .id(id)
        .px_3()
        .py_1()
        .cursor_pointer()
        .when(danger, |d| d.text_color(color(cx, T::StatusError)))
        .hover(move |s| s.bg(hover))
        .child(label.into())
}

/// An on/off entry of a [`context_menu`]: a tick left of the label when
/// `checked`, the same room left blank otherwise so labels line up.
pub fn check_menu_item(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    checked: bool,
    cx: &App,
) -> Stateful<Div> {
    let hover = color(cx, T::BgHover);
    div()
        .id(id)
        .pl_2()
        .pr_3()
        .py_1()
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(
            div()
                .w(px(16.0))
                .flex_none()
                .flex()
                .justify_center()
                .when(checked, |d| d.child("✓")),
        )
        .child(label.into())
}

/// A rule between groups of [`menu_item`]s.
pub fn menu_separator(cx: &App) -> Div {
    div().my_1().h(px(1.0)).bg(color(cx, T::BorderSubtle))
}

/// A filter chip, highlighted when active.
pub fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let (bg, fg) = if active {
        (color(cx, T::AccentSubtle), color(cx, T::AccentText))
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

/// A hover tooltip of a few lines of text.
pub struct TextTooltip(Vec<SharedString>);

impl TextTooltip {
    /// The view a `.tooltip(...)` builder returns.
    pub fn view(lines: &[String], cx: &mut App) -> AnyView {
        let lines = lines.iter().cloned().map(SharedString::from).collect();
        cx.new(|_| Self(lines)).into()
    }
}

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .font_family(ui_font(cx))
            .text_size(text_size(cx))
            .text_color(color(cx, T::FgDefault))
            .whitespace_nowrap()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_md()
            .children(self.0.iter().map(|line| div().child(line.clone())))
    }
}

/// A virtualized list filling its parent, with a vertical scrollbar while
/// its rows overflow. It scrolls through the handle given with
/// [`ScrollList::handle`], else through one kept under `id`.
#[derive(IntoElement)]
pub struct ScrollList {
    id: ElementId,
    list: UniformList,
    handle: Option<UniformListScrollHandle>,
}

pub fn scroll_list(id: impl Into<ElementId>, list: UniformList) -> ScrollList {
    ScrollList {
        id: id.into(),
        list,
        handle: None,
    }
}

impl ScrollList {
    /// Scrolls through `handle`, for a list also scrolled from outside.
    pub fn handle(mut self, handle: &UniformListScrollHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }
}

impl RenderOnce for ScrollList {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let handle = self.handle.unwrap_or_else(|| {
            window
                .use_keyed_state(self.id, cx, |_, _| UniformListScrollHandle::new())
                .read(cx)
                .clone()
        });
        div()
            .relative()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.list.track_scroll(&handle).flex_1())
            .vertical_scrollbar(&handle)
    }
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
    /// A repo view's refs (left of the graph).
    Refs,
    /// A repo view's details (right of the graph).
    Details,
    /// The dashboard's quick edit pane (right of the rows).
    QuickEdit,
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
        Pane::Refs => (x - bounds.left(), ui.details_width),
        Pane::Details => (bounds.right() - x, ui.refs_width),
        Pane::QuickEdit => (bounds.right() - x, 0.0),
    };
    let max = (f32::from(bounds.size.width) - other - MIN_PANE).max(MIN_PANE);
    let width = f32::from(width).clamp(MIN_PANE, max);
    *match pane {
        Pane::Refs => &mut ui.refs_width,
        Pane::Details => &mut ui.details_width,
        Pane::QuickEdit => &mut ui.quick_edit_width,
    } = width;
}

/// Shortens a status message to a few lines: git errors can list thousands
/// of files, which would push the whole view out of the window. Keeps the
/// first lines and the last two (git's hint, "Aborting"), and counts the
/// lines left out.
pub fn condense_status(text: &str) -> String {
    const HEAD: usize = 3;
    const TAIL: usize = 2;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= HEAD + TAIL + 1 {
        return text.to_owned();
    }
    let hidden = lines.len() - HEAD - TAIL;
    let mut out = lines[..HEAD].join("\n");
    let _ = write!(out, "\n… {hidden} more lines\n");
    out.push_str(&lines[lines.len() - TAIL..].join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use gpui_kit::{point, size};

    use super::*;

    #[test]
    fn condense_status_keeps_short_text_and_shortens_long_lists() {
        assert_eq!(condense_status("one\ntwo"), "one\ntwo");
        let long: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        assert_eq!(
            condense_status(&long.join("\n")),
            "line 0\nline 1\nline 2\n… 95 more lines\nline 98\nline 99"
        );
    }

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
