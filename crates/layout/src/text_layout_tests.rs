#![allow(clippy::cast_possible_truncation)]
use super::*;
use crate::{TextBrush, linebreak::SpacingAdjustment};
use parley::{BaseDirection, FontContext, LayoutContext, StyleProperty};

fn native(text: &str, direction: BaseDirection) -> parley::Layout<TextBrush> {
    let mut fonts = FontContext::new();
    fonts.collection.register_fonts(
        crate::ReaderFontBlob::new(std::sync::Arc::new(
            include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").to_vec(),
        )),
        None,
    );
    let mut context = LayoutContext::new();
    let mut builder = context.ranged_builder(&mut fonts, text, 1., false);
    builder.push_default(StyleProperty::FontFamily("Literata".into()));
    builder.push_default(StyleProperty::FontSize(24.));
    builder.set_base_direction(direction);
    builder.build(text)
}
fn close(a: f32, b: f32) {
    assert!((a - b).abs() < 0.002, "{a} != {b}");
}
fn glyphs(l: &Layout<TextBrush>) -> Vec<parley::Glyph> {
    l.lines()
        .flat_map(|line| {
            line.items().flat_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => run.positioned_glyphs().collect::<Vec<_>>(),
                PositionedLayoutItem::InlineBox(_) => vec![],
            })
        })
        .collect()
}

#[test]
fn spacing_preserves_glyphs_and_maps_caret_selection_hit_testing_and_alignment() {
    let text = "alpha beta gamma";
    let mut layout: Layout<TextBrush> = native(text, BaseDirection::Ltr).into();
    layout.break_all_lines(Some(500.));
    layout.align(Alignment::Start, AlignmentOptions::default());
    let before = glyphs(&layout);
    let before_width = layout.width();
    let gap = 5..6;
    let original_caret =
        Cursor::from_byte_index(&layout, 6, Affinity::Downstream).geometry(&layout, 0.);
    layout
        .apply_spacing(&[SpacingAdjustment {
            range: gap.clone(),
            amount: 8.,
        }])
        .unwrap();
    close(layout.width(), before_width + 8.);
    let after = glyphs(&layout);
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(&after) {
        assert_eq!(a.id, b.id);
        close(a.y, b.y);
        if a.x >= original_caret.x0 as f32 - 0.001 {
            close(b.x, a.x + 8.);
        } else {
            close(b.x, a.x);
        }
    }
    let c = Cursor::from_byte_index(&layout, 6, Affinity::Downstream);
    let rect = c.geometry(&layout, 0.);
    close(rect.x0 as f32, original_caret.x0 as f32 + 8.);
    let hit = Cursor::from_point(
        &layout,
        rect.x0 as f32 + 0.01,
        ((rect.y0 + rect.y1) * 0.5) as f32,
    );
    assert_eq!(hit.index(), 6);
    let selection = Selection::new(Cursor::from_byte_index(&layout, 5, Affinity::Downstream), c)
        .geometry(&layout);
    let selected = selection[0].0;
    close(selected.x1 as f32, rect.x0 as f32);
    layout.align(Alignment::Start, AlignmentOptions::default());
    close(
        Cursor::from_byte_index(&layout, 6, Affinity::Downstream)
            .geometry(&layout, 0.)
            .x0 as f32,
        rect.x0 as f32,
    );
    layout.break_all_lines(Some(500.));
    close(layout.width(), before_width);
}

#[test]
fn unsafe_spacing_keeps_ligatures_on_the_reshaping_path() {
    let mut layout: Layout<TextBrush> = native("office", BaseDirection::Ltr).into();
    layout.break_all_lines(None);
    assert!(
        layout
            .lines()
            .flat_map(|l| l.runs())
            .flat_map(|r| r.clusters())
            .any(|c| c.is_ligature_start())
    );
    assert!(
        layout
            .apply_spacing(&[SpacingAdjustment {
                range: 1..2,
                amount: 2.
            }])
            .is_none()
    );
    let mut marks: Layout<TextBrush> = native("q\u{301} x", BaseDirection::Ltr).into();
    marks.break_all_lines(None);
    assert_eq!(
        marks
            .get(0)
            .unwrap()
            .runs()
            .next()
            .unwrap()
            .clusters()
            .next()
            .unwrap()
            .glyphs()
            .count(),
        2
    );
    assert!(
        marks
            .apply_spacing(&[SpacingAdjustment {
                range: 0..3,
                amount: 2.
            }])
            .is_none()
    );
}

#[test]
fn composed_paragraphs_and_ruby_keep_global_ranges_and_shifted_hit_geometry() {
    let original = "אבג אבג אבג\n";
    let translated = "English translation wraps into several lines";
    let mut layout = Layout::paragraphs(vec![
        (native(original, BaseDirection::Rtl), 0, false, true),
        (
            native(translated, BaseDirection::Ltr),
            original.len(),
            true,
            false,
        ),
    ]);
    layout.break_all_lines(Some(155.));
    layout.align(Alignment::Right, AlignmentOptions::default());
    let first_translated = layout
        .lines()
        .position(|l| l.text_range().start >= original.len())
        .unwrap();
    let line = layout.get(first_translated).unwrap();
    close(line.metrics().offset, 0.);
    assert_eq!(line.text_range().start, original.len());
    assert!(line.runs().all(|r| !r.is_rtl()));
    let range = line.text_range();
    let before = line.metrics();
    let downstream = Cursor::from_byte_index(&layout, original.len(), Affinity::Downstream)
        .geometry(&layout, 0.);
    let upstream =
        Cursor::from_byte_index(&layout, original.len(), Affinity::Upstream).geometry(&layout, 0.);
    close(downstream.y0 as f32, upstream.y0 as f32);
    layout.reserve_text_paint_bounds(&[(range.clone(), -80., 12.)]);
    let height = layout.height();
    layout.reserve_text_paint_bounds(&[(range.clone(), -80., 12.)]);
    close(height, layout.height());
    assert!(layout.get(first_translated).unwrap().metrics().baseline > before.baseline);
    let l = layout.get(first_translated).unwrap();
    let first = Cursor::from_byte_index(&layout, original.len(), Affinity::Downstream)
        .geometry(&layout, 0.);
    let ruby_hit = Cursor::from_point(
        &layout,
        first.x0 as f32 + 0.01,
        l.metrics().block_min_coord + 0.01,
    );
    assert_eq!(ruby_hit.index(), original.len());
    for l in layout.lines().skip(first_translated) {
        let end = Cursor::from_byte_index(&layout, l.text_range().end, Affinity::Upstream)
            .geometry(&layout, 0.);
        let clicked = Cursor::from_point(
            &layout,
            end.x0 as f32 - 0.01,
            ((end.y0 + end.y1) * 0.5) as f32,
        );
        assert_eq!(clicked.index(), l.text_range().end);
        close(clicked.geometry(&layout, 0.).y0 as f32, end.y0 as f32);
        for run in l.runs() {
            for cluster in run.clusters() {
                let range = cluster.text_range();
                assert!(range.start >= original.len());
                let caret = Cursor::from_byte_index(&layout, range.start, Affinity::Downstream);
                let rect = caret.geometry(&layout, 0.);
                let hit = Cursor::from_point(
                    &layout,
                    rect.x0 as f32 + 0.01,
                    ((rect.y0 + rect.y1) * 0.5) as f32,
                );
                assert_eq!(hit.index(), range.start);
            }
        }
    }
    let selection = Selection::new(
        Cursor::from_byte_index(&layout, 0, Affinity::Downstream),
        Cursor::from_byte_index(
            &layout,
            original.len() + translated.len(),
            Affinity::Upstream,
        ),
    );
    assert_eq!(selection.text_range(), 0..original.len() + translated.len());
    let rects = selection.geometry(&layout);
    assert!(rects.iter().any(|(_, i)| *i >= first_translated));
    for (rect, i) in rects {
        let line = layout.get(i).unwrap();
        close(
            rect.y0 as f32,
            line.metrics()
                .block_min_coord
                .min(line.metrics().content_block_min_coord),
        );
    }
}
