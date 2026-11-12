use super::*;

fn item(depth: u8, marker: bool) -> TextBlock {
    TextBlock {
        kind: TextBlockKind::ListItem {
            ordered: false,
            ordinal: 1,
            depth,
            marker_visible: marker,
        },
        style: BlockStyle::default(),
        source: None,
        content: vec![Inline::Text(TextRun {
            // Short tokens also fit the deepest, largest-font stress column;
            // long unbreakable words have their own overflow-policy tests.
            text: "An a to on in a. ".repeat(32),
            style: TextStyle::default(),
            link: None,
        })],
    }
}

fn engine() -> LayoutEngine {
    LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
        include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
    ))])
}

#[test]
fn unordered_bodies_keep_the_existing_inset_and_the_shared_right_edge() {
    let mut engine = engine();
    for size in [16.0, 20.0, 40.0] {
        for depth in [0, 1, 3] {
            for width in [350.0, 520.0, 800.0] {
                let mut style = ReaderStyle {
                    typesetting: ReaderTypesetting::unified(),
                    writing_system: WritingSystem::Latin,
                    ..Default::default()
                };
                style.typography.font_size = size;
                let block = item(depth, true);
                let resolved = resolve_text_block(&block, &style, TextContext::Flow);
                let prepared = engine.shape_text(&resolved, &style, width);
                assert!(prepared.layout.len() > 1);
                let marker = engine.measure_list_marker_width(
                    "•\u{00a0}",
                    &style.typography.default_stack(),
                    &style.typography,
                );
                let expected = (f32::from(depth) + 1.0) * size * style.typesetting.list_indent_em;
                assert!((prepared.start_offset + marker - expected).abs() < 0.01);
                assert!(
                    prepared.start_offset >= 0.0,
                    "the page margin must remain empty"
                );
                for line in prepared.layout.lines().skip(1) {
                    assert!(
                        (prepared.start_offset + line.metrics().offset - expected).abs() < 0.01
                    );
                    let clusters = line
                        .runs()
                        .flat_map(|run| {
                            run.visual_clusters().map(|cluster| {
                                (
                                    prepared.text[cluster.text_range()].trim().is_empty(),
                                    cluster.advance(),
                                )
                            })
                        })
                        .collect::<Vec<_>>();
                    let trailing: f32 = clusters
                        .iter()
                        .rev()
                        .take_while(|(space, _)| *space)
                        .map(|(_, advance)| advance)
                        .sum();
                    let end = positioned_line_content_end(line) + line.metrics().hanging_advance
                        - trailing;
                    assert!(
                        prepared.start_offset + end <= width + 0.1,
                        "size={size} depth={depth} width={width} end={} available={} start={} text={:?}",
                        prepared.start_offset + end,
                        prepared.available_width,
                        prepared.start_offset,
                        &prepared.text[line.text_range()]
                    );
                }
                assert_eq!(prepared.source_text_start, "•\u{00a0}".len());
                assert_eq!(
                    &prepared.text[prepared.source_text_start..],
                    &rebook_publication::Inline::text_runs(&block.content[0])[0].text
                );
            }
        }
    }
}

#[test]
fn bullets_borrow_only_list_indentation_and_keep_continuations_aligned() {
    let mut engine = engine();
    let mut style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        writing_system: WritingSystem::Latin,
        ..Default::default()
    };
    style.typography.font_size = 40.0;
    for margin in [0.0, 8.0, 20.0] {
        style.horizontal_margin = margin;
        let marked = item(0, true);
        let plain = item(0, false);
        let marked = resolve_text_block(&marked, &style, TextContext::Flow);
        let plain = resolve_text_block(&plain, &style, TextContext::Flow);
        let marked = engine.shape_text(&marked, &style, 180.0);
        let plain = engine.shape_text(&plain, &style, 180.0);
        let body = marked.start_offset + marked.layout.get(1).unwrap().metrics().offset;
        assert!(marked.start_offset >= 0.0);
        assert!(
            (body - style.typography.font_size * style.typesetting.list_indent_em).abs() < 0.01
        );
        assert!((plain.start_offset - body).abs() < 0.01);
        assert!((marked.start_offset + marked.available_width - 180.0).abs() < 0.01);
        assert!((plain.start_offset + plain.available_width - 180.0).abs() < 0.01);
    }
}

#[test]
fn table_cells_and_authored_mode_do_not_outdent_their_markers() {
    let mut engine = engine();
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        writing_system: WritingSystem::Latin,
        ..Default::default()
    };
    let block = item(0, true);
    let resolved = resolve_text_block(&block, &style, TextContext::Table);
    let prepared = engine.shape_table_cell(&resolved, &style, 200.0, 8.0, &[], None);
    assert_eq!(prepared.start_offset, 0.0);
    assert_eq!(prepared.available_width, 200.0);
    let mut block = block;
    block.style.margin_start = 24.0;
    let prepared = engine.shape_text(&block, &ReaderStyle::default(), 300.0);
    assert_eq!(prepared.start_offset, 24.0);
}

#[test]
fn ordered_numbers_and_paragraph_first_line_indents_keep_their_own_rules() {
    let mut engine = engine();
    let mut style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        writing_system: WritingSystem::Latin,
        ..Default::default()
    };
    let mut block = item(0, true);
    block.kind = TextBlockKind::ListItem {
        ordered: true,
        ordinal: 123,
        depth: 0,
        marker_visible: true,
    };
    let resolved = resolve_text_block(&block, &style, TextContext::Flow);
    let prepared = engine.shape_text(&resolved, &style, 300.0);
    let marker = engine.measure_list_marker_width(
        "123.\u{00a0}",
        &style.typography.default_stack(),
        &style.typography,
    );
    let expected = (style.typography.font_size * 1.5).max(marker);
    assert!(
        (prepared.start_offset + prepared.layout.get(1).unwrap().metrics().offset - expected).abs()
            < 0.01
    );
    assert!(prepared.start_offset >= 0.0);
    assert!(prepared.text.starts_with("123.\u{00a0}"));
    block.kind = TextBlockKind::Paragraph;
    for (system, em) in [(WritingSystem::Latin, 1.5), (WritingSystem::Cjk, 2.0)] {
        style.writing_system = system;
        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        let prepared = engine.shape_text(&resolved, &style, 300.0);
        assert_eq!(prepared.start_offset, 0.0);
        assert_eq!(
            prepared.layout.get(0).unwrap().metrics().offset,
            style.typography.font_size * em
        );
        assert_eq!(prepared.layout.get(1).unwrap().metrics().offset, 0.0);
    }
    style.typesetting.paragraph_indent_mode = ParagraphIndentMode::Custom;
    style.typesetting.paragraph_indent_em = 0.75;
    assert_eq!(
        resolve_text_block(&block, &style, TextContext::Flow)
            .style
            .indent,
        style.typography.font_size * 0.75
    );
}

#[test]
fn ordered_numbers_hang_inside_the_paragraph_inset_at_every_depth_and_font_size() {
    let mut engine = engine();
    let mut style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        writing_system: WritingSystem::Latin,
        ..Default::default()
    };
    for size in [16.0, 20.0, 40.0] {
        style.typography.font_size = size;
        for depth in [0, 1, 2] {
            for ordinal in [1, 12, 1000] {
                let mut block = item(depth, true);
                block.kind = TextBlockKind::ListItem {
                    ordered: true,
                    ordinal,
                    depth,
                    marker_visible: true,
                };
                let resolved = resolve_text_block(&block, &style, TextContext::Flow);
                let prepared = engine.shape_text(&resolved, &style, 800.0);
                let expected = (f32::from(depth) + 1.0) * size * style.typesetting.list_indent_em;
                let prefix = format!("{ordinal}.\u{00a0}");
                let marker = engine.measure_list_marker_width(
                    &prefix,
                    &style.typography.default_stack(),
                    &style.typography,
                );
                let body = prepared.start_offset + prepared.layout.get(1).unwrap().metrics().offset;
                assert!((body - expected.max(marker)).abs() < 0.01);
                assert!(prepared.start_offset >= 0.0);
                assert!(prepared.text.starts_with(&format!("{ordinal}.\u{00a0}")));
            }
        }
    }
}

#[test]
fn inferred_ordered_paragraphs_use_the_display_paragraph_inset_before_and_after_translation() {
    let mut engine = engine();
    for size in [16.0, 20.0, 40.0] {
        let mut style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            writing_system: WritingSystem::Cjk,
            horizontal_margin: 20.0,
            ..Default::default()
        };
        style.typography.font_size = size;
        for ordinal in [1, 2] {
            for translated in [false, true] {
                let mut block = item(0, false);
                block.kind = TextBlockKind::ListItem {
                    ordered: true,
                    ordinal,
                    depth: 0,
                    marker_visible: false,
                };
                block.style.margin_start = 32.0;
                block.style.authored_list_marker = Some(2);
                if let Inline::Text(run) = &mut block.content[0] {
                    run.text.insert_str(0, &format!("{ordinal}."));
                    run.style.display_writing_system = translated.then_some(WritingSystem::Cjk);
                }
                let resolved = resolve_text_block(&block, &style, TextContext::Flow);
                let prepared = engine.shape_text(&resolved, &style, 350.0);
                let body = prepared.start_offset + prepared.layout.get(1).unwrap().metrics().offset;
                assert!((body - size * 2.0).abs() < 0.01);
                assert!(prepared.start_offset >= 0.0);
                assert!(
                    prepared
                        .text
                        .starts_with(&format!("{ordinal}.\u{00a0}\u{2060}\u{2060}"))
                );
            }
        }
    }
}

#[test]
fn list_body_uses_the_same_language_and_custom_indent_as_its_paragraph() {
    let mut engine = engine();
    for ordered in [false, true] {
        for (system, display, custom, em) in [
            (WritingSystem::Latin, None, None, 1.5),
            (WritingSystem::Latin, Some(WritingSystem::Cjk), None, 2.0),
            (WritingSystem::Cjk, Some(WritingSystem::Latin), None, 1.5),
            (
                WritingSystem::Latin,
                Some(WritingSystem::Cjk),
                Some(1.0),
                1.0,
            ),
        ] {
            let mut style = ReaderStyle {
                typesetting: ReaderTypesetting::unified(),
                writing_system: system,
                ..Default::default()
            };
            if let Some(indent) = custom {
                style.typesetting.paragraph_indent_mode = ParagraphIndentMode::Custom;
                style.typesetting.paragraph_indent_em = indent;
            }
            for depth in [0, 1, 2] {
                let mut block = item(depth, true);
                block.kind = TextBlockKind::ListItem {
                    ordered,
                    ordinal: 1,
                    depth,
                    marker_visible: true,
                };
                if let Inline::Text(run) = &mut block.content[0] {
                    run.style.display_writing_system = display;
                }
                let resolved = resolve_text_block(&block, &style, TextContext::Flow);
                let prepared = engine.shape_text(&resolved, &style, 350.0);
                let body = prepared.start_offset + prepared.layout.get(1).unwrap().metrics().offset;
                let expected = style.typography.font_size
                    * (em + f32::from(depth) * style.typesetting.list_indent_em);
                assert!((body - expected).abs() < 0.01);
                assert!(prepared.start_offset >= 0.0);
                block.kind = TextBlockKind::Paragraph;
                assert_eq!(
                    resolve_text_block(&block, &style, TextContext::Flow)
                        .style
                        .indent,
                    style.typography.font_size * em
                );
            }
        }
    }
}

#[test]
fn wide_and_mixed_number_markers_share_one_body_column_without_entering_page_margins() {
    let mut engine = engine();
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        writing_system: WritingSystem::Cjk,
        ..Default::default()
    };
    let mut blocks = Vec::new();
    for (ordinal, depth, marker, ordered) in [
        (1, 0, true, true),
        (2, 0, true, true),
        (1000, 0, true, true),
        (1000, 0, false, true),
        (1, 1, true, false),
    ] {
        let mut block = item(depth, marker);
        block.kind = TextBlockKind::ListItem {
            ordered,
            ordinal,
            depth,
            marker_visible: marker,
        };
        blocks.push(Block::Text(block));
    }
    let refs = blocks.iter().collect::<Vec<_>>();
    let insets = engine.list_body_insets(&refs, &[0..blocks.len()], &style);
    assert!(insets[0] > 0.0);
    for (index, block) in blocks.iter().enumerate() {
        let Block::Text(block) = block else {
            unreachable!()
        };
        let mut resolved = resolve_text_block(block, &style, TextContext::Flow).into_owned();
        resolved.style.margin_start += insets[index];
        let prepared = engine.shape_text(&resolved, &style, 350.0);
        let body = prepared.start_offset + prepared.layout.get(1).unwrap().metrics().offset;
        let depth = if index == 4 { 1.0 } else { 0.0 };
        assert!((body - (40.0 + depth * 30.0 + insets[0])).abs() < 0.01);
        assert!(prepared.start_offset >= 0.0);
        assert!((prepared.start_offset + prepared.available_width - 350.0).abs() < 0.01);
    }
}

#[test]
fn marker_gap_stays_fixed_while_first_and_wrapped_body_lines_are_justified() {
    use crate::text_layout::{Affinity, Cursor};
    let mut engine = engine();
    for strategy in [LineBreakStrategy::Greedy, LineBreakStrategy::Optimized] {
        for ordered in [false, true] {
            for cjk in [false, true] {
                let mut style = ReaderStyle {
                    typesetting: ReaderTypesetting::unified(),
                    writing_system: WritingSystem::Latin,
                    ..Default::default()
                };
                style.typesetting.line_break_strategy = strategy;
                for ordinal in [1, 2] {
                    let mut block = item(0, true);
                    block.kind = TextBlockKind::ListItem {
                        ordered,
                        ordinal,
                        depth: 0,
                        marker_visible: true,
                    };
                    if let Inline::Text(run) = &mut block.content[0] {
                        if cjk {
                            run.text = "股票市场的价格水平已经处于低位区。".repeat(8);
                            run.style.display_writing_system = Some(WritingSystem::Cjk);
                        }
                    }
                    let resolved = resolve_text_block(&block, &style, TextContext::Flow);
                    let prepared = engine.shape_text(&resolved, &style, 350.0);
                    let first = Cursor::from_byte_index(
                        &prepared.layout,
                        prepared.source_text_start,
                        Affinity::Downstream,
                    )
                    .geometry(&prepared.layout, 0.0)
                    .x0 as f32
                        + prepared.start_offset;
                    let continuation =
                        prepared.start_offset + prepared.layout.get(1).unwrap().metrics().offset;
                    let expected = style.typography.font_size * if cjk { 2.0 } else { 1.5 };
                    assert!(
                        (first - expected).abs() < 0.05,
                        "{strategy:?} ordered={ordered} cjk={cjk}: first={first} expected={expected}"
                    );
                    assert!((continuation - expected).abs() < 0.05);
                }
            }
        }
    }
}
