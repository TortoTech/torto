//! GPUI text-system adapter used by the migration probe and staged desktop UI.
//!
//! GPUI shapes each request, then the adapter snapshots neutral line, cluster
//! and paint geometry. No GPUI object is retained by `LayoutFrame`.

use std::{borrow::Cow, ops::Range, sync::Arc};

use gpui::{
    FontStyle, FontWeight, SharedString, TextRun, UnderlineStyle, WindowTextSystem, WrappedLine,
    font, px, rgba,
};
use rebook_layout::linebreak::{
    measured::{self, LineWidthProfile, MeasuredCluster, ShrinkSupport},
    unicode::{ParagraphScript, classify_paragraph},
};
use rebook_layout::text::{
    TextCluster, TextEngine, TextFontBlob, TextIndent, TextLayoutRequest, TextLayoutStore,
    TextLineBreak, TextLineBreakStrategy, TextLineMetrics, TextLineSnapshot, TextNativeRun,
    TextPaintItem, legacy_parley::LegacyParleyTextEngine,
};
use rebook_publication::{Rgba, TextAlignment, TextBaseline};

/// Minimal GPUI-backed shaper for phase-gated LTR/CJK probe text.
///
/// The current migration slice supports ordinary LTR/CJK styled runs, uniform
/// paragraph font sizes, first-line indentation, and hanging list indentation.
/// BiDi-specific geometry, mixed font sizes and custom inline boxes remain on
/// the legacy adapter until the core contract grows the required semantics.
/// Application-provided font bytes are registered with both backends; only a
/// failed GPUI registration closes the native gate.
pub struct GpuiTextEngine {
    text_system: Arc<WindowTextSystem>,
    legacy: LegacyParleyTextEngine,
    gpui_font_registration_failed: bool,
}

impl GpuiTextEngine {
    #[must_use]
    pub fn new(text_system: Arc<WindowTextSystem>) -> Self {
        Self {
            text_system,
            legacy: LegacyParleyTextEngine::default(),
            gpui_font_registration_failed: false,
        }
    }
}

impl TextEngine for GpuiTextEngine {
    fn register_font(&mut self, font: &TextFontBlob) {
        self.legacy.register_font(font);
        if font.is_empty()
            || self
                .text_system
                .add_fonts(vec![Cow::Owned(font.as_bytes().to_vec())])
                .is_err()
        {
            self.gpui_font_registration_failed = true;
        }
    }

    fn shape(&mut self, request: &TextLayoutRequest<'_>) -> TextLayoutStore {
        let style_runs = native_style_runs(request);
        if !supports_native_request(request, &style_runs, self.gpui_font_registration_failed) {
            return self.legacy.shape(request);
        }
        let font_size = uniform_font_size(&style_runs).unwrap_or(request.font_size);
        let font_family = primary_font_family(request.font_family).map(Arc::<str>::from);
        let gpui_runs = style_runs
            .iter()
            .map(|run| gpui_text_run(run, font_family.as_deref()))
            .collect::<Vec<_>>();
        let Some(shaped) = shape_probe_lines(&self.text_system, request, font_size, &gpui_runs)
        else {
            return self.legacy.shape(request);
        };
        let shaped_hard_lines = shaped.lines;
        let selected_ranges = shaped.selected_ranges;

        let mut lines = Vec::new();
        let mut hard_line_start = 0;
        let mut block_cursor = 0.0;
        for (hard_line_index, shaped) in shaped_hard_lines.into_iter().enumerate() {
            let ranges = if hard_line_index == 0 {
                selected_ranges
                    .clone()
                    .unwrap_or_else(|| visual_line_ranges(&shaped))
            } else {
                visual_line_ranges(&shaped)
            };
            let visual_line_count = ranges.len();
            let is_last_hard_line = hard_line_index + 1 == request.text.split('\n').count();
            for (visual_line_index, local_range) in ranges.into_iter().enumerate() {
                let line_indent = resolved_line_indent(request.indent, lines.len());
                let start_x = f32::from(shaped.unwrapped_layout.x_for_index(local_range.start));
                let end_x = f32::from(shaped.unwrapped_layout.x_for_index(local_range.end));
                let width = (end_x - start_x).max(0.0);
                let line_height = (f32::from(shaped.ascent()) + f32::from(shaped.descent()))
                    .max(font_size * request.line_height.unwrap_or(1.2));
                let block_min = block_cursor;
                let line_measure = request
                    .width
                    .map(|available| (available - line_indent).max(0.0));
                let offset = line_indent + alignment_offset(request.alignment, line_measure, width);
                let text_range =
                    hard_line_start + local_range.start..hard_line_start + local_range.end;
                let break_kind = if visual_line_index + 1 < visual_line_count {
                    TextLineBreak::Soft
                } else if is_last_hard_line {
                    TextLineBreak::End
                } else {
                    TextLineBreak::Hard
                };
                let inline_max = if break_kind == TextLineBreak::Soft
                    && request.alignment == TextAlignment::Justify
                {
                    line_measure.unwrap_or(width).max(width)
                } else {
                    width
                };
                lines.push(GpuiVisualLine {
                    shaped: shaped.clone(),
                    hard_line_start,
                    local_range,
                    text_range,
                    metrics: TextLineMetrics {
                        block_min,
                        block_max: block_min + line_height,
                        line_height,
                        offset,
                        inline_min: 0.0,
                        inline_max,
                        content_inline_max: width,
                    },
                    start_x,
                    width,
                    break_kind,
                });
                block_cursor += line_height;
            }
            hard_line_start = hard_line_start.saturating_add(shaped.len() + 1);
        }

        let full_width = lines
            .iter()
            .map(|line| line.metrics.offset + line.width)
            .fold(0.0_f32, f32::max);
        let snapshots = lines
            .iter()
            .map(|line| TextLineSnapshot {
                text_range: line.text_range.clone(),
                metrics: line.metrics,
                break_kind: line.break_kind,
                clusters: native_clusters(line),
                items: native_paint_runs(line, &style_runs, font_family.as_ref()),
            })
            .collect();
        TextLayoutStore::from_snapshots(snapshots, full_width)
            .expect("GPUI produced a valid text layout snapshot")
    }
}

struct ShapedProbeLines {
    lines: Vec<WrappedLine>,
    selected_ranges: Option<Vec<Range<usize>>>,
}

fn shape_probe_lines(
    text_system: &WindowTextSystem,
    request: &TextLayoutRequest<'_>,
    font_size: f32,
    runs: &[TextRun],
) -> Option<ShapedProbeLines> {
    let Some(width) = request.width else {
        return Some(ShapedProbeLines {
            lines: shape_probe_text(text_system, request.text, font_size, runs, None),
            selected_ranges: None,
        });
    };
    if !width.is_finite() || width <= 0.0 {
        return None;
    }
    let unwrapped = shape_probe_text(text_system, request.text, font_size, runs, None);
    let [line] = unwrapped.as_slice() else {
        return None;
    };
    let ranges = match request.line_break_strategy {
        TextLineBreakStrategy::Optimized => {
            optimized_line_ranges(line, request).or_else(|| greedy_line_ranges(line, request))
        }
        TextLineBreakStrategy::Greedy => greedy_line_ranges(line, request),
    }?;
    Some(ShapedProbeLines {
        lines: unwrapped,
        selected_ranges: Some(ranges),
    })
}

fn shape_probe_text(
    text_system: &WindowTextSystem,
    text: &str,
    font_size: f32,
    runs: &[TextRun],
    width: Option<f32>,
) -> Vec<WrappedLine> {
    text_system
        .shape_text(
            SharedString::from(text.to_owned()),
            px(font_size),
            runs,
            width.map(px),
            None,
        )
        .expect("GPUI should shape ordinary probe text")
        .into_vec()
}

fn optimized_line_ranges(
    line: &WrappedLine,
    request: &TextLayoutRequest<'_>,
) -> Option<Vec<Range<usize>>> {
    if line.len() != request.text.len() {
        return None;
    }
    let clusters = measured_clusters(line);
    let selected = measured::optimize_ltr(
        request.text,
        &clusters,
        line_width_profile(request)?,
        ShrinkSupport::Unsupported,
    )?;
    selected_line_ranges(selected, request.text.len())
}

fn greedy_line_ranges(
    line: &WrappedLine,
    request: &TextLayoutRequest<'_>,
) -> Option<Vec<Range<usize>>> {
    if line.len() != request.text.len() {
        return None;
    }
    let selected = measured::greedy_uax14(
        request.text,
        &measured_clusters(line),
        line_width_profile(request)?,
    )?;
    selected_line_ranges(selected, request.text.len())
}

fn line_width_profile(request: &TextLayoutRequest<'_>) -> Option<LineWidthProfile> {
    LineWidthProfile::from_indents(
        request.width?,
        resolved_line_indent(request.indent, 0),
        resolved_line_indent(request.indent, 1),
    )
}

fn resolved_line_indent(indent: TextIndent, line_index: usize) -> f32 {
    let applies = if indent.hanging {
        line_index > 0
    } else {
        line_index == 0
    };
    if applies { indent.amount.max(0.0) } else { 0.0 }
}

fn measured_clusters(line: &WrappedLine) -> Vec<MeasuredCluster> {
    char_ranges(&line.text, 0..line.len())
        .into_iter()
        .map(|text_range| {
            let start = f32::from(line.unwrapped_layout.x_for_index(text_range.start));
            let end = f32::from(line.unwrapped_layout.x_for_index(text_range.end));
            MeasuredCluster {
                is_space: line
                    .text
                    .get(text_range.clone())
                    .is_some_and(|text| text == " "),
                text_range,
                advance: (end - start).max(0.0),
                backend_count: 1,
            }
        })
        .collect()
}

fn selected_line_ranges(
    selected: Vec<measured::MeasuredLineBreak>,
    text_len: usize,
) -> Option<Vec<Range<usize>>> {
    let mut start = 0;
    let ranges = selected
        .into_iter()
        .map(|selected| {
            let end = selected.byte_index;
            let range = (end > start && end <= text_len).then_some(start..end)?;
            start = end;
            Some(range)
        })
        .collect::<Option<Vec<_>>>()?;
    (start == text_len).then_some(ranges)
}

#[derive(Clone)]
struct GpuiVisualLine {
    shaped: WrappedLine,
    hard_line_start: usize,
    local_range: Range<usize>,
    text_range: Range<usize>,
    metrics: TextLineMetrics,
    start_x: f32,
    width: f32,
    break_kind: TextLineBreak,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NativeStyle {
    font_size: f32,
    font_weight: f32,
    italic: bool,
    underline: bool,
    color: Rgba,
}

#[derive(Clone, Debug, PartialEq)]
struct NativeStyleRun {
    range: Range<usize>,
    style: NativeStyle,
}

fn native_style_runs(request: &TextLayoutRequest<'_>) -> Vec<NativeStyleRun> {
    let mut boundaries = vec![0, request.text.len()];
    for span in request.spans {
        let start = span.range.start.min(request.text.len());
        let end = span.range.end.min(request.text.len());
        if start < end && request.text.is_char_boundary(start) && request.text.is_char_boundary(end)
        {
            boundaries.extend([start, end]);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let base = NativeStyle {
        font_size: request.font_size,
        font_weight: request.font_weight.unwrap_or(FontWeight::NORMAL.0),
        italic: false,
        underline: false,
        color: request.color,
    };
    let mut runs: Vec<NativeStyleRun> = Vec::new();
    for range in boundaries.windows(2).map(|pair| pair[0]..pair[1]) {
        if range.is_empty() {
            continue;
        }
        let mut style = base;
        for span in request
            .spans
            .iter()
            .filter(|span| span.range.start <= range.start && span.range.end >= range.end)
        {
            style.font_size = span.font_size.unwrap_or(style.font_size);
            style.font_weight = span.font_weight.unwrap_or(style.font_weight);
            style.italic |= span.italic;
            style.underline |= span.underline;
            style.color = span.color.unwrap_or(style.color);
        }
        if let Some(previous) = runs.last_mut()
            && previous.style == style
            && previous.range.end == range.start
        {
            previous.range.end = range.end;
        } else {
            runs.push(NativeStyleRun { range, style });
        }
    }
    runs
}

fn uniform_font_size(runs: &[NativeStyleRun]) -> Option<f32> {
    let first = runs.first()?.style.font_size;
    if runs
        .iter()
        .all(|run| (run.style.font_size - first).abs() <= f32::EPSILON)
    {
        Some(first)
    } else {
        None
    }
}

fn supports_native_request(
    request: &TextLayoutRequest<'_>,
    runs: &[NativeStyleRun],
    gpui_font_registration_failed: bool,
) -> bool {
    !gpui_font_registration_failed
        && request.inline_objects.is_empty()
        && !request.indent.each_line
        && request.indent.amount.is_finite()
        && request.indent.amount >= 0.0
        && request
            .spans
            .iter()
            .all(|span| span.baseline == TextBaseline::Normal && span.footnote_reference_group == 0)
        && uniform_font_size(runs).is_some()
        && matches!(
            classify_paragraph(request.text),
            ParagraphScript::Ltr | ParagraphScript::Cjk | ParagraphScript::LtrCjk
        )
}

fn primary_font_family(stack: Option<&str>) -> Option<String> {
    let stack = stack?.trim();
    if stack.is_empty() {
        return None;
    }
    let mut quote = None;
    let mut end = stack.len();
    for (index, character) in stack.char_indices() {
        match character {
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            ',' if quote.is_none() => {
                end = index;
                break;
            }
            _ => {}
        }
    }
    let family = stack[..end].trim();
    let family = family
        .strip_prefix('"')
        .and_then(|family| family.strip_suffix('"'))
        .or_else(|| {
            family
                .strip_prefix('\'')
                .and_then(|family| family.strip_suffix('\''))
        })
        .unwrap_or(family)
        .trim();
    (!family.is_empty()).then(|| family.to_owned())
}

fn gpui_text_run(run: &NativeStyleRun, font_family: Option<&str>) -> TextRun {
    let mut selected_font = font(font_family.unwrap_or(".SystemUIFont").to_owned());
    selected_font.weight = FontWeight(run.style.font_weight.clamp(100.0, 900.0));
    selected_font.style = if run.style.italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    TextRun {
        len: run.range.len(),
        font: selected_font,
        color: gpui_color(run.style.color).into(),
        background_color: None,
        underline: run.style.underline.then_some(UnderlineStyle {
            thickness: px(1.0),
            color: None,
            wavy: false,
        }),
        strikethrough: None,
    }
}

fn native_paint_runs(
    line: &GpuiVisualLine,
    styles: &[NativeStyleRun],
    font_family: Option<&Arc<str>>,
) -> Arc<[TextPaintItem]> {
    styles
        .iter()
        .filter_map(|run| {
            let start = run.range.start.max(line.text_range.start);
            let end = run.range.end.min(line.text_range.end);
            if start >= end {
                return None;
            }
            let local_start = start.checked_sub(line.hard_line_start)?;
            let x = f32::from(line.shaped.unwrapped_layout.x_for_index(local_start)) - line.start_x
                + line.metrics.offset;
            Some(TextPaintItem::NativeRun(TextNativeRun {
                text_range: start..end,
                x,
                baseline: line.metrics.block_min + f32::from(line.shaped.ascent()),
                font_family: font_family.cloned(),
                font_size: run.style.font_size,
                font_weight: run.style.font_weight,
                italic: run.style.italic,
                underline: run.style.underline,
                color: run.style.color,
            }))
        })
        .collect::<Vec<_>>()
        .into()
}

fn gpui_color(color: Rgba) -> gpui::Rgba {
    rgba(
        u32::from(color.red) << 24
            | u32::from(color.green) << 16
            | u32::from(color.blue) << 8
            | u32::from(color.alpha),
    )
}

fn native_clusters(line: &GpuiVisualLine) -> Arc<[TextCluster]> {
    char_ranges(&line.shaped.text, line.local_range.clone())
        .into_iter()
        .map(|local_range| {
            let inline_start =
                f32::from(line.shaped.unwrapped_layout.x_for_index(local_range.start))
                    - line.start_x
                    + line.metrics.offset;
            let inline_end = f32::from(line.shaped.unwrapped_layout.x_for_index(local_range.end))
                - line.start_x
                + line.metrics.offset;
            TextCluster {
                text_range: line.hard_line_start + local_range.start
                    ..line.hard_line_start + local_range.end,
                inline_start,
                inline_end,
                rtl: false,
            }
        })
        .collect::<Vec<_>>()
        .into()
}

fn visual_line_ranges(line: &WrappedLine) -> Vec<Range<usize>> {
    let mut boundaries = line
        .wrap_boundaries()
        .iter()
        .filter_map(|boundary| {
            line.unwrapped_layout
                .runs
                .get(boundary.run_ix)?
                .glyphs
                .get(boundary.glyph_ix)
                .map(|glyph| glyph.index)
        })
        .filter(|boundary| *boundary > 0 && *boundary < line.len())
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut start = 0;
    boundaries
        .into_iter()
        .chain(std::iter::once(line.len()))
        .map(|end| {
            let range = start..end;
            start = end;
            range
        })
        .collect()
}

fn alignment_offset(alignment: TextAlignment, available: Option<f32>, width: f32) -> f32 {
    let slack = (available.unwrap_or(width) - width).max(0.0);
    match alignment {
        TextAlignment::Center => slack / 2.0,
        TextAlignment::End => slack,
        TextAlignment::Start | TextAlignment::Justify => 0.0,
    }
}

fn char_ranges(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    text.get(range.clone())
        .map(|slice| {
            slice
                .char_indices()
                .map(|(offset, character)| {
                    let start = range.start + offset;
                    start..start + character.len_utf8()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::text::TextStyleSpan;

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }

    #[test]
    fn character_ranges_remain_utf8_aligned() {
        assert_eq!(char_ranges("a中b", 1..5), [1..4, 4..5]);
    }

    #[test]
    fn alignment_offsets_only_consume_positive_slack() {
        assert_close(
            alignment_offset(TextAlignment::Center, Some(100.0), 60.0),
            20.0,
        );
        assert_close(alignment_offset(TextAlignment::End, Some(40.0), 60.0), 0.0);
    }

    #[test]
    fn uniform_heading_style_becomes_a_native_gpui_run() {
        let text = "Styled title";
        let spans = [TextStyleSpan {
            range: 0..text.len(),
            font_size: Some(30.0),
            font_weight: Some(700.0),
            italic: true,
            underline: true,
            color: Some(Rgba {
                red: 12,
                green: 34,
                blue: 56,
                alpha: 255,
            }),
            ..TextStyleSpan::default()
        }];
        let request = TextLayoutRequest {
            spans: &spans,
            ..TextLayoutRequest::plain(text, 20.0, Some(320.0))
        };

        let runs = native_style_runs(&request);
        assert_eq!(runs.len(), 1);
        assert_eq!(uniform_font_size(&runs), Some(30.0));
        assert_close(runs[0].style.font_weight, 700.0);
        assert!(runs[0].style.italic);
        assert!(runs[0].style.underline);
        assert_eq!(runs[0].style.color, spans[0].color.unwrap());
    }

    #[test]
    fn mixed_spans_keep_byte_aligned_native_run_boundaries() {
        let text = "a中b";
        let spans = [TextStyleSpan {
            range: 1..4,
            font_weight: Some(700.0),
            ..TextStyleSpan::default()
        }];
        let request = TextLayoutRequest {
            spans: &spans,
            ..TextLayoutRequest::plain(text, 20.0, None)
        };

        let runs = native_style_runs(&request);
        assert_eq!(
            runs.iter().map(|run| run.range.clone()).collect::<Vec<_>>(),
            [0..1, 1..4, 4..5]
        );
        assert_close(runs[1].style.font_weight, 700.0);
    }

    #[test]
    fn native_support_gate_is_explicit_for_phase_one_requests() {
        let plain = TextLayoutRequest::plain("Ordinary left-to-right prose.", 20.0, Some(320.0));
        let runs = native_style_runs(&plain);
        assert!(supports_native_request(&plain, &runs, false));
        assert!(!supports_native_request(&plain, &runs, true));

        let cjk = TextLayoutRequest::plain("普通中文段落。", 20.0, Some(320.0));
        let runs = native_style_runs(&cjk);
        assert!(supports_native_request(&cjk, &runs, false));

        let hanging = TextLayoutRequest {
            indent: TextIndent {
                amount: 24.0,
                hanging: true,
                each_line: false,
            },
            ..TextLayoutRequest::plain("List content wraps onto another line.", 20.0, Some(320.0))
        };
        let runs = native_style_runs(&hanging);
        assert!(supports_native_request(&hanging, &runs, false));
        assert_eq!(
            line_width_profile(&hanging),
            Some(LineWidthProfile::new(320.0, 296.0))
        );
        assert_close(resolved_line_indent(hanging.indent, 0), 0.0);
        assert_close(resolved_line_indent(hanging.indent, 1), 24.0);

        let mixed = TextLayoutRequest::plain("Latin 与中文混排", 20.0, Some(320.0));
        let runs = native_style_runs(&mixed);
        assert!(supports_native_request(&mixed, &runs, false));

        let mixed_rtl = TextLayoutRequest::plain("Latin مرحبا 中文", 20.0, Some(320.0));
        let runs = native_style_runs(&mixed_rtl);
        assert!(!supports_native_request(&mixed_rtl, &runs, false));

        let rtl = TextLayoutRequest::plain("مرحبا", 20.0, Some(320.0));
        let runs = native_style_runs(&rtl);
        assert!(!supports_native_request(&rtl, &runs, false));

        let mixed_sizes = [TextStyleSpan {
            range: 0..8,
            font_size: Some(28.0),
            ..TextStyleSpan::default()
        }];
        let mixed = TextLayoutRequest {
            spans: &mixed_sizes,
            ..TextLayoutRequest::plain("Mixed font sizes", 20.0, Some(320.0))
        };
        let runs = native_style_runs(&mixed);
        assert!(!supports_native_request(&mixed, &runs, false));

        let raised = [TextStyleSpan {
            range: 0..1,
            baseline: TextBaseline::Superscript,
            ..TextStyleSpan::default()
        }];
        let footnote = TextLayoutRequest {
            spans: &raised,
            ..TextLayoutRequest::plain("1 footnote", 20.0, Some(320.0))
        };
        let runs = native_style_runs(&footnote);
        assert!(!supports_native_request(&footnote, &runs, false));
    }

    #[test]
    fn primary_font_family_parses_quoted_css_stacks() {
        assert_eq!(
            primary_font_family(Some("\"Noto Serif\", serif")),
            Some("Noto Serif".into())
        );
        assert_eq!(
            primary_font_family(Some("Literata, Georgia, serif")),
            Some("Literata".into())
        );
        assert_eq!(primary_font_family(Some("  ")), None);
    }
}
