use super::*;
use anyrender::recording::RenderCommand;
use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle, ReaderTypesetting, SpreadMode};
use rebook_publication::*;

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
            style: Default::default(),
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

#[test]
fn joined_heading_copy_hits_anchors_and_highlighting_keep_the_original_fragments() {
    let source = Source(Book {
        id: PublicationId::new("heading-hit-test").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        spread: SpreadMode::Single,
        ..Default::default()
    };
    for (width, value) in [
        (800, "Formal Models"),
        (
            360,
            "Formal Models and a Long Description of Their Architecture",
        ),
        (800, "形式模型"),
    ] {
        let ordinal = block("ordinal", "2", TextBlockKind::HeadingOrdinal(1));
        let title = block("title", value, TextBlockKind::Heading(1));
        let ranges = [
            ordinal.source.clone().unwrap(),
            title.source.clone().unwrap(),
        ];
        let layout = LayoutEngine::new()
            .layout_blocks(
                &source,
                &[Block::Text(ordinal), Block::Text(title)],
                LayoutViewport::new(width, 600).unwrap(),
                &style,
            )
            .unwrap();
        let display = DisplayListCompiler.compile(&layout.pages[0]);
        let mut indices = Vec::new();
        for (range, expected) in ranges.iter().zip(["2", value]) {
            let (index, bytes) = (0..display.text_region_count())
                .find_map(|index| {
                    display
                        .text_region_byte_range_for_source(index, range)
                        .map(|bytes| (index, bytes))
                })
                .unwrap();
            let copied = display.selection_fragment(index, bytes).unwrap();
            assert_eq!(copied.quote, expected);
            assert_eq!(&copied.range, range);
            assert!(display.contains_source_anchor(&range.start));
            for exact in [true, false] {
                let point = display.source_rects(std::slice::from_ref(range))[0].center();
                assert_eq!(
                    display
                        .hit_test_text(point.x as f32, point.y as f32, exact)
                        .unwrap()
                        .region_index,
                    index
                );
            }
            indices.push(index);
        }
        assert_eq!(
            display.text_region_joiner(&display, indices[0], indices[1]),
            Some(" ")
        );
        let left = display.source_rects(&ranges[..1])[0];
        let right = display.source_rects(&ranges[1..])[0];
        let separator = kurbo::Point::new((left.x1 + right.x0) * 0.5, left.center().y);
        for (selected, filled) in [
            (&ranges[..], true),
            (&ranges[..1], false),
            (&ranges[1..], false),
        ] {
            let mut scene = anyrender::Scene::new();
            display.paint_source_ranges(&mut scene, selected, Color::BLACK, 0.0);
            let RenderCommand::Fill(fill) = &scene.commands[0] else {
                panic!("one highlight fill expected");
            };
            assert_eq!(fill.shape.winding(separator) != 0, filled);
        }
    }
}

#[test]
fn softened_internal_heading_break_keeps_title_character_offsets() {
    let source = Source(Book {
        id: PublicationId::new("heading-break-hit-test").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let heading = block(
        "heading",
        "Chapter 1\nIntroduction",
        TextBlockKind::Heading(1),
    );
    let mut title_source = heading.source.clone().unwrap();
    title_source.start.text_offset += "Chapter 1\n".chars().count() as u64;
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        spread: SpreadMode::Single,
        ..Default::default()
    };
    let layout = LayoutEngine::new()
        .layout_blocks(
            &source,
            &[Block::Text(heading)],
            LayoutViewport::new(800, 600).unwrap(),
            &style,
        )
        .unwrap();
    let display = DisplayListCompiler.compile(&layout.pages[0]);
    let bytes = display
        .text_region_byte_range_for_source(0, &title_source)
        .unwrap();
    let fragment = display.selection_fragment(0, bytes).unwrap();
    assert_eq!(fragment.quote, "Introduction");
    assert_eq!(fragment.range, title_source);
}
