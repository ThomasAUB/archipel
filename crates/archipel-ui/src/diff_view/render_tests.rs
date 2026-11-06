//! Rendering tests: real offscreen frames (WGPU on Linux, Metal on macOS),
//! checked pixel by pixel. They catch what the element tree cannot, such as
//! a marker drawn inside the file's text.
//!
//! `ARCHIPEL_SHOTS=<dir>` saves every captured frame as a PNG.

use std::sync::Arc;

use archipel_model::diff::{DiffLine, LineOrigin};
use archipel_theme::ColorToken as T;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, HeadlessAppContext, IntoElement,
    ParentElement as _, Render, Rgba, Styled as _, Window, WindowBounds, WindowOptions,
    assets::Assets, div, px, size,
};

use super::diff_line_row;
use crate::theme::{UiTheme, color};

/// Gutter digits used by the rows under test.
const LINENO_WIDTH: usize = 2;

fn context() -> HeadlessAppContext {
    let mut cx = HeadlessAppContext::with_platform(
        gpui_kit::platform::current_platform(true).text_system(),
        Arc::new(Assets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.update(|cx: &mut App| {
        gpui_kit::init(cx);
        // No user themes: the built-in default.
        let theme = UiTheme::load(
            std::path::Path::new(""),
            archipel_theme::builtin::DEFAULT_ID,
            cx,
        );
        cx.set_global(theme);
    });
    cx
}

/// Diff lines stacked as in the diff view.
struct Lines(Vec<DiffLine>);

impl Render for Lines {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(color(cx, T::BgApp))
            .text_color(color(cx, T::FgDefault))
            .children(
                self.0
                    .iter()
                    .map(|l| diff_line_row(l, LINENO_WIDTH, false, cx)),
            )
    }
}

fn line(origin: LineOrigin, content: &str) -> DiffLine {
    let (old, new) = match origin {
        LineOrigin::Added => (None, Some(12)),
        LineOrigin::Removed => (Some(12), None),
        LineOrigin::Context => (Some(12), Some(12)),
    };
    DiffLine {
        origin,
        content: content.to_owned(),
        old_lineno: old,
        new_lineno: new,
        no_newline_at_eof: false,
    }
}

/// A rendered frame, with the device pixels of each line row.
struct Shot {
    width: u32,
    /// RGBA, row-major.
    raw: Vec<u8>,
    /// Device-pixel center row of each diff line.
    rows: Vec<u32>,
}

fn render(lines: Vec<DiffLine>) -> (HeadlessAppContext, Shot) {
    let mut cx = context();
    let count = lines.len();
    let handle: AnyWindowHandle = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::Point::default(),
                        size: size(px(320.), px(200.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Lines(lines)),
            )
        })
        .expect("offscreen window")
        .into();
    let scale = cx
        .update_window(handle, |_, window, _| window.scale_factor())
        .unwrap();
    let row_height = cx.update(|cx| f32::from(crate::theme::row_height(cx)));
    let image = cx
        .capture_screenshot(handle)
        .expect("offscreen rendering must be available");
    if let Ok(dir) = std::env::var("ARCHIPEL_SHOTS") {
        static SHOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SHOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        image.save(format!("{dir}/diff-{n:02}.png")).unwrap();
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    let rows = (0..count)
        .map(|i| ((i as f32 + 0.5) * row_height * scale).round() as u32)
        .collect();
    let (width, _) = image.dimensions();
    let raw = image.into_raw();
    (cx, Shot { width, raw, rows })
}

fn token(cx: &mut HeadlessAppContext, token: T) -> [u8; 4] {
    let c = cx.update(|cx| Rgba::from(color(cx, token)));
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |v: f32| (v * 255.).round() as u8;
    [byte(c.r), byte(c.g), byte(c.b), byte(c.a)]
}

/// Rendering rounds colors: a pixel matches a token within a small tolerance.
fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2)
}

impl Shot {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.raw[i],
            self.raw[i + 1],
            self.raw[i + 2],
            self.raw[i + 3],
        ]
    }

    /// Every pixel of line `row`, left to right, across the whole row height
    /// (glyphs are thinner than a row).
    fn columns(&self, row: usize) -> impl Iterator<Item = (u32, Vec<[u8; 4]>)> + '_ {
        let mid = self.rows[row];
        let half = self.rows[0] / 2;
        (0..self.width).map(move |x| {
            let column = (mid - half..mid + half).map(|y| self.pixel(x, y)).collect();
            (x, column)
        })
    }

    /// First device column where line `row`'s background is `bg`: where the
    /// file's text area starts.
    fn text_area_start(&self, row: usize, bg: [u8; 4]) -> u32 {
        self.columns(row)
            .find(|(_, column)| column.iter().all(|p| near(*p, bg)))
            .map(|(x, _)| x)
            .expect("the row has a text area")
    }

    /// First device column right of `from` where line `row` differs from
    /// `bg`: where the file's text starts. `None` when the text area is empty.
    fn ink_start(&self, row: usize, from: u32, bg: [u8; 4]) -> Option<u32> {
        self.columns(row)
            .skip(from as usize)
            .find(|(_, column)| column.iter().any(|p| !near(*p, bg)))
            .map(|(x, _)| x)
    }
}

/// The regression: the +/- marker was drawn on the line's background, where
/// it read as the first character of the file's line.
#[test]
fn markers_sit_in_the_gutter_not_in_the_text() {
    let (mut cx, shot) = render(vec![
        line(LineOrigin::Added, ""),
        line(LineOrigin::Removed, ""),
    ]);
    for (row, bg) in [(0, T::DiffAddBg), (1, T::DiffDelBg)] {
        let bg = token(&mut cx, bg);
        let start = shot.text_area_start(row, bg);
        assert_eq!(
            shot.ink_start(row, start, bg),
            None,
            "an empty line must leave its text area blank (row {row}, from x = {start})"
        );
    }
    // The marker is still drawn, in the gutter.
    let add_fg = token(&mut cx, T::DiffAddFg);
    let gutter_end = shot.text_area_start(0, token(&mut cx, T::DiffAddBg));
    assert!(
        shot.columns(0)
            .take(gutter_end as usize)
            .any(|(_, column)| column.iter().any(|p| near(*p, add_fg))),
        "the + marker must be drawn in the gutter"
    );
}

/// Added, removed and context lines start their text at the same column, so
/// indentation compares across them.
#[test]
fn text_starts_at_the_same_column_for_every_origin() {
    let (mut cx, shot) = render(vec![
        line(LineOrigin::Added, "x"),
        line(LineOrigin::Removed, "x"),
        line(LineOrigin::Context, "x"),
    ]);
    let starts: Vec<_> = [T::DiffAddBg, T::DiffDelBg, T::BgApp]
        .into_iter()
        .enumerate()
        .map(|(row, bg)| {
            let bg = token(&mut cx, bg);
            let from = shot.text_area_start(row, bg);
            shot.ink_start(row, from, bg)
                .expect("the line's text is drawn")
        })
        .collect();
    assert!(
        starts.iter().all(|&x| x == starts[0]),
        "text columns differ: {starts:?}"
    );
}
