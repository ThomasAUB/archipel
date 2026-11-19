//! Rendering tests of images: decoded, then painted offscreen and checked
//! pixel by pixel.

use std::io::Cursor;
use std::sync::Arc;

use archipel_model::RepoPath;
use archipel_model::changes::ChangeKind;
use archipel_model::diff::FileDiff;
use archipel_model::ports::DiffSide;
use archipel_model::testing::{FakeGit, FakeRepo, InMemoryStore};
use archipel_ops::{GitPorts, Journal, RepoActions};

use archipel_theme::ColorToken as T;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Context, HeadlessAppContext, ImageFormat, IntoElement,
    ParentElement as _, Render, Styled as _, Window, WindowBounds, WindowOptions, div, px, size,
};
use image::RgbaImage;

use super::compare::{ImageDiff, sides};
use super::stage::{Layer, Zoom, stage};
use super::viewer::ImageViewer;
use super::{Picture, decode};
use crate::diff_view::render_tests::context;
use crate::theme::color;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// A PNG of `w × h` pixels, the left half `left`, the rest `right`.
fn png(w: u32, h: u32, left: [u8; 4], right: [u8; 4]) -> Vec<u8> {
    let image = RgbaImage::from_fn(w, h, |x, _| {
        image::Rgba(if x < w / 2 { left } else { right })
    });
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

/// Layers on a stage, filling the window.
struct Stage {
    canvas: (u32, u32),
    layers: Vec<(Picture, f32)>,
}

impl Render for Stage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layers = self
            .layers
            .iter()
            .map(|(p, opacity)| Layer {
                opacity: *opacity,
                ..Layer::whole(p)
            })
            .collect();
        div()
            .size_full()
            .flex()
            .bg(color(cx, T::BgApp))
            .child(stage("test", self.canvas, layers, Zoom::Fit, None, cx))
    }
}

/// Decodes `bytes` with the app's SVG renderer.
fn picture(cx: &mut HeadlessAppContext, format: ImageFormat, bytes: &[u8]) -> Picture {
    let svg = cx.update(|cx| cx.svg_renderer());
    decode(format, bytes, &svg).unwrap()
}

/// The frame of a 200 × 100 window showing `view`, with its scale factor.
fn shoot(cx: &mut HeadlessAppContext, view: Stage) -> (RgbaImage, f32) {
    shoot_with(cx, (200.0, 100.0), move |_| view)
}

/// The frame of a `w × h` window showing the view `build` makes, with its
/// scale factor; saved under `ARCHIPEL_SHOTS` when set.
fn shoot_with<V: Render>(
    cx: &mut HeadlessAppContext,
    (w, h): (f32, f32),
    build: impl FnOnce(&mut Context<V>) -> V + 'static,
) -> (RgbaImage, f32) {
    let handle: AnyWindowHandle = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::Point::default(),
                        size: size(px(w), px(h)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_, cx| cx.new(build),
            )
        })
        .expect("offscreen window")
        .into();
    let scale = cx
        .update_window(handle, |_, window, _| window.scale_factor())
        .unwrap();
    let image = cx
        .capture_screenshot(handle)
        .expect("offscreen rendering must be available");
    save(&image);
    (image, scale)
}

/// Saves `image` under `ARCHIPEL_SHOTS` when set.
fn save(image: &RgbaImage) {
    if let Ok(dir) = std::env::var("ARCHIPEL_SHOTS") {
        static SHOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SHOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        image.save(format!("{dir}/image-{n:02}.png")).unwrap();
    }
}

/// The pixel at logical `(x, y)`.
fn at(shot: &(RgbaImage, f32), x: f32, y: f32) -> [u8; 4] {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let device = |v: f32| (v * shot.1).round() as u32;
    shot.0.get_pixel(device(x), device(y)).0
}

/// Rendering rounds and blends colors: a pixel matches within a tolerance.
fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
}

#[test]
fn a_png_fills_its_fitted_canvas() {
    let mut cx = context();
    let p = picture(&mut cx, ImageFormat::Png, &png(4, 2, RED, BLUE));
    assert_eq!((p.width, p.height), (4, 2));
    let shot = shoot(
        &mut cx,
        Stage {
            canvas: (4, 2),
            layers: vec![(p, 1.0)],
        },
    );
    assert!(
        near(at(&shot, 40.0, 50.0), RED),
        "{:?}",
        at(&shot, 40.0, 50.0)
    );
    assert!(near(at(&shot, 160.0, 50.0), BLUE));
}

#[test]
fn an_svg_is_rasterized_at_its_own_size() {
    let mut cx = context();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
        <rect width="40" height="20" fill="lime"/></svg>"#;
    let p = picture(&mut cx, ImageFormat::Svg, svg);
    assert_eq!((p.width, p.height), (40, 20));
    let shot = shoot(
        &mut cx,
        Stage {
            canvas: (40, 20),
            layers: vec![(p, 1.0)],
        },
    );
    assert!(near(at(&shot, 100.0, 50.0), [0, 255, 0, 255]));
}

#[test]
fn a_smaller_image_sits_top_left_over_the_checkerboard() {
    let mut cx = context();
    let p = picture(&mut cx, ImageFormat::Png, &png(2, 2, RED, RED));
    let shot = shoot(
        &mut cx,
        Stage {
            canvas: (4, 2),
            layers: vec![(p, 1.0)],
        },
    );
    assert!(near(at(&shot, 50.0, 50.0), RED));
    assert!(!near(at(&shot, 150.0, 50.0), RED));
}

#[test]
fn a_translucent_layer_blends_over_the_one_below() {
    let mut cx = context();
    let below = picture(&mut cx, ImageFormat::Png, &png(2, 2, RED, RED));
    let above = picture(&mut cx, ImageFormat::Png, &png(2, 2, BLUE, BLUE));
    let shot = shoot(
        &mut cx,
        Stage {
            canvas: (2, 2),
            layers: vec![(below, 1.0), (above, 0.5)],
        },
    );
    let [r, g, b, _] = at(&shot, 100.0, 50.0);
    assert!(
        r.abs_diff(128) < 20 && g < 10 && b.abs_diff(128) < 20,
        "{r} {g} {b}"
    );
}

#[test]
fn the_viewer_shows_a_transparent_image_over_the_checkerboard() {
    let mut cx = context();
    let p = picture(&mut cx, ImageFormat::Png, &png(64, 32, RED, [0, 0, 0, 0]));
    let shot = shoot_with(&mut cx, (400.0, 300.0), move |cx| ImageViewer::new(p, cx));
    // The transparent half shows two checkerboard colors.
    let (light, dark) = cx.update(|cx| (color(cx, T::BgPanel), color(cx, T::BgHover)));
    assert_ne!(light, dark);
    let row: Vec<[u8; 4]> = (300..390u16)
        .map(|x| at(&shot, f32::from(x), 150.0))
        .collect();
    assert!(row.iter().any(|p| *p != row[0]), "a checkerboard shows");
    assert!(near(at(&shot, 100.0, 150.0), RED));
}

/// Repo actions on a fake repo holding `index` and `worktree` versions of
/// `a.png`.
fn fake_actions(index: Vec<u8>, worktree: Vec<u8>) -> (archipel_ops::RepoActions, RepoPath) {
    let git = Arc::new(FakeGit::new());
    let repo = RepoPath::new("/fake/repo");
    let mut state = FakeRepo::default();
    state.index.insert("a.png".into(), index);
    state.files.insert("a.png".into(), worktree);
    git.insert(&repo, state);
    let journal = Journal::new(Arc::new(InMemoryStore::new()), git.clone(), git.clone(), 10);
    (RepoActions::new(GitPorts::from_backend(git), journal), repo)
}

#[test]
fn an_image_diff_shows_both_versions_side_by_side() {
    let mut cx = context();
    let (actions, repo) = fake_actions(png(2, 2, RED, RED), png(2, 2, BLUE, BLUE));
    let file = FileDiff {
        old_path: Some("a.png".into()),
        new_path: Some("a.png".into()),
        kind: ChangeKind::Modified,
        binary: true,
        hunks: Vec::new(),
        truncated: false,
    };
    let sides = sides(&file, Some(DiffSide::Unstaged), None, false).unwrap();
    let handle: AnyWindowHandle = cx
        .update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds {
                        origin: gpui_kit::Point::default(),
                        size: size(px(400.), px(200.)),
                    })),
                    focus: false,
                    show: false,
                    ..Default::default()
                },
                |_, cx| cx.new(|cx| ImageDiff::new(actions, repo, sides, 0, cx)),
            )
        })
        .unwrap()
        .into();
    cx.run_until_parked();
    let scale = cx
        .update_window(handle, |_, window, _| window.scale_factor())
        .unwrap();
    let image = cx.capture_screenshot(handle).unwrap();
    save(&image);
    let shot = (image, scale);
    assert!(
        near(at(&shot, 100.0, 120.0), RED),
        "{:?}",
        at(&shot, 100.0, 120.0)
    );
    assert!(near(at(&shot, 300.0, 120.0), BLUE));
}
