//! Legacy Parley adapter.
//!
//! This is the only text-contract module allowed to translate Parley layout
//! objects into backend-neutral retained text data.

use std::collections::HashMap;
use std::sync::Arc;

use parley::fontique::Blob;
use parley::layout::BreakReason;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle, FontWeight, IndentOptions,
    InlineBox, InlineBoxKind, Layout, LayoutContext, LineHeight, PositionedLayoutItem,
    StyleProperty,
};
use rebook_publication::{Rgba, TextAlignment, TextBaseline};

use super::{
    TextCluster, TextEngine, TextFontBlob, TextFontResource, TextFootnoteReference, TextGlyph,
    TextGlyphRun, TextIndent, TextInlineBox, TextLayoutRequest, TextLayoutStore, TextLineBreak,
    TextLineBreakStrategy, TextLineMetrics, TextLineSnapshot, TextPaintItem, TextRule,
};

/// Brush payload kept entirely inside the legacy Parley boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TextBrush {
    pub color: Rgba,
    pub underline: bool,
    pub baseline: TextBaseline,
    pub footnote_reference: bool,
    /// Stable identifier for glyph runs produced by one semantic footnote marker.
    pub footnote_reference_group: u32,
}

/// Snapshots a Parley layout behind the backend-neutral text contract.
///
/// Kept in an explicitly named adapter module so new consumers cannot acquire
/// a Parley dependency accidentally through [`TextLayoutStore`].
///
/// # Panics
///
/// Panics if Parley produces more than `u32::MAX` lines for one paragraph.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the adapter snapshots one shaped line atomically before Parley objects are discarded"
)]
fn snapshot(layout: &Arc<Layout<TextBrush>>) -> TextLayoutStore {
    let full_width = layout.full_width();
    let lines = layout
        .lines()
        .map(|line| {
            let metrics = line.metrics();
            let mut items = Vec::new();
            let mut run_offsets = HashMap::new();
            for item in line.items() {
                match item {
                    PositionedLayoutItem::GlyphRun(glyph_run) => {
                        let run = glyph_run.run();
                        run_offsets.entry(run.index()).or_insert(glyph_run.offset());
                        let brush = glyph_run.style().brush;
                        let baseline_offset = match brush.baseline {
                            TextBaseline::Normal => 0.0,
                            TextBaseline::Superscript => -run.font_size() * 0.35,
                            TextBaseline::Subscript => run.font_size() * 0.2,
                        };
                        if brush.footnote_reference {
                            items.push(TextPaintItem::FootnoteReference(TextFootnoteReference {
                                group: brush.footnote_reference_group,
                                center_x: glyph_run.offset() + glyph_run.advance() / 2.0,
                                baseline: glyph_run.baseline(),
                                font_size: run.font_size(),
                            }));
                            continue;
                        }
                        let glyphs = glyph_run
                            .positioned_glyphs()
                            .map(|glyph| TextGlyph {
                                id: glyph.id,
                                x: glyph.x,
                                y: glyph.y + baseline_offset,
                            })
                            .collect::<Vec<_>>();
                        let font = run.font().clone();
                        let collection_index = font.index;
                        let (font_data, resource_id) = font.data.into_raw_parts();
                        items.push(TextPaintItem::GlyphRun(TextGlyphRun {
                            font: TextFontResource::from_raw_parts(
                                font_data,
                                resource_id,
                                collection_index,
                            ),
                            font_size: run.font_size(),
                            normalized_coords: run.normalized_coords().to_vec().into(),
                            color: brush.color,
                            skew_tan: run.synthesis().skew().map(|angle| angle.to_radians().tan()),
                            glyphs: glyphs.into(),
                        }));
                        if brush.underline {
                            let run_metrics = run.metrics();
                            items.push(TextPaintItem::Rule(TextRule {
                                x: glyph_run.offset(),
                                y: glyph_run.baseline() + baseline_offset
                                    - run_metrics.underline_offset
                                    + run_metrics.underline_size / 2.0,
                                width: glyph_run.advance(),
                                thickness: run_metrics.underline_size.max(1.0),
                                color: brush.color,
                            }));
                        }
                    }
                    PositionedLayoutItem::InlineBox(inline_box) => {
                        items.push(TextPaintItem::InlineBox(TextInlineBox {
                            id: inline_box.id,
                            x: inline_box.x,
                            y: inline_box.y,
                            width: inline_box.width,
                            height: inline_box.height,
                        }));
                    }
                }
            }
            let content_inline_max = metrics.offset
                + metrics.inline_min_coord
                + line
                    .items()
                    .map(|item| match item {
                        PositionedLayoutItem::GlyphRun(run) => run.advance(),
                        PositionedLayoutItem::InlineBox(inline_box) => inline_box.width,
                    })
                    .sum::<f32>();
            let mut clusters = Vec::new();
            for run in line.runs() {
                let Some(mut inline_start) = run_offsets.get(&run.index()).copied() else {
                    continue;
                };
                for cluster in run.visual_clusters() {
                    let inline_end = inline_start + cluster.advance();
                    clusters.push(TextCluster {
                        text_range: cluster.text_range(),
                        inline_start,
                        inline_end,
                        rtl: cluster.is_rtl(),
                    });
                    inline_start = inline_end;
                }
            }
            let text_range = line.text_range();
            // Parley represents an inline-box-only line with its internal
            // sentinel `usize::MAX..0`. It has no selectable authored text, so
            // normalize it at the adapter boundary instead of leaking invalid
            // ranges into the retained core contract.
            let text_range = if text_range.start <= text_range.end {
                text_range
            } else {
                0..0
            };
            TextLineSnapshot {
                text_range,
                metrics: TextLineMetrics {
                    block_min: metrics.block_min_coord,
                    block_max: metrics.block_max_coord,
                    line_height: metrics.line_height,
                    offset: metrics.offset,
                    inline_min: metrics.inline_min_coord,
                    inline_max: metrics.inline_max_coord,
                    content_inline_max,
                },
                break_kind: match line.break_reason() {
                    BreakReason::Regular | BreakReason::Emergency => TextLineBreak::Soft,
                    BreakReason::Explicit => TextLineBreak::Hard,
                    BreakReason::None => TextLineBreak::End,
                },
                clusters: clusters.into(),
                items: items.into(),
            }
        })
        .collect::<Vec<_>>();
    TextLayoutStore::from_snapshots(lines, full_width)
        .expect("Parley produced an invalid text layout snapshot")
}

/// Current text adapter used while the main layout engine migrates away from
/// direct Parley builders.
pub struct LegacyParleyTextEngine {
    font_context: FontContext,
    layout_context: LayoutContext<TextBrush>,
}

impl Default for LegacyParleyTextEngine {
    fn default() -> Self {
        Self {
            font_context: FontContext::new(),
            layout_context: LayoutContext::new(),
        }
    }
}

impl TextEngine for LegacyParleyTextEngine {
    fn register_font(&mut self, font: &TextFontBlob) {
        self.font_context
            .collection
            .register_fonts(Blob::new(font.shared_data()), None);
    }

    fn shape(&mut self, request: &TextLayoutRequest<'_>) -> TextLayoutStore {
        let mut builder =
            self.layout_context
                .ranged_builder(&mut self.font_context, request.text, 1.0, false);
        if let Some(font_family) = request.font_family {
            builder.push_default(StyleProperty::FontFamily(FontFamily::from(font_family)));
        }
        builder.push_default(StyleProperty::FontSize(request.font_size));
        if let Some(font_weight) = request.font_weight {
            builder.push_default(StyleProperty::FontWeight(FontWeight::new(font_weight)));
        }
        if let Some(line_height) = request.line_height {
            builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
                line_height,
            )));
        }
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: request.color,
            underline: false,
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
        }));
        for span in request.spans {
            if let Some(font_size) = span.font_size {
                builder.push(StyleProperty::FontSize(font_size), span.range.clone());
            }
            if let Some(font_weight) = span.font_weight {
                builder.push(
                    StyleProperty::FontWeight(FontWeight::new(font_weight)),
                    span.range.clone(),
                );
            }
            if span.italic {
                builder.push(
                    StyleProperty::FontStyle(FontStyle::Italic),
                    span.range.clone(),
                );
            }
            if span.underline {
                builder.push(StyleProperty::Underline(true), span.range.clone());
            }
            if span.color.is_some()
                || span.underline
                || span.baseline != TextBaseline::Normal
                || span.footnote_reference_group != 0
            {
                builder.push(
                    StyleProperty::Brush(TextBrush {
                        color: span.color.unwrap_or(request.color),
                        underline: span.underline,
                        baseline: span.baseline,
                        footnote_reference: span.footnote_reference_group != 0,
                        footnote_reference_group: span.footnote_reference_group,
                    }),
                    span.range.clone(),
                );
            }
        }
        for object in request.inline_objects {
            builder.push_inline_box(InlineBox {
                id: object.id,
                kind: InlineBoxKind::InFlow,
                index: object.index,
                width: object.width,
                height: object.height,
            });
        }
        let mut layout = builder.build(request.text);
        let TextIndent {
            amount,
            hanging,
            each_line,
        } = request.indent;
        if amount.abs() > f32::EPSILON {
            layout.set_text_indent(amount, IndentOptions { hanging, each_line });
        }
        let optimized = request.line_break_strategy == TextLineBreakStrategy::Optimized
            && request.width.is_some_and(|width| {
                crate::linebreak::parley::break_optimized(
                    &mut layout,
                    request.text,
                    width,
                    request.indent,
                )
                .is_some()
            });
        if !optimized {
            layout.break_all_lines(request.width);
        }
        layout.align(
            match request.alignment {
                TextAlignment::Start => Alignment::Start,
                TextAlignment::Center => Alignment::Center,
                TextAlignment::End => Alignment::End,
                TextAlignment::Justify => Alignment::Justify,
            },
            AlignmentOptions::default(),
        );
        snapshot(&Arc::new(layout))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_ranges(text: &str, strategy: TextLineBreakStrategy) -> Vec<std::ops::Range<usize>> {
        let mut engine = LegacyParleyTextEngine::default();
        let mut request = TextLayoutRequest::plain(text, 18.0, Some(180.0));
        request.alignment = TextAlignment::Justify;
        request.line_break_strategy = strategy;
        let layout = engine.shape(&request);
        (0..layout.line_count())
            .map(|index| {
                layout
                    .line_at(index)
                    .expect("dense line ids")
                    .text_range
                    .clone()
            })
            .collect()
    }

    #[test]
    fn unsupported_scripts_fall_back_to_the_same_greedy_breaks() {
        for text in [
            "普通中文正文不应进入第一阶段仅支持英文的优化断行算法。",
            "مرحبا بالعالم هذا نص عربي لا يدخل خوارزمية كنوث بلاس الحالية.",
        ] {
            assert_eq!(
                line_ranges(text, TextLineBreakStrategy::Optimized),
                line_ranges(text, TextLineBreakStrategy::Greedy),
                "unsupported prose must retain Parley's greedy fallback"
            );
        }
    }
}
