//! Painting images: a stage holds a canvas of a given size (the larger of
//! the images compared), scaled to fit its pane or zoomed with the scroll
//! wheel and dragged around, over a checkerboard showing transparency.
//! Images are layers on it, aligned top-left, each with an opacity and
//! possibly clipped to one side of a split (the swipe).

use std::cell::Cell;
use std::rc::Rc;

use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, Context, Corners, Div, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PinchEvent, Pixels, Point, ScrollEvent, SharedString, Stateful,
    Window, canvas, div, fill, point, px, size,
};

use super::Picture;
use crate::theme::color;

/// Side of a checkerboard square.
const CHECKER: f32 = 8.0;

/// Space left around a canvas fitted in its pane.
const MARGIN: f32 = 8.0;

/// The largest zoom the scroll wheel reaches, unless fitting goes further.
const MAX_SCALE: f32 = 64.0;

/// Zoom of one scroll wheel notch.
const NOTCH: f32 = 1.25;

/// Height of a scrolled line, in pixels.
const LINE: f32 = 20.0;

/// How the canvas is scaled into its pane and which part of it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Zoom {
    /// Logical pixels per image pixel; `None` fits the whole canvas in the
    /// pane, as large as it allows.
    pub(crate) scale: Option<f32>,
    /// The canvas point (in image pixels) at the pane's center, kept where
    /// the canvas still covers the pane once painted.
    pub(crate) center: (f32, f32),
}

impl Zoom {
    /// The whole canvas, as large as the pane allows.
    pub(crate) const FIT: Self = Self {
        scale: None,
        center: (0.0, 0.0),
    };

    /// The scale in `pane` of a canvas of `canvas_size`.
    fn scale_in(self, pane: Bounds<Pixels>, canvas_size: (u32, u32)) -> f32 {
        self.scale.unwrap_or_else(|| fit_scale(pane, canvas_size))
    }

    /// The canvas point at the center of `pane`, at `scale`: on each axis,
    /// the canvas's middle if it fits the pane, else kept so the canvas
    /// covers the pane.
    fn center_in(self, pane: Bounds<Pixels>, canvas_size: (u32, u32), scale: f32) -> (f32, f32) {
        let axis = |center: f32, canvas: f32, pane: Pixels| {
            let half = f32::from(pane) / scale / 2.0;
            if canvas <= half * 2.0 {
                canvas / 2.0
            } else {
                center.clamp(half, canvas - half)
            }
        };
        let (w, h) = (side(canvas_size.0.max(1)), side(canvas_size.1.max(1)));
        (
            axis(self.center.0, w, pane.size.width),
            axis(self.center.1, h, pane.size.height),
        )
    }

    /// Zoomed by `factor` around window point `at` in `pane`, between 100 %
    /// (or the fit, if smaller) and `MAX_SCALE` (or the fit, if larger);
    /// passing the fit lands on it.
    pub(crate) fn zoomed_at(
        self,
        pane: Bounds<Pixels>,
        canvas_size: (u32, u32),
        at: Point<Pixels>,
        factor: f32,
    ) -> Self {
        let fit = fit_scale(pane, canvas_size);
        let scale = self.scale_in(pane, canvas_size);
        let center = self.center_in(pane, canvas_size, scale);
        let new = (scale * factor).clamp(fit.min(1.0), fit.max(MAX_SCALE));
        if (scale - fit) * (new - fit) < 0.0 || (new - fit).abs() <= fit * 1e-3 {
            return Self {
                scale: None,
                center,
            };
        }
        let mid = pane.center();
        let (dx, dy) = (f32::from(at.x - mid.x), f32::from(at.y - mid.y));
        // The canvas point under `at` stays there.
        let under = (center.0 + dx / scale, center.1 + dy / scale);
        let zoomed = Self {
            scale: Some(new),
            center: (under.0 - dx / new, under.1 - dy / new),
        };
        Self {
            center: zoomed.center_in(pane, canvas_size, new),
            ..zoomed
        }
    }

    /// Dragged by `by` (in window pixels) in `pane`, as far as the canvas
    /// still covers it.
    pub(crate) fn panned(
        self,
        pane: Bounds<Pixels>,
        canvas_size: (u32, u32),
        by: Point<Pixels>,
    ) -> Self {
        let scale = self.scale_in(pane, canvas_size);
        let center = self.center_in(pane, canvas_size, scale);
        let moved = Self {
            center: (
                center.0 - f32::from(by.x) / scale,
                center.1 - f32::from(by.y) / scale,
            ),
            ..self
        };
        Self {
            center: moved.center_in(pane, canvas_size, scale),
            ..moved
        }
    }
}

/// The scale fitting a canvas of `canvas_size` in `pane`, with a margin.
fn fit_scale(pane: Bounds<Pixels>, canvas_size: (u32, u32)) -> f32 {
    let (w, h) = (side(canvas_size.0.max(1)), side(canvas_size.1.max(1)));
    let room = |s: Pixels| (f32::from(s) - 2.0 * MARGIN).max(0.0);
    (room(pane.size.width) / w).min(room(pane.size.height) / h)
}

/// A stage's zoom, and the drag panning it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Nav {
    pub(crate) zoom: Zoom,
    /// Where the pointer dragging the canvas last was, while it does.
    pub(crate) grab: Option<Point<Pixels>>,
}

impl Nav {
    /// At `zoom`, not dragged.
    pub(crate) fn new(zoom: Zoom) -> Self {
        Self { zoom, grab: None }
    }
}

/// A view showing stages, zoomed and dragged around together.
pub(crate) trait Navigate: Sized + 'static {
    /// Its stages' zoom and drag.
    fn nav_mut(&mut self) -> &mut Nav;
    /// A left button drag pans (else only the middle button does).
    fn left_pans(&self) -> bool {
        true
    }
}

/// The part of an image a layer shows, split at a fraction of the canvas
/// width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Clip {
    Whole,
    /// Left of the split.
    Before(f32),
    /// Right of the split.
    After(f32),
}

/// One image painted on a stage.
pub(crate) struct Layer {
    pub(crate) picture: Picture,
    pub(crate) opacity: f32,
    pub(crate) clip: Clip,
}

impl Layer {
    /// `picture`, whole and opaque.
    pub(crate) fn whole(picture: &Picture) -> Self {
        Self {
            picture: picture.clone(),
            opacity: 1.0,
            clip: Clip::Whole,
        }
    }
}

/// Where a stage was last painted, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Painted {
    /// The stage's pane.
    pub(crate) pane: Bounds<Pixels>,
    /// The canvas in it.
    pub(crate) canvas: Bounds<Pixels>,
}

/// Where a stage was last painted (read back to zoom around the pointer
/// and to place the swipe split under it).
pub(crate) type Placed = Rc<Cell<Option<Painted>>>;

/// A logical size as `f32`.
#[allow(
    clippy::cast_precision_loss,
    reason = "image sides stay far below 2^24"
)]
fn side(n: u32) -> f32 {
    n as f32
}

/// The canvas of `canvas_size` placed in `bounds` at `zoom`.
pub(crate) fn place(bounds: Bounds<Pixels>, canvas_size: (u32, u32), zoom: Zoom) -> Bounds<Pixels> {
    let (w, h) = (side(canvas_size.0.max(1)), side(canvas_size.1.max(1)));
    let scale = zoom.scale_in(bounds, canvas_size);
    let center = zoom.center_in(bounds, canvas_size, scale);
    let mid = bounds.center();
    Bounds::new(
        point(mid.x - px(center.0 * scale), mid.y - px(center.1 * scale)),
        size(px(w * scale), px(h * scale)),
    )
}

/// The fraction of the canvas width at window x `x`, given where it was
/// painted.
pub(crate) fn fraction_at(placed: &Placed, x: Pixels) -> Option<f32> {
    let rect = placed.get()?.canvas;
    let width = f32::from(rect.size.width);
    (width > 0.0).then(|| (f32::from(x - rect.origin.x) / width).clamp(0.0, 1.0))
}

/// A stage of `canvas_size` showing `layers` (bottom first), then
/// `marks` over them, filling its parent, at `nav`'s zoom; `placed` learns
/// where it is painted. The scroll wheel (or a pinch) zooms around the
/// pointer and a drag pans, changing `V`'s nav.
pub(crate) fn stage<V: Navigate>(
    id: impl Into<SharedString>,
    canvas_size: (u32, u32),
    layers: Vec<Layer>,
    nav: Nav,
    placed: Option<Placed>,
    marks: Option<AnyElement>,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let placed = placed.unwrap_or_default();
    let zoom = nav.zoom;
    let colors = (color(cx, T::BgPanel), color(cx, T::BgHover));
    let mut content =
        div()
            .relative()
            .size_full()
            .child(checkerboard(canvas_size, zoom, colors, placed.clone()));
    for layer in layers {
        let opacity = layer.opacity;
        let painted = paint_layer(layer, canvas_size, zoom);
        content = content.child(if opacity < 1.0 {
            div()
                .absolute()
                .size_full()
                .opacity(opacity)
                .child(painted)
                .into_any_element()
        } else {
            painted
        });
    }
    let content = content.children(marks);
    let pane = div()
        .id(id.into())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .child(content);
    navigable(pane, canvas_size, nav, &placed, cx)
}

/// `pane` zooming with the scroll wheel or a pinch, around the pointer,
/// and panning with a drag.
fn navigable<V: Navigate>(
    pane: Stateful<Div>,
    canvas_size: (u32, u32),
    nav: Nav,
    placed: &Placed,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let zoom_by = |placed: Placed| {
        move |v: &mut V, at: Point<Pixels>, factor: f32, cx: &mut Context<V>| {
            if let Some(painted) = placed.get()
                && factor.is_finite()
                && factor > 0.0
            {
                let nav = v.nav_mut();
                nav.zoom = nav.zoom.zoomed_at(painted.pane, canvas_size, at, factor);
                cx.notify();
            }
        }
    };
    let on_scroll = zoom_by(placed.clone());
    let on_pinch = zoom_by(placed.clone());
    let panning = placed.clone();
    let grab = |v: &mut V, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<V>| {
        v.nav_mut().grab = Some(e.position);
        cx.notify();
    };
    let drop = |v: &mut V, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<V>| {
        if v.nav_mut().grab.take().is_some() {
            cx.notify();
        }
    };
    pane.when(nav.zoom.scale.is_some(), |pane| {
        if nav.grab.is_some() {
            pane.cursor_grabbing()
        } else {
            pane.cursor_grab()
        }
    })
    .on_scroll(cx.listener(move |v: &mut V, e: &ScrollEvent, _, cx| {
        let dy = f32::from(e.delta.pixel_delta(px(LINE)).y);
        if dy != 0.0 {
            on_scroll(v, e.position, NOTCH.powf(dy / LINE), cx);
            cx.stop_propagation();
        }
    }))
    .on_pinch(cx.listener(move |v: &mut V, e: &PinchEvent, _, cx| {
        on_pinch(v, e.position, 1.0 + e.delta, cx);
    }))
    .on_mouse_down(
        MouseButton::Left,
        cx.listener(move |v: &mut V, e: &MouseDownEvent, w, cx| {
            if v.left_pans() {
                grab(v, e, w, cx);
            }
        }),
    )
    .on_mouse_down(MouseButton::Middle, cx.listener(grab))
    .on_mouse_move(cx.listener(move |v: &mut V, e: &MouseMoveEvent, _, cx| {
        let Some(last) = v.nav_mut().grab else {
            return;
        };
        let held = match e.pressed_button {
            Some(MouseButton::Left) => v.left_pans(),
            Some(MouseButton::Middle) => true,
            _ => false,
        };
        let nav = v.nav_mut();
        if !held {
            nav.grab = None;
        } else if let Some(painted) = panning.get() {
            nav.zoom = nav
                .zoom
                .panned(painted.pane, canvas_size, e.position - last);
            nav.grab = Some(e.position);
        }
        cx.notify();
    }))
    .on_mouse_up(MouseButton::Left, cx.listener(drop))
    .on_mouse_up(MouseButton::Middle, cx.listener(drop))
    .on_mouse_up_out(MouseButton::Left, cx.listener(drop))
    .on_mouse_up_out(MouseButton::Middle, cx.listener(drop))
}

/// The checkerboard under the canvas (squares in view only), telling
/// transparent pixels from the pane.
fn checkerboard(
    canvas_size: (u32, u32),
    zoom: Zoom,
    (light, dark): (Hsla, Hsla),
    placed: Placed,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            let rect = place(bounds, canvas_size, zoom);
            placed.set(Some(Painted {
                pane: bounds,
                canvas: rect,
            }));
            let seen = rect.intersect(&window.content_mask().bounds);
            if f32::from(seen.size.width) <= 0.0 || f32::from(seen.size.height) <= 0.0 {
                return;
            }
            window.paint_quad(fill(seen, light));
            let first = |from: Pixels, to: Pixels| ((f32::from(to - from)) / CHECKER).floor();
            let (col0, row0) = (
                first(rect.left(), seen.left()),
                first(rect.top(), seen.top()),
            );
            let mut row = row0;
            loop {
                let y = f32::from(rect.top()) + row * CHECKER;
                if y >= f32::from(seen.bottom()) {
                    break;
                }
                let mut col = col0 + (row + col0).rem_euclid(2.0);
                loop {
                    let x = f32::from(rect.left()) + col * CHECKER;
                    if x >= f32::from(seen.right()) {
                        break;
                    }
                    let square = Bounds::new(point(px(x), px(y)), size(px(CHECKER), px(CHECKER)));
                    window.paint_quad(fill(square.intersect(&seen), dark));
                    col += 2.0;
                }
                row += 1.0;
            }
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// One layer: its image at the canvas's top-left, at the canvas scale,
/// clipped to its side of the split.
fn paint_layer(layer: Layer, canvas_size: (u32, u32), zoom: Zoom) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            let rect = place(bounds, canvas_size, zoom);
            let scale = f32::from(rect.size.width) / side(canvas_size.0.max(1));
            let picture = &layer.picture;
            let image = Bounds::new(
                rect.origin,
                size(
                    px(side(picture.width) * scale),
                    px(side(picture.height) * scale),
                ),
            );
            let split = |f: f32| rect.origin.x + rect.size.width * f;
            let shown = match layer.clip {
                Clip::Whole => image,
                Clip::Before(f) => Bounds::from_corners(
                    image.origin,
                    point(split(f).min(image.right()), image.bottom()),
                ),
                Clip::After(f) => Bounds::from_corners(
                    point(split(f).max(image.left()), image.top()),
                    image.bottom_right(),
                ),
            };
            if f32::from(shown.size.width) > 0.0 {
                let _ = window.paint_image(
                    shown,
                    image,
                    Corners::default(),
                    picture.render.clone(),
                    0,
                    false,
                );
            }
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// The swipe's split line, at fraction `at` of the canvas width.
pub(crate) fn split_line(canvas_size: (u32, u32), zoom: Zoom, at: f32, cx: &App) -> AnyElement {
    let line = color(cx, T::Accent);
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            let rect = place(bounds, canvas_size, zoom);
            let x = rect.origin.x + rect.size.width * at;
            window.paint_quad(fill(
                Bounds::new(
                    point(x - px(1.0), rect.top()),
                    size(px(2.0), rect.size.height),
                ),
                line,
            ));
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    fn at(scale: f32, center: (f32, f32)) -> Zoom {
        Zoom {
            scale: Some(scale),
            center,
        }
    }

    /// The canvas point under window point `p`, with the canvas at `rect`
    /// and `scale`.
    fn under(rect: Bounds<Pixels>, scale: f32, p: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(p.x - rect.origin.x) / scale,
            f32::from(p.y - rect.origin.y) / scale,
        )
    }

    #[test]
    fn a_fit_canvas_is_scaled_and_centered_within_a_margin() {
        let placed = place(bounds(10.0, 0.0, 416.0, 116.0), (200, 100), Zoom::FIT);
        assert_eq!(placed, bounds(118.0, 8.0, 200.0, 100.0));
        let placed = place(bounds(0.0, 0.0, 116.0, 116.0), (400, 200), Zoom::FIT);
        assert_eq!(placed, bounds(8.0, 33.0, 100.0, 50.0));
    }

    #[test]
    fn a_canvas_smaller_than_its_pane_is_centered() {
        let placed = place(
            bounds(0.0, 0.0, 300.0, 300.0),
            (100, 50),
            at(1.0, (0.0, 0.0)),
        );
        assert_eq!(placed, bounds(100.0, 125.0, 100.0, 50.0));
    }

    #[test]
    fn a_larger_canvas_shows_its_center_but_still_covers_the_pane() {
        let pane = bounds(0.0, 0.0, 100.0, 100.0);
        let placed = place(pane, (400, 400), at(1.0, (200.0, 100.0)));
        assert_eq!(placed, bounds(-150.0, -50.0, 400.0, 400.0));
        let placed = place(pane, (400, 400), at(1.0, (-50.0, 900.0)));
        assert_eq!(placed, bounds(0.0, -300.0, 400.0, 400.0));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let pane = bounds(0.0, 0.0, 200.0, 200.0);
        let pointer = point(px(150.0), px(80.0));
        let before = under(place(pane, (100, 100), Zoom::FIT), 1.84, pointer);
        let zoom = Zoom::FIT.zoomed_at(pane, (100, 100), pointer, 2.0);
        assert_eq!(zoom.scale, Some(3.68));
        let after = under(place(pane, (100, 100), zoom), 3.68, pointer);
        assert!((before.0 - after.0).abs() < 1e-3, "{before:?} {after:?}");
        assert!((before.1 - after.1).abs() < 1e-3, "{before:?} {after:?}");
    }

    #[test]
    fn zooming_out_past_the_fit_lands_on_it_and_stops_at_100_percent() {
        let pane = bounds(0.0, 0.0, 216.0, 216.0);
        let mid = pane.center();
        // Fitted at 2×: zooming out lands on the fit, then goes to 100 %.
        let zoom = at(3.0, (50.0, 50.0)).zoomed_at(pane, (100, 100), mid, 0.5);
        assert_eq!(zoom.scale, None);
        let zoom = zoom.zoomed_at(pane, (100, 100), mid, 0.1);
        assert_eq!(zoom.scale, Some(1.0));
        // A large canvas, fitted at ½×, stops at the fit.
        let zoom = at(1.0, (0.0, 0.0)).zoomed_at(pane, (400, 400), mid, 0.1);
        assert_eq!(zoom.scale, None);
    }

    #[test]
    fn panning_stops_at_the_canvas_edges() {
        let pane = bounds(0.0, 0.0, 100.0, 100.0);
        let zoom = at(2.0, (50.0, 50.0)).panned(pane, (100, 100), point(px(20.0), px(1000.0)));
        assert_eq!(zoom.center, (40.0, 25.0));
        assert_eq!(
            place(pane, (100, 100), zoom),
            bounds(-30.0, 0.0, 200.0, 200.0)
        );
    }

    #[test]
    fn the_split_follows_the_pointer_within_the_canvas() {
        let placed: Placed = Rc::new(Cell::new(Some(Painted {
            pane: bounds(0.0, 0.0, 400.0, 50.0),
            canvas: bounds(100.0, 0.0, 200.0, 50.0),
        })));
        assert_eq!(fraction_at(&placed, px(150.0)), Some(0.25));
        assert_eq!(fraction_at(&placed, px(0.0)), Some(0.0));
        assert_eq!(fraction_at(&placed, px(900.0)), Some(1.0));
        assert_eq!(fraction_at(&Rc::new(Cell::new(None)), px(1.0)), None);
    }
}
