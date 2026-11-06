//! Bounded A* over real, pre-shaped cell height candidates. No glyph shaping in
//! search, no external solver, and no claim of optimality outside these curves.
use crate::{PreparedText, ReaderStyle, TableCell, break_text_lines};
use std::fmt::Write as _;
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    hash::Hasher,
    time::{Duration, Instant},
};

const MAX_CELLS: usize = 256;
const MAX_MEASUREMENTS: usize = 3072;
const MAX_STATES: usize = 4096;
const MAX_FRONTIER: usize = 512;
const BUDGET: Duration = Duration::from_millis(8);

struct KeyHasher(std::collections::hash_map::DefaultHasher);
impl std::fmt::Write for KeyHasher {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.write(text.as_bytes());
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct TableWidthStats {
    pub searches: usize,
    pub improvements: usize,
    pub cache_hits: usize,
    pub measurements: usize,
    pub profile_limits: usize,
}
#[derive(Default)]
pub(super) struct Cache(VecDeque<(u64, Vec<f32>)>, TableWidthStats);
impl Cache {
    pub(super) fn stats(&self) -> TableWidthStats {
        self.1
    }
    fn insert(&mut self, key: u64, value: Vec<f32>) -> Vec<f32> {
        if self.0.len() >= 16 {
            self.0.pop_front();
        }
        self.0.push_back((key, value.clone()));
        value
    }
}
#[derive(Clone)]
struct Choice {
    width: f32,
    heights: Vec<f32>,
}
struct State {
    bound: f32,
    column: usize,
    used: f32,
    heights: Vec<f32>,
    widths: Vec<f32>,
}
impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.bound == other.bound && self.column == other.column
    }
}
impl Eq for State {}
impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .bound
            .total_cmp(&self.bound)
            .then(self.column.cmp(&other.column))
    }
}

// The incumbent is always feasible. Budget exhaustion returns its best verified
// improvement; it never publishes a partial layout or waits on the UI thread.
fn search(
    columns: &[Vec<Choice>],
    budget: f32,
    baseline: &[f32],
    baseline_heights: &[f32],
    started: Instant,
) -> Vec<f32> {
    let rows = baseline_heights.len();
    let n = columns.len();
    let mut suffix_width = vec![0.; n + 1];
    let mut suffix_height = vec![vec![0f32; rows]; n + 1];
    for c in (0..n).rev() {
        suffix_width[c] = suffix_width[c + 1]
            + columns[c]
                .iter()
                .map(|x| x.width)
                .fold(f32::INFINITY, f32::min);
        for r in 0..rows {
            let min = columns[c]
                .iter()
                .map(|x| x.heights[r])
                .fold(f32::INFINITY, f32::min);
            suffix_height[c][r] = suffix_height[c + 1][r].max(min);
        }
    }
    let mut best = baseline.to_vec();
    let mut best_height: f32 = baseline_heights.iter().sum();
    let mut open = BinaryHeap::new();
    open.push(State {
        bound: suffix_height[0].iter().sum(),
        column: 0,
        used: 0.,
        heights: vec![0.; rows],
        widths: vec![],
    });
    let mut generated = 0;
    while let Some(node) = open.pop() {
        if node.bound >= best_height - 0.01 {
            break;
        }
        if started.elapsed() >= BUDGET {
            break;
        }
        let c = node.column;
        for option in &columns[c] {
            generated += 1;
            if generated > MAX_STATES || open.len() >= MAX_FRONTIER {
                return best;
            }
            let used = node.used + option.width;
            if used + suffix_width[c + 1] > budget + 0.01 {
                continue;
            }
            let heights = node
                .heights
                .iter()
                .zip(&option.heights)
                .map(|(a, b)| a.max(*b))
                .collect::<Vec<_>>();
            let bound: f32 = heights
                .iter()
                .zip(&suffix_height[c + 1])
                .map(|(a, b)| a.max(*b))
                .sum();
            if bound >= best_height - 0.01 {
                continue;
            }
            let mut widths = node.widths.clone();
            widths.push(option.width);
            if c + 1 == n {
                best_height = bound;
                best = widths;
            } else {
                open.push(State {
                    bound,
                    column: c + 1,
                    used,
                    heights,
                    widths,
                });
            }
        }
    }
    // Keep selected widths: indiscriminately stretching a spare pixel across
    // columns can change actual rich-text line heights. Center spare space.
    best
}

#[allow(clippy::too_many_arguments)]
pub(super) fn optimize(
    cache: &mut Cache,
    cells: &[(usize, usize, usize, usize, &TableCell)],
    measured: &[Option<PreparedText>],
    baseline: &[f32],
    minimum: &[f32],
    preferred: &[f32],
    width: f32,
    style: &ReaderStyle,
    padding: f32,
) -> Vec<f32> {
    let n = baseline.len();
    if !(2..=10).contains(&n)
        || cells.len() > MAX_CELLS
        || cells.is_empty()
        || cells.iter().any(|(_, rs, _, cs, _)| *rs != 1 || *cs != 1)
        || measured.iter().any(Option::is_none)
    {
        return baseline.to_vec();
    }
    // Nothing can reduce natural single-line heights; skip profile generation,
    // hashing and search for the many small tables in technical books.
    if preferred.iter().sum::<f32>() <= width + 0.01 {
        return baseline.to_vec();
    }
    let rows = cells.iter().map(|c| c.0).max().unwrap() + 1;
    if cells.len() != rows * n || minimum.iter().sum::<f32>() > width {
        return baseline.to_vec();
    }
    let bytes: usize = measured.iter().flatten().map(|m| m.text.len()).sum();
    if bytes > 32768 || cells.iter().map(|c| c.4.text.content.len()).sum::<usize>() > 2048 {
        return baseline.to_vec();
    }
    // Engine-local and bounded: the actual translated text, all styles, width
    // and source identities participate, rather than only unchanged node IDs.
    let mut hasher = KeyHasher(std::collections::hash_map::DefaultHasher::new());
    let _ = write!(hasher, "{width:?}/{style:?}/{cells:?}");
    let key = hasher.0.finish();
    if let Some(at) = cache.0.iter().position(|(k, _)| *k == key) {
        cache.1.cache_hits += 1;
        let entry = cache.0.remove(at).unwrap();
        let answer = entry.1.clone();
        cache.0.push_back(entry);
        return answer;
    }
    let started = Instant::now();
    let total_min: f32 = minimum.iter().sum();
    let mut columns = Vec::new();
    let mut baseline_heights = vec![0f32; rows];
    let mut measurements = 0;
    for c in 0..n {
        let lo = minimum[c].max(20. + padding * 2.);
        let hi = preferred[c].max(lo).min(width - total_min + minimum[c]);
        if hi < lo {
            return baseline.to_vec();
        }
        let mut widths = vec![baseline[c], lo, hi];
        // Sample useful break widths, not every screen pixel. Actual heights
        // are remeasured with the same Parley wrapper as final table shaping.
        for step in 1..12 {
            widths.push(lo + (hi - lo) * step as f32 / 12.);
        }
        widths.sort_by(f32::total_cmp);
        widths.dedup_by(|a, b| (*a - *b).abs() < 0.1);
        let mut choices: Vec<Choice> = widths
            .iter()
            .map(|w| Choice {
                width: *w,
                heights: vec![0.; rows],
            })
            .collect();
        for (i, (row, _, col, _, _)) in cells.iter().enumerate().filter(|(_, x)| x.2 == c) {
            let m = measured[i].as_ref().unwrap();
            let mut layout = (*m.layout).clone();
            for choice in &mut choices {
                measurements += 1;
                cache.1.measurements += 1;
                if measurements > MAX_MEASUREMENTS || started.elapsed() >= BUDGET {
                    cache.1.profile_limits += 1;
                    return cache.insert(key, baseline.to_vec());
                }
                break_text_lines(&mut layout, Some((choice.width - 2. * padding).max(20.)));
                let visible = layout.lines().filter(|line| {
                    m.text
                        .get(line.text_range())
                        .is_some_and(|s| !s.trim().is_empty())
                });
                let mut first = None;
                let mut end = 0.;
                for line in visible {
                    first.get_or_insert(line.metrics().block_min_coord);
                    end = line.metrics().block_max_coord;
                }
                choice.heights[*row] =
                    first.map_or(0., |start| (end - start).max(0.)) + 2. * padding;
                if (choice.width - baseline[*col]).abs() < 0.01 {
                    baseline_heights[*row] = baseline_heights[*row].max(choice.heights[*row]);
                }
            }
        }
        // A wider option with no smaller row height never helps; do not assume
        // measured rich-text heights are monotone in width.
        let mut kept: Vec<Choice> = Vec::new();
        for choice in choices {
            if !kept
                .iter()
                .any(|p| p.heights.iter().zip(&choice.heights).all(|(a, b)| a <= b))
            {
                kept.push(choice);
            }
        }
        columns.push(kept);
    }
    cache.1.searches += 1;
    let result = search(&columns, width, baseline, &baseline_heights, started);
    if result != baseline {
        cache.1.improvements += 1;
    }
    cache.insert(key, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn astar_matches_exhaustive_rectangular_choices_and_honors_budget() {
        let columns = vec![
            vec![
                Choice {
                    width: 20.,
                    heights: vec![90., 40.],
                },
                Choice {
                    width: 40.,
                    heights: vec![30., 20.],
                },
            ],
            vec![
                Choice {
                    width: 20.,
                    heights: vec![20., 30.],
                },
                Choice {
                    width: 40.,
                    heights: vec![10., 10.],
                },
            ],
        ];
        let result = search(&columns, 60., &[20., 40.], &[90., 40.], Instant::now());
        assert_eq!(result, vec![40., 20.]);
        assert!(result.iter().sum::<f32>() <= 60.);
        assert_eq!(
            search(
                &columns,
                60.,
                &[20., 40.],
                &[90., 40.],
                Instant::now() - BUDGET
            ),
            vec![20., 40.]
        );
    }
    #[test]
    fn astar_matches_exhaustive_search_without_assuming_monotone_heights() {
        let mut seed = 17u32;
        for _ in 0..40 {
            let mut columns = Vec::new();
            for _ in 0..4 {
                let mut choices = Vec::new();
                for width in [1., 3., 5.] {
                    let heights = (0..3)
                        .map(|_| {
                            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                            (seed % 20 + 1) as f32
                        })
                        .collect();
                    choices.push(Choice { width, heights });
                }
                columns.push(choices);
            }
            let height = |widths: &[f32]| -> f32 {
                (0..3)
                    .map(|r| {
                        columns
                            .iter()
                            .zip(widths)
                            .map(|(c, w)| c.iter().find(|x| x.width == *w).unwrap().heights[r])
                            .fold(0., f32::max)
                    })
                    .sum()
            };
            let mut optimal = f32::INFINITY;
            for a in [1., 3., 5.] {
                for b in [1., 3., 5.] {
                    for c in [1., 3., 5.] {
                        for d in [1., 3., 5.] {
                            let widths = [a, b, c, d];
                            if widths.iter().sum::<f32>() <= 12. {
                                optimal = optimal.min(height(&widths));
                            }
                        }
                    }
                }
            }
            let baseline_heights = (0..3)
                .map(|r| columns.iter().map(|c| c[0].heights[r]).fold(0., f32::max))
                .collect::<Vec<_>>();
            let result = search(
                &columns,
                12.,
                &[1.; 4],
                &baseline_heights,
                Instant::now() + Duration::from_secs(1),
            );
            assert_eq!(height(&result), optimal);
        }
    }
}
