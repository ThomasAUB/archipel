//! Rendering tests of the steady widgets: a change of label must not move
//! what comes after.

use archipel_theme::ColorToken as T;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, IntoElement, ParentElement as _, Render, Rgba,
    Styled as _, Window, WindowBounds, WindowOptions, div, px, size,
};

use crate::diff_view::render_tests::context;
use crate::theme::color;

/// The marker's color: none of a button's.
const MARKER: T = T::StatusError;

/// Which button the row shows, with which label.
#[derive(Clone, Copy)]
enum Kind {
    Plain(&'static str),
    Steady(&'static str),
}

/// A button, then a marker right after it.
struct Row(Kind);

impl Render for Row {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let button = match self.0 {
            Kind::Plain(label) => super::button("b", label, cx),
            Kind::Steady(label) => {
                let other = if label == "Merge" {
                    "Confirm merge"
                } else {
                    "Merge"
                };
                super::steady_button("b", label, &[other], cx)
            }
        };
        div()
            .size_full()
            .flex()
            .items_start()
            .bg(color(cx, T::BgApp))
            .child(button)
            .child(div().flex_none().size(px(8.)).bg(color(cx, MARKER)))
    }
}

/// The first column of the marker, the button drawn as `kind`.
fn marker_x(kind: Kind) -> u32 {
    let mut cx = context();
    let handle: AnyWindowHandle = cx
        .update(|cx: &mut App| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::Point::default(),
                        size: size(px(320.), px(40.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Row(kind)),
            )
        })
        .expect("offscreen window")
        .into();
    cx.run_until_parked();
    let image = cx
        .capture_screenshot(handle)
        .expect("offscreen rendering must be available");
    let marker = cx.update(|cx| Rgba::from(color(cx, MARKER)));
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |v: f32| (v * 255.).round() as u8;
    let want = [byte(marker.r), byte(marker.g), byte(marker.b)];
    let is_marker = |p: &image::Rgba<u8>| (0..3).all(|i| p[i].abs_diff(want[i]) <= 8);
    (0..image.width())
        .find(|&x| (0..image.height()).any(|y| is_marker(image.get_pixel(x, y))))
        .expect("the marker is drawn")
}

#[test]
fn steady_button_keeps_what_follows_in_place() {
    // The plain button moves the marker: the test can tell.
    assert_ne!(
        marker_x(Kind::Plain("Merge")),
        marker_x(Kind::Plain("Confirm merge"))
    );
    assert_eq!(
        marker_x(Kind::Steady("Merge")),
        marker_x(Kind::Steady("Confirm merge"))
    );
    // As wide as its widest label, no wider.
    assert_eq!(
        marker_x(Kind::Steady("Merge")),
        marker_x(Kind::Plain("Confirm merge"))
    );
}
