use super::*;
use crate::{
    LayoutEngine, LayoutViewport, PageItem, ReaderFontBlob, ReaderStyle, ReaderTypesetting,
    SpreadMode,
};
use rebook_publication::{
    BlockStyle, Book, BookSource, Metadata, PublicationError, PublicationId, PublicationUrl,
    Resource, Section, SourceAnchor, SourceRange, SpineItemId, TextStyle,
};
use std::sync::Arc;

fn block(node: &str, value: &str, kind: TextBlockKind) -> TextBlock {
    let start = SourceAnchor {
        spine: SpineItemId::new("chapter").unwrap(),
        node: node.into(),
        text_offset: 17,
    };
    TextBlock {
        kind,
        style: BlockStyle::default(),
        content: vec![Inline::Text(TextRun {
            text: value.into(),
            style: TextStyle {
                keyword_size_scale: Some(0.7),
                ..Default::default()
            },
            link: None,
        })],
        source: Some(SourceRange {
            end: SourceAnchor {
                text_offset: 17 + value.chars().count() as u64,
                ..start.clone()
            },
            start,
        }),
    }
}

struct Source(Book);
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.0
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        unreachable!()
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Err(PublicationError::ResourceNotFound(href.to_string()))
    }
}

fn layout(blocks: &[Block], unified: bool, width: u32) -> crate::SectionLayout {
    let source = Source(Book {
        id: PublicationId::new("heading-test").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let style = ReaderStyle {
        spread: SpreadMode::Single,
        typesetting: if unified {
            ReaderTypesetting::unified()
        } else {
            ReaderTypesetting::default()
        },
        ..Default::default()
    };
    LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
        include_bytes!("../../../../assets/fonts/Literata-opsz-wght.ttf")
            .as_slice()
            .to_vec(),
    ))])
    .layout_blocks(
        &source,
        blocks,
        LayoutViewport::new(width, 600).unwrap(),
        &style,
    )
    .unwrap()
}

#[test]
fn joined_headings_shape_once_with_equal_sizes_and_independent_sources() {
    let ordinal = block("ordinal", "2", TextBlockKind::HeadingOrdinal(1));
    let title = block("title", "Formal Models", TextBlockKind::Heading(1));
    let blocks = vec![Block::Text(ordinal.clone()), Block::Text(title.clone())];
    for unified in [false, true] {
        let result = layout(&blocks, unified, 800);
        let texts: Vec<_> = result
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .filter_map(|item| {
                if let PageItem::Text(text) = item {
                    Some(text)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(texts.len(), if unified { 1 } else { 2 });
        if unified {
            let text = texts[0];
            assert_eq!(text.text.as_ref(), "2 Formal Models");
            assert_eq!(text.layout.len(), 1);
            assert_eq!(text.source_spans.len(), 2);
            assert_eq!(
                text.source_spans[0].source,
                *ordinal.source.as_ref().unwrap()
            );
            assert_eq!(text.source_spans[1].source, *title.source.as_ref().unwrap());
            for run in text.layout.lines().flat_map(|line| line.runs()) {
                assert!((run.font_size() - 32.0).abs() < 0.01);
            }
        }
    }
    assert_eq!(blocks, vec![Block::Text(ordinal), Block::Text(title)]);
}

fn translated(original: &TextBlock, value: &str) -> TextBlock {
    let mut result = block("unused", value, original.kind);
    result.style.reference_owner = original.source.as_ref().map(source_block_identity);
    result.source = None;
    result
}

#[test]
fn bilingual_headings_group_languages_including_partial_translation_and_numeric_reuse() {
    for has_ordinal in [false, true] {
        for has_title in [false, true] {
            let ordinal = block("ordinal", "2", TextBlockKind::HeadingOrdinal(1));
            let title = block("title", "Formal Models", TextBlockKind::Heading(1));
            let mut blocks = vec![Block::Text(ordinal.clone())];
            if has_ordinal {
                blocks.push(Block::Text(translated(&ordinal, "2")));
            }
            blocks.push(Block::Text(title.clone()));
            if has_title {
                blocks.push(Block::Text(translated(&title, "形式模型")));
            }
            let result = layout(&blocks, true, 800);
            let texts: Vec<_> = result
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .filter_map(|item| {
                    if let PageItem::Text(text) = item {
                        Some(text)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(texts.len(), 1 + usize::from(has_title));
            assert_eq!(texts[0].text.as_ref(), "2 Formal Models");
            assert_eq!(texts[0].source_spans.len(), 2);
            if has_title {
                assert_eq!(texts[1].text.as_ref(), "2 形式模型");
                assert!(texts[1].source.is_none());
                assert!(texts[1].source_spans.is_empty());
            }
        }
    }
    let ordinal = block("ordinal", "Chapter Two", TextBlockKind::HeadingOrdinal(1));
    let title = block("title", "Formal Models", TextBlockKind::Heading(1));
    let blocks = vec![
        Block::Text(ordinal.clone()),
        Block::Text(translated(&ordinal, "第二章")),
        Block::Text(title.clone()),
        Block::Text(translated(&title, "形式模型")),
    ];
    let result = layout(&blocks, true, 800);
    let texts: Vec<_> = result.pages[0]
        .items
        .iter()
        .filter_map(|item| {
            if let PageItem::Text(text) = item {
                Some(text.text.as_ref())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(texts, ["Chapter Two Formal Models", "第二章 形式模型"]);
}

#[test]
fn internal_heading_breaks_keep_subtitles_and_source_offsets() {
    for value in [
        "Chapter 1\nIntroduction\nA subtitle",
        "ONE\n\nIntroduction",
        "第1章\n黑缎缠目",
    ] {
        let heading = block("heading", value, TextBlockKind::Heading(1));
        let mut normalized = heading.clone();
        normalize_internal(&mut normalized);
        let shown: String = normalized
            .content
            .iter()
            .flat_map(Inline::text_runs)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(shown.chars().count(), value.chars().count());
        assert!(!shown.lines().next().unwrap().trim().ends_with("1"));
        assert_eq!(normalized.source, heading.source);
        let blocks = [Block::Text(heading)];
        for unified in [false, true] {
            let result = layout(&blocks, unified, 800);
            let text = result.pages[0]
                .items
                .iter()
                .find_map(|item| {
                    if let PageItem::Text(t) = item {
                        Some(t)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(
                text.text.as_ref(),
                if unified { shown.as_str() } else { value }
            );
        }
    }
    let mut heading = block(
        "heading",
        "Introduction\nA subtitle",
        TextBlockKind::Heading(1),
    );
    let before = heading.clone();
    normalize_internal(&mut heading);
    assert_eq!(heading, before);
}

#[test]
fn long_joined_headings_wrap_and_unrelated_companions_are_not_consumed() {
    let ordinal = block("ordinal", "Appendix C", TextBlockKind::HeadingOrdinal(1));
    let title = block(
        "title",
        "Connectionist Models and a Long Description of Their Architecture",
        TextBlockKind::Heading(1),
    );
    let result = layout(
        &[Block::Text(ordinal.clone()), Block::Text(title.clone())],
        true,
        360,
    );
    let text = result.pages[0]
        .items
        .iter()
        .find_map(|item| {
            if let PageItem::Text(t) = item {
                Some(t)
            } else {
                None
            }
        })
        .unwrap();
    assert!(text.layout.len() > 1);
    assert_eq!(text.source_spans.len(), 2);
    for run in text.layout.lines().flat_map(|line| line.runs()) {
        assert!((run.font_size() - 32.0).abs() < 0.01);
    }
    let unrelated = translated(&block("other", "2", ordinal.kind), "2");
    let blocks = [
        Block::Text(ordinal),
        Block::Text(unrelated),
        Block::Text(title),
    ];
    assert!(group(&blocks.iter().collect::<Vec<_>>()).is_none());
}
