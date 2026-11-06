use super::model::Screenshot;
use base64::Engine;
use std::io::Cursor;
pub const MAX_IMAGES: usize = 3;
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

pub fn prepare(bytes: Vec<u8>) -> Result<Screenshot, String> {
    if bytes.len() > 20 * 1024 * 1024 {
        return Err("image".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "image")?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    encode(reader.decode().map_err(|_| "image")?)
}
pub fn encoded_bytes(screenshot: &Screenshot) -> Result<Vec<u8>, String> {
    if screenshot.data.len() > MAX_BYTES.div_ceil(3) * 4 {
        return Err("image".into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(&screenshot.data)
        .map_err(|_| "image".into())
}
pub fn decode(screenshot: &Screenshot) -> Result<image::DynamicImage, String> {
    if screenshot.data.len() > MAX_BYTES.div_ceil(3) * 4 {
        return Err("image".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&screenshot.data)
        .map_err(|_| "image")?;
    let format = image::guess_format(&bytes).map_err(|_| "image")?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err("image".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(32 * 1024 * 1024);
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    reader.limits(limits);
    reader.decode().map_err(|_| "image".into())
}

pub fn encode(image: image::DynamicImage) -> Result<Screenshot, String> {
    // Clipboard images often have an alpha channel even when every pixel is opaque.
    let transparent = match &image {
        image::DynamicImage::ImageLumaA8(pixels) => pixels.pixels().any(|p| p[1] < 255),
        image::DynamicImage::ImageRgba8(pixels) => pixels.pixels().any(|p| p[3] < 255),
        image::DynamicImage::ImageLumaA16(pixels) => pixels.pixels().any(|p| p[1] < 65535),
        image::DynamicImage::ImageRgba16(pixels) => pixels.pixels().any(|p| p[3] < 65535),
        image::DynamicImage::ImageRgba32F(pixels) => pixels.pixels().any(|p| p[3] != 1.0),
        _ => image.color().has_alpha(),
    };
    let mut image = image.thumbnail(2048, 2048);
    if !transparent {
        image = image::DynamicImage::ImageRgb8(image.to_rgb8());
    }
    loop {
        let mut bytes = Cursor::new(Vec::new());
        if transparent {
            image.write_to(&mut bytes, image::ImageFormat::Png)
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 92).encode_image(&image)
        }
        .map_err(|_| "image")?;
        let bytes = bytes.into_inner();
        if bytes.len() <= MAX_BYTES {
            return Ok(Screenshot {
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            });
        }
        if image.width().max(image.height()) <= 512 {
            return Err("image".into());
        }
        image = image.thumbnail(
            (image.width() * 3 / 4).max(1),
            (image.height() * 3 / 4).max(1),
        );
    }
}
