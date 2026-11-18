//! Small styled building blocks, all colored through theme tokens.

use std::fmt::Write as _;

use archipel_model::ui_state::UiState;
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext, Bounds, ClickEvent, Context, Deferred, Div, ElementId,
    Empty, Hsla, Modifiers, Pixels, Point, SharedString, Stateful, UniformList,
    UniformListScrollHandle, Window, anchored, deferred, div, px, svg,
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

/// Clicks on an element that can be disabled (a button, a checkbox).
pub trait ClickIf: Sized {
    /// Runs `handler` on click while `enabled`; dimmed, without the hand
    /// cursor and deaf to clicks otherwise. The one way to disable a
    /// control: dimming it alone left it clickable.
    #[must_use]
    fn on_click_if(
        self,
        enabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self;
}

impl ClickIf for Stateful<Div> {
    fn on_click_if(
        self,
        enabled: bool,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        if enabled {
            self.on_click(handler)
        } else {
            self.opacity(0.5).cursor_default()
        }
    }
}

/// A checkbox glyph, for a row that is a checkbox with more in it.
pub fn check_box(checked: bool, cx: &App) -> Div {
    div()
        .flex_none()
        .text_color(color(cx, if checked { T::AccentText } else { T::FgMuted }))
        .child(if checked { "☑" } else { "☐" })
}

/// A checkbox and its label; disable it with [`ClickIf::on_click_if`].
pub fn check(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    checked: bool,
    cx: &App,
) -> Stateful<Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .gap_2()
        .cursor_pointer()
        .child(check_box(checked, cx))
        .child(label.into())
}

/// The frame of a dialog: title, body, error, buttons.
pub fn dialog_frame(id: &'static str, title: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    div()
        .id(id)
        .occlude()
        .w(px(560.0))
        .max_h(px(640.0))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .bg(color(cx, T::BgElevated))
        .border_1()
        .border_color(color(cx, T::Border))
        .rounded(radius(cx))
        .shadow_lg()
        .child(
            div()
                .text_color(color(cx, T::FgDefault))
                .child(title.into()),
        )
}

/// Width of a [`field`]'s label column.
pub const FIELD_LABEL_WIDTH: f32 = 110.0;

/// A labelled row of a form.
pub fn field(label: &'static str, input: impl IntoElement, cx: &App) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(FIELD_LABEL_WIDTH))
                .flex_none()
                .text_color(color(cx, T::FgMuted))
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(input))
        .into_any_element()
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

/// A move through a list by keyboard: ↑ ↓, Page Up / Page Down, Home /
/// End, the same in every list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListMove {
    /// Down (positive) or up (negative) by this many items.
    By(i64),
    /// To the first item.
    First,
    /// To the last item.
    Last,
}

/// Items a Page Up / Page Down moves.
pub const PAGE_ITEMS: i64 = 20;

impl ListMove {
    /// The move of a list key (`down`, `pageup`, `home`, …), if it is one.
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "down" => Self::By(1),
            "up" => Self::By(-1),
            "pagedown" => Self::By(PAGE_ITEMS),
            "pageup" => Self::By(-PAGE_ITEMS),
            "home" => Self::First,
            "end" => Self::Last,
            _ => return None,
        })
    }

    /// Where it leads from item `from` (`None`: none selected, so it
    /// starts above the first) in a list of `len` items, kept inside the
    /// list; `None` when the list is empty.
    pub fn apply(self, from: Option<usize>, len: usize) -> Option<usize> {
        let last = len.checked_sub(1)?;
        Some(match self {
            Self::First => 0,
            Self::Last => last,
            Self::By(delta) => {
                let from = from.map_or(-1, |i| i64::try_from(i).unwrap_or(i64::MAX));
                let to = from.saturating_add(delta).max(0);
                usize::try_from(to).unwrap_or(last).min(last)
            }
        })
    }
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
        .font_weight(crate::theme::ui_weight(cx))
        .text_size(text_size(cx))
        .text_color(color(cx, T::FgDefault))
        .whitespace_nowrap()
        .bg(color(cx, T::BgElevated))
        .border_1()
        .border_color(color(cx, T::Border))
        .rounded(radius(cx))
        .shadow_md()
}

/// A [`context_menu`] frame for a long list (a dropdown): it scrolls past
/// a few dozen entries.
pub fn scroll_menu(id: &'static str, cx: &App) -> impl IntoElement + ParentElement + use<> {
    context_menu(cx)
        .id(id)
        .max_h(px(360.0))
        .overflow_y_scrollbar()
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

/// A clickable row of a list, highlighted while `selected`. Only an
/// unselected row tints under the pointer: hovering the selected one keeps
/// its highlight.
pub fn list_row(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div> {
    let hover = color(cx, T::BgHover);
    div()
        .id(id)
        .cursor_pointer()
        .when(selected, |d| d.bg(color(cx, T::BgSelected)))
        .when(!selected, |d| d.hover(move |s| s.bg(hover)))
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
            .font_weight(crate::theme::ui_weight(cx))
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
        Pane::Details => {
            let refs = if ui.refs_collapsed { 0.0 } else { ui.refs_width };
            (bounds.right() - x, refs)
        }
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
    fn list_moves_stay_inside_the_list() {
        let down = ListMove::from_key("down").unwrap();
        let page_up = ListMove::from_key("pageup").unwrap();
        assert_eq!(down.apply(None, 5), Some(0));
        assert_eq!(down.apply(Some(4), 5), Some(4));
        assert_eq!(page_up.apply(Some(3), 5), Some(0));
        assert_eq!(ListMove::By(PAGE_ITEMS).apply(Some(1), 5), Some(4));
        assert_eq!(ListMove::Last.apply(None, 5), Some(4));
        assert_eq!(ListMove::First.apply(Some(3), 5), Some(0));
        assert_eq!(down.apply(None, 0), None);
        assert_eq!(ListMove::from_key("a"), None);
    }

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
