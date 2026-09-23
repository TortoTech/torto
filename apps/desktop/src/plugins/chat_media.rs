//! Book images attached to a user turn. Resources are captured at send time;
//! decoding and encoding happen on the chat worker, never during painting.
use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use rebook_publication::{Block, BookSource, Inline, SourceRange, TextBlock};
use serde_json::{Value, json};

const MAX_IMAGES: usize = 8;
const MAX_BYTES: usize = 20 * 1024 * 1024;
const MAX_DIMENSION: u32 = 2048;

#[derive(Clone, Debug)]
pub struct ChatImage {
    pub label: String,
    bytes: Arc<[u8]>,
    encoded: Arc<OnceLock<Result<String, String>>>,
}

impl PartialEq for ChatImage {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label && self.bytes == other.bytes
    }
}
impl Eq for ChatImage {}

impl ChatImage {
    pub fn preview_image(&self) -> Result<egui::ColorImage, String> {
        let url = self
            .encoded
            .get()
            .ok_or("图片正在准备，请稍后再试。")?
            .as_ref()
            .map_err(Clone::clone)?;
        let data = url.split_once(',').ok_or("图片数据无效")?.1;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| e.to_string())?;
        crate::ui::decode_color_image(&bytes).map_err(|e| e.to_string())
    }
    pub fn data_url(&self) -> Result<&str, String> {
        self.encoded
            .get_or_init(|| encode_image(&self.bytes))
            .as_deref()
            .map_err(Clone::clone)
    }
}

pub fn capture_images(
    source: &dyn BookSource,
    ranges: &[SourceRange],
) -> Result<Vec<ChatImage>, String> {
    let mut images = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut bytes_total = 0;
    for (index, spine) in source.book().sections.iter().enumerate() {
        if !ranges.iter().any(|range| range.start.spine == spine.id) {
            continue;
        }
        let section = source.parse_section(index).map_err(|e| e.to_string())?;
        let mut candidates = Vec::new();
        for block in &section.blocks {
            collect_images(block, ranges, false, &mut candidates);
        }
        for image in candidates {
            if !seen.insert(image.href.clone()) {
                continue;
            }
            if images.len() == MAX_IMAGES {
                return Err("引用内容超过 8 张图片，请缩小选取范围后重试。".into());
            }
            let resource = source
                .resource(&image.href)
                .map_err(|e| format!("无法读取聊天图片 {}：{e}", image.href))?;
            bytes_total += resource.bytes.len();
            if bytes_total > MAX_BYTES {
                return Err("引用图片总大小超过 20 MB，请缩小选取范围后重试。".into());
            }
            images.push(ChatImage {
                label: format!("{} {}", image.href, image.alt),
                bytes: resource.bytes,
                encoded: Arc::new(OnceLock::new()),
            });
        }
    }
    Ok(images)
}

fn matches_range(source: Option<&SourceRange>, ranges: &[SourceRange]) -> bool {
    source.is_some_and(|source| {
        ranges.iter().any(|range| {
            source.start.spine == range.start.spine
                && (source.start.node == range.start.node || source.end.node == range.end.node)
        })
    })
}

fn collect_text_images<'a>(
    text: &'a TextBlock,
    ranges: &[SourceRange],
    parent: bool,
    output: &mut Vec<&'a rebook_publication::ImageBlock>,
) {
    if parent || matches_range(text.source.as_ref(), ranges) {
        for inline in &text.content {
            if let Inline::Image(image) = inline {
                output.push(&image.image);
            }
        }
    }
}

fn collect_images<'a>(
    block: &'a Block,
    ranges: &[SourceRange],
    parent: bool,
    output: &mut Vec<&'a rebook_publication::ImageBlock>,
) {
    match block {
        Block::Text(text) => collect_text_images(text, ranges, parent, output),
        Block::Image(image) => {
            if parent || matches_range(image.source.as_ref(), ranges) {
                output.push(image);
            }
        }
        Block::Figure(figure) => {
            let selected = parent
                || matches_range(figure.source.as_ref(), ranges)
                || figure
                    .images
                    .iter()
                    .any(|i| matches_range(i.source.as_ref(), ranges))
                || figure
                    .captions
                    .iter()
                    .any(|c| matches_range(c.source.as_ref(), ranges));
            if selected {
                output.extend(&figure.images);
            }
            for caption in &figure.captions {
                collect_text_images(caption, ranges, selected, output);
            }
        }
        Block::Quote(quote) => {
            let selected = parent || matches_range(quote.source.as_ref(), ranges);
            for text in quote.body.iter().chain(quote.attribution.iter()) {
                collect_text_images(text, ranges, selected, output);
            }
        }
        Block::Table(table) => {
            let selected = parent || matches_range(table.source.as_ref(), ranges);
            for cell in table.rows.iter().flat_map(|row| &row.cells) {
                collect_text_images(&cell.text, ranges, selected, output);
            }
        }
        Block::Note(note) => {
            let selected = parent || matches_range(note.source.as_ref(), ranges);
            for child in &note.blocks {
                collect_images(child, ranges, selected, output);
            }
        }
        _ => {}
    }
}

pub fn message_content(text: &str, images: &[ChatImage]) -> Result<Value, String> {
    if images.is_empty() {
        return Ok(json!(text));
    }
    let mut parts = vec![json!({"type":"text", "text":text})];
    for image in images {
        parts.push(json!({"type":"text", "text":format!("Attached book image: {}", image.label)}));
        parts.push(json!({"type":"image_url", "image_url":{"url":image.data_url()?}}));
    }
    Ok(json!(parts))
}

fn encode_image(bytes: &[u8]) -> Result<String, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(_) => {
            let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
                .map_err(|e| format!("无法解码聊天图片：{e}"))?;
            let size = tree.size();
            let scale = (MAX_DIMENSION as f32 / size.width().max(size.height())).min(1.0);
            let mut pixmap = resvg::tiny_skia::Pixmap::new(
                (size.width() * scale).ceil().max(1.0) as u32,
                (size.height() * scale).ceil().max(1.0) as u32,
            )
            .ok_or("图片尺寸无效")?;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            // tiny-skia stores premultiplied pixels; encode_png restores straight alpha.
            image::load_from_memory(&pixmap.encode_png().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
        }
    };
    let image = if image.width().max(image.height()) > MAX_DIMENSION {
        image.resize(
            MAX_DIMENSION,
            MAX_DIMENSION,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image
    }
    .to_rgba8();
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    if bytes.get_ref().len() > MAX_BYTES / MAX_IMAGES {
        return Err("图片压缩后仍过大，请缩小选取范围或图片尺寸。".into());
    }
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment() -> ChatImage {
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 128]));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        ChatImage {
            label: "formula.png".into(),
            bytes: bytes.into_inner().into(),
            encoded: Arc::new(OnceLock::new()),
        }
    }

    #[test]
    fn transparent_attachment_preserves_alpha_and_survives_history_clone() {
        let image = attachment();
        let history_image = image.clone();
        let content = message_content("Explain the formula", &[image]).unwrap();
        assert_eq!(content[0]["text"], "Explain the formula");
        assert_eq!(content[2]["type"], "image_url");
        let preview = history_image.preview_image().unwrap();
        assert!(
            preview
                .pixels
                .iter()
                .all(|pixel| pixel.a() == 128 && pixel.r() == 0)
        );
        assert_eq!(
            message_content("Follow up", &[history_image]).unwrap()[2],
            content[2]
        );
        assert_eq!(
            message_content("Text only", &[]).unwrap(),
            json!("Text only")
        );
    }

    #[test]
    fn invalid_image_fails_instead_of_sending_text_only() {
        let image = ChatImage {
            label: "bad.png".into(),
            bytes: Arc::from(&b"invalid"[..]),
            encoded: Arc::new(OnceLock::new()),
        };
        assert!(message_content("Explain", &[image]).is_err());
    }

    #[test]
    fn semantic_images_include_inline_and_figure_images_but_exclude_other_blocks() {
        use rebook_publication::*;
        let spine = SpineItemId::new("chapter").unwrap();
        let range = |node: &str| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 10,
            },
        };
        let image = |name: &str| ImageBlock {
            formula_image: false,
            formula: None,
            href: PublicationUrl::parse(name).unwrap(),
            alt: name.into(),
            style: ImageStyle::default(),
            source: Some(range(name)),
            text_layer: None,
        };
        let paragraph = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Image(Box::new(InlineImageRun {
                image: image("inline.png"),
                size_scale: 1.0,
                intrinsic_sizing: true,
                vertical_align: InlineImageAlignment::Baseline,
                presentation: false,
            }))],
            style: BlockStyle::default(),
            source: Some(range("paragraph")),
        });
        let figure = Block::Figure(FigureBlock {
            images: vec![image("first.png"), image("second.png")],
            captions: vec![],
            caption_position: CaptionPosition::After,
            style: BlockStyle::default(),
            source: Some(range("figure")),
        });
        let unrelated = Block::Image(image("unrelated.png"));
        let mut result = Vec::new();
        let selected = vec![range("paragraph"), range("figure")];
        for block in [&paragraph, &figure, &unrelated] {
            collect_images(block, &selected, false, &mut result);
        }
        assert_eq!(
            result.iter().map(|i| i.href.path()).collect::<Vec<_>>(),
            vec!["inline.png", "first.png", "second.png"]
        );
    }
}
