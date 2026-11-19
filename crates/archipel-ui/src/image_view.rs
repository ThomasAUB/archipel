//! Images, in the file browser and in diffs: which files are images, their
//! decoding off the UI thread into textures, and the pixel difference of
//! two of them. [`stage`] paints them, [`viewer`] shows one.

pub(crate) mod compare;
pub(crate) mod stage;
pub(crate) mod viewer;

use std::io::Cursor;
use std::sync::Arc;

use gpui_kit::{App, ImageFormat, RenderImage, SMOOTH_SVG_SCALE_FACTOR, SvgRenderer};
use image::{DynamicImage, Frame, ImageDecoder as _, ImageReader, RgbaImage};

/// Image files above this size are not decoded.
pub(crate) const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// The image format of `path`, after its extension (any case).
pub(crate) fn image_format(path: &str) -> Option<ImageFormat> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "webp" => ImageFormat::Webp,
        "gif" => ImageFormat::Gif,
        "svg" => ImageFormat::Svg,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        "pbm" | "pgm" | "ppm" | "pnm" => ImageFormat::Pnm,
        _ => return None,
    })
}

/// The name shown for `format`.
pub(crate) fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Webp => "WebP",
        ImageFormat::Gif => "GIF",
        ImageFormat::Svg => "SVG",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Ico => "ICO",
        ImageFormat::Pnm => "PNM",
    }
}

/// A decoded image, ready to paint.
#[derive(Clone)]
pub(crate) struct Picture {
    /// Its pixels, BGRA, as the renderer wants them.
    pub(crate) render: Arc<RenderImage>,
    /// Width, in logical pixels (shown at 100 %).
    pub(crate) width: u32,
    /// Height, in logical pixels.
    pub(crate) height: u32,
    /// Texture pixels per logical pixel: a small image is enlarged, an
    /// SVG rasterized finer.
    pub(crate) density: u32,
    pub(crate) format: ImageFormat,
    /// Size of the file.
    pub(crate) bytes: usize,
}

impl Picture {
    /// Its texture, as pixels to sample.
    fn sampled(&self) -> Option<Sampled<'_>> {
        let size = self.render.size(0);
        Some(Sampled {
            pixels: self.render.as_bytes(0)?,
            texture: (
                u32::try_from(size.width.0).ok()?,
                u32::try_from(size.height.0).ok()?,
            ),
            density: self.density.max(1),
            size: (self.width, self.height),
        })
    }

    /// `W × H · FORMAT · N KB`.
    pub(crate) fn info(&self) -> String {
        format!(
            "{} × {} · {} · {}",
            self.width,
            self.height,
            format_name(self.format),
            size_label(self.bytes)
        )
    }
}

/// A file size, in KB or MB.
pub(crate) fn size_label(bytes: usize) -> String {
    let kb = bytes.div_ceil(1024);
    if kb < 1024 {
        format!("{kb} KB")
    } else {
        format!("{}.{} MB", kb / 1024, (kb % 1024) * 10 / 1024)
    }
}

/// Decodes `bytes` of `format` (off the UI thread): raster formats with
/// the `image` crate (first frame of an animation), SVG with `svg`.
pub(crate) fn decode(
    format: ImageFormat,
    bytes: &[u8],
    svg: &SvgRenderer,
) -> Result<Picture, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("Too large to show ({})", size_label(bytes.len())));
    }
    let (render, width, height, density) = if format == ImageFormat::Svg {
        rasterize(bytes, svg)?
    } else {
        let rgba = decode_raster(format, bytes)?;
        let (width, height) = rgba.dimensions();
        let density = upscale(width, height);
        let pixels = to_bgra(nearest(&rgba, density));
        (texture(pixels), width, height, density)
    };
    Ok(Picture {
        render,
        width,
        height,
        density,
        format,
        bytes: bytes.len(),
    })
}

/// Texture side a small image is enlarged to at least: the renderer
/// smooths a texture it enlarges, which blurs a small image's pixels.
const MIN_TEXTURE: u32 = 512;

/// The whole factor enlarging a `width × height` image's texture to
/// [`MIN_TEXTURE`] (1 for a large image).
fn upscale(width: u32, height: u32) -> u32 {
    MIN_TEXTURE.div_ceil(width.max(height).max(1)).clamp(1, 64)
}

/// `pixels` enlarged `factor` times, each pixel a square.
fn nearest(pixels: &RgbaImage, factor: u32) -> RgbaImage {
    if factor <= 1 {
        return pixels.clone();
    }
    let (w, h) = pixels.dimensions();
    RgbaImage::from_fn(w * factor, h * factor, |x, y| {
        *pixels.get_pixel(x / factor, y / factor)
    })
}

/// An SVG's texture, its logical size and density: rendered finer when
/// small.
fn rasterize(bytes: &[u8], svg: &SvgRenderer) -> Result<(Arc<RenderImage>, u32, u32, u32), String> {
    let smooth = svg_density();
    let render = |scale: f32| {
        let render = svg
            .render_single_frame(bytes, scale)
            .map_err(|e| e.to_string())?;
        let size = render.size(0);
        let px = |n: i32| u32::try_from(n).unwrap_or(0);
        Ok::<_, String>((px(size.width.0), px(size.height.0), render))
    };
    let (w, h, image) = render(1.0)?;
    let (width, height) = (w.div_ceil(smooth), h.div_ceil(smooth));
    let factor = upscale(width, height).min(32);
    if factor == 1 {
        return Ok((image, width, height, smooth));
    }
    let scale = u16::try_from(factor).map_or(1.0, f32::from);
    let (_, _, image) = render(scale)?;
    Ok((image, width, height, smooth * factor))
}

/// Texture pixels per logical pixel of an SVG rendered at scale 1.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a small positive whole factor"
)]
fn svg_density() -> u32 {
    SMOOTH_SVG_SCALE_FACTOR.round().max(1.0) as u32
}

/// The RGBA pixels of a raster image, turned upright after its EXIF
/// orientation.
fn decode_raster(format: ImageFormat, bytes: &[u8]) -> Result<RgbaImage, String> {
    let format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        ImageFormat::Ico => image::ImageFormat::Ico,
        ImageFormat::Pnm => image::ImageFormat::Pnm,
        ImageFormat::Svg => return Err("SVG is not a raster format".into()),
    };
    let mut decoder = ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    Ok(image.into_rgba8())
}

/// RGBA pixels as BGRA (the two swap back the same way).
fn to_bgra(mut pixels: RgbaImage) -> RgbaImage {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    pixels
}

/// A texture of BGRA `pixels`.
fn texture(pixels: RgbaImage) -> Arc<RenderImage> {
    Arc::new(RenderImage::new(vec![Frame::new(pixels)]))
}

/// Frees the GPU copy of `render` once the current update is over (the
/// window being drawn is out of reach while it runs).
pub(crate) fn release(render: Arc<RenderImage>, cx: &mut App) {
    cx.defer(move |cx| cx.drop_image(render, None));
}

// Needs a headless renderer, which gpui only has on Linux and macOS.
/// The pixel difference of two images: `|a − b|` per channel, on the
/// canvas holding both (aligned top-left; a pixel only one of them covers
/// differs from nothing), opaque.
pub(crate) struct Difference {
    pub(crate) picture: Picture,
    /// Pixels differing.
    pub(crate) changed: u64,
    /// Pixels of the canvas.
    pub(crate) total: u64,
}

impl Difference {
    /// `N px changed (x %)`.
    pub(crate) fn summary(&self) -> String {
        if self.changed == 0 {
            return "no pixel changed".to_owned();
        }
        let per_mille = (self.changed * 1000).div_ceil(self.total.max(1));
        format!(
            "{} px changed ({}.{} %)",
            self.changed,
            per_mille / 10,
            per_mille % 10
        )
    }
}

/// The difference of `a` and `b`, pixel by logical pixel (off the UI
/// thread), amplified ×4 with `amplify` so faint changes show.
pub(crate) fn difference(a: &Picture, b: &Picture, amplify: bool) -> Option<Difference> {
    let (pixels, changed) = diff_pixels(&a.sampled()?, &b.sampled()?, amplify);
    let (width, height) = pixels.dimensions();
    let density = upscale(width, height);
    Some(Difference {
        picture: Picture {
            render: texture(nearest(&pixels, density)),
            width,
            height,
            density,
            format: b.format,
            bytes: 0,
        },
        changed,
        total: u64::from(width) * u64::from(height),
    })
}

/// An image's texture, read pixel by logical pixel.
struct Sampled<'a> {
    /// 4 bytes a pixel.
    pixels: &'a [u8],
    /// Texture width and height.
    texture: (u32, u32),
    /// Texture pixels per logical pixel.
    density: u32,
    /// Logical width and height.
    size: (u32, u32),
}

impl Sampled<'_> {
    /// Logical pixel `(x, y)`, transparent outside the image; a fully
    /// transparent pixel reads as transparent black, whatever its color.
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.size.0 || y >= self.size.1 {
            return [0; 4];
        }
        let (tx, ty) = (x * self.density, y * self.density);
        if tx >= self.texture.0 || ty >= self.texture.1 {
            return [0; 4];
        }
        let at = (ty as usize * self.texture.0 as usize + tx as usize) * 4;
        match self.pixels.get(at..at + 4) {
            Some(&[_, _, _, 0]) | None => [0; 4],
            Some(p) => [p[0], p[1], p[2], p[3]],
        }
    }
}

/// `|a − b|` of two images (same channel order), on the canvas holding
/// both, with the count of pixels differing. A color channel shows the
/// larger of its own and the alpha difference.
fn diff_pixels(a: &Sampled, b: &Sampled, amplify: bool) -> (RgbaImage, u64) {
    let width = a.size.0.max(b.size.0);
    let height = a.size.1.max(b.size.1);
    let gain = if amplify { 4 } else { 1 };
    let mut changed = 0;
    let out = RgbaImage::from_fn(width, height, |x, y| {
        let (before, after) = (a.at(x, y), b.at(x, y));
        let alpha = before[3].abs_diff(after[3]);
        if before != after {
            changed += 1;
        }
        let channel = |i: usize| before[i].abs_diff(after[i]).max(alpha).saturating_mul(gain);
        image::Rgba([channel(0), channel(1), channel(2), u8::MAX])
    });
    (out, changed)
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod render_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_follow_the_extension() {
        assert_eq!(image_format("a/b/logo.PNG"), Some(ImageFormat::Png));
        assert_eq!(image_format("photo.jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format("photo.Jpg"), Some(ImageFormat::Jpeg));
        assert_eq!(image_format("icon.svg"), Some(ImageFormat::Svg));
        assert_eq!(image_format("scan.tif"), Some(ImageFormat::Tiff));
        assert_eq!(image_format("a.ppm"), Some(ImageFormat::Pnm));
        assert_eq!(image_format("main.rs"), None);
        assert_eq!(image_format("png"), None);
        assert_eq!(image_format("dir.png/readme"), None);
    }

    #[test]
    fn pixels_turn_bgra() {
        let rgba = RgbaImage::from_raw(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(to_bgra(rgba).into_raw(), [3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn a_png_decodes_upright_with_its_size() {
        let mut png = Vec::new();
        RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let rgba = decode_raster(ImageFormat::Png, &png).unwrap();
        assert_eq!(rgba.dimensions(), (3, 2));
        assert_eq!(rgba.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert!(decode_raster(ImageFormat::Png, b"not a png").is_err());
    }

    #[test]
    fn small_images_are_enlarged_in_whole_squares() {
        assert_eq!(upscale(2000, 10), 1);
        assert_eq!(upscale(100, 50), 6);
        assert_eq!(upscale(1, 1), 64);
        let small = RgbaImage::from_raw(2, 1, vec![1, 1, 1, 1, 2, 2, 2, 2]).unwrap();
        let big = nearest(&small, 3);
        assert_eq!(big.dimensions(), (6, 3));
        assert_eq!(big.get_pixel(2, 2).0, [1; 4]);
        assert_eq!(big.get_pixel(3, 0).0, [2; 4]);
    }

    /// A `w × h` image of `fill`, at density 1.
    fn sampled(pixels: &[u8], w: u32, h: u32) -> Sampled<'_> {
        Sampled {
            pixels,
            texture: (w, h),
            density: 1,
            size: (w, h),
        }
    }

    fn image(w: u32, h: u32, fill: [u8; 4]) -> Vec<u8> {
        fill.repeat((w * h) as usize)
    }

    #[test]
    fn identical_images_do_not_differ() {
        let a = image(4, 3, [1, 2, 3, 255]);
        let (out, changed) = diff_pixels(&sampled(&a, 4, 3), &sampled(&a, 4, 3), false);
        assert_eq!(changed, 0);
        assert_eq!(out.dimensions(), (4, 3));
        assert!(out.pixels().all(|p| p.0 == [0, 0, 0, 255]));
    }

    #[test]
    fn one_pixel_differs() {
        let a = image(4, 3, [1, 2, 3, 255]);
        let mut b = a.clone();
        b[(4 + 2) * 4] = 11; // pixel (2, 1), first channel
        let (a, b) = (sampled(&a, 4, 3), sampled(&b, 4, 3));
        let (out, changed) = diff_pixels(&a, &b, false);
        assert_eq!(changed, 1);
        assert_eq!(out.get_pixel(2, 1).0, [10, 0, 0, 255]);
        let (out, _) = diff_pixels(&a, &b, true);
        assert_eq!(out.get_pixel(2, 1).0, [40, 0, 0, 255]);
    }

    #[test]
    fn sizes_differ_on_the_canvas_holding_both() {
        let a = image(2, 2, [9, 9, 9, 255]);
        let b = image(3, 1, [9, 9, 9, 255]);
        let (out, changed) = diff_pixels(&sampled(&a, 2, 2), &sampled(&b, 3, 1), false);
        assert_eq!(out.dimensions(), (3, 2));
        // (2, 0) is only in b, (0, 1) and (1, 1) only in a; (2, 1) in none.
        assert_eq!(changed, 3);
        assert_eq!(out.get_pixel(2, 1).0, [0, 0, 0, 255]);
    }

    #[test]
    fn enlarged_textures_are_read_per_logical_pixel() {
        let small = RgbaImage::from_raw(2, 1, vec![1, 1, 1, 255, 2, 2, 2, 255]).unwrap();
        let big = nearest(&small, 4);
        let enlarged = Sampled {
            pixels: big.as_raw(),
            texture: (8, 4),
            density: 4,
            size: (2, 1),
        };
        let (_, changed) = diff_pixels(&enlarged, &sampled(small.as_raw(), 2, 1), false);
        assert_eq!(changed, 0);
    }

    #[test]
    fn transparent_pixels_match_whatever_their_color() {
        let a = image(1, 1, [5, 5, 5, 0]);
        let b = image(1, 1, [7, 7, 7, 0]);
        assert_eq!(
            diff_pixels(&sampled(&a, 1, 1), &sampled(&b, 1, 1), false).1,
            0
        );
    }

    #[test]
    fn the_summary_counts_changed_pixels() {
        let p = |changed, total| {
            Difference {
                picture: Picture {
                    render: texture(RgbaImage::new(1, 1)),
                    width: 1,
                    height: 1,
                    density: 1,
                    format: ImageFormat::Png,
                    bytes: 0,
                },
                changed,
                total,
            }
            .summary()
        };
        assert_eq!(p(0, 100), "no pixel changed");
        assert_eq!(p(1, 3), "1 px changed (33.4 %)");
        assert_eq!(p(50, 100), "50 px changed (50.0 %)");
    }

    #[test]
    fn sizes_read_in_kb_or_mb() {
        assert_eq!(size_label(1), "1 KB");
        assert_eq!(size_label(1536 * 1024), "1.5 MB");
    }
}
