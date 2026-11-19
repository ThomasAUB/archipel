//! Painting images: a stage holds a canvas of a given size (the larger of
//! the images compared), scaled to fit its pane or shown at 100 % in a
//! scrolled pane, over a checkerboard showing transparency. Images are
//! layers on it, aligned top-left, each with an opacity and possibly
//! clipped to one side of a split (the swipe).

use std::cell::Cell;
use std::rc::Rc;

use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, Corners, Div, Hsla, Pixels, SharedString, Stateful, Window, canvas,
    div, fill, point, px, size,
};

use super::Picture;
use crate::theme::color;

/// Side of a checkerboard square.
const CHECKER: f32 = 8.0;

/// How the canvas is scaled into its pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Zoom {
    /// Whole, as large as the pane allows.
    #[default]
    Fit,
    /// One image pixel per logical pixel, scrolled.
    Actual,
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

/// Where the canvas was last painted, in window coordinates (read back to
/// place the swipe split under the pointer).
pub(crate) type Placed = Rc<Cell<Option<Bounds<Pixels>>>>;

/// A logical size as `f32`.
#[allow(
    clippy::cast_precision_loss,
    reason = "image sides stay far below 2^24"
)]
fn side(n: u32) -> f32 {
    n as f32
}

/// The canvas of `canvas_size` placed in `bounds`: scaled to fit (`Fit`)
/// or not, centered.
pub(crate) fn place(bounds: Bounds<Pixels>, canvas_size: (u32, u32), zoom: Zoom) -> Bounds<Pixels> {
    let (w, h) = (side(canvas_size.0.max(1)), side(canvas_size.1.max(1)));
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let scale = match zoom {
        Zoom::Fit => (bw / w).min(bh / h).max(0.0),
        Zoom::Actual => 1.0,
    };
    let (sw, sh) = (w * scale, h * scale);
    Bounds::new(
        point(
            bounds.origin.x + px((bw - sw) / 2.0),
            bounds.origin.y + px((bh - sh) / 2.0),
        ),
        size(px(sw), px(sh)),
    )
}

/// The fraction of the canvas width at window x `x`, given where it was
/// painted.
pub(crate) fn fraction_at(placed: &Placed, x: Pixels) -> Option<f32> {
    let rect = placed.get()?;
    let width = f32::from(rect.size.width);
    (width > 0.0).then(|| (f32::from(x - rect.origin.x) / width).clamp(0.0, 1.0))
}

/// A stage of `canvas_size` showing `layers` (bottom first), then
/// `marks` over them, filling its parent; `placed` learns where the canvas
/// is painted. `id` keys the scroll position at 100 %.
pub(crate) fn stage(
    id: impl Into<SharedString>,
    canvas_size: (u32, u32),
    layers: Vec<Layer>,
    zoom: Zoom,
    placed: Option<Placed>,
    marks: Option<AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    let colors = (color(cx, T::BgPanel), color(cx, T::BgHover));
    let mut content = div()
        .relative()
        .child(checkerboard(canvas_size, zoom, colors, placed));
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
    let id = id.into();
    match zoom {
        Zoom::Fit => div()
            .id(id)
            .p_2()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .child(content.size_full()),
        Zoom::Actual => div()
            .id(id)
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_scroll()
            .child(
                content
                    .flex_none()
                    .min_w_full()
                    .min_h_full()
                    .w(px(side(canvas_size.0)))
                    .h(px(side(canvas_size.1))),
            ),
    }
}

/// The checkerboard under the canvas (squares in view only), telling
/// transparent pixels from the pane.
fn checkerboard(
    canvas_size: (u32, u32),
    zoom: Zoom,
    (light, dark): (Hsla, Hsla),
    placed: Option<Placed>,
) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            let rect = place(bounds, canvas_size, zoom);
            if let Some(placed) = &placed {
                placed.set(Some(rect));
            }
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

    #[test]
    fn a_fit_canvas_is_scaled_and_centered() {
        let placed = place(bounds(10.0, 0.0, 400.0, 100.0), (200, 100), Zoom::Fit);
        assert_eq!(placed, bounds(110.0, 0.0, 200.0, 100.0));
        let placed = place(bounds(0.0, 0.0, 100.0, 100.0), (400, 200), Zoom::Fit);
        assert_eq!(placed, bounds(0.0, 25.0, 100.0, 50.0));
    }

    #[test]
    fn an_actual_size_canvas_is_not_scaled() {
        let placed = place(bounds(0.0, 0.0, 300.0, 300.0), (100, 50), Zoom::Actual);
        assert_eq!(placed, bounds(100.0, 125.0, 100.0, 50.0));
    }

    #[test]
    fn the_split_follows_the_pointer_within_the_canvas() {
        let placed: Placed = Rc::new(Cell::new(Some(bounds(100.0, 0.0, 200.0, 50.0))));
        assert_eq!(fraction_at(&placed, px(150.0)), Some(0.25));
        assert_eq!(fraction_at(&placed, px(0.0)), Some(0.0));
        assert_eq!(fraction_at(&placed, px(900.0)), Some(1.0));
        assert_eq!(fraction_at(&Rc::new(Cell::new(None)), px(1.0)), None);
    }
}
