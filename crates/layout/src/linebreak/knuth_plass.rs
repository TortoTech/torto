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
    pub line_width: f32,
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
            line_penalty: 10.0,
            minimum_adjustment_ratio: -1.0,
        }
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

/// One shaped cluster in the Unicode-aware paragraph model.
///
/// Spacing owned by a boundary is stored on the cluster before that boundary,
/// which lets the adapter write the result back as Parley letter spacing
/// without changing the source text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClusterItem {
    pub width: f32,
    pub stretch: f32,
    pub shrink: f32,
    pub boundary_width_after: f32,
    pub boundary_shrink_after: f32,
    pub line_end_adjustment: f32,
    pub clusters: u32,
    pub break_after: bool,
    pub trimmable: bool,
    pub justifiable_after: bool,
}

/// Tunable values for Unicode-aware whole-paragraph optimization.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParagraphOptions {
    pub line_width: f32,
    pub first_line_indent: f32,
    pub em: f32,
    pub line_penalty: f32,
    pub minimum_adjustment_ratio: f32,
}

impl ParagraphOptions {
    #[must_use]
    pub const fn new(line_width: f32, em: f32) -> Self {
        Self {
            line_width,
            first_line_indent: 0.0,
            em,
            line_penalty: 10.0,
            minimum_adjustment_ratio: -1.0,
        }
    }
}

/// Optimized breaks and the trailing spacing delta for every input cluster.
#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphLayout {
    pub lines: Vec<LineBreak>,
    pub adjustments: Vec<f32>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ClusterPrefixSums {
    width: f32,
    stretch: f32,
    shrink: f32,
    boundary_width: f32,
    boundary_shrink: f32,
    justifiable_boundaries: f32,
    clusters: u32,
}

#[derive(Clone, Copy, Debug)]
struct ClusterCandidateLine {
    start: usize,
    measured_end: usize,
    cluster_count: u32,
    breakpoint: u32,
    ratio: f32,
    badness: f32,
    natural_width: f32,
    difference: f32,
    stretch: f32,
    shrink: f32,
    justifiable_boundaries: f32,
    is_last: bool,
}

#[derive(Clone, Copy, Debug)]
enum ClusterLineFit {
    Feasible(ClusterCandidateLine),
    TooLong,
    Infeasible,
}

const CLUSTER_FIT_EPSILON: f32 = 0.001;

/// Finds whole-paragraph breakpoints using legal Unicode line boundaries.
///
/// Unlike [`optimize`], this model works at shaped-cluster granularity and can
/// distribute width over CJK boundaries as well as stretch or shrink spaces.
#[must_use]
pub fn optimize_clusters(
    items: &[ClusterItem],
    options: ParagraphOptions,
) -> Option<ParagraphLayout> {
    if items.is_empty() || !paragraph_options_are_valid(options) || !cluster_items_are_valid(items)
    {
        return None;
    }

    let prefix = cluster_prefix_sums(items)?;
    let mut candidates = Vec::with_capacity(items.len() + 1);
    candidates.push(0);
    candidates.extend(
        items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.break_after.then_some(index + 1)),
    );
    if candidates.last().copied() != Some(items.len()) {
        candidates.push(items.len());
    }
    candidates.dedup();

    let mut best = vec![f64::INFINITY; candidates.len()];
    let mut predecessor = vec![None; candidates.len()];
    let mut selected_line = vec![None; candidates.len()];
    best[0] = 0.0;

    for end_candidate in 1..candidates.len() {
        let end = candidates[end_candidate];
        // Walk from the shortest line towards progressively longer ones. Once
        // a line cannot fit even after all permitted shrink, every earlier
        // legal start is wider as well, so the remaining candidates can be
        // skipped without changing the optimum. This is especially important
        // for CJK paragraphs, where almost every cluster is a legal break.
        for start_candidate in (0..end_candidate).rev() {
            let start = candidates[start_candidate];
            let line = match measure_cluster_line(items, &prefix, start, end, options) {
                ClusterLineFit::Feasible(line) => line,
                ClusterLineFit::TooLong => break,
                ClusterLineFit::Infeasible => continue,
            };
            if !best[start_candidate].is_finite() {
                continue;
            }
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
    let mut selected = Vec::new();
    let mut cursor = candidates.len() - 1;
    while cursor != 0 {
        selected.push(selected_line[cursor]?);
        cursor = predecessor[cursor]?;
    }
    selected.reverse();

    let mut adjustments = vec![0.0; items.len()];
    for line in &selected {
        apply_cluster_line_adjustments(items, line, &mut adjustments);
    }
    let lines = selected
        .into_iter()
        .map(|line| LineBreak {
            cluster_count: line.cluster_count,
            breakpoint: line.breakpoint,
            adjustment_ratio: line.ratio,
            badness: line.badness,
            natural_width: line.natural_width,
        })
        .collect();
    Some(ParagraphLayout { lines, adjustments })
}

fn paragraph_options_are_valid(options: ParagraphOptions) -> bool {
    options.line_width.is_finite()
        && options.line_width > 0.0
        && options.first_line_indent.is_finite()
        && options.first_line_indent >= 0.0
        && options.first_line_indent < options.line_width
        && options.em.is_finite()
        && options.em > 0.0
        && options.line_penalty.is_finite()
        && options.line_penalty >= 0.0
        && options.minimum_adjustment_ratio.is_finite()
        && (-1.0..=0.0).contains(&options.minimum_adjustment_ratio)
}

fn cluster_items_are_valid(items: &[ClusterItem]) -> bool {
    items.iter().all(|item| {
        item.width.is_finite()
            && item.width >= 0.0
            && item.stretch.is_finite()
            && item.stretch >= 0.0
            && item.shrink.is_finite()
            && item.shrink >= 0.0
            && item.boundary_width_after.is_finite()
            && item.boundary_shrink_after.is_finite()
            && item.boundary_shrink_after >= 0.0
            && item.line_end_adjustment.is_finite()
            && item.clusters > 0
            // Required by the predecessor-pruning proof: adding a cluster and
            // its following internal boundary must not reduce the line's
            // minimum achievable width.
            && item.width + item.boundary_width_after
                - item.shrink
                - item.boundary_shrink_after
                >= -CLUSTER_FIT_EPSILON
    })
}

fn cluster_prefix_sums(items: &[ClusterItem]) -> Option<Vec<ClusterPrefixSums>> {
    let mut prefix = Vec::with_capacity(items.len() + 1);
    prefix.push(ClusterPrefixSums::default());
    for item in items {
        let previous = *prefix.last()?;
        let next = ClusterPrefixSums {
            width: previous.width + item.width,
            stretch: previous.stretch + item.stretch,
            shrink: previous.shrink + item.shrink,
            boundary_width: previous.boundary_width + item.boundary_width_after,
            boundary_shrink: previous.boundary_shrink + item.boundary_shrink_after,
            justifiable_boundaries: previous.justifiable_boundaries
                + if item.justifiable_after { 1.0 } else { 0.0 },
            clusters: previous.clusters.checked_add(item.clusters)?,
        };
        if !next.width.is_finite()
            || !next.stretch.is_finite()
            || !next.shrink.is_finite()
            || !next.boundary_width.is_finite()
            || !next.boundary_shrink.is_finite()
            || !next.justifiable_boundaries.is_finite()
        {
            return None;
        }
        prefix.push(next);
    }
    Some(prefix)
}

fn internal_boundary_sum(
    prefix: &[ClusterPrefixSums],
    start: usize,
    measured_end: usize,
    select: impl Fn(ClusterPrefixSums) -> f32,
) -> f32 {
    if measured_end.saturating_sub(start) < 2 {
        return 0.0;
    }
    select(prefix[measured_end - 1]) - select(prefix[start])
}

fn internal_justifiable_count(
    prefix: &[ClusterPrefixSums],
    start: usize,
    measured_end: usize,
) -> f32 {
    if measured_end.saturating_sub(start) < 2 {
        return 0.0;
    }
    prefix[measured_end - 1].justifiable_boundaries - prefix[start].justifiable_boundaries
}

fn measure_cluster_line(
    items: &[ClusterItem],
    prefix: &[ClusterPrefixSums],
    start: usize,
    end: usize,
    options: ParagraphOptions,
) -> ClusterLineFit {
    if start >= end {
        return ClusterLineFit::Infeasible;
    }
    let mut measured_end = end;
    while measured_end > start && items[measured_end - 1].trimmable {
        measured_end -= 1;
    }
    if measured_end <= start {
        return ClusterLineFit::Infeasible;
    }

    let item_width = prefix[measured_end].width - prefix[start].width;
    let boundary_width =
        internal_boundary_sum(prefix, start, measured_end, |sum| sum.boundary_width);
    let natural_width = item_width + boundary_width + items[measured_end - 1].line_end_adjustment;
    let stretch = prefix[measured_end].stretch - prefix[start].stretch;
    let shrink = prefix[measured_end].shrink - prefix[start].shrink
        + internal_boundary_sum(prefix, start, measured_end, |sum| sum.boundary_shrink);
    let justifiable_boundaries = internal_justifiable_count(prefix, start, measured_end);
    let Some(cluster_count) = prefix[end].clusters.checked_sub(prefix[start].clusters) else {
        return ClusterLineFit::Infeasible;
    };
    let breakpoint = prefix[end].clusters;
    let target_width = options.line_width
        - if start == 0 {
            options.first_line_indent
        } else {
            0.0
        };
    if target_width <= 0.0 {
        return ClusterLineFit::Infeasible;
    }
    let difference = target_width - natural_width;
    let is_last = end == items.len();
    let minimum_width = natural_width + options.minimum_adjustment_ratio * shrink;
    if target_width + CLUSTER_FIT_EPSILON < minimum_width {
        return ClusterLineFit::TooLong;
    }
    let Some((ratio, badness)) = cluster_adjustment_cost(
        difference,
        stretch,
        shrink,
        justifiable_boundaries,
        options,
        is_last,
    ) else {
        return ClusterLineFit::Infeasible;
    };
    ClusterLineFit::Feasible(ClusterCandidateLine {
        start,
        measured_end,
        cluster_count,
        breakpoint,
        ratio,
        badness,
        natural_width,
        difference,
        stretch,
        shrink,
        justifiable_boundaries,
        is_last,
    })
}

fn cluster_adjustment_cost(
    difference: f32,
    stretch: f32,
    shrink: f32,
    justifiable_boundaries: f32,
    options: ParagraphOptions,
    is_last: bool,
) -> Option<(f32, f32)> {
    if is_last && difference >= 0.0 {
        return Some((0.0, 0.0));
    }
    let ratio = if difference.abs() <= f32::EPSILON {
        0.0
    } else if difference < 0.0 {
        if shrink <= f32::EPSILON {
            return None;
        }
        difference / shrink
    } else if stretch > f32::EPSILON {
        let natural_ratio = difference / stretch;
        if natural_ratio <= 1.0 || justifiable_boundaries <= f32::EPSILON {
            natural_ratio
        } else {
            1.0 + (difference - stretch) / justifiable_boundaries / (options.em * 0.5)
        }
    } else if justifiable_boundaries > f32::EPSILON {
        1.0 + difference / justifiable_boundaries / (options.em * 0.5)
    } else {
        return None;
    };
    if !ratio.is_finite() || ratio < options.minimum_adjustment_ratio {
        return None;
    }
    Some((ratio, 100.0 * ratio.abs().powi(3)))
}

fn apply_cluster_line_adjustments(
    items: &[ClusterItem],
    line: &ClusterCandidateLine,
    adjustments: &mut [f32],
) {
    for index in line.start..line.measured_end.saturating_sub(1) {
        adjustments[index] += items[index].boundary_width_after;
    }
    adjustments[line.measured_end - 1] += items[line.measured_end - 1].line_end_adjustment;

    if line.difference.abs() <= f32::EPSILON || (line.is_last && line.difference >= 0.0) {
        return;
    }
    if line.difference < 0.0 {
        let ratio = line.difference / line.shrink;
        for index in line.start..line.measured_end {
            adjustments[index] += items[index].shrink * ratio;
            if index + 1 < line.measured_end {
                adjustments[index] += items[index].boundary_shrink_after * ratio;
            }
        }
        return;
    }

    let stretch_amount = if line.justifiable_boundaries <= f32::EPSILON {
        line.difference
    } else {
        line.difference.min(line.stretch)
    };
    if line.stretch > f32::EPSILON {
        let stretch_ratio = stretch_amount / line.stretch;
        for index in line.start..line.measured_end {
            adjustments[index] += items[index].stretch * stretch_ratio;
        }
    }
    let remainder = line.difference - stretch_amount;
    if remainder > f32::EPSILON && line.justifiable_boundaries > f32::EPSILON {
        let per_boundary = remainder / line.justifiable_boundaries;
        for index in line.start..line.measured_end.saturating_sub(1) {
            if items[index].justifiable_after {
                adjustments[index] += per_boundary;
            }
        }
    }
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
            let Some(line) = measure_line(
                items,
                &prefix,
                start,
                end,
                options.line_width,
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

    #[test]
    fn cluster_optimizer_distributes_cjk_justification_without_spaces() {
        let items = (0..7)
            .map(|index| ClusterItem {
                width: 20.0,
                stretch: 0.0,
                shrink: 0.0,
                boundary_width_after: 0.0,
                boundary_shrink_after: 0.0,
                line_end_adjustment: 0.0,
                clusters: 1,
                break_after: true,
                trimmable: false,
                justifiable_after: index != 6,
            })
            .collect::<Vec<_>>();

        let result = optimize_clusters(&items, ParagraphOptions::new(66.0, 20.0))
            .expect("CJK gaps should make the paragraph feasible");

        assert!(result.lines.len() > 1);
        assert!(result.adjustments.iter().any(|amount| *amount > 0.0));
        assert_eq!(
            result
                .lines
                .iter()
                .map(|line| line.cluster_count)
                .sum::<u32>(),
            7
        );
    }

    #[test]
    fn cluster_optimizer_uses_boundary_shrink_for_mixed_script_spacing() {
        let items = [
            ClusterItem {
                width: 20.0,
                stretch: 0.0,
                shrink: 0.0,
                boundary_width_after: 5.0,
                boundary_shrink_after: 2.5,
                line_end_adjustment: 0.0,
                clusters: 1,
                break_after: false,
                trimmable: false,
                justifiable_after: false,
            },
            ClusterItem {
                width: 20.0,
                stretch: 0.0,
                shrink: 0.0,
                boundary_width_after: 0.0,
                boundary_shrink_after: 0.0,
                line_end_adjustment: 0.0,
                clusters: 1,
                break_after: true,
                trimmable: false,
                justifiable_after: false,
            },
        ];

        let result = optimize_clusters(&items, ParagraphOptions::new(43.0, 20.0))
            .expect("mixed-script boundary should shrink within its capacity");

        assert_eq!(result.lines.len(), 1);
        assert!((result.adjustments[0] - 3.0).abs() < 0.001);
        assert!(result.lines[0].adjustment_ratio < 0.0);
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
