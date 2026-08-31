//! Adapter between Parley's shaped clusters and the paragraph optimizer.

use std::ops::Range;

use icu_segmenter::{LineSegmenter, options::LineBreakOptions};
use parley::{InlineBox, InlineBoxKind, Layout};
use unicode_script::{Script, UnicodeScript};

use crate::{TextBaseline, TextBrush};

use super::knuth_plass::{self, ClusterItem, LineBreak, ParagraphOptions};

const MIXED_SCRIPT_SPACING_EM: f32 = 0.25;
const MIXED_SCRIPT_SHRINK_EM: f32 = 0.125;

/// A spacing delta applied to every shaped cluster in this byte range.
#[derive(Clone, Debug, PartialEq)]
pub struct SpacingAdjustment {
    pub range: Range<usize>,
    pub amount: f32,
}

/// Breakpoints and range styles needed to reproduce an optimized paragraph.
#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphPlan {
    pub lines: Vec<LineBreak>,
    pub adjustments: Vec<SpacingAdjustment>,
}

/// One renderer-shaped text cluster supplied to the shared paragraph optimizer.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredCluster {
    pub range: Range<usize>,
    pub advance: f32,
    pub em: f32,
    pub ordinary_baseline: bool,
    pub footnote_reference: bool,
}

/// Reads exact shaped-cluster metrics, maps ICU4X UAX #14 boundaries onto
/// them, and computes both line breaks and cluster spacing for a paragraph.
pub(crate) fn plan_optimized(
    layout: &mut Layout<TextBrush>,
    text: &str,
    column_width: f32,
    first_line_indent: f32,
    default_em: f32,
) -> Option<ParagraphPlan> {
    let has_in_flow_box = layout
        .inline_boxes()
        .iter()
        .any(|inline_box| inline_box.kind == InlineBoxKind::InFlow);
    if (text.is_empty() && !has_in_flow_box)
        || !column_width.is_finite()
        || column_width <= 0.0
        || !default_em.is_finite()
        || default_em <= 0.0
        || !is_supported_ltr_prose(text)
        || layout.is_rtl()
    {
        return None;
    }

    layout.break_all_lines(None);
    if layout.len() != 1 {
        return None;
    }
    let line = layout.get(0)?;
    let mut clusters = Vec::new();
    for run in line.runs() {
        if run.is_rtl() {
            return None;
        }
        let em = run.font_size();
        for cluster in run.clusters() {
            if cluster.is_hard_line_break() {
                return None;
            }
            let range = cluster.text_range();
            let brush = cluster.first_style().brush;
            clusters.push(MeasuredCluster {
                range,
                advance: cluster.advance(),
                em,
                ordinary_baseline: brush.baseline == TextBaseline::Normal,
                footnote_reference: brush.footnote_reference,
            });
        }
    }

    plan_measured_content(
        text,
        &clusters,
        layout.inline_boxes(),
        column_width,
        first_line_indent,
        default_em,
    )
}

/// Applies the same Unicode-aware Knuth--Plass paragraph optimization used by
/// the reader to clusters measured by another renderer.
#[must_use]
pub fn plan_measured_text(
    text: &str,
    measured: &[MeasuredCluster],
    column_width: f32,
    first_line_indent: f32,
    default_em: f32,
) -> Option<ParagraphPlan> {
    plan_measured_content(
        text,
        measured,
        &[],
        column_width,
        first_line_indent,
        default_em,
    )
}

fn plan_measured_content(
    text: &str,
    measured: &[MeasuredCluster],
    inline_boxes: &[InlineBox],
    column_width: f32,
    first_line_indent: f32,
    default_em: f32,
) -> Option<ParagraphPlan> {
    let mut in_flow_boxes = inline_boxes
        .iter()
        .filter(|inline_box| inline_box.kind == InlineBoxKind::InFlow)
        .collect::<Vec<_>>();
    in_flow_boxes.sort_by_key(|inline_box| inline_box.index);
    if (text.is_empty() && in_flow_boxes.is_empty())
        || (measured.is_empty() && in_flow_boxes.is_empty())
        || !column_width.is_finite()
        || column_width <= 0.0
        || !default_em.is_finite()
        || default_em <= 0.0
        || !is_supported_ltr_prose(text)
    {
        return None;
    }
    let legal_breaks = LineSegmenter::new_auto(LineBreakOptions::default())
        .segment_str(text)
        .collect::<Vec<_>>();
    let mut expected_start = 0;
    let mut text_clusters = Vec::with_capacity(measured.len());
    for cluster in measured {
        if cluster.range.start != expected_start || cluster.range.end <= cluster.range.start {
            return None;
        }
        let source = text.get(cluster.range.clone())?;
        let mut characters = source.chars();
        let first = characters.next()?;
        let last = characters.last().unwrap_or(first);
        text_clusters.push(ShapedCluster {
            range: cluster.range.clone(),
            advance: cluster.advance,
            em: cluster.em,
            first,
            last,
            is_space: source.chars().all(|character| character.is_whitespace()),
            is_breakable_space: source.chars().all(is_breakable_space),
            break_after: legal_breaks.binary_search(&cluster.range.end).is_ok(),
            ordinary_baseline: cluster.ordinary_baseline,
            footnote_reference: cluster.footnote_reference,
            inline_box: false,
        });
        expected_start = cluster.range.end;
    }
    if expected_start != text.len() {
        return None;
    }

    let mut clusters = Vec::with_capacity(text_clusters.len() + in_flow_boxes.len());
    let mut text_clusters = text_clusters.into_iter().peekable();
    for inline_box in in_flow_boxes {
        if inline_box.index > text.len()
            || !text.is_char_boundary(inline_box.index)
            || !inline_box.width.is_finite()
            || inline_box.width < 0.0
        {
            return None;
        }
        while text_clusters
            .peek()
            .is_some_and(|cluster| cluster.range.end <= inline_box.index)
        {
            clusters.push(text_clusters.next()?);
        }
        clusters.push(ShapedCluster {
            range: inline_box.index..inline_box.index,
            advance: inline_box.width,
            em: default_em,
            first: '\u{fffc}',
            last: '\u{fffc}',
            is_space: false,
            is_breakable_space: false,
            break_after: legal_breaks.binary_search(&inline_box.index).is_ok(),
            ordinary_baseline: false,
            footnote_reference: false,
            inline_box: true,
        });
    }
    clusters.extend(text_clusters);
    clusters.last_mut()?.break_after = true;

    let items = shaped_cluster_items(&clusters)?;
    let cluster_total = u32::try_from(clusters.len()).ok()?;
    let mut options = ParagraphOptions::new(column_width, default_em);
    options.first_line_indent = first_line_indent.max(0.0);
    let optimized = knuth_plass::optimize_clusters(&items, options)?;
    if optimized
        .lines
        .iter()
        .map(|line| line.cluster_count)
        .sum::<u32>()
        != cluster_total
    {
        return None;
    }
    let adjustments = merge_spacing_adjustments(&clusters, &optimized.adjustments);
    Some(ParagraphPlan {
        lines: optimized.lines,
        adjustments,
    })
}

/// Forces a rebuilt layout to use the selected cluster counts.
pub(crate) fn apply_breaks(
    layout: &mut Layout<TextBrush>,
    lines: &[LineBreak],
    column_width: f32,
) -> Option<()> {
    if lines.is_empty() || !column_width.is_finite() || column_width <= 0.0 {
        return None;
    }
    layout.break_all_lines(None);
    let mut breaker = layout.break_lines();
    for line in lines {
        breaker.break_next_with_length(line.cluster_count)?;
        breaker.set_prior_line_width(column_width);
    }
    breaker.finish();
    (layout.len() == lines.len()).then_some(())
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct ShapedCluster {
    range: Range<usize>,
    advance: f32,
    em: f32,
    first: char,
    last: char,
    is_space: bool,
    is_breakable_space: bool,
    break_after: bool,
    ordinary_baseline: bool,
    footnote_reference: bool,
    inline_box: bool,
}

fn shaped_cluster_items(clusters: &[ShapedCluster]) -> Option<Vec<ClusterItem>> {
    let mut items = Vec::with_capacity(clusters.len());
    for (index, cluster) in clusters.iter().enumerate() {
        if !cluster.advance.is_finite()
            || cluster.advance < 0.0
            || !cluster.em.is_finite()
            || cluster.em <= 0.0
        {
            return None;
        }
        let punctuation = punctuation_metrics(cluster);
        let mut boundary_width_after = 0.0;
        let mut boundary_shrink_after = 0.0;
        let mut justifiable_after = false;
        if let Some(next) = clusters.get(index + 1) {
            if should_add_mixed_script_spacing(cluster, next) {
                boundary_width_after += MIXED_SCRIPT_SPACING_EM * cluster.em;
                boundary_shrink_after += MIXED_SCRIPT_SHRINK_EM * cluster.em;
            }
            let next_punctuation = punctuation_metrics(next);
            if punctuation.is_punctuation() && next_punctuation.is_punctuation() {
                let punctuation_compression =
                    (punctuation.trailing + next_punctuation.leading).min(cluster.em * 0.5);
                boundary_width_after -= punctuation_compression;
            }
            justifiable_after = is_cjk_justification_char(cluster.last)
                && is_cjk_justification_char(next.first)
                && !cluster.footnote_reference
                && !next.footnote_reference;
        }
        let (stretch, shrink) = if cluster.is_space {
            (cluster.advance * 0.5, cluster.advance * 0.33)
        } else {
            (0.0, 0.0)
        };
        items.push(ClusterItem {
            width: cluster.advance,
            stretch,
            shrink,
            boundary_width_after,
            boundary_shrink_after,
            line_end_adjustment: -punctuation.trailing,
            clusters: 1,
            break_after: cluster.break_after,
            trimmable: cluster.is_breakable_space,
            justifiable_after,
        });
    }
    Some(items)
}

fn merge_spacing_adjustments(
    clusters: &[ShapedCluster],
    adjustments: &[f32],
) -> Vec<SpacingAdjustment> {
    const EPSILON: f32 = 0.000_1;
    let mut merged: Vec<SpacingAdjustment> = Vec::new();
    for (cluster, amount) in clusters.iter().zip(adjustments.iter().copied()) {
        if cluster.inline_box || cluster.range.is_empty() || amount.abs() <= EPSILON {
            continue;
        }
        if let Some(previous) = merged.last_mut()
            && previous.range.end == cluster.range.start
            && (previous.amount - amount).abs() <= EPSILON
        {
            previous.range.end = cluster.range.end;
        } else {
            merged.push(SpacingAdjustment {
                range: cluster.range.clone(),
                amount,
            });
        }
    }
    merged
}

#[derive(Clone, Copy, Debug, Default)]
struct PunctuationMetrics {
    leading: f32,
    trailing: f32,
}

impl PunctuationMetrics {
    fn is_punctuation(self) -> bool {
        self.leading > f32::EPSILON || self.trailing > f32::EPSILON
    }
}

fn punctuation_metrics(cluster: &ShapedCluster) -> PunctuationMetrics {
    if cluster.inline_box {
        return PunctuationMetrics::default();
    }
    // Curly quotes in proportional Latin fonts should not be treated as
    // full-width CJK punctuation merely because they share a code point.
    let full_width_quote = cluster.advance >= cluster.em * 0.75;
    let leading = if is_opening_punctuation(cluster.first, full_width_quote) {
        cluster.advance * 0.5
    } else if is_centered_punctuation(cluster.first) {
        cluster.advance * 0.25
    } else {
        0.0
    };
    let trailing = if is_closing_punctuation(cluster.last, full_width_quote) {
        cluster.advance * 0.5
    } else if is_centered_punctuation(cluster.last) {
        cluster.advance * 0.25
    } else {
        0.0
    };
    PunctuationMetrics { leading, trailing }
}

fn should_add_mixed_script_spacing(left: &ShapedCluster, right: &ShapedCluster) -> bool {
    if left.inline_box
        || right.inline_box
        || !left.ordinary_baseline
        || !right.ordinary_baseline
        || left.footnote_reference
        || right.footnote_reference
    {
        return false;
    }
    let left_cjk = is_cjk_script(left.last);
    let right_cjk = is_cjk_script(right.first);
    let left_western = is_western_script(left.last);
    let right_western = is_western_script(right.first);
    left_cjk && right_western || left_western && right_cjk
}

fn is_breakable_space(character: char) -> bool {
    character.is_whitespace() && character != '\u{00a0}'
}

fn is_cjk_script(character: char) -> bool {
    matches!(
        character.script(),
        Script::Han | Script::Hiragana | Script::Katakana
    ) || character == '\u{30fc}'
}

fn is_western_script(character: char) -> bool {
    matches!(
        character.script(),
        Script::Latin | Script::Greek | Script::Cyrillic
    ) || character.is_ascii_digit()
        || matches!(character, '#' | '$' | '%' | '&')
}

fn is_cjk_justification_char(character: char) -> bool {
    is_cjk_script(character)
        || is_opening_punctuation(character, true)
        || is_closing_punctuation(character, true)
        || is_centered_punctuation(character)
}

fn is_opening_punctuation(character: char, full_width_quote: bool) -> bool {
    matches!(
        character,
        '\u{300a}'
            | '\u{3008}'
            | '\u{ff08}'
            | '\u{300e}'
            | '\u{300c}'
            | '\u{3010}'
            | '\u{3016}'
            | '\u{3014}'
            | '\u{ff3b}'
            | '\u{ff5b}'
    ) || full_width_quote && matches!(character, '\u{201c}' | '\u{2018}')
}

fn is_closing_punctuation(character: char, full_width_quote: bool) -> bool {
    matches!(
        character,
        '\u{ff0c}'
            | '\u{ff0e}'
            | '\u{3002}'
            | '\u{3001}'
            | '\u{ff1a}'
            | '\u{ff1b}'
            | '\u{300b}'
            | '\u{3009}'
            | '\u{ff09}'
            | '\u{300f}'
            | '\u{300d}'
            | '\u{3011}'
            | '\u{3017}'
            | '\u{3015}'
            | '\u{ff3d}'
            | '\u{ff5d}'
            | '\u{ff1f}'
            | '\u{ff01}'
    ) || full_width_quote && matches!(character, '\u{201d}' | '\u{2019}')
}

fn is_centered_punctuation(character: char) -> bool {
    matches!(character, '\u{00b7}' | '\u{30fb}')
}

pub(crate) fn is_cjk(character: char) -> bool {
    is_cjk_script(character)
        || matches!(
            character as u32,
            0x2E80..=0x2FFF
                | 0x31F0..=0x31FF
                | 0x3400..=0x4DBF
                | 0x4E00..=0x9FFF
                | 0xAC00..=0xD7AF
                | 0xF900..=0xFAFF
                | 0x20000..=0x323AF
        )
}

fn is_supported_ltr_prose(text: &str) -> bool {
    text.chars().all(|character| {
        !matches!(character, '\n' | '\r' | '\t')
            && !is_rtl_codepoint(character)
            && (!character.is_control() || matches!(character, '\u{200b}' | '\u{2060}'))
    })
}

fn is_rtl_codepoint(character: char) -> bool {
    matches!(
        character as u32,
        0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x1EE00..=0x1EEFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley::{FontContext, InlineBox, InlineBoxKind, LayoutContext, StyleProperty};

    fn layout_for(text: &str, font_size: f32) -> Layout<TextBrush> {
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::<TextBrush>::new();
        let mut builder = layout_context.ranged_builder(&mut font_context, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(font_size));
        builder.build(text)
    }

    #[test]
    fn accepts_cjk_and_uses_unicode_line_boundaries() {
        let text = "\u{4e2d}\u{6587}\u{6392}\u{7248}\u{9700}\u{8981}\u{6b63}\u{786e}\u{7684}\u{65ad}\u{884c}\u{673a}\u{4f1a}";
        let mut layout = layout_for(text, 18.0);
        let plan = plan_optimized(&mut layout, text, 75.0, 0.0, 18.0)
            .expect("CJK prose should use optimized line breaking");

        assert!(plan.lines.len() > 1);
        assert_eq!(
            plan.lines
                .iter()
                .map(|line| line.cluster_count)
                .sum::<u32>(),
            u32::try_from(text.chars().count()).unwrap()
        );
        assert!(!plan.adjustments.is_empty());
    }

    #[test]
    fn adds_spacing_at_cjk_western_boundaries() {
        let text = "\u{4e2d}\u{6587}abc\u{4e2d}\u{6587}";
        let mut layout = layout_for(text, 18.0);
        let plan = plan_optimized(&mut layout, text, 400.0, 0.0, 18.0)
            .expect("mixed prose should produce a plan");

        assert_eq!(plan.lines.len(), 1);
        assert!(
            plan.adjustments
                .iter()
                .filter(|item| item.amount > 0.0)
                .count()
                >= 2
        );
    }

    #[test]
    fn rebuilt_layout_reproduces_justified_cjk_lines() {
        let text = "\u{4e2d}\u{6587}\u{6392}\u{7248}\u{9700}\u{8981}\u{6b63}\u{786e}\u{7684}\u{65ad}\u{884c}\u{673a}\u{4f1a}";
        let width = 75.0;
        let mut natural = layout_for(text, 18.0);
        let plan = plan_optimized(&mut natural, text, width, 0.0, 18.0)
            .expect("CJK prose should produce a plan");

        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::<TextBrush>::new();
        let mut builder = layout_context.ranged_builder(&mut font_context, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        for adjustment in &plan.adjustments {
            builder.push(
                StyleProperty::LetterSpacing(adjustment.amount),
                adjustment.range.clone(),
            );
        }
        let mut adjusted = builder.build(text);
        apply_breaks(&mut adjusted, &plan.lines, width).expect("breaks should apply");

        assert_eq!(adjusted.len(), plan.lines.len());
        assert!(
            adjusted
                .lines()
                .take(adjusted.len().saturating_sub(1))
                .all(|line| (line.metrics().inline_max_coord - width).abs() < 0.01)
        );
    }

    #[test]
    fn compresses_cjk_punctuation_without_rewriting_text() {
        let text = "\u{4e2d}\u{6587}\u{ff0c}\u{3002}\u{6b63}\u{6587}";
        let mut layout = layout_for(text, 18.0);
        let plan = plan_optimized(&mut layout, text, 400.0, 0.0, 18.0)
            .expect("CJK punctuation should remain optimizable");

        assert!(plan.adjustments.iter().any(|item| item.amount < 0.0));
        assert_eq!(
            plan.lines
                .iter()
                .map(|line| line.cluster_count)
                .sum::<u32>(),
            u32::try_from(text.chars().count()).unwrap()
        );
    }

    #[test]
    fn keeps_non_breaking_spaces_out_of_break_candidates() {
        let text = "keep\u{00a0}together across ordinary words";
        let mut layout = layout_for(text, 18.0);
        let plan = plan_optimized(&mut layout, text, 180.0, 0.0, 18.0)
            .expect("NBSP prose should remain optimizable");
        let nbsp_cluster =
            u32::try_from(text[..text.find('\u{00a0}').unwrap()].chars().count()).unwrap() + 1;

        assert!(
            plan.lines
                .iter()
                .all(|line| line.breakpoint != nbsp_cluster)
        );
    }

    #[test]
    fn optimized_breaks_count_inline_boxes_as_indivisible_items() {
        let text = "中文排版需要正确的断行机会";
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::<TextBrush>::new();
        let mut builder = layout_context.ranged_builder(&mut font_context, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_inline_box(InlineBox {
            id: 7,
            kind: InlineBoxKind::InFlow,
            index: "中文排版".len(),
            width: 36.0,
            height: 18.0,
        });
        let mut layout = builder.build(text);
        let plan = plan_optimized(&mut layout, text, 75.0, 0.0, 18.0)
            .expect("inline boxes should remain optimizable");

        assert!(plan.lines.len() > 1);
        assert_eq!(
            plan.lines
                .iter()
                .map(|line| line.cluster_count)
                .sum::<u32>(),
            u32::try_from(text.chars().count()).unwrap() + 1
        );
        assert!(
            plan.adjustments
                .iter()
                .all(|adjustment| !adjustment.range.is_empty())
        );
        apply_breaks(&mut layout, &plan.lines, 75.0).expect("breaks should include the inline box");
    }

    #[test]
    fn rejects_rtl_and_hard_breaks() {
        assert!(!is_supported_ltr_prose("abc\u{05d0}\u{05d1}\u{05d2}"));
        assert!(!is_supported_ltr_prose("two\tcolumns"));
        assert!(!is_supported_ltr_prose("two\nparagraphs"));
    }
}
