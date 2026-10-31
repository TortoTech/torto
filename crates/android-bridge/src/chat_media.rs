//! Local, bounded assistant attachments. Kotlin supplies picker copies, never credentials.
use crate::chat::ChatMessage;
use crate::*;
use rebook_app_core::plugins::chat_media::{self, ChatImage};
use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Clone, Debug, Serialize, Deserialize, uniffi::Record)]
pub struct ChatAttachment {
    pub id: String,
    pub label: String,
}
fn image_path(s: &ReaderState, id: &str) -> Result<PathBuf> {
    if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(error("图片标识无效"));
    }
    Ok(s.chat_path
        .with_extension("images")
        .join(format!("{id}.png")))
}
pub(crate) fn load(s: &ReaderState, attachment: &ChatAttachment) -> Result<ChatImage> {
    let mut bytes = Vec::new();
    std::fs::File::open(image_path(s, &attachment.id)?)
        .map_err(error)?
        .take(3 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 3 * 1024 * 1024 {
        return Err(error("缓存图片超过大小限制"));
    }
    ChatImage::from_encoded_bytes(attachment.label.clone(), bytes).map_err(error)
}
pub(crate) fn store(s: &ReaderState, image: &ChatImage) -> Result<ChatAttachment> {
    let bytes = image.png_bytes().map_err(error)?;
    let id = blake3::hash(&bytes).to_hex().to_string();
    let attachment = ChatAttachment {
        id,
        label: image.label.chars().take(160).collect(),
    };
    let path = image_path(s, &attachment.id)?;
    if !path.exists() {
        let root = path.parent().unwrap();
        std::fs::create_dir_all(root).map_err(error)?;
        let size = std::fs::read_dir(root)
            .map_err(error)?
            .filter_map(|f| f.ok()?.metadata().ok())
            .map(|m| m.len())
            .sum::<u64>();
        if size + bytes.len() as u64 > 64 * 1024 * 1024 {
            return Err(error("本书对话图片已达 64 MB，请清理对话后重试"));
        }
        rebook_app_core::persistence::write_bytes_atomic(&path, &bytes).map_err(error)?;
    }
    Ok(attachment)
}
pub(crate) fn message_images(
    s: &ReaderState,
    messages: &[ChatMessage],
    budget: &mut usize,
) -> Vec<Vec<ChatImage>> {
    let mut output = vec![Vec::new(); messages.len()];
    let mut loaded = std::collections::HashMap::<String, (ChatImage, usize)>::new();
    for (index, message) in messages.iter().enumerate().rev() {
        for attachment in message
            .images
            .iter()
            .take(chat_media::MAX_IMAGES_PER_MESSAGE)
        {
            if let Some((image, size)) = loaded.get(&attachment.id) {
                if *size <= *budget {
                    *budget -= *size;
                    output[index].push(image.clone());
                }
                continue;
            }
            let Ok(path) = image_path(s, &attachment.id) else {
                continue;
            };
            let Ok(metadata) = std::fs::metadata(path) else {
                continue;
            };
            if metadata.len() as usize > *budget {
                continue;
            }
            if let Ok(image) = load(s, attachment) {
                *budget -= metadata.len() as usize;
                loaded.insert(
                    attachment.id.clone(),
                    (image.clone(), metadata.len() as usize),
                );
                output[index].push(image);
            }
        }
    }
    output
}
#[uniffi::export]
impl MobileReader {
    pub async fn add_chat_image(
        self: Arc<Self>,
        path: String,
        label: String,
    ) -> Result<ChatAttachment> {
        work(move || {
            self.with(|s| {
                let file = std::fs::File::open(path).map_err(error)?;
                let mut bytes = Vec::new();
                file.take(20 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(error)?;
                let image = ChatImage::from_encoded_bytes(label.chars().take(160).collect(), bytes)
                    .map_err(error)?;
                store(s, &image)
            })
        })
        .await
    }
    pub async fn add_chat_book_images(
        self: Arc<Self>,
        source: String,
    ) -> Result<Vec<ChatAttachment>> {
        work(move || {
            self.with(|s| {
                let range: SourceRange = serde_json::from_str(&source).map_err(error)?;
                chat_media::capture_images(s.source.as_ref(), &[range])
                    .map_err(error)?
                    .iter()
                    .map(|image| store(s, image))
                    .collect()
            })
        })
        .await
    }
    pub async fn remove_chat_image(self: Arc<Self>, id: String) -> Result<()> {
        work(move || {
            self.with(|s| {
                let path = image_path(s, &id)?;
                if !s
                    .messages
                    .iter()
                    .flat_map(|m| &m.images)
                    .any(|a| a.id == id)
                    && path.exists()
                {
                    std::fs::remove_file(path).map_err(error)?;
                }
                Ok(())
            })
        })
        .await
    }
    pub async fn chat_image(self: Arc<Self>, id: String) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                let image = load(
                    s,
                    &ChatAttachment {
                        id,
                        label: String::new(),
                    },
                )?;
                image.data_url().map_err(error)?;
                let mut pixels = image.preview_rgba().map_err(error)?;
                let (width, height) = pixels.dimensions();
                for pixel in pixels.pixels_mut() {
                    let alpha = u16::from(pixel.0[3]);
                    for c in &mut pixel.0[..3] {
                        *c = ((u16::from(*c) * alpha + 127) / 255) as u8;
                    }
                }
                Ok(interactions::preview_page(width, height, pixels.into_raw()))
            })
        })
        .await
    }
    pub async fn follow_chat_citation(self: Arc<Self>, locator: String) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                let citation = rebook_app_core::chat_markdown::parse_chat_citation(&locator)
                    .ok_or_else(|| error("引用链接无效"))?;
                if citation.section_index >= s.source.book().sections.len() {
                    return Err(error("引用章节不存在"));
                }
                let range = if let Some(node) = citation.node {
                    Some(
                        rebook_app_core::reading_text::source_range_for_node(
                            s.source.as_ref(),
                            citation.section_index,
                            &node,
                        )
                        .ok_or_else(|| error("引用段落不存在，可能已重新排版"))?,
                    )
                } else {
                    None
                };
                s.scroll_center = true;
                s.scroll_slices.clear();
                let previous = s.session.current_locator();
                if let Some(range) = &range {
                    s.session.go_to_source(&range.start).map_err(error)?;
                } else {
                    s.session
                        .go_to_section(citation.section_index)
                        .map_err(error)?;
                }
                s.history.push(previous);
                if s.history.len() > 32 {
                    s.history.remove(0);
                }
                s.focus_anchor = range;
                render(s)
            })
        })
        .await
    }
}
