//! Adapter between Parley's shaped clusters and the pure paragraph optimizer.

use parley::{Layout, style::Brush};

use super::knuth_plass::{self, Item, LineBreak, Options};

/// Shapes have already been constructed when this function is called. The
/// temporary unbounded line only exposes Parley's public run/cluster view; the
/// same shaped data is then re-broken at the optimized cluster counts.
pub(crate) fn break_optimized<B: Brush>(
    layout: &mut Layout<B>,
    text: &str,
    column_width: f32,
    first_line_indent: f32,
) -> Option<Vec<LineBreak>> {
    if text.is_empty()
        || !column_width.is_finite()
        || column_width <= 0.0
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
            let source = text.get(range)?;
            clusters.push(ShapedCluster {
                advance: cluster.advance(),
                count: 1,
                is_ascii_space: source == " " && cluster.is_space_or_nbsp(),
                is_line_boundary: cluster.is_word_boundary(),
            });
        }
    }
    let cluster_total = u32::try_from(clusters.len()).ok()?;
    let mut items = shaped_items(&clusters)?;
    if first_line_indent > 0.0 {
        let Item::Box { width, .. } = items.first_mut()? else {
            return None;
        };
        *width += first_line_indent;
    }
    // Parley 0.11 justification only expands positive free space. It does not
    // shrink spaces for an overfull line, so accepting a negative ratio here
    // would leave the line protruding past the column edge.
    let lines = knuth_plass::optimize(&items, Options::new(column_width).without_shrink())?;
    if lines.iter().map(|line| line.cluster_count).sum::<u32>() != cluster_total {
        return None;
    }

    let mut breaker = layout.break_lines();
    for line in &lines {
        breaker.break_next_with_length(line.cluster_count)?;
        breaker.set_prior_line_width(column_width);
    }
    breaker.finish();
    Some(lines)
}

#[derive(Clone, Copy, Debug)]
struct ShapedCluster {
    advance: f32,
    count: u32,
    is_ascii_space: bool,
    is_line_boundary: bool,
}

fn shaped_items(clusters: &[ShapedCluster]) -> Option<Vec<Item>> {
    let mut items = Vec::new();
    let mut word_width = 0.0;
    let mut word_clusters = 0_u32;
    let mut pending_space = None;

    for cluster in clusters {
        if !cluster.advance.is_finite() || cluster.advance < 0.0 {
            return None;
        }
        if cluster.is_ascii_space {
            if word_clusters == 0 || pending_space.is_some() {
                return None;
            }
            pending_space = Some(*cluster);
            continue;
        }

        if let Some(space) = pending_space.take() {
            // Parley places a soft line boundary on the first cluster after
            // ordinary breakable whitespace. Reject styles/rules that do not.
            if !cluster.is_line_boundary {
                return None;
            }
            items.push(Item::Box {
                width: word_width,
                clusters: word_clusters,
            });
            items.push(Item::Glue {
                width: space.advance,
                stretch: space.advance * 0.5,
                shrink: space.advance * 0.33,
                clusters: space.count,
            });
            word_width = 0.0;
            word_clusters = 0;
        }
        word_width += cluster.advance;
        word_clusters = word_clusters.checked_add(cluster.count)?;
    }

    if pending_space.is_some() || word_clusters == 0 {
        return None;
    }
    items.push(Item::Box {
        width: word_width,
        clusters: word_clusters,
    });
    Some(items)
}

fn is_supported_ltr_prose(text: &str) -> bool {
    text.chars().all(|character| {
        character == ' '
            || (!character.is_control()
                && !character.is_whitespace()
                && !is_cjk(character)
                && !is_rtl_codepoint(character))
    })
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x2E80..=0x2FFF
            | 0x3040..=0x30FF
            | 0x31F0..=0x31FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0x20000..=0x323AF
    )
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
    use crate::TextBrush;
    use parley::{FontContext, LayoutContext, StyleProperty};

    #[test]
    fn only_accepts_single_breakable_ascii_spaces() {
        let clusters = [
            ShapedCluster {
                advance: 20.0,
                count: 1,
                is_ascii_space: false,
                is_line_boundary: false,
            },
            ShapedCluster {
                advance: 5.0,
                count: 1,
                is_ascii_space: true,
                is_line_boundary: false,
            },
            ShapedCluster {
                advance: 18.0,
                count: 1,
                is_ascii_space: false,
                is_line_boundary: true,
            },
        ];
        assert_eq!(shaped_items(&clusters).map(|items| items.len()), Some(3));
    }

    #[test]
    fn rejects_scripts_outside_phase_one_scope() {
        assert!(is_supported_ltr_prose("Ordinary English prose."));
        assert!(!is_supported_ltr_prose("普通正文"));
        assert!(!is_supported_ltr_prose("مرحبا"));
        assert!(!is_supported_ltr_prose("two\tcolumns"));
    }

    #[test]
    fn forces_optimized_cluster_breakpoints_back_into_parley() {
        let text = "Balanced paragraph spacing can improve the color of ordinary English prose across several lines.";
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::<TextBrush>::new();
        let mut builder = layout_context.ranged_builder(&mut font_context, text, 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        let mut layout = builder.build(text);

        let selected = break_optimized(&mut layout, text, 240.0, 0.0)
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
}
