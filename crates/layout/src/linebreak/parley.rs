//! Adapter between Parley's shaped clusters and the pure paragraph optimizer.

use parley::{Layout, style::Brush};

use super::knuth_plass::LineBreak;
use super::measured::{self, LineWidthProfile, MeasuredCluster, ShrinkSupport};
use super::unicode;
use crate::text::TextIndent;

/// Shapes have already been constructed when this function is called. The
/// temporary unbounded line only exposes Parley's public run/cluster view; the
/// same shaped data is then re-broken at the optimized cluster counts.
pub(crate) fn break_optimized<B: Brush>(
    layout: &mut Layout<B>,
    text: &str,
    column_width: f32,
    indent: TextIndent,
) -> Option<Vec<LineBreak>> {
    if text.is_empty()
        || !column_width.is_finite()
        || column_width <= 0.0
        || !unicode::supports_phase_one_optimized(text)
        || !layout.inline_boxes().is_empty()
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
        for cluster in run.clusters() {
            if cluster.is_hard_line_break() || cluster.is_emoji() {
                return None;
            }
            let range = cluster.text_range();
            let source = text.get(range.clone())?;
            clusters.push(MeasuredCluster {
                text_range: range,
                advance: cluster.advance(),
                backend_count: 1,
                is_space: source == " " && cluster.is_space_or_nbsp(),
            });
        }
    }
    // Parley 0.11 justification only expands positive free space. It does not
    // shrink spaces for an overfull line, so accepting a negative ratio here
    // would leave the line protruding past the column edge.
    let (first_indent, continuation_indent) = if indent.hanging {
        (0.0, indent.amount)
    } else {
        (indent.amount, 0.0)
    };
    let measured = measured::optimize_ltr(
        text,
        &clusters,
        LineWidthProfile::from_indents(column_width, first_indent, continuation_indent)?,
        ShrinkSupport::Unsupported,
    )?;

    let mut breaker = layout.break_lines();
    for selected in &measured {
        breaker.break_next_with_length(selected.line.cluster_count)?;
        breaker.set_prior_line_width(column_width);
    }
    breaker.finish();
    Some(measured.into_iter().map(|selected| selected.line).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::legacy_parley::TextBrush;
    use parley::{
        Alignment, AlignmentOptions, FontContext, IndentOptions, LayoutContext, StyleProperty,
    };

    #[test]
    fn only_accepts_single_breakable_ascii_spaces() {
        let text = "one two";
        let clusters = [
            MeasuredCluster {
                text_range: 0..3,
                advance: 20.0,
                backend_count: 3,
                is_space: false,
            },
            MeasuredCluster {
                text_range: 3..4,
                advance: 5.0,
                backend_count: 1,
                is_space: true,
            },
            MeasuredCluster {
                text_range: 4..7,
                advance: 18.0,
                backend_count: 3,
                is_space: false,
            },
        ];
        assert!(
            measured::optimize_ltr(
                text,
                &clusters,
                LineWidthProfile::uniform(50.0),
                ShrinkSupport::Unsupported,
            )
            .is_some()
        );
    }

    #[test]
    fn rejects_scripts_outside_phase_one_scope() {
        assert!(unicode::supports_phase_one_optimized(
            "Ordinary English prose."
        ));
        assert!(!unicode::supports_phase_one_optimized("普通正文"));
        assert!(!unicode::supports_phase_one_optimized("مرحبا"));
        assert!(!unicode::supports_phase_one_optimized("two\tcolumns"));
    }

    #[test]
    fn forces_optimized_cluster_breakpoints_back_into_parley() {
        let text = "Balanced paragraph spacing can improve the color of ordinary English prose across several lines.";
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::<TextBrush>::new();
        let mut builder = layout_context.ranged_builder(&mut font_context, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        let mut layout = builder.build(text);

        let selected = break_optimized(&mut layout, text, 240.0, TextIndent::default())
            .expect("ordinary shaped LTR prose should use the optimized adapter");

        assert!(selected.len() > 1);
        assert!(selected.iter().all(|line| line.adjustment_ratio >= 0.0));
        assert_eq!(layout.len(), selected.len());
        assert_eq!(
            selected.iter().map(|line| line.cluster_count).sum::<u32>(),
            u32::try_from(text.chars().count()).unwrap()
        );
        assert!(
            layout
                .lines()
                .all(|line| (line.metrics().inline_max_coord - 240.0).abs() < 0.01)
        );
    }

    #[test]
    fn hanging_indent_uses_the_narrower_continuation_measure() {
        let text = "List content should retain a full first line and narrower continuation lines when optimized.";
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::<TextBrush>::new();
        let mut builder = layout_context.ranged_builder(&mut font_context, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        let mut layout = builder.build(text);
        let indent = TextIndent {
            amount: 24.0,
            hanging: true,
            each_line: false,
        };
        layout.set_text_indent(
            indent.amount,
            IndentOptions {
                hanging: true,
                each_line: false,
            },
        );

        let selected = break_optimized(&mut layout, text, 240.0, indent)
            .expect("ordinary hanging LTR prose should optimize");
        layout.align(Alignment::Start, AlignmentOptions::default());

        assert!(selected.len() > 1);
        assert!(layout.get(0).unwrap().metrics().offset.abs() < 0.01);
        assert!(
            layout
                .lines()
                .skip(1)
                .all(|line| (line.metrics().offset - indent.amount).abs() < 0.01)
        );
    }
}
