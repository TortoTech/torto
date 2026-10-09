//! Preserve provider evidence without retaining a second copy of OCR prose.
//! Legacy Markdown-only documents still use the conservative shared rules.
use super::*;
use rebook_publication::{Block, TextBlockKind};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredBlock {
    kind: String,
    /// Digest of normalized content; no duplicate paragraphs in resident memory.
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bbox: Option<[f64; 4]>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    continues_prev: bool,
}

fn key(raw: &str) -> String {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let tags = TAGS.get_or_init(|| Regex::new(r"</?[A-Za-z][^>]*>").unwrap());
    let plain = tags.replace_all(raw, "");
    let plain = decode_html_entities_once(&plain);
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{:x}", Sha256::digest(plain.as_bytes()))
}

fn bbox(value: Option<&Value>) -> Option<[f64; 4]> {
    let values = value?.as_array()?;
    if values.len() != 4 {
        return None;
    }
    let rect = [
        values[0].as_f64()?,
        values[1].as_f64()?,
        values[2].as_f64()?,
        values[3].as_f64()?,
    ];
    (rect.iter().all(|n| n.is_finite()) && rect[2] > rect[0] && rect[3] > rect[1]).then_some(rect)
}

fn stored(
    kind: &str,
    text: &str,
    rect: Option<[f64; 4]>,
    continuation: bool,
) -> Option<StoredBlock> {
    // Image/diagram blocks often have geometry but no recognized prose.
    if kind.len() > 64 || (text.trim().is_empty() && rect.is_none()) {
        return None;
    }
    Some(StoredBlock {
        kind: kind.to_owned(),
        key: key(text),
        bbox: rect,
        continues_prev: continuation,
    })
}

pub(super) fn paddle_blocks(entry: &Value) -> Vec<StoredBlock> {
    entry
        .pointer("/prunedResult/parsing_res_list")
        .or_else(|| entry.get("parsing_res_list"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(4096)
        .filter_map(|block| {
            stored(
                block.get("block_label")?.as_str()?,
                block.get("block_content")?.as_str()?,
                bbox(block.get("block_bbox")),
                false,
            )
        })
        .collect()
}

fn mineru_blocks(items: &[&Value]) -> Vec<StoredBlock> {
    items
        .iter()
        .take(4096)
        .filter_map(|item| {
            stored(
                item.get("type")?.as_str()?,
                &preferred_text(item),
                bbox(item.get("bbox")),
                item.get("continues_prev")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            )
        })
        .collect()
}

pub(super) fn mineru_page<'a>(items: impl IntoIterator<Item = &'a Value>) -> StoredOcrPage {
    let items = items.into_iter().collect::<Vec<_>>();
    StoredOcrPage {
        markdown: items
            .iter()
            .filter_map(|item| mineru_item_markdown(item))
            .collect::<Vec<_>>()
            .join("\n\n"),
        blocks: mineru_blocks(&items),
    }
}

/// Apply roles only when a provider content key is unique across the section.
/// Ambiguous duplicate strings do not erase body prose or confer continuation.
pub(super) fn apply(section: &mut Section, pages: &[StoredOcrPage]) -> HashSet<String> {
    if pages.iter().all(|p| p.blocks.is_empty()) {
        return HashSet::new();
    }
    let mut evidence = HashMap::new();
    for block in pages.iter().flat_map(|p| &p.blocks) {
        evidence
            .entry(block.key.as_str())
            .and_modify(|value| *value = None)
            .or_insert(Some(block));
    }
    let mut occurrences = HashMap::<String, usize>::new();
    for block in &section.blocks {
        if let Block::Text(t) = block {
            *occurrences
                .entry(key(&rebook_formats::reflow::text(t)))
                .or_default() += 1;
        }
    }
    let mut continuations = HashSet::new();
    let mut removed = HashSet::new();
    for block in &mut section.blocks {
        let Block::Text(t) = block else { continue };
        let digest = key(&rebook_formats::reflow::text(t));
        if occurrences.get(&digest) != Some(&1) {
            continue;
        }
        let Some(Some(hint)) = evidence.get(digest.as_str()) else {
            continue;
        };
        let Some(source) = &t.source else { continue };
        match hint.kind.as_str() {
            "header" | "footer" | "page_number" | "number" => {
                removed.insert(source.start.node.clone());
            }
            "figure_caption" | "image_caption" | "chart_caption" => {
                t.kind = TextBlockKind::Caption;
            }
            "text" | "paragraph" if hint.continues_prev => {
                continuations.insert(source.start.node.clone());
            }
            _ => {}
        }
    }
    // Empty page/TOC anchors attached to a discarded header advance to the next
    // real block. They must never reference a deleted node.
    let mut replacement: Option<rebook_publication::SourceAnchor> = None;
    for block in section.blocks.iter().rev() {
        let source = match block {
            Block::Text(t) => t.source.as_ref(),
            Block::Image(i) => i.source.as_ref(),
            Block::Figure(f) => f.source.as_ref(),
            Block::Table(t) => t.source.as_ref(),
            _ => None,
        };
        let Some(source) = source else { continue };
        if removed.contains(&source.start.node) {
            if let Some(target) = &replacement {
                for anchor in &mut section.anchors {
                    if anchor.source.node == source.start.node {
                        anchor.source = target.clone();
                    }
                }
            } else {
                // A trailing header has no forward destination. Keep the source
                // block until its anchors can be recovered safely.
                removed.remove(&source.start.node);
            }
        } else {
            replacement = Some(source.start.clone());
        }
    }
    section.blocks.retain(|b| !matches!(b, Block::Text(t) if t.source.as_ref().is_some_and(|s| removed.contains(&s.start.node))));
    continuations
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mineru_caption_is_selectable_and_provider_header_keeps_page_anchor() {
        let page = mineru_page(&[
            json!({"type":"header", "text":"Running title"}),
            json!({"type":"text", "text":"Body."}),
            json!({"type":"image", "img_path":"../OcrResources/figure.png", "image_caption":["Figure 1 A real caption."]}),
        ]);
        let descriptor = SpineItem {
            id: SpineItemId::new("provider-fixture").unwrap(),
            href: PublicationUrl::parse("Text/test.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        };
        let mut section = rebook_html::parse_section(
            &format!(
                "<html><body><div id='pdf-page-1'></div>{}</body></html>",
                markdown_to_html(&page.markdown)
            ),
            &descriptor,
            |_| None,
        )
        .unwrap();
        apply(&mut section, &[page]);
        assert!(!section.blocks.iter().any(
            |b| matches!(b, Block::Text(t) if rebook_formats::reflow::text(t) == "Running title")
        ));
        let Block::Text(body) = &section.blocks[0] else {
            panic!()
        };
        assert_eq!(
            section.anchors[0].source.node,
            body.source.as_ref().unwrap().start.node
        );
        assert!(section.blocks.iter().any(|b| matches!(b, Block::Figure(f) if f.captions.iter().any(|c| rebook_formats::reflow::text(c) == "Figure 1 A real caption."))));
    }

    #[test]
    fn optional_provider_layout_survives_serialization_without_duplicate_prose() {
        let page = mineru_page(&[
            json!({"type":"text", "text":"续接内容", "bbox":[20,30,300,60], "continues_prev":true}),
            json!({"type":"header", "text":"Running title", "bbox":[20,1,300,15]}),
        ]);
        let json = serde_json::to_string(&page).unwrap();
        assert_eq!(json.matches("续接内容").count(), 1);
        let restored: StoredOcrPage = serde_json::from_str(&json).unwrap();
        assert!(restored.blocks[0].continues_prev);
        assert_eq!(restored.blocks[0].bbox, Some([20.0, 30.0, 300.0, 60.0]));
        let legacy: StoredOcrPage = serde_json::from_str(r#"{"markdown":"Old OCR"}"#).unwrap();
        assert!(legacy.blocks.is_empty());
    }
}
