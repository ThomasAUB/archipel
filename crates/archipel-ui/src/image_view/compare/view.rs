//! The image diff's rendering: a bar with the compare modes, the current
//! mode's controls and the zoom, then the images in that mode.

use archipel_theme::ColorToken as T;
use gpui_kit::component::slider::Slider;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Div, MouseButton, MouseDownEvent, MouseMoveEvent, SharedString, Window,
    div, px,
};

use super::{Computed, ImageDiff, Loaded, Mode};
use crate::image_view::Picture;
use crate::image_view::stage::{Clip, Layer, fraction_at, split_line, stage};
use crate::image_view::viewer::{info, zoom_chips};
use crate::theme::color;
use crate::widgets::{button, check, chip};

impl ImageDiff {
    /// The canvas both images are shown on: wide and tall enough for each.
    fn canvas(&self) -> (u32, u32) {
        [self.old.picture(), self.new.picture()]
            .into_iter()
            .flatten()
            .fold((1, 1), |(w, h), p| (w.max(p.width), h.max(p.height)))
    }

    /// A stage of the shared canvas, showing `layers`.
    fn stage_of(&self, id: &'static str, layers: Vec<Layer>, cx: &Context<Self>) -> AnyElement {
        stage(id, self.canvas(), layers, self.prefs.zoom, None, None, cx).into_any_element()
    }

    /// One side's pane: its label and size, then its image.
    fn pane(&self, before: bool, cx: &Context<Self>) -> AnyElement {
        let (loaded, other) = if before {
            (&self.old, &self.new)
        } else {
            (&self.new, &self.old)
        };
        let name = match (other, before) {
            (Loaded::Absent, true) => "Deleted",
            (Loaded::Absent, false) => "Added",
            (_, true) => "Before",
            (_, false) => "After",
        };
        let (label, body) = match loaded {
            Loaded::Ready(p) => (
                format!("{name} · {}", p.info()),
                self.stage_of(
                    if before { "image-old" } else { "image-new" },
                    vec![Layer::whole(p)],
                    cx,
                ),
            ),
            Loaded::Loading => (name.to_owned(), note("Loading…", cx)),
            Loaded::Absent => (name.to_owned(), note("Not in this version", cx)),
            Loaded::Failed(e) => (name.to_owned(), note(e.clone(), cx)),
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(info(label, cx).px_2().py_1().truncate())
            .child(body)
            .into_any_element()
    }

    /// Both sides next to each other, or the one there is.
    fn side_by_side(&self, cx: &Context<Self>) -> AnyElement {
        let panes = div().flex_1().min_h_0().flex();
        match (&self.old, &self.new) {
            (Loaded::Absent, _) => panes.child(self.pane(false, cx)),
            (_, Loaded::Absent) => panes.child(self.pane(true, cx)),
            _ => panes
                .child(self.pane(true, cx))
                .child(div().w(px(1.0)).bg(color(cx, T::BorderSubtle)))
                .child(self.pane(false, cx)),
        }
        .into_any_element()
    }

    /// One canvas: before left of the split, after right of it; a press
    /// or drag on it moves the split.
    fn swipe(&self, a: &Picture, b: &Picture, cx: &mut Context<Self>) -> AnyElement {
        let layer = |p: &Picture, clip| Layer {
            clip,
            ..Layer::whole(p)
        };
        let layers = vec![
            layer(a, Clip::Before(self.split)),
            layer(b, Clip::After(self.split)),
        ];
        let canvas = self.canvas();
        let line = split_line(canvas, self.prefs.zoom, self.split, cx);
        let placed = self.placed.clone();
        stage(
            "image-swipe",
            canvas,
            layers,
            self.prefs.zoom,
            Some(placed),
            Some(line),
            cx,
        )
        .cursor_ew_resize()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|v: &mut Self, e: &MouseDownEvent, _, cx| {
                v.dragging = true;
                v.move_split(e.position.x, cx);
            }),
        )
        .on_mouse_move(cx.listener(|v: &mut Self, e: &MouseMoveEvent, _, cx| {
            if v.dragging && e.pressed_button == Some(MouseButton::Left) {
                v.move_split(e.position.x, cx);
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|v: &mut Self, _, _, _| v.dragging = false),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|v: &mut Self, _, _, _| v.dragging = false),
        )
        .into_any_element()
    }

    /// Puts the swipe split under window x `x`.
    fn move_split(&mut self, x: gpui_kit::Pixels, cx: &mut Context<Self>) {
        if let Some(f) = fraction_at(&self.placed, x) {
            self.split = f;
            cx.notify();
        }
    }

    /// The opacity of the image after, over the one before, in onion skin.
    fn onion_opacity(&self, cx: &Context<Self>) -> f32 {
        (self.onion.read(cx).value().start() / 100.0).clamp(0.0, 1.0)
    }

    /// The images in the current mode, once both are read.
    fn compared(&self, a: &Picture, b: &Picture, cx: &mut Context<Self>) -> AnyElement {
        match self.prefs.mode {
            Mode::SideBySide => self.side_by_side(cx),
            Mode::Swipe => self.swipe(a, b, cx),
            Mode::Onion => {
                let over = Layer {
                    opacity: self.onion_opacity(cx),
                    ..Layer::whole(b)
                };
                self.stage_of("image-onion", vec![Layer::whole(a), over], cx)
            }
            Mode::Blink => {
                let shown = if self.held || self.flipped { a } else { b };
                self.stage_of("image-blink", vec![Layer::whole(shown)], cx)
            }
            Mode::Difference => match &self.difference {
                Some((_, Computed::Done(Some(d)))) => {
                    self.stage_of("image-difference", vec![Layer::whole(&d.picture)], cx)
                }
                Some((_, Computed::Done(None))) => note("No pixels to compare", cx),
                _ => note("Computing the difference…", cx),
            },
        }
    }

    /// The controls of the current mode.
    fn mode_controls(&self, cx: &mut Context<Self>) -> Option<Div> {
        let row = div().flex().flex_none().items_center().gap_2().text_xs();
        Some(match self.prefs.mode {
            Mode::SideBySide => return None,
            Mode::Swipe => row.child(info("before ◀ drag ▶ after", cx)),
            Mode::Onion => row
                .child(info("Before", cx))
                .child(div().w(px(140.0)).child(Slider::new(&self.onion)))
                .child(info("After", cx)),
            Mode::Blink => {
                let shown = if self.held || self.flipped {
                    "showing before"
                } else {
                    "showing after"
                };
                row.child(blink_button(cx))
                    .child(
                        check("blink-auto", "Auto", self.auto.is_some(), cx).on_click(cx.listener(
                            |v: &mut Self, _, _, cx| {
                                let on = v.auto.is_none();
                                v.set_auto(on, cx);
                            },
                        )),
                    )
                    .child(info(shown, cx))
            }
            Mode::Difference => {
                let summary = match &self.difference {
                    Some((_, Computed::Done(Some(d)))) => d.summary(),
                    _ => String::new(),
                };
                row.child(
                    check("diff-amplify", "Amplify ×4", self.prefs.amplify, cx).on_click(
                        cx.listener(|v: &mut Self, _, _, cx| {
                            let amplify = !v.prefs.amplify;
                            v.set_amplify(amplify, cx);
                        }),
                    ),
                )
                .child(info(summary, cx))
            }
        })
    }

    /// The bar: modes (once both images are read), the mode's controls,
    /// the zoom.
    fn bar(&self, both: bool, cx: &mut Context<Self>) -> Div {
        let modes = Mode::ALL.map(|mode| {
            let label = mode.label();
            chip(
                SharedString::from(format!("image-mode-{label}")),
                label,
                self.prefs.mode == mode,
                cx,
            )
            .on_click(cx.listener(move |v: &mut Self, _, _, cx| v.set_mode(mode, cx)))
        });
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .when(both, |bar| {
                bar.child(div().flex().flex_none().gap_1().children(modes))
            })
            .when_some(self.mode_controls(cx).filter(|_| both), Div::child)
            .child(div().flex_1())
            .child(zoom_chips(self.prefs.zoom, cx, |v: &mut Self, z| {
                v.prefs.zoom = z;
            }))
    }
}

/// The button showing the image before while it is held.
fn blink_button(cx: &mut Context<ImageDiff>) -> impl IntoElement {
    let release = |v: &mut ImageDiff,
                   _: &gpui_kit::MouseUpEvent,
                   _: &mut Window,
                   cx: &mut Context<ImageDiff>| {
        v.held = false;
        cx.notify();
    };
    button("blink-hold", "Hold for before", cx)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|v: &mut ImageDiff, _, _, cx| {
                v.held = true;
                cx.notify();
            }),
        )
        .on_mouse_up(MouseButton::Left, cx.listener(release))
        .on_mouse_up_out(MouseButton::Left, cx.listener(release))
}

/// A muted note in place of an image.
fn note(text: impl Into<SharedString>, cx: &Context<ImageDiff>) -> AnyElement {
    div()
        .p_3()
        .text_color(color(cx, T::FgMuted))
        .child(text.into())
        .into_any_element()
}

impl Render for ImageDiff {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pair = self.pair().map(|(a, b)| (a.clone(), b.clone()));
        // Overlaid, the images' sizes show above them.
        let sizes = pair
            .as_ref()
            .filter(|_| self.prefs.mode != Mode::SideBySide)
            .map(|(a, b)| {
                let aligned = if (a.width, a.height) == (b.width, b.height) {
                    ""
                } else {
                    " · sizes differ, aligned top-left"
                };
                format!("Before · {} — After · {}{aligned}", a.info(), b.info())
            });
        let body = match &pair {
            Some((a, b)) => self.compared(a, b, cx),
            None => self.side_by_side(cx),
        };
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .text_color(color(cx, T::FgDefault))
            .child(self.bar(pair.is_some(), cx))
            .when_some(sizes, |d, s| d.child(info(s, cx).px_2().py_1().truncate()))
            .child(body)
    }
}
