use super::*;
use parley::{Alignment, AlignmentOptions, FontContext, LayoutContext, StyleProperty};
use rebook_layout::LayoutViewport;
use rebook_publication::SpineItemId;

#[test]
fn blank_lines_have_no_highlight_but_remain_in_copied_text() {
    let text: Arc<str> = "A centered passage with several lines of actual text.\n\n \t\u{00a0}\n\u{2060}\nAnother line after the blank space.\n\nFinal line.".into();
    for alignment in [
        Alignment::Start,
        Alignment::Center,
        Alignment::End,
        Alignment::Justify,
    ] {
        let mut fonts = FontContext::new();
        let mut context = LayoutContext::new();
        let mut builder = context.ranged_builder(&mut fonts, &text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
        }));
        let mut layout = builder.build(&text);
        layout.break_all_lines(Some(250.0));
        layout.align(alignment, AlignmentOptions::default());
        let source = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("test").unwrap(),
                node: "p".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("test").unwrap(),
                node: "p".into(),
                text_offset: text.chars().count() as u64,
            },
        };
        let blank_lines: Vec<_> = layout
            .lines()
            .enumerate()
            .filter_map(|(i, line)| {
                let slice = &text[line.text_range()];
                slice
                    .chars()
                    .all(|ch| ch.is_whitespace() || ch == '\u{2060}')
                    .then_some((
                        i,
                        line.metrics().block_min_coord,
                        line.metrics().block_max_coord,
                        line.text_range(),
                    ))
            })
            .collect();
        assert!(blank_lines.len() >= 4);
        let layout = Arc::new(layout);
        // Exercise both the whole paragraph and page fragments that begin/end
        // on blank lines. Copy/source mapping must not depend on painted boxes.
        for lines in [
            0..layout.len(),
            blank_lines[0].0..layout.len(),
            0..blank_lines[1].0 + 1,
        ] {
            let page = PageLayout {
                viewport: LayoutViewport::new(320, 800).unwrap(),
                background: Rgba::BLACK,
                leading_gap: 0.0,
                items: vec![PageItem::Text(TextPlacement {
                    citations: Arc::from([]),
                    layout: layout.clone(),
                    text: text.clone(),
                    source_text_start: 0,
                    lines: lines.clone(),
                    origin_x: 24.0,
                    origin_y: 32.0,
                    available_width: 250.0,
                    source: Some(source.clone()),
                    inline_images: Arc::from([]),
                })],
            };
            let display = DisplayListCompiler.compile(&page);
            let fragment = display.selection_fragment(0, 0..text.len()).unwrap();
            if lines == (0..layout.len()) {
                assert_eq!(fragment.quote, text.as_ref());
                assert_eq!(fragment.range, source);
            }
            let active = display.source_rects(std::slice::from_ref(&source));
            assert!(!active.is_empty());
            for (i, top, bottom, range) in &blank_lines {
                if !lines.contains(i) {
                    continue;
                }
                let y = f64::from(32.0 + (top + bottom) * 0.5);
                assert!(
                    !active.iter().any(|r| r.y0 < y && r.y1 > y),
                    "blank line {i} has activation geometry for {alignment:?}"
                );
                assert!(!fragment.rects.iter().any(|r| r.y0 < y && r.y1 > y));
                if !range.is_empty() {
                    let blank = display.selection_fragment(0, range.clone()).unwrap();
                    assert!(blank.rects.is_empty());
                    assert_eq!(blank.quote, &text[range.clone()]);
                }
            }
        }
    }
}
