//! Adapter between Parley's shaped clusters and the paragraph optimizer.

use std::ops::Range;

use icu_segmenter::{LineSegmenter, options::LineBreakOptions};
use parley::Layout;
use unicode_script::{Script, UnicodeScript};

use crate::{TextBaseline, TextBrush};

use super::knuth_plass::{self, ClusterItem, LineBreak, ParagraphOptions};

const MIXED_SCRIPT_SPACING_EM: f32 = 0.25;
const MIXED_SCRIPT_SHRINK_EM: f32 = 0.125;

/// A spacing delta applied to every shaped cluster in this byte range.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpacingAdjustment {
    pub range: Range<usize>,
    pub amount: f32,
}

/// Breakpoints and range styles needed to reproduce an optimized paragraph.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParagraphPlan {
    pub lines: Vec<LineBreak>,
    pub adjustments: Vec<SpacingAdjustment>,
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
    if text.is_empty()
        || !column_width.is_finite()
        || column_width <= 0.0
        || !default_em.is_finite()
        || default_em <= 0.0
        || !is_supported_ltr_prose(text)
        || !layout.inline_boxes().is_empty()
        || layout.is_rtl()
    {
        return None;
    }

    layout.break_all_lines(None);
    if layout.len() != 1 {
        return None;
    }
    let legal_breaks = LineSegmenter::new_auto(LineBreakOptions::default())
        .segment_str(text)
        .collect::<Vec<_>>();
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
            let source = text.get(range.clone())?;
            let mut characters = source.chars();
            let first = characters.next()?;
            let last = characters.last().unwrap_or(first);
            let brush = cluster.first_style().brush;
            clusters.push(ShapedCluster {
                range,
                advance: cluster.advance(),
                em,
                first,
                last,
                is_space: cluster.is_space_or_nbsp(),
                is_breakable_space: source.chars().all(is_breakable_space),
                break_after: false,
                ordinary_baseline: brush.baseline == TextBaseline::Normal,
                footnote_reference: brush.footnote_reference,
            });
        }
    }
    if clusters.is_empty() {
        return None;
    }
    for cluster in &mut clusters {
        cluster.break_after = legal_breaks.binary_search(&cluster.range.end).is_ok();
    }
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
        if amount.abs() <= EPSILON {
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
    if !left.ordinary_baseline
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
    use parley::{FontContext, LayoutContext, StyleProperty};

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
    fn rejects_rtl_and_hard_breaks() {
        assert!(!is_supported_ltr_prose("abc\u{05d0}\u{05d1}\u{05d2}"));
        assert!(!is_supported_ltr_prose("two\tcolumns"));
        assert!(!is_supported_ltr_prose("two\nparagraphs"));
    }
}
