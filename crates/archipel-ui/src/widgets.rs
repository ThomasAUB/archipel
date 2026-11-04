//! Small styled building blocks, all colored through theme tokens.

use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{App, Div, ElementId, Hsla, SharedString, Stateful, div, px};

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
