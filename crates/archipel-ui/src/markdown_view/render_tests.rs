//! Rendering tests of Markdown files: painted offscreen, Mermaid diagram
//! included. `ARCHIPEL_SHOTS=<dir>` saves the frames as PNGs.

use std::collections::HashMap;

use archipel_theme::ColorToken as T;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, HeadlessAppContext, IntoElement, ParentElement as _,
    Render, Rgba, Styled as _, Window, WindowBounds, WindowOptions, div, px, size,
};
use image::RgbaImage;

use super::MarkdownDoc;
use crate::diff_view::render_tests::context;
use crate::theme::color;

struct Doc(MarkdownDoc);

impl Render for Doc {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.0.prepare(cx);
        div()
            .size_full()
            .flex()
            .bg(color(cx, T::BgApp))
            .text_color(color(cx, T::FgDefault))
            .child(self.0.render(|_, _, _| {}))
    }
}

/// `text` rendered in a 640 × 480 window, once laid out.
fn shoot(name: &str, text: &str) -> (HeadlessAppContext, RgbaImage) {
    let mut cx = context();
    // As the app does: code blocks take the theme's syntax colors.
    cx.update(crate::theme::sync_toolkit_theme);
    let text = text.to_owned();
    let handle: AnyWindowHandle = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::Point::default(),
                        size: size(px(640.), px(480.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| Doc(MarkdownDoc::new(text, HashMap::new(), cx))),
            )
        })
        .expect("offscreen window")
        .into();
    // The first frame sets the extensions, which parses the text again.
    let mut image = None;
    for _ in 0..4 {
        cx.run_until_parked();
        image = Some(
            cx.capture_screenshot(handle)
                .expect("offscreen rendering must be available"),
        );
    }
    let image = image.unwrap();
    if let Ok(dir) = std::env::var("ARCHIPEL_SHOTS") {
        image.save(format!("{dir}/markdown-{name}.png")).unwrap();
    }
    (cx, image)
}

/// Pixels of rows `rows` (as a share of the height) far from `bg`.
fn ink(image: &RgbaImage, rows: std::ops::Range<f32>, bg: Rgba) -> usize {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |v: f32| (v * 255.).round() as u8;
    let bg = [byte(bg.r), byte(bg.g), byte(bg.b)];
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    let row = |share: f32| (share * image.height() as f32) as u32;
    image
        .enumerate_pixels()
        .filter(|(_, y, _)| (row(rows.start)..row(rows.end)).contains(y))
        .filter(|(_, _, p)| {
            p.0.iter()
                .zip(bg)
                .map(|(a, b)| u32::from(a.abs_diff(b)))
                .sum::<u32>()
                > 90
        })
        .count()
}

#[test]
fn a_mermaid_block_shows_as_its_diagram() {
    let text = "# Title\n\nSome *text*.\n\n```mermaid\nflowchart LR\n  A[Start] --> B[Middle] --> C[End]\n```\n";
    let (mut cx, image) = shoot("mermaid", text);
    let bg = cx.update(|cx| Rgba::from(color(cx, T::BgApp)));
    // The title and paragraph are at the top; below them, the diagram.
    assert!(ink(&image, 0.0..0.2, bg) > 100, "the title shows");
    assert!(ink(&image, 0.2..0.9, bg) > 300, "the diagram shows");

    let (_, without) = shoot("text-only", "# Title\n\nSome *text*.\n");
    assert!(ink(&without, 0.2..0.9, bg) < 20, "nothing below the text");
}

#[test]
fn a_broken_diagram_shows_its_source() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n\n```mermaid\nflowchart LR\n  A -->\n```\n";
    let (mut cx, image) = shoot("broken", text);
    let bg = cx.update(|cx| Rgba::from(color(cx, T::BgApp)));
    // The table and the code block, then the error and the source.
    assert!(
        ink(&image, 0.45..0.9, bg) > 100,
        "the diagram's source shows"
    );
}
