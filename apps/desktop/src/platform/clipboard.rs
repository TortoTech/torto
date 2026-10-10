//! System clipboard image access for the chat composer.
//!
//! egui-winit turns the paste shortcut into `egui::Event::Paste` only when the
//! clipboard holds text, so an image-only clipboard (a screenshot) produces no
//! event at all and the composer can never see it. The event loop reads the
//! pixels here, before the shortcut is handed to egui.

use std::path::Path;

/// A paste brings in at most one message's worth of images.
const MAX_CLIPBOARD_FILES: usize = crate::plugins::chat_media::MAX_IMAGES_PER_MESSAGE;

/// Files bigger than this are not decoded at all.
const MAX_CLIPBOARD_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// Straight (non-premultiplied) RGBA8 pixels taken from the clipboard.
pub(crate) struct ClipboardImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// Every image the clipboard carries, or an empty list when it holds text,
/// holds no image, or cannot be read. Text wins so an ordinary text paste keeps
/// reaching egui unchanged.
///
/// The clipboard carries an image either as pixels or as files: Explorer's Copy
/// and screenshot tools that put the file they saved on the clipboard leave only
/// a file list, which egui and `arboard::Clipboard::get_image` both ignore. Those
/// files are read here, so a screenshot pasted from such a tool lands in the
/// composer either way.
pub(crate) fn images_without_text() -> Vec<ClipboardImage> {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return Vec::new();
    };
    if clipboard.get_text().is_ok_and(|text| !text.is_empty()) {
        return Vec::new();
    }
    if let Ok(image) = clipboard.get_image() {
        let width = u32::try_from(image.width).ok();
        let height = u32::try_from(image.height).ok();
        if let (Some(width), Some(height)) = (width, height) {
            return vec![ClipboardImage {
                width,
                height,
                rgba: image.bytes.into_owned(),
            }];
        }
        return Vec::new();
    }
    let Ok(files) = clipboard.get().file_list() else {
        return Vec::new();
    };
    files
        .iter()
        .filter_map(|path| read_image_file(path))
        .take(MAX_CLIPBOARD_FILES)
        .collect()
}

/// One clipboard file as RGBA8. A file that is not an image, is too large, or
/// fails to decode is skipped, so pasting several files keeps the images among
/// them.
fn read_image_file(path: &Path) -> Option<ClipboardImage> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CLIPBOARD_FILE_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    // Attachments are scaled down to 2048 points when they are encoded, so a
    // larger decode would only waste memory on the event loop.
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    let pixels = reader.decode().ok()?.into_rgba8();
    let (width, height) = pixels.dimensions();
    Some(ClipboardImage {
        width,
        height,
        rgba: pixels.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(name)
    }

    fn write_png(name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let pixels = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let path = temp_path(name);
        std::fs::write(&path, bytes.into_inner()).unwrap();
        path
    }

    #[test]
    fn reads_the_image_a_clipboard_file_points_at() {
        let path = write_png("torto-clipboard-file.png", 3, 2);
        let image = read_image_file(&path).expect("a png file reads as an image");
        assert_eq!((image.width, image.height), (3, 2));
        assert_eq!(image.rgba.len(), 3 * 2 * 4);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn skips_a_clipboard_file_that_is_not_an_image() {
        let path = temp_path("torto-clipboard-file.txt");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(read_image_file(&path).is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn skips_a_clipboard_file_that_is_a_directory() {
        assert!(read_image_file(&std::env::temp_dir()).is_none());
    }
}
