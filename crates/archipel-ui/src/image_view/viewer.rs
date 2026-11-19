//! One image, in the file browser: its size, format and weight, a Fit /
//! 100 % toggle, then the image over a checkerboard.

use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Div, Window, div};

use super::stage::{Layer, Zoom, stage};
use super::{Picture, release};
use crate::theme::color;
use crate::widgets::chip;

/// The image viewer's state.
pub(crate) struct ImageViewer {
    picture: Picture,
    zoom: Zoom,
}

impl ImageViewer {
    /// A viewer of `picture`, fitted; its texture is freed with it.
    pub(crate) fn new(picture: Picture, cx: &mut Context<Self>) -> Self {
        cx.on_release(|v: &mut Self, cx| release(v.picture.render.clone(), cx))
            .detach();
        Self {
            picture,
            zoom: Zoom::Fit,
        }
    }
}

/// The Fit / 100 % chips, `zoom` active; a click sets it with `set`.
pub(crate) fn zoom_chips<V: 'static>(
    zoom: Zoom,
    cx: &mut Context<V>,
    set: fn(&mut V, Zoom),
) -> Div {
    let chips = [(Zoom::Fit, "Fit"), (Zoom::Actual, "100 %")].map(|(z, label)| {
        chip(format!("zoom-{label}"), label, zoom == z, cx).on_click(cx.listener(
            move |v: &mut V, _, _, cx| {
                set(v, z);
                cx.notify();
            },
        ))
    });
    div().flex().flex_none().gap_1().children(chips)
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
            .child(zoom_chips(self.zoom, cx, |v: &mut Self, z| v.zoom = z));
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
                self.zoom,
                None,
                cx,
            ))
    }
}
