//! One image, in the file browser: its size, format and weight, its zoom,
//! then the image over a checkerboard, zoomed with the scroll wheel and
//! dragged around.

use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Div, Window, div};

use super::stage::{Layer, Nav, Navigate, Zoom, stage};
use super::{Picture, release};
use crate::theme::color;
use crate::widgets::chip;

/// The image viewer's state.
pub(crate) struct ImageViewer {
    picture: Picture,
    nav: Nav,
}

impl ImageViewer {
    /// A viewer of `picture`, fitted; its texture is freed with it.
    pub(crate) fn new(picture: Picture, cx: &mut Context<Self>) -> Self {
        cx.on_release(|v: &mut Self, cx| release(v.picture.render.clone(), cx))
            .detach();
        Self {
            picture,
            nav: Nav::default(),
        }
    }
}

impl Navigate for ImageViewer {
    fn nav_mut(&mut self) -> &mut Nav {
        &mut self.nav
    }
}

/// The zoom: its percentage once zoomed, and the Fit chip, active while
/// fitted, setting it back.
pub(crate) fn zoom_control<V: Navigate>(zoom: Zoom, cx: &mut Context<V>) -> Div {
    let fit = zoom.scale.is_none();
    let percent = zoom
        .scale
        .map(|s| format!("{:.0} %", s * 100.0))
        .unwrap_or_default();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .child(info(percent, cx))
        .child(
            chip("zoom-fit", "Fit", fit, cx).on_click(cx.listener(|v: &mut V, _, _, cx| {
                v.nav_mut().zoom = Zoom::FIT;
                cx.notify();
            })),
        )
}

/// A muted line of information.
pub(crate) fn info(text: impl Into<gpui_kit::SharedString>, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(color(cx, T::FgMuted))
        .child(text.into())
}

impl Render for ImageViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = &self.picture;
        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(info(p.info(), cx).flex_1().min_w_0().truncate())
            .child(zoom_control(self.nav.zoom, cx));
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(stage(
                "image-view",
                (p.width, p.height),
                vec![Layer::whole(p)],
                self.nav,
                None,
                None,
                cx,
            ))
    }
}
