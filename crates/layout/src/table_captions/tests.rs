use super::*;
use crate::{BookSource, LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting};
use rebook_publication::{
    Block, BlockStyle, Book, Metadata, PublicationError, PublicationId, PublicationUrl, Resource,
    Section, SourceAnchor, SourceRange, SpineItemId, TableCell, TableRow, TextRun, TextStyle,
};

fn text(value: &str, kind: TextBlockKind) -> TextBlock {
    let start = SourceAnchor {
        spine: SpineItemId::new("chapter").unwrap(),
        node: value.into(),
        text_offset: 0,
    };
    TextBlock {
        kind,
        content: vec![Inline::Text(TextRun {
            text: value.into(),
            style: TextStyle::default(),
            link: None,
        })],
        style: BlockStyle::default(),
        source: Some(SourceRange {
            end: SourceAnchor {
                text_offset: value.chars().count() as u64,
                ..start.clone()
            },
            start,
        }),
    }
}

fn table() -> TableBlock {
    let cell = |value, kind, span| TableCell {
        text: text(value, kind),
        authored_alignment: None,
        column_span: span,
        row_span: 1,
        header: false,
    };
    TableBlock {
        before: vec![],
        after: vec![],
        source: None,
        rows: vec![
            TableRow {
                cells: vec![cell("Benchmark phenomena", TextBlockKind::Caption, 3)],
            },
            TableRow {
                cells: vec![
                    cell("Category", TextBlockKind::Paragraph, 1),
                    cell("Description", TextBlockKind::Paragraph, 1),
                ],
            },
            TableRow {
                cells: vec![
                    cell("Learning", TextBlockKind::Paragraph, 1),
                    cell("Measured effects", TextBlockKind::Paragraph, 1),
                ],
            },
        ],
    }
}

#[test]
fn joined_captions_respect_distinct_author_alignments_and_source_ranges() {
    let source = EmptySource(Book {
        id: PublicationId::new("caption-join-alignment").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let mut engine = LayoutEngine::new();
    for (label_alignment, title_alignment, joined) in [
        (
            Some(crate::TextAlignment::Center),
            Some(crate::TextAlignment::Start),
            false,
        ),
        (Some(crate::TextAlignment::Center), None, false),
        (
            Some(crate::TextAlignment::Center),
            Some(crate::TextAlignment::Center),
            true,
        ),
    ] {
        let mut captions = [
            text("Table 3.1", TextBlockKind::Caption),
            text(
                "A title long enough to naturally wrap at the narrow measure",
                TextBlockKind::Caption,
            ),
        ];
        captions[0].style.authored_alignment = label_alignment;
        captions[1].style.authored_alignment = title_alignment;
        let original = captions.clone();
        let shaped = engine
            .shape_captions_with_join(&source, &captions, &style, 180.0, true, Some(0))
            .unwrap();
        assert_eq!(shaped.len(), if joined { 1 } else { 2 });
        assert_eq!(shaped[0].1.style.align, crate::TextAlignment::Center);
        if joined {
            assert_eq!(shaped[0].0.source_spans.len(), 2);
            assert!(
                shaped[0]
                    .0
                    .source_spans
                    .iter()
                    .any(|span| span.source == captions[0].source.clone().unwrap())
            );
            assert!(
                shaped[0]
                    .0
                    .source_spans
                    .iter()
                    .any(|span| span.source == captions[1].source.clone().unwrap())
            );
        } else {
            assert_eq!(shaped[1].1.style.align, crate::TextAlignment::Start);
            assert_eq!(shaped[0].1.source, captions[0].source);
            assert_eq!(shaped[1].1.source, captions[1].source);
        }
        assert_eq!(captions, original);
    }
}

#[test]
fn absent_or_number_only_caption_promotes_title_without_mutating_ir() {
    for label in [None, Some("TABLE 3.1"), Some("3.1"), Some("表 3.1")] {
        let mut table = table();
        if let Some(label) = label {
            table.before.push(text(label, TextBlockKind::Caption));
        }
        let original = table.clone();
        let view = presentation(&table, true);
        assert!(view.skip_title);
        assert_eq!(view.before.last(), Some(&table.rows[0].cells[0].text));
        assert_eq!(view.before.len(), table.before.len() + 1);
        assert_eq!(table, original);
        let book = presentation(&table, false);
        assert!(!book.skip_title);
        assert!(matches!(book.before, Cow::Borrowed(_)));
        assert_eq!(&*book.before, &original.before);
    }
}

#[test]
fn complete_or_ambiguous_caption_keeps_title_in_grid() {
    for value in [
        "Table. 0.1 More than two dozen ways to input dian using different Chinese IMEs",
        "Table 1 shows the results",
        "表 3.1 基准现象",
        "Observed results",
    ] {
        for after in [false, true] {
            let mut table = table();
            let captions = if after {
                &mut table.after
            } else {
                &mut table.before
            };
            captions.push(text(value, TextBlockKind::Caption));
            let view = presentation(&table, true);
            assert!(!view.skip_title, "{value}");
            assert!(matches!(view.before, Cow::Borrowed(_)));
            assert!(matches!(view.after, Cow::Borrowed(_)));
        }
    }
    let mut table = table();
    table.before.push(text("Table 1", TextBlockKind::Caption));
    table.after.push(text("Table 2", TextBlockKind::Caption));
    assert!(!presentation(&table, true).skip_title);
    table.before.clear();
    table.after.clear();
    table.rows[0].cells[0].text.kind = TextBlockKind::Paragraph;
    assert!(!presentation(&table, true).skip_title);
}

#[test]
fn bottom_caption_notes_bilingual_text_and_links_keep_their_sources() {
    let mut table = table();
    let mut label = text("TABLE 3.1", TextBlockKind::Caption);
    label.content.push(Inline::Break);
    label
        .content
        .extend(text("表 3.1", TextBlockKind::Caption).content);
    table.after = vec![
        label,
        text("NOTE: Rounded values.", TextBlockKind::Paragraph),
    ];
    let title = &mut table.rows[0].cells[0].text;
    title.content.push(Inline::Break);
    title
        .content
        .extend(text("基准现象", TextBlockKind::Caption).content);
    let mut footnote = text("1", TextBlockKind::Paragraph);
    if let Inline::Text(run) = &mut footnote.content[0] {
        run.link = Some(PublicationUrl::parse("notes.xhtml#n1").unwrap());
        run.style.link_role = LinkRole::FootnoteReference;
    }
    title.content.extend(footnote.content);
    let view = presentation(&table, true);
    assert!(view.skip_title);
    assert_eq!(
        view.join, None,
        "bilingual labels keep their authored language order"
    );
    assert!(view.before.is_empty());
    assert_eq!(view.after[0], table.after[0]);
    assert_eq!(view.after[1], table.rows[0].cells[0].text);
    assert_eq!(view.after[2], table.after[1]);
}

#[test]
fn label_detection_requires_a_number_and_no_description() {
    for value in [
        "Table. 0.1",
        "TABLE A.1:",
        "Tab. IV",
        "表格 二",
        "３.１",
        "1",
        "Table 1(a)",
    ] {
        assert!(
            super::super::caption_labels::table_label_only(value),
            "{value}"
        );
    }
    for value in [
        "Table",
        "Table 1 Results",
        "Table tennis",
        "Figure 1",
        "3 important effects",
        "Table 1\nMeasured results",
    ] {
        assert!(
            !super::super::caption_labels::table_label_only(value),
            "{value}"
        );
    }
}

struct EmptySource(Book);
impl BookSource for EmptySource {
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

#[test]
fn unified_table_cells_trim_edge_lines_without_changing_geometry_or_source_offsets() {
    let source = EmptySource(Book {
        id: PublicationId::new("cell-edge-lines").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let cell = |value: &str, header, span| TableCell {
        text: text(value, TextBlockKind::Paragraph),
        header,
        row_span: span,
        column_span: 1,
        authored_alignment: None,
    };
    for unified in [false, true] {
        for row_span in [1, 2] {
            let style = ReaderStyle {
                typesetting: if unified {
                    ReaderTypesetting::unified()
                } else {
                    ReaderTypesetting::default()
                },
                ..Default::default()
            };
            let mut expected_height = 0.0;
            let mut expected_baseline = 0.0;
            for padded in [false, true] {
                let value = if padded {
                    "\u{a0}\n\nFirst line\n\n第二行\n\u{a0}\n"
                } else {
                    "First line\n\n第二行"
                };
                let mut table = TableBlock {
                    source: None,
                    before: vec![],
                    after: vec![],
                    rows: vec![TableRow {
                        cells: vec![cell(value, true, row_span), cell(" ", false, 1)],
                    }],
                };
                if row_span == 2 {
                    table.rows.push(TableRow {
                        cells: vec![cell(" ", false, 1)],
                    });
                }
                let target = table.rows[0].cells[0].text.source.clone();
                let layout = LayoutEngine::new()
                    .layout_blocks(
                        &source,
                        &[Block::Table(table.clone())],
                        LayoutViewport::new(800, 600).unwrap(),
                        &style,
                    )
                    .unwrap();
                let table = layout.pages[0]
                    .items
                    .iter()
                    .find_map(|item| {
                        if let PageItem::Table(table) = item {
                            Some(table)
                        } else {
                            None
                        }
                    })
                    .unwrap();
                let positioned = table
                    .cells
                    .iter()
                    .find(|cell| cell.text.as_ref().is_some_and(|text| text.source == target))
                    .unwrap();
                let rendered = positioned.text.as_ref().unwrap();
                assert_eq!(rendered.text.as_ref(), value);
                assert_eq!(rendered.source, target);
                assert_eq!(
                    rendered.lines.len(),
                    if unified || !padded {
                        3
                    } else {
                        rendered.layout.len()
                    }
                );
                assert!(
                    rendered.text[rendered
                        .layout
                        .get(rendered.lines.start + 1)
                        .unwrap()
                        .text_range()]
                    .trim()
                    .is_empty(),
                    "keep the internal blank line"
                );
                let baseline = rendered.origin_y
                    + rendered
                        .layout
                        .get(rendered.lines.start)
                        .unwrap()
                        .metrics()
                        .baseline;
                if !padded {
                    expected_height = positioned.height;
                    expected_baseline = baseline;
                } else if unified {
                    assert!(
                        (positioned.height - expected_height).abs() < 0.01,
                        "edge wrappers must not enlarge the cell or row spans"
                    );
                    assert!(
                        (baseline - expected_baseline).abs() < 0.01,
                        "center the trimmed content, not the original empty lines"
                    );
                    assert_eq!(rendered.lines.start, 2);
                    assert_eq!(
                        rendered
                            .layout
                            .get(rendered.lines.start)
                            .unwrap()
                            .text_range()
                            .start,
                        4,
                        "keep the NBSP and two source newlines in the byte offsets"
                    );
                    assert!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.text.is_none())
                            .count()
                            >= 1,
                        "whitespace-only cells remain empty"
                    );
                } else {
                    assert!(positioned.height > expected_height);
                    assert_eq!(rendered.lines.start, 0);
                }
            }
        }
    }
}

#[test]
fn unified_table_cell_keeps_formula_only_line_between_edge_breaks() {
    let mut table = table();
    table.rows.truncate(1);
    let cell = &mut table.rows[0].cells[0];
    cell.column_span = 1;
    cell.text.kind = TextBlockKind::Paragraph;
    cell.text.content = text("\u{a0}", TextBlockKind::Paragraph).content;
    cell.text.content.push(Inline::Break);
    cell.text
        .content
        .push(Inline::Math(rebook_publication::MathRun {
            original: None,
            latex: "x^2".into(),
            display: false,
            size_scale: 1.0,
        }));
    cell.text.content.push(Inline::Break);
    cell.text
        .content
        .extend(text("\u{a0}", TextBlockKind::Paragraph).content);
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let source = EmptySource(Book {
        id: PublicationId::new("table-formula-only").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let prepared = LayoutEngine::new()
        .shape_table(&source, &table, &style, 400.0)
        .unwrap();
    let text = &prepared.cells[0].text;
    assert_eq!(text.lines, 1..2);
    assert_eq!(text.inline_images.len(), 1);
    assert!(super::super::prepared_text_height(text) > 0.0);
}

#[test]
fn actual_layout_extracts_once_in_unified_only_and_preserves_title_location() {
    let source = EmptySource(Book {
        id: PublicationId::new("table-title-test").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let mut table = table();
    table.before.push(text("Table 3.1", TextBlockKind::Caption));
    let title_source = table.rows[0].cells[0].text.source.clone();
    let section = Section {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        anchors: vec![],
        blocks: vec![Block::Table(table)],
    };
    for unified in [false, true] {
        let mut style = ReaderStyle::default();
        if unified {
            style.typesetting = ReaderTypesetting::unified();
        }
        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(640, 600).unwrap(),
                &style,
            )
            .unwrap();
        let items = layout
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .collect::<Vec<_>>();
        let captions = items
            .iter()
            .filter(|item| matches!(item, PageItem::Text(text) if text.source == title_source))
            .count();
        let grid_titles = items
            .iter()
            .filter_map(|item| match item {
                PageItem::Table(grid) => Some(grid),
                _ => None,
            })
            .flat_map(|grid| &grid.cells)
            .filter(|cell| {
                cell.text
                    .as_ref()
                    .is_some_and(|text| text.source == title_source)
            })
            .count();
        assert_eq!(captions, usize::from(unified));
        assert_eq!(grid_titles, usize::from(!unified));
        assert_eq!(captions + grid_titles, 1);
    }
}

#[test]
fn promoted_caption_centers_authored_lines_without_natural_wraps() {
    let source = EmptySource(Book {
        id: PublicationId::new("caption-alignment-test").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    for (label, second_line, centered) in [
        (false, false, true),
        (true, false, true),
        (false, true, true),
        (true, true, true),
    ] {
        let mut table = table();
        let original_title = table.rows[0].cells[0].text.clone();
        let mut content = text("\u{a0}", TextBlockKind::Caption).content;
        content.push(Inline::Break);
        content.extend(original_title.content);
        if second_line {
            content.push(Inline::Break);
            content.extend(text("Another visible line", TextBlockKind::Caption).content);
        }
        table.rows[0].cells[0].text.content = content;
        if label {
            table.before.push(text("Table 3.1", TextBlockKind::Caption));
        }
        let view = presentation(&table, true);
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        let shaped = LayoutEngine::new()
            .shape_captions_with_join(
                &source,
                &view.before,
                &style,
                640.0,
                true,
                view.join.map(|(_, index)| index),
            )
            .unwrap();
        for (prepared, _) in &shaped {
            for line in prepared
                .layout
                .lines()
                .filter(|line| !prepared.text[line.text_range()].trim().is_empty())
            {
                if centered {
                    assert!(line.metrics().offset > 100.0);
                } else {
                    assert!(line.metrics().offset.abs() < 0.01);
                }
            }
        }
        let promoted = &shaped.last().unwrap().0;
        assert_eq!(
            shaped.last().unwrap().1.source,
            table.rows[0].cells[0].text.source
        );
        if view.join.is_some() {
            assert_eq!(promoted.text.contains('\n'), second_line);
            assert_eq!(promoted.source_spans.len(), 2);
        } else {
            assert!(promoted.text.starts_with("\u{a0}\n"));
        }
    }
}

#[test]
fn caption_edge_whitespace_has_no_height_and_keeps_source_offsets() {
    let source = EmptySource(Book {
        id: PublicationId::new("caption-spacing-test").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    // Compare both promotion and an already authored caption, including page edges.
    for promoted in [false, true] {
        for height in [220, 600] {
            let mut expected_table_y: Option<f32> = None;
            for padded in [false, true] {
                let value = if padded {
                    "\u{a0}\nBenchmark phenomena\n\u{a0}"
                } else {
                    "Benchmark phenomena"
                };
                let caption = text(value, TextBlockKind::Caption);
                let caption_source = caption.source.clone();
                let mut table = table();
                if promoted {
                    table.rows[0].cells[0].text = caption;
                } else {
                    table.rows[0].cells[0].text.kind = TextBlockKind::Paragraph;
                    table.before.push(caption);
                }
                let section = Section {
                    id: SpineItemId::new("chapter").unwrap(),
                    href: PublicationUrl::parse("chapter.xhtml").unwrap(),
                    anchors: vec![],
                    blocks: vec![
                        Block::Text(text("Preceding paragraph.", TextBlockKind::Paragraph)),
                        Block::Table(table),
                    ],
                };
                let style = ReaderStyle {
                    typesetting: ReaderTypesetting::unified(),
                    ..ReaderStyle::default()
                };
                let layout = LayoutEngine::new()
                    .layout_section(
                        &source,
                        &section,
                        LayoutViewport::new(640, height).unwrap(),
                        &style,
                    )
                    .unwrap();
                let items = layout
                    .pages
                    .iter()
                    .flat_map(|page| &page.items)
                    .collect::<Vec<_>>();
                let caption = items
                    .iter()
                    .find_map(|item| match item {
                        PageItem::Text(text) if text.source == caption_source => Some(text),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(caption.text.as_ref(), value);
                assert_eq!(caption.lines, if padded { 1..2 } else { 0..1 });
                // UTF-8 and character offsets must still include the invisible NBSP + newline.
                assert_eq!(
                    caption
                        .layout
                        .get(caption.lines.start)
                        .unwrap()
                        .text_range()
                        .start,
                    if padded { 3 } else { 0 }
                );
                assert_eq!(caption.source, caption_source);
                let table_y = items
                    .iter()
                    .find_map(|item| match item {
                        PageItem::Table(table) => Some(table.y),
                        _ => None,
                    })
                    .unwrap();
                if let Some(expected) = expected_table_y {
                    assert!(
                        (table_y - expected).abs() < 0.01,
                        "promoted={promoted}, height={height}"
                    );
                } else {
                    expected_table_y = Some(table_y);
                }
            }
        }
    }
}

#[test]
fn caption_trim_keeps_internal_blank_lines_media_and_original_book_layout() {
    let source = EmptySource(Book {
        id: PublicationId::new("caption-internal-lines-test").unwrap(),
        metadata: Metadata::default(),
        cover: None,
        sections: vec![],
        table_of_contents: vec![],
    });
    let caption = text(
        "\u{a0}\nFirst line\n\u{a0}\nSecond line\n\u{a0}",
        TextBlockKind::Caption,
    );
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..ReaderStyle::default()
    };
    let mut engine = LayoutEngine::new();
    let unified = engine
        .shape_figure_caption(&source, &caption, &style, 600.0, true, false)
        .unwrap();
    assert_eq!(unified.0.lines, 1..4);
    assert!(
        unified.0.text[unified.0.layout.get(2).unwrap().text_range()]
            .trim()
            .is_empty()
    );
    let book = engine
        .shape_figure_caption(
            &source,
            &caption,
            &ReaderStyle::default(),
            600.0,
            false,
            false,
        )
        .unwrap();
    assert_eq!(book.0.lines, 0..5);
    assert!(
        super::super::prepared_flow_height(&book.0)
            > super::super::prepared_flow_height(&unified.0)
    );
    let blank = text("\u{a0}\n\u{a0}", TextBlockKind::Caption);
    let blank = engine
        .shape_figure_caption(&source, &blank, &style, 600.0, true, false)
        .unwrap();
    assert!(blank.0.lines.is_empty());
    assert_eq!(super::super::prepared_flow_height(&blank.0), 0.0);
    let mut formula = text("\u{a0}", TextBlockKind::Caption);
    formula.content.push(Inline::Break);
    formula
        .content
        .push(Inline::Math(rebook_publication::MathRun {
            original: None,
            latex: "x^2".into(),
            display: false,
            size_scale: 1.0,
        }));
    formula.content.push(Inline::Break);
    formula
        .content
        .extend(text("\u{a0}", TextBlockKind::Caption).content);
    let formula = engine
        .shape_figure_caption(&source, &formula, &style, 600.0, true, false)
        .unwrap();
    assert_eq!(formula.0.lines, 1..2);
    assert!(!formula.0.inline_images.is_empty());
    assert!(super::super::prepared_flow_height(&formula.0) > 0.0);
}
