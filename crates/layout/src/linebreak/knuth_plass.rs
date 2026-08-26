//! A deliberately small Knuth--Plass inspired paragraph optimizer.
//!
//! This module only models fixed-width boxes and stretchable/shrinkable glue.
//! It does not know about fonts, shaping engines, source documents, or
//! renderers. Callers are responsible for turning their shaped content into
//! items and for applying the resulting cluster breakpoints.

/// One shaped item in the simplified paragraph model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Item {
    /// Unbreakable shaped content, normally one word and its punctuation.
    Box { width: f32, clusters: u32 },
    /// Inter-word whitespace at which a line may end.
    Glue {
        width: f32,
        stretch: f32,
        shrink: f32,
        clusters: u32,
    },
}

impl Item {
    fn width(self) -> f32 {
        match self {
            Self::Box { width, .. } | Self::Glue { width, .. } => width,
        }
    }

    fn stretch(self) -> f32 {
        match self {
            Self::Glue { stretch, .. } => stretch,
            Self::Box { .. } => 0.0,
        }
    }

    fn shrink(self) -> f32 {
        match self {
            Self::Glue { shrink, .. } => shrink,
            Self::Box { .. } => 0.0,
        }
    }

    fn clusters(self) -> u32 {
        match self {
            Self::Box { clusters, .. } | Self::Glue { clusters, .. } => clusters,
        }
    }
}

/// Tunable values for the first-stage optimizer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Width used by continuation lines.
    pub line_width: f32,
    /// Optional distinct width for the first line. This keeps paragraph-indent
    /// policy outside the optimizer while allowing ordinary and hanging
    /// indents to participate in the global demerit calculation.
    pub first_line_width: Option<f32>,
    pub line_penalty: f32,
    /// Lowest permitted glue adjustment ratio. The core defaults to full
    /// configured shrink (`-1`); adapters can raise this to zero when their
    /// renderer only supports stretching spaces.
    pub minimum_adjustment_ratio: f32,
}

impl Options {
    #[must_use]
    pub const fn new(line_width: f32) -> Self {
        Self {
            line_width,
            first_line_width: None,
            line_penalty: 10.0,
            minimum_adjustment_ratio: -1.0,
        }
    }

    /// Uses a distinct measure for the first line of the paragraph.
    #[must_use]
    pub const fn with_first_line_width(mut self, width: f32) -> Self {
        self.first_line_width = Some(width);
        self
    }

    /// Disables shrink candidates while retaining stretch-based optimization.
    #[must_use]
    pub const fn without_shrink(mut self) -> Self {
        self.minimum_adjustment_ratio = 0.0;
        self
    }
}

/// One selected output line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineBreak {
    /// Number of shaped clusters consumed by this line. Trailing break-space
    /// clusters are included because shaping adapters must consume them.
    pub cluster_count: u32,
    /// Cumulative shaped-cluster position immediately after this line.
    pub breakpoint: u32,
    /// Glue adjustment required to reach the target width. Positive values
    /// stretch and negative values shrink.
    pub adjustment_ratio: f32,
    /// Approximate TeX badness: `100 * abs(adjustment_ratio)^3`.
    pub badness: f32,
    /// Natural width excluding whitespace discarded at the line ending.
    pub natural_width: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct PrefixSums {
    width: f32,
    stretch: f32,
    shrink: f32,
    clusters: u32,
}

#[derive(Clone, Copy, Debug)]
struct CandidateLine {
    cluster_count: u32,
    breakpoint: u32,
    ratio: f32,
    badness: f32,
    natural_width: f32,
}

/// Finds the minimum-total-demerit breakpoints for a whole paragraph.
///
/// Break opportunities are the positions after every [`Item::Glue`] plus the
/// paragraph end. The final line is treated as ragged, matching ordinary
/// justified prose where the last line is not expanded.
#[must_use]
pub fn optimize(items: &[Item], options: Options) -> Option<Vec<LineBreak>> {
    if items.is_empty()
        || !options.line_width.is_finite()
        || options.line_width <= 0.0
        || options
            .first_line_width
            .is_some_and(|width| !width.is_finite() || width <= 0.0)
        || !options.line_penalty.is_finite()
        || options.line_penalty < 0.0
        || !options.minimum_adjustment_ratio.is_finite()
        || !(-1.0..=0.0).contains(&options.minimum_adjustment_ratio)
        || !items_are_valid(items)
    {
        return None;
    }

    let prefix = prefix_sums(items)?;
    let mut candidates = Vec::with_capacity(items.len() / 2 + 2);
    candidates.push(0);
    candidates.extend(
        items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| matches!(item, Item::Glue { .. }).then_some(index + 1)),
    );
    if candidates.last().copied() != Some(items.len()) {
        candidates.push(items.len());
    }

    let mut best = vec![f64::INFINITY; candidates.len()];
    let mut predecessor = vec![None; candidates.len()];
    let mut selected_line = vec![None; candidates.len()];
    best[0] = 0.0;

    for end_candidate in 1..candidates.len() {
        let end = candidates[end_candidate];
        let is_last_line = end == items.len();
        for start_candidate in 0..end_candidate {
            if !best[start_candidate].is_finite() {
                continue;
            }
            let start = candidates[start_candidate];
            let line_width = if start == 0 {
                options.first_line_width.unwrap_or(options.line_width)
            } else {
                options.line_width
            };
            let Some(line) = measure_line(
                items,
                &prefix,
                start,
                end,
                line_width,
                options.minimum_adjustment_ratio,
                is_last_line,
            ) else {
                continue;
            };
            let demerit = f64::from((options.line_penalty + line.badness).powi(2));
            let total = best[start_candidate] + demerit;
            if total < best[end_candidate] {
                best[end_candidate] = total;
                predecessor[end_candidate] = Some(start_candidate);
                selected_line[end_candidate] = Some(line);
            }
        }
    }

    if !best.last()?.is_finite() {
        return None;
    }
    let mut lines = Vec::new();
    let mut cursor = candidates.len() - 1;
    while cursor != 0 {
        let line = selected_line[cursor]?;
        lines.push(LineBreak {
            cluster_count: line.cluster_count,
            breakpoint: line.breakpoint,
            adjustment_ratio: line.ratio,
            badness: line.badness,
            natural_width: line.natural_width,
        });
        cursor = predecessor[cursor]?;
    }
    lines.reverse();
    Some(lines)
}

fn items_are_valid(items: &[Item]) -> bool {
    matches!(items.first(), Some(Item::Box { .. }))
        && matches!(items.last(), Some(Item::Box { .. }))
        && items.iter().enumerate().all(|(index, item)| match *item {
            Item::Box { width, clusters } => {
                index % 2 == 0 && width.is_finite() && width >= 0.0 && clusters > 0
            }
            Item::Glue {
                width,
                stretch,
                shrink,
                clusters,
            } => {
                index % 2 == 1
                    && width.is_finite()
                    && width >= 0.0
                    && stretch.is_finite()
                    && stretch >= 0.0
                    && shrink.is_finite()
                    && shrink >= 0.0
                    && clusters > 0
            }
        })
}

fn prefix_sums(items: &[Item]) -> Option<Vec<PrefixSums>> {
    let mut prefix = Vec::with_capacity(items.len() + 1);
    prefix.push(PrefixSums::default());
    for item in items {
        let previous = *prefix.last()?;
        let next = PrefixSums {
            width: previous.width + item.width(),
            stretch: previous.stretch + item.stretch(),
            shrink: previous.shrink + item.shrink(),
            clusters: previous.clusters.checked_add(item.clusters())?,
        };
        if !next.width.is_finite() || !next.stretch.is_finite() || !next.shrink.is_finite() {
            return None;
        }
        prefix.push(next);
    }
    Some(prefix)
}

fn measure_line(
    items: &[Item],
    prefix: &[PrefixSums],
    start: usize,
    end: usize,
    line_width: f32,
    minimum_adjustment_ratio: f32,
    is_last_line: bool,
) -> Option<CandidateLine> {
    if start >= end {
        return None;
    }
    // A break-space is consumed by Parley but discarded from the TeX measure.
    let measured_end = if end < items.len() && matches!(items[end - 1], Item::Glue { .. }) {
        end - 1
    } else {
        end
    };
    if measured_end <= start {
        return None;
    }
    let natural_width = prefix[measured_end].width - prefix[start].width;
    let stretch = prefix[measured_end].stretch - prefix[start].stretch;
    let shrink = prefix[measured_end].shrink - prefix[start].shrink;
    let cluster_count = prefix[end].clusters.checked_sub(prefix[start].clusters)?;
    let breakpoint = prefix[end].clusters;

    let difference = line_width - natural_width;
    let (ratio, badness) = if is_last_line && difference >= 0.0 {
        (0.0, 0.0)
    } else {
        let ratio = if difference.abs() <= f32::EPSILON {
            0.0
        } else if difference > 0.0 {
            if stretch <= f32::EPSILON {
                return None;
            }
            difference / stretch
        } else {
            if shrink <= f32::EPSILON {
                return None;
            }
            difference / shrink
        };
        // Glue cannot shrink beyond its configured capacity. Stretch remains
        // unbounded in this PoC, but rapidly becomes prohibitively expensive.
        if !ratio.is_finite() || ratio < minimum_adjustment_ratio {
            return None;
        }
        (ratio, 100.0 * ratio.abs().powi(3))
    };

    Some(CandidateLine {
        cluster_count,
        breakpoint,
        ratio,
        badness,
        natural_width,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(word_widths: &[f32], space_width: f32) -> Vec<Item> {
        let mut items = Vec::new();
        for (index, width) in word_widths.iter().copied().enumerate() {
            if index != 0 {
                items.push(Item::Glue {
                    width: space_width,
                    stretch: space_width * 0.5,
                    shrink: space_width * 0.33,
                    clusters: 1,
                });
            }
            items.push(Item::Box { width, clusters: 1 });
        }
        items
    }

    #[test]
    fn chooses_breakpoints_and_reports_cluster_counts() {
        let items = paragraph(&[32.0, 24.0, 28.0, 20.0], 8.0);
        let lines = optimize(&items, Options::new(72.0)).expect("paragraph should be feasible");
        assert_eq!(lines.last().map(|line| line.breakpoint), Some(7));
        assert_eq!(lines.iter().map(|line| line.cluster_count).sum::<u32>(), 7);
        assert!(lines.iter().all(|line| line.badness >= 0.0));
    }

    #[test]
    fn rejects_over_shrunk_lines() {
        let items = paragraph(&[50.0, 50.0], 10.0);
        let lines = optimize(&items, Options::new(80.0));
        assert!(lines.is_none());
    }

    #[test]
    fn adapters_can_disable_shrink_without_removing_it_from_the_core() {
        let items = paragraph(&[28.0, 12.0, 12.0, 12.0, 12.0, 12.0, 12.0], 8.0);
        let shrinkable = optimize(&items, Options::new(64.0)).expect("shrink should be feasible");
        let stretch_only = optimize(&items, Options::new(64.0).without_shrink())
            .expect("a stretch-only solution should remain feasible");

        assert!(shrinkable.iter().any(|line| line.adjustment_ratio < 0.0));
        assert!(stretch_only.iter().all(|line| line.adjustment_ratio >= 0.0));
    }

    #[test]
    fn distinct_first_line_measure_changes_only_the_first_break_search() {
        let items = paragraph(&[20.0, 20.0, 20.0, 20.0], 10.0);
        let uniform = optimize(&items, Options::new(80.0).without_shrink()).unwrap();
        let indented = optimize(
            &items,
            Options::new(80.0)
                .with_first_line_width(60.0)
                .without_shrink(),
        )
        .unwrap();

        assert_eq!(uniform[0].breakpoint, 6);
        assert_eq!(indented[0].breakpoint, 4);
        assert_eq!(indented.last().unwrap().breakpoint, 7);
    }

    #[test]
    fn optimized_breaks_trade_a_full_local_line_for_even_spacing() {
        let widths = [12.0, 12.0, 12.0, 12.0, 36.0, 12.0, 16.0, 12.0];
        let items = paragraph(&widths, 8.0);
        let options = Options::new(72.0);
        let optimized = optimize(&items, options).expect("optimized paragraph should be feasible");
        let greedy = greedy_lines_for_test(&items, options).expect("greedy paragraph should work");

        println!("Greedy:");
        print_lines(&greedy);
        println!("Optimized:");
        print_lines(&optimized);

        assert_eq!(greedy[0].breakpoint, 8);
        assert_eq!(optimized[0].breakpoint, 6);
        assert!((greedy[0].natural_width - 72.0).abs() < f32::EPSILON);
        assert!((optimized[0].natural_width - 52.0).abs() < f32::EPSILON);
        assert!(total_demerit(&optimized, options) < total_demerit(&greedy, options));
        assert!(optimized[1].badness < greedy[1].badness);
    }

    fn greedy_lines_for_test(items: &[Item], options: Options) -> Option<Vec<LineBreak>> {
        let prefix = prefix_sums(items)?;
        let mut candidates = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| matches!(item, Item::Glue { .. }).then_some(index + 1))
            .collect::<Vec<_>>();
        candidates.push(items.len());
        let mut start = 0;
        let mut lines = Vec::new();
        while start < items.len() {
            let end = candidates
                .iter()
                .copied()
                .skip_while(|candidate| *candidate <= start)
                .take_while(|candidate| {
                    let measured_end = if *candidate < items.len() {
                        candidate - 1
                    } else {
                        *candidate
                    };
                    prefix[measured_end].width - prefix[start].width <= options.line_width
                })
                .last()
                .or_else(|| {
                    candidates
                        .iter()
                        .copied()
                        .find(|candidate| *candidate > start)
                })?;
            let measured = measure_line(
                items,
                &prefix,
                start,
                end,
                options.line_width,
                options.minimum_adjustment_ratio,
                end == items.len(),
            )?;
            lines.push(LineBreak {
                cluster_count: measured.cluster_count,
                breakpoint: measured.breakpoint,
                adjustment_ratio: measured.ratio,
                badness: measured.badness,
                natural_width: measured.natural_width,
            });
            start = end;
        }
        Some(lines)
    }

    fn total_demerit(lines: &[LineBreak], options: Options) -> f32 {
        lines
            .iter()
            .map(|line| (options.line_penalty + line.badness).powi(2))
            .sum()
    }

    fn print_lines(lines: &[LineBreak]) {
        for (index, line) in lines.iter().enumerate() {
            println!(
                "  line {}: breakpoint={}, ratio={:.3}, badness={:.3}",
                index + 1,
                line.breakpoint,
                line.adjustment_ratio,
                line.badness
            );
        }
    }
}
