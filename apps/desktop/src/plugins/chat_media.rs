use std::sync::atomic::{AtomicU64, Ordering};
pub use rebook_app_core::plugins::chat_media::*;
const THUMBNAIL_MAX_DIMENSION: u32 = 64;
pub trait ChatImagePreview { fn preview_image(&self) -> Result<egui::ColorImage, String>; }
impl ChatImagePreview for ChatImage {
    fn preview_image(&self) -> Result<egui::ColorImage, String> {
        let image = self.preview_rgba()?;
        Ok(egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw()))
    }
}
/// A clipboard image attached to the next turn, together with the small
/// preview the composer shows until the message is sent.
pub struct PendingChatImage {
    id: u64,
    image: ChatImage,
    thumbnail: egui::ColorImage,
    texture: Option<egui::TextureHandle>,
}

impl PendingChatImage {
    /// `ordinal` numbers the attachment in the label handed to the model.
    pub fn from_clipboard(
        ordinal: usize,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<Self, String> {
        let pixels = rgba_image(width, height, rgba)?;
        let thumbnail = thumbnail_color_image(&pixels);
        Ok(Self {
            id: NEXT_PENDING_IMAGE_ID.fetch_add(1, Ordering::Relaxed),
            image: ChatImage::from_rgba_image(format!("clipboard image {ordinal}"), pixels)?,
            thumbnail,
            texture: None,
        })
    }

    pub fn image(&self) -> &ChatImage {
        &self.image
    }

    pub fn into_image(self) -> ChatImage {
        self.image
    }

    /// The thumbnail's pixel size, so the composer can lay it out at its own
    /// aspect ratio instead of the square the chip reserves.
    pub fn thumbnail_size(&self) -> egui::Vec2 {
        let axis = |value: usize| f32::from(u16::try_from(value).unwrap_or(u16::MAX));
        egui::Vec2::new(axis(self.thumbnail.size[0]), axis(self.thumbnail.size[1]))
    }

    /// Upload the thumbnail once, then reuse the handle; the full image is only
    /// decoded when the preview is opened. The name is unique per attachment
    /// because removing one shifts the ordinals of the others.
    pub fn texture(&mut self, ctx: &egui::Context) -> egui::TextureHandle {
        self.texture
            .get_or_insert_with(|| {
                ctx.load_texture(
                    format!("chat-pending-image-{}", self.id),
                    self.thumbnail.clone(),
                    egui::TextureOptions::LINEAR,
                )
            })
            .clone()
    }
}

static NEXT_PENDING_IMAGE_ID: AtomicU64 = AtomicU64::new(1);

fn rgba_image(width: u32, height: u32, rgba: Vec<u8>) -> Result<image::RgbaImage, String> {
    let expected = (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(4);
    if width == 0 || height == 0 || rgba.len() != expected {
        return Err("剪贴板图片数据不完整。".into());
    }
    image::RgbaImage::from_raw(width, height, rgba).ok_or_else(|| "剪贴板图片尺寸无效。".into())
}

fn thumbnail_color_image(pixels: &image::RgbaImage) -> egui::ColorImage {
    let (width, height) = pixels.dimensions();
    let scale = (THUMBNAIL_MAX_DIMENSION as f32 / width.max(height).max(1) as f32).min(1.0);
    let thumbnail = image::imageops::thumbnail(
        pixels,
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    );
    egui::ColorImage::from_rgba_unmultiplied(
        [thumbnail.width() as usize, thumbnail.height() as usize],
        thumbnail.as_raw(),
    )
}


#[cfg(test)]
mod tests {
use super::*;
fn clipboard_pixels(width:u32,height:u32)->Vec<u8> { (0..width*height).flat_map(|i| [(i%width) as u8,(i/width) as u8,7,255]).collect() }
    #[test]
    fn clipboard_pixels_become_a_sendable_attachment() {
        let pending = PendingChatImage::from_clipboard(2, 3, 2, clipboard_pixels(3, 2)).unwrap();
        let preview = pending.image().preview_image().unwrap();
        assert_eq!(preview.size, [3, 2]);
        assert_eq!(
            preview.pixels[4],
            egui::Color32::from_rgba_unmultiplied(1, 1, 7, 255)
        );
        let content = message_content("What is this?", &[pending.into_image()]).unwrap();
        assert_eq!(content[1]["text"], "Attached image: clipboard image 2");
        assert_eq!(content[2]["type"], "image_url");
    }

    #[test]
    fn thumbnail_keeps_its_shape_and_bounds_its_longest_side() {
        let pixels = image::RgbaImage::from_pixel(200, 100, image::Rgba([9, 8, 7, 255]));
        let thumbnail = thumbnail_color_image(&pixels);
        let expected = usize::try_from(THUMBNAIL_MAX_DIMENSION).expect("thumbnail size fits");
        assert_eq!(thumbnail.size, [expected, expected / 2]);
    }

    #[test]
    fn clipboard_pixels_that_do_not_match_their_dimensions_are_rejected() {
        let result = PendingChatImage::from_clipboard(1, 2, 2, clipboard_pixels(3, 2));
        assert!(matches!(result, Err(reason) if reason.contains("不完整")));
    }


}
