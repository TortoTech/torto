//! CSS floating initials use a separate glyph layout and a line exclusion.
use super::*;
use skrifa::{
    GlyphId, MetadataProvider,
    instance::{LocationRef, NormalizedCoord, Size},
};

impl LayoutEngine {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn shape_floating_initial(
        &mut self,
        text: &str,
        spans: &[StyledRange],
        images: &[PreparedInlineImage],
        font_stack: &str,
        typography: &ReaderTypography,
        block: &TextBlock,
        first_line_indent: f32,
        width: f32,
        foreground: Rgba,
    ) -> Option<Layout<TextBrush>> {
        let initial = spans.first()?;
        let shape = initial.style.drop_cap?;
        let end = initial.range.end;
        if initial.range.start != 0
            || end >= text.len()
            || initial.style.display_writing_system.is_some()
            || images.iter().any(|image| image.index < end)
        {
            return None;
        }
        let body_text = &text[end..];
        let base = typography.font_size;
        let mut cap = self.build_text_layout_paragraph(
            &text[..end],
            std::slice::from_ref(initial),
            &[],
            font_stack,
            typography,
            1.0,
            foreground,
            &[],
            &[],
            &[],
            rebook_publication::TextDirection::Auto,
            parley::OverflowWrap::Normal,
        );
        cap.break_all_lines(None);
        cap.align(Alignment::Left, AlignmentOptions::default());
        let metrics = *cap.get(0)?.metrics();
        let content_height = shape.line_height
            * (base * initial.style.size_scale.clamp(0.5, 6.0)).max(typography.minimum_font_size);
        let [top, right, bottom, left] = shape.insets.map(|inset| inset * base);
        let float_width = (left + cap.full_width() + right).max(0.0);
        let body_width = width - float_width;
        if !float_width.is_finite() || body_width - first_line_indent.max(0.0) < base * 3.0 {
            return None;
        }
        let y = top + (content_height - metrics.line_height) * 0.5;
        // CSS wraps around the margin box, not the font's ascent and descent.
        // Keep the actual painted glyph with its paragraph at page boundaries
        // even if its ink extends beyond the authored margin box.
        let float_bottom = (top + content_height + bottom).max(0.0);
        let ink = initial_ink(&cap)
            .unwrap_or(metrics.content_block_min_coord..metrics.content_block_max_coord);
        let ink = (y + ink.start).max(0.0)..(y + ink.end).max(0.0);
        if !y.is_finite() || !float_bottom.is_finite() {
            return None;
        }
        let x = if shape.right { body_width + left } else { left };
        let body_spans = spans
            .iter()
            .skip(1)
            .cloned()
            .map(|mut span| {
                span.range = span.range.start - end..span.range.end - end;
                span
            })
            .collect::<Vec<_>>();
        let body_images = images
            .iter()
            .cloned()
            .map(|mut image| {
                image.index -= end;
                image
            })
            .collect::<Vec<_>>();
        let mut body = self.build_text_layout_paragraph(
            body_text,
            &body_spans,
            &body_images,
            font_stack,
            typography,
            block.style.line_height,
            foreground,
            &[],
            &[],
            &[],
            block.style.direction,
            parley::OverflowWrap::BreakWord,
        );
        body.set_text_indent(first_line_indent, parley::IndentOptions::default());
        let mut breaker = body.break_lines();
        breaker.state_mut().set_layout_max_advance(width);
        loop {
            let wraps = breaker.committed_y() < f64::from(float_bottom);
            let state = breaker.state_mut();
            state.set_line_max_advance(if wraps { body_width } else { width });
            state.set_line_x(if wraps && !shape.right {
                float_width
            } else {
                0.0
            });
            if breaker.break_next().is_none() {
                break;
            }
        }
        breaker.finish();
        (!body.is_empty()).then(|| Layout::with_initial(body, cap, x, y, ink, float_bottom))
    }
}

fn initial_ink(layout: &parley::Layout<TextBrush>) -> Option<std::ops::Range<f32>> {
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for item in layout.get(0)?.items() {
        let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
            continue;
        };
        let run = glyph_run.run();
        let Ok(font) = FontRef::from_index(run.font().data.as_ref(), run.font().index) else {
            continue;
        };
        let coords = run
            .normalized_coords()
            .iter()
            .map(|c| NormalizedCoord::from_bits(c.to_bits()))
            .collect::<Vec<_>>();
        let glyph_metrics =
            font.glyph_metrics(Size::new(run.font_size()), LocationRef::new(&coords));
        for glyph in glyph_run.glyphs() {
            if let Some(bounds) = glyph_metrics.bounds(GlyphId::new(glyph.id)) {
                let y = glyph_run.baseline() + glyph.y;
                top = top.min(y - bounds.y_max);
                bottom = bottom.max(y - bounds.y_min);
            }
        }
    }
    (top.is_finite() && bottom > top).then_some(top..bottom)
}
