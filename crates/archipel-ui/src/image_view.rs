//! Images, in the file browser and in diffs: which files are images, their
//! decoding off the UI thread into textures, and the pixel difference of
//! two of them. [`stage`] paints them, [`viewer`] shows one.

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
    pub(crate) format: ImageFormat,
    /// Size of the file.
    pub(crate) bytes: usize,
}

impl Picture {
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
    if format == ImageFormat::Svg {
        let render = svg
            .render_single_frame(bytes, 1.0)
            .map_err(|e| e.to_string())?;
        let size = render.size(0);
        let density = svg_density();
        let logical = |n: i32| u32::try_from(n).unwrap_or(0).div_ceil(density);
        return Ok(Picture {
            width: logical(size.width.0),
            height: logical(size.height.0),
            render,
            format,
            bytes: bytes.len(),
        });
    }
    let rgba = decode_raster(format, bytes)?;
    let (width, height) = rgba.dimensions();
    Ok(Picture {
        render: texture(to_bgra(rgba)),
        width,
        height,
        format,
        bytes: bytes.len(),
    })
}

/// Texture pixels per logical pixel of a rasterized SVG.
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
    fn sizes_read_in_kb_or_mb() {
        assert_eq!(size_label(1), "1 KB");
        assert_eq!(size_label(1536 * 1024), "1.5 MB");
    }
}
