//! Pure adapter from shaped cluster advances to Knuth–Plass boxes and glue.
//!
//! Shaping backends provide only UTF-8 ranges and real advances. UAX #14
//! opportunities, Box/Glue construction and optimized breakpoint selection
//! remain owned by Torto.

use std::ops::Range;

use super::knuth_plass::{self, Item, LineBreak, Options};
use super::unicode;

/// One shaped logical cluster, independent of any font or renderer type.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredCluster {
    pub text_range: Range<usize>,
    pub advance: f32,
    /// Number understood by the shaping backend's forced-break API.
    pub backend_count: u32,
    pub is_space: bool,
}

/// One optimized line with both backend cluster count and authored byte end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasuredLineBreak {
    pub line: LineBreak,
    pub byte_index: usize,
}

/// Whether a shaping backend can realize negative glue adjustment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShrinkSupport {
    Supported,
    Unsupported,
}

/// Available inline measure for the first and continuation lines.
///
/// Text indentation is resolved by adapters into this neutral profile. The
/// line-break core therefore does not need to know CSS/EPUB indent semantics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineWidthProfile {
    pub first: f32,
    pub continuation: f32,
}

impl LineWidthProfile {
    #[must_use]
    pub const fn uniform(width: f32) -> Self {
        Self {
            first: width,
            continuation: width,
        }
    }

    #[must_use]
    pub const fn new(first: f32, continuation: f32) -> Self {
        Self {
            first,
            continuation,
        }
    }

    /// Resolves start-edge insets into the remaining line measures.
    #[must_use]
    pub fn from_indents(
        column_width: f32,
        first_indent: f32,
        continuation_indent: f32,
    ) -> Option<Self> {
        if !column_width.is_finite()
            || !first_indent.is_finite()
            || !continuation_indent.is_finite()
            || first_indent < 0.0
            || continuation_indent < 0.0
        {
            return None;
        }
        let profile = Self::new(
            column_width - first_indent,
            column_width - continuation_indent,
        );
        profile.is_valid().then_some(profile)
    }

    const fn is_valid(self) -> bool {
        self.first.is_finite()
            && self.first > 0.0
            && self.continuation.is_finite()
            && self.continuation > 0.0
    }
}

#[derive(Clone, Copy)]
struct GreedyCandidate {
    end: usize,
    byte_index: usize,
    natural_width: f32,
}

/// Selects the fullest legal UAX #14 line prefix from real shaped advances.
///
/// This is the backend-neutral fallback for scripts that do not yet use the
/// Knuth--Plass optimizer. It still keeps line-break authority in Torto: the
/// shaping backend supplies advances only, and receives authored byte/cluster
/// breakpoints in return.
#[must_use]
pub fn greedy_uax14(
    text: &str,
    clusters: &[MeasuredCluster],
    widths: LineWidthProfile,
) -> Option<Vec<MeasuredLineBreak>> {
    if text.is_empty()
        || clusters.is_empty()
        || !widths.is_valid()
        || !clusters_match_text(text, clusters)
    {
        return None;
    }
    let opportunities = unicode::opportunities(text);
    let mut prefix_counts = Vec::with_capacity(clusters.len() + 1);
    prefix_counts.push(0_u32);
    for cluster in clusters {
        prefix_counts.push(prefix_counts.last()?.checked_add(cluster.backend_count)?);
    }

    let mut selected = Vec::new();
    let mut start = 0;
    while start < clusters.len() {
        let target_width = if selected.is_empty() {
            widths.first
        } else {
            widths.continuation
        };
        let mut width = 0.0;
        let mut best_fit = None;
        let mut line = None;
        for (index, cluster) in clusters.iter().enumerate().skip(start) {
            width += cluster.advance;
            let opportunity = opportunities
                .binary_search_by_key(&cluster.text_range.end, |item| item.byte_index)
                .ok()
                .and_then(|index| opportunities.get(index));
            let Some(opportunity) = opportunity else {
                continue;
            };
            let candidate = GreedyCandidate {
                end: index + 1,
                byte_index: cluster.text_range.end,
                natural_width: (width
                    - if cluster.is_space {
                        cluster.advance
                    } else {
                        0.0
                    })
                .max(0.0),
            };
            if opportunity.kind == unicode::LineBreakKind::Mandatory {
                line = if width > target_width {
                    best_fit.or(Some(candidate))
                } else {
                    Some(candidate)
                };
                break;
            }
            if width <= target_width {
                best_fit = Some(candidate);
            } else {
                line = best_fit.or(Some(candidate));
                break;
            }
        }
        let line = line.or(best_fit)?;
        if line.end <= start {
            return None;
        }
        let breakpoint = *prefix_counts.get(line.end)?;
        let previous = *prefix_counts.get(start)?;
        selected.push(MeasuredLineBreak {
            line: LineBreak {
                cluster_count: breakpoint.checked_sub(previous)?,
                breakpoint,
                adjustment_ratio: 0.0,
                badness: 0.0,
                natural_width: line.natural_width,
            },
            byte_index: line.byte_index,
        });
        start = line.end;
    }
    (selected.last()?.byte_index == text.len()
        && selected
            .iter()
            .map(|line| line.line.cluster_count)
            .sum::<u32>()
            == *prefix_counts.last()?)
    .then_some(selected)
}

/// Optimizes ordinary space-delimited LTR prose from real shaped advances.
#[must_use]
pub fn optimize_ltr(
    text: &str,
    clusters: &[MeasuredCluster],
    widths: LineWidthProfile,
    shrink_support: ShrinkSupport,
) -> Option<Vec<MeasuredLineBreak>> {
    if text.is_empty()
        || clusters.is_empty()
        || !widths.is_valid()
        || !unicode::supports_phase_one_optimized(text)
        || !clusters_match_text(text, clusters)
    {
        return None;
    }
    let opportunities = unicode::opportunities(text);
    let items = shaped_items(clusters, &opportunities)?;
    let options = match shrink_support {
        ShrinkSupport::Supported => Options::new(widths.continuation),
        ShrinkSupport::Unsupported => Options::new(widths.continuation).without_shrink(),
    }
    .with_first_line_width(widths.first);
    let lines = knuth_plass::optimize(&items, options)?;
    let cluster_total = clusters.iter().try_fold(0_u32, |total, cluster| {
        total.checked_add(cluster.backend_count)
    })?;
    if lines.iter().map(|line| line.cluster_count).sum::<u32>() != cluster_total {
        return None;
    }
    lines
        .into_iter()
        .map(|line| {
            let byte_index = byte_index_for_backend_count(clusters, line.breakpoint)?;
            Some(MeasuredLineBreak { line, byte_index })
        })
        .collect()
}

fn clusters_match_text(text: &str, clusters: &[MeasuredCluster]) -> bool {
    let mut previous_end = 0;
    clusters.iter().all(|cluster| {
        let valid = cluster.backend_count > 0
            && cluster.advance.is_finite()
            && cluster.advance >= 0.0
            && cluster.text_range.start == previous_end
            && cluster.text_range.end > cluster.text_range.start
            && cluster.text_range.end <= text.len()
            && text.is_char_boundary(cluster.text_range.start)
            && text.is_char_boundary(cluster.text_range.end)
            && text
                .get(cluster.text_range.clone())
                .is_some_and(|source| cluster.is_space == (source == " "));
        previous_end = cluster.text_range.end;
        valid
    }) && previous_end == text.len()
}

fn shaped_items(
    clusters: &[MeasuredCluster],
    opportunities: &[unicode::LineBreakOpportunity],
) -> Option<Vec<Item>> {
    let mut items = Vec::new();
    let mut word_width = 0.0;
    let mut word_clusters = 0_u32;
    let mut pending_space = None;

    for cluster in clusters {
        if cluster.is_space {
            if word_clusters == 0 || pending_space.is_some() {
                return None;
            }
            pending_space = Some(cluster);
            continue;
        }
        if let Some(space) = pending_space.take() {
            if !unicode::contains(opportunities, cluster.text_range.start) {
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
                clusters: space.backend_count,
            });
            word_width = 0.0;
            word_clusters = 0;
        }
        word_width += cluster.advance;
        word_clusters = word_clusters.checked_add(cluster.backend_count)?;
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

fn byte_index_for_backend_count(clusters: &[MeasuredCluster], breakpoint: u32) -> Option<usize> {
    let mut count = 0_u32;
    for cluster in clusters {
        count = count.checked_add(cluster.backend_count)?;
        if count == breakpoint {
            return Some(cluster.text_range.end);
        }
        if count > breakpoint {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii_clusters(text: &str, widths: &[f32]) -> Vec<MeasuredCluster> {
        text.char_indices()
            .zip(widths.iter().copied())
            .map(|((start, character), advance)| MeasuredCluster {
                text_range: start..start + character.len_utf8(),
                advance,
                backend_count: 1,
                is_space: character == ' ',
            })
            .collect()
    }

    #[test]
    fn returns_backend_counts_and_authored_byte_breakpoints() {
        let text = "one two three four";
        let widths = text
            .chars()
            .map(|character| if character == ' ' { 4.0 } else { 10.0 })
            .collect::<Vec<_>>();
        let lines = optimize_ltr(
            text,
            &ascii_clusters(text, &widths),
            LineWidthProfile::uniform(100.0),
            ShrinkSupport::Unsupported,
        )
        .expect("ordinary LTR clusters should optimize");
        assert!(lines.len() > 1);
        assert_eq!(lines.last().unwrap().byte_index, text.len());
        assert!(
            lines
                .windows(2)
                .all(|pair| pair[0].byte_index < pair[1].byte_index)
        );
    }

    #[test]
    fn rejects_non_contiguous_or_non_uax_cluster_streams() {
        let text = "one two";
        let mut clusters = ascii_clusters(text, &[10.0, 10.0, 10.0, 4.0, 10.0, 10.0, 10.0]);
        clusters[4].text_range.start += 1;
        assert!(
            optimize_ltr(
                text,
                &clusters,
                LineWidthProfile::uniform(60.0),
                ShrinkSupport::Unsupported,
            )
            .is_none()
        );
    }

    #[test]
    fn greedy_uax14_uses_spaces_without_delegating_wrap_boundaries() {
        let text = "one two three";
        let widths = text
            .chars()
            .map(|character| if character == ' ' { 4.0 } else { 10.0 })
            .collect::<Vec<_>>();
        let lines = greedy_uax14(
            text,
            &ascii_clusters(text, &widths),
            LineWidthProfile::uniform(64.0),
        )
        .unwrap();

        assert_eq!(
            lines.iter().map(|line| line.byte_index).collect::<Vec<_>>(),
            ["one ".len(), "one two ".len(), text.len()]
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| line.line.cluster_count)
                .sum::<u32>(),
            13
        );
    }

    #[test]
    fn greedy_uax14_respects_cjk_closing_punctuation() {
        let text = "甲，乙丙。";
        let clusters = text
            .char_indices()
            .map(|(start, character)| MeasuredCluster {
                text_range: start..start + character.len_utf8(),
                advance: 10.0,
                backend_count: 1,
                is_space: false,
            })
            .collect::<Vec<_>>();
        let lines = greedy_uax14(text, &clusters, LineWidthProfile::uniform(20.0)).unwrap();
        let comma_start = '甲'.len_utf8();

        assert!(lines.iter().all(|line| line.byte_index != comma_start));
        assert_eq!(lines.last().unwrap().byte_index, text.len());
    }

    #[test]
    fn greedy_profile_uses_a_narrower_continuation_measure_for_hanging_indent() {
        let text = "one two three four";
        let widths = text
            .chars()
            .map(|character| if character == ' ' { 4.0 } else { 10.0 })
            .collect::<Vec<_>>();
        let lines = greedy_uax14(
            text,
            &ascii_clusters(text, &widths),
            LineWidthProfile::new(80.0, 64.0),
        )
        .unwrap();

        assert_eq!(
            lines.iter().map(|line| line.byte_index).collect::<Vec<_>>(),
            ["one two ".len(), "one two three ".len(), text.len()]
        );
    }
}
