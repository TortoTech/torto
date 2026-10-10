use super::*;
use anyrender::recording::RenderCommand;
use parley::{FontContext, LayoutContext, StyleProperty};
use rebook_layout::{LayoutViewport, TableCellPlacement};
use rebook_publication::SpineItemId;

#[test]
fn joined_table_caption_preserves_fragment_copy_hits_and_source_highlights() {
    use rebook_layout::{LayoutEngine, ReaderStyle, ReaderTypesetting, SpreadMode};
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
    let source = Source(Book {
        id: PublicationId::new("joined-caption").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let block = |node: &str, value: &str, kind| TextBlock {
        kind,
        style: BlockStyle::default(),
        content: vec![Inline::Text(TextRun {
            text: value.into(),
            style: TextStyle::default(),
            link: None,
        })],
        source: Some(SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: node.into(),
                text_offset: 17,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: node.into(),
                text_offset: 17 + value.chars().count() as u64,
            },
        }),
    };
    for bottom in [false, true] {
        for (width, title) in [
            (800, "Benchmark phenomena"),
            (
                360,
                "Important Benchmark Phenomena Related to Printed Word Identification, Organized by Categories",
            ),
            (800, "\u{a0}\n\u{a0}Benchmark phenomena"),
            (800, "Benchmark phenomena\n基准现象"),
            // Translated text maps back to the original title's source range.
            (800, "印刷词识别的基准现象"),
        ] {
            let mut label = block("label", "TABLE 3.1", TextBlockKind::Caption);
            if let Inline::Text(run) = &mut label.content[0] {
                run.style.bold = true;
            }
            let mut title_block = block("title", title, TextBlockKind::Caption);
            if title.starts_with('印') {
                title_block.source.as_mut().unwrap().end.text_offset = 78;
            }
            let label_source = label.source.clone().unwrap();
            let title_source = title_block.source.clone().unwrap();
            let cell = |text, span| TableCell {
                text,
                column_span: span,
                row_span: 1,
                header: false,
                authored_alignment: None,
            };
            let mut table = TableBlock {
                source: None,
                before: vec![],
                after: vec![],
                rows: vec![
                    TableRow {
                        cells: vec![cell(title_block, 2)],
                    },
                    TableRow {
                        cells: vec![
                            cell(block("a", "Category", TextBlockKind::Paragraph), 1),
                            cell(block("b", "Description", TextBlockKind::Paragraph), 1),
                        ],
                    },
                ],
            };
            if bottom {
                table.after.push(label);
            } else {
                table.before.push(label);
            }
            let style = ReaderStyle {
                spread: SpreadMode::Single,
                typesetting: ReaderTypesetting::unified(),
                ..Default::default()
            };
            let layout = LayoutEngine::new()
                .layout_blocks(
                    &source,
                    &[Block::Table(table)],
                    LayoutViewport::new(width, 600).unwrap(),
                    &style,
                )
                .unwrap();
            let display = DisplayListCompiler.compile(&layout.pages[0]);
            let mut indices = Vec::new();
            for range in [&label_source, &title_source] {
                let (index, bytes) = (0..display.text_region_count())
                    .find_map(|index| {
                        display
                            .text_region_byte_range_for_source(index, range)
                            .map(|bytes| (index, bytes))
                    })
                    .unwrap();
                let fragment = display.selection_fragment(index, bytes.clone()).unwrap();
                assert_eq!(&fragment.range, range);
                if range == &label_source {
                    assert_eq!(fragment.quote, "TABLE 3.1");
                } else {
                    assert_eq!(
                        fragment.quote,
                        title.replace(
                            '\n',
                            if title.starts_with('\u{a0}') {
                                " "
                            } else {
                                "\n"
                            }
                        )
                    );
                }
                let rects = display.source_rects(std::slice::from_ref(range));
                assert!(!rects.is_empty());
                let point = rects[0].center();
                for exact in [true, false] {
                    let hit = display
                        .hit_test_text(point.x as f32, point.y as f32, exact)
                        .unwrap();
                    assert_eq!(
                        hit.region_index, index,
                        "hit must resolve to its own source fragment"
                    );
                }
                indices.push(index);
            }
            assert_ne!(indices[0], indices[1]);
            assert_eq!(
                display.text_region_joiner(&display, indices[0], indices[1]),
                Some(" ")
            );
            let label_rects = display.source_rects(std::slice::from_ref(&label_source));
            let title_rects = display.source_rects(std::slice::from_ref(&title_source));
            let last = title_rects.last().unwrap();
            let hit = display
                .hit_test_text((last.x1 + 24.0) as f32, last.center().y as f32, false)
                .unwrap();
            assert_eq!(
                hit.region_index, indices[1],
                "dragging into the right margin must keep the title selected"
            );
            assert!(
                label_rects[0].x1 <= title_rects[0].x0 + 0.1,
                "label highlighting must not cover the title"
            );
            let separator = kurbo::Point::new(
                (label_rects[0].x1 + title_rects[0].x0) * 0.5,
                label_rects[0].center().y,
            );
            assert!(title_rects[0].x0 > label_rects[0].x1);
            // The presentation-only space is painted only when the selected
            // source fragments meet on both sides. It remains outside copying
            // and hit-test ranges, including translated and wrapped captions.
            let mut partial_label = label_source.clone();
            partial_label.end.text_offset -= 1;
            let mut partial_title = title_source.clone();
            partial_title.start.text_offset += 1;
            for (ranges, selected) in [
                (vec![label_source.clone(), title_source.clone()], true),
                (vec![title_source.clone(), label_source.clone()], true),
                (vec![label_source.clone()], false),
                (vec![title_source.clone()], false),
                (vec![partial_label, title_source.clone()], false),
                (vec![label_source.clone(), partial_title], false),
            ] {
                let mut scene = anyrender::Scene::new();
                display.paint_source_ranges(&mut scene, &ranges, Color::BLACK, 0.0);
                let RenderCommand::Fill(fill) = &scene.commands[0] else {
                    panic!("source highlighting should paint one fill");
                };
                assert_eq!(
                    fill.shape.winding(separator) != 0,
                    selected,
                    "caption separator highlight: {title:?}, bottom={bottom}"
                );
            }
            let caption = layout.pages[0]
                .items
                .iter()
                .find_map(|item| {
                    if let PageItem::Text(text) = item {
                        (text.source_spans.len() == 2).then_some(text)
                    } else {
                        None
                    }
                })
                .unwrap();
            if width == 360 || title.contains('\n') && !title.starts_with('\u{a0}') {
                assert!(caption.layout.len() > 1);
            } else {
                assert_eq!(caption.layout.len(), 1);
            }
            // A local correction requested for either source moves the shared
            // paragraph and both interaction regions exactly once.
            let shifted = display.translate_source_text(&label_source, 11.0);
            for range in [&label_source, &title_source] {
                let old = display.source_rects(std::slice::from_ref(range));
                let new = shifted.source_rects(std::slice::from_ref(range));
                assert_eq!(old.len(), new.len());
                for (old, new) in old.iter().zip(new) {
                    assert!((new.y0 - old.y0 - 11.0).abs() < 0.01);
                }
            }
        }
    }
}

fn text(node: &str, value: &str, y: f32) -> TextPlacement {
    let mut fonts = FontContext::new();
    let mut context = LayoutContext::new();
    let mut builder = context.ranged_builder(&mut fonts, value, 1.0, false);
    builder.push_default(StyleProperty::FontSize(16.0));
    let mut layout = builder.build(value);
    layout.break_all_lines(Some(180.0));
    let source = SourceRange {
        start: SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: node.into(),
            text_offset: 0,
        },
        end: SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: node.into(),
            text_offset: value.chars().count() as u64,
        },
    };
    TextPlacement {
        source_spans: Arc::from([]),
        ruby: Arc::from([]),
        citations: Arc::from([]),
        lines: 0..layout.len(),
        layout: Arc::new(layout.into()),
        text: value.into(),
        source_text_start: 0,
        origin_x: 25.0,
        origin_y: y,
        available_width: 180.0,
        source: Some(source),
        inline_images: Arc::from([]),
    }
}

fn table_page(
    continued_before: bool,
    continued_after: bool,
) -> (PageDisplayList, Vec<SourceRange>) {
    let caption = text("caption", "Table 1. Values", 10.0);
    let cell = text("cell", "42", 55.0);
    let note = text("note", "NOTE: Rounded.", 110.0);
    let ranges = [&caption, &cell, &note]
        .into_iter()
        .map(|text| text.source.clone().unwrap())
        .collect();
    let page = PageLayout {
        viewport: LayoutViewport::new(240, 160).unwrap(),
        background: Rgba::BLACK,
        leading_gap: 0.0,
        items: vec![
            PageItem::Text(caption),
            PageItem::Table(TablePlacement {
                cells: vec![TableCellPlacement {
                    x: 20.0,
                    y: 50.0,
                    width: 200.0,
                    height: 50.0,
                    header: false,
                    text: Some(cell),
                }],
                y: 50.0,
                height: 50.0,
                continued_before,
                continued_after,
                border: Rgba::BLACK,
                header_fill: Rgba::BLACK,
            }),
            PageItem::Text(note),
        ],
    };
    (DisplayListCompiler.compile(&page), ranges)
}

#[test]
fn table_focus_outline_keeps_outer_edges_with_captions_and_paginated_rows() {
    for (before, after, top, bottom) in [
        (false, false, 49.0, 101.0),
        (false, true, 49.0, 100.0),
        (true, true, 50.0, 100.0),
        (true, false, 50.0, 101.0),
    ] {
        let (page, ranges) = table_page(before, after);
        let mut scene = anyrender::Scene::new();
        page.paint_source_table_borders(&mut scene, &ranges, Color::BLACK, 30.0);
        let mut edges = Vec::new();
        for command in &scene.commands {
            let RenderCommand::Stroke(stroke) = command else {
                panic!("table activation should only stroke its outline");
            };
            assert_eq!(stroke.transform, Affine::translate((30.0, 0.0)));
            assert_eq!(stroke.style.width, 2.0);
            edges.push(stroke.shape.bounding_box());
        }
        assert!(edges.contains(&Rect::new(19.0, top, 19.0, bottom)));
        assert!(edges.contains(&Rect::new(221.0, top, 221.0, bottom)));
        assert_eq!(edges.contains(&Rect::new(19.0, 49.0, 221.0, 49.0)), !before);
        assert_eq!(
            edges.contains(&Rect::new(19.0, 101.0, 221.0, 101.0)),
            !after
        );
        assert_eq!(edges.len(), 2 + usize::from(!before) + usize::from(!after));
    }
}

#[test]
fn table_focus_highlights_caption_and_note_like_figure_captions_without_filling_cells() {
    let (page, ranges) = table_page(false, false);
    let color = Color::from_rgba8(68, 137, 103, 72);
    let mut actual = anyrender::Scene::new();
    page.paint_source_table_annotations(&mut actual, &ranges, color, 30.0);
    let mut expected = anyrender::Scene::new();
    page.paint_source_ranges(
        &mut expected,
        &[ranges[0].clone(), ranges[2].clone()],
        color,
        30.0,
    );
    assert!(!actual.commands.is_empty());
    assert_eq!(actual.commands, expected.commands);
    let RenderCommand::Fill(fill) = &actual.commands[0] else {
        panic!("captions should have the same highlight fill as ordinary text");
    };
    for (index, selected) in [true, false, true].into_iter().enumerate() {
        let rects = page.source_rects(std::slice::from_ref(&ranges[index]));
        assert!(!rects.is_empty());
        for rect in rects {
            assert_eq!(fill.shape.winding(rect.center()) != 0, selected);
        }
    }
}
