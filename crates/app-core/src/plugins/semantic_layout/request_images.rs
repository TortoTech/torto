use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::{DynamicImage, Rgba, RgbaImage, codecs::jpeg::JpegEncoder};

pub(super) const MAX_RESOURCE_BYTES: usize = 12 * 1024 * 1024;
pub(super) const MAX_IMAGES: usize = 5;
pub(super) const MAX_BYTES: usize = 6 * 1024 * 1024;
pub(super) const JPEG_PREFIX: &str = "data:image/jpeg;base64,";
pub(super) const REVISION: &str = "jpeg-92-1200-min280-v1";
const LONG_EDGE: u16 = 1200;
const JPEG_QUALITY: u8 = 92;
const PADDING: u32 = 8;
// The encoded canvas must meet the vision endpoint's minimum dimensions.
const MIN_CANVAS_SIDE: u32 = 280;

pub(super) fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    if let Ok(reader) = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()
        && let Ok((width, height)) = reader.into_dimensions()
        && u64::from(width) * u64::from(height) > 16_000_000
    {
        return Err("Image exceeds layout decoding limits".into());
    }
    if let Ok(image) = image::load_from_memory(bytes) {
        return Ok(image);
    }
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .map_err(|e| e.to_string())?;
    let scale = (f32::from(LONG_EDGE) / tree.size().width().max(tree.size().height())).min(1.0);
    let w = (tree.size().width() * scale).ceil().max(1.0) as u32;
    let h = (tree.size().height() * scale).ceil().max(1.0) as u32;
    let mut pixels = resvg::tiny_skia::Pixmap::new(w, h).ok_or("Invalid SVG dimensions")?;
    pixels.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixels.as_mut(),
    );
    Ok(DynamicImage::ImageRgba8(
        RgbaImage::from_raw(w, h, pixels.take()).ok_or("Invalid raster")?,
    ))
}

pub(super) fn image_url(image: DynamicImage) -> Result<String, String> {
    let scale = (f32::from(LONG_EDGE) / image.width().max(image.height()) as f32).min(3.0);
    let size = [
        (image.width() as f32 * scale).round().max(1.0) as u32,
        (image.height() as f32 * scale).round().max(1.0) as u32,
    ];
    let enlarged = rebook_layout::image_processing::resize_exact(
        image,
        size,
        image::imageops::FilterType::Lanczos3,
    );
    let width = (enlarged.width() + 2 * PADDING).max(MIN_CANVAS_SIDE);
    let height = (enlarged.height() + 2 * PADDING).max(MIN_CANVAS_SIDE);
    let mut canvas = RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]));
    let x = (width - enlarged.width()) / 2;
    let y = (height - enlarged.height()) / 2;
    image::imageops::overlay(&mut canvas, &enlarged, i64::from(x), i64::from(y));
    let mut bytes = Vec::new();
    let rgb = DynamicImage::ImageRgba8(canvas).into_rgb8();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|error| error.to_string())?;
    Ok(format!("{JPEG_PREFIX}{}", BASE64.encode(bytes)))
}

pub(super) fn decode_url(url: &str) -> Option<DynamicImage> {
    let bytes = BASE64.decode(url.strip_prefix(JPEG_PREFIX)?).ok()?;
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()
}

pub(super) fn unsupported(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("unsupported content type")
        || ((error.contains("image") || error.contains("vision") || error.contains("text-only"))
            && [
                "not support",
                "doesn't support",
                "unsupported",
                "only supported",
                "not allowed",
                "text-only",
                "only text",
                "not a multimodal",
            ]
            .iter()
            .any(|part| error.contains(part)))
}
