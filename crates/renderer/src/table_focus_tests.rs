use super::*;
use anyrender::recording::RenderCommand;
use parley::{FontContext, LayoutContext, StyleProperty};
use rebook_layout::{LayoutViewport, TableCellPlacement};
use rebook_publication::SpineItemId;

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
        citations: Arc::from([]),
        lines: 0..layout.len(),
        layout: Arc::new(layout),
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
