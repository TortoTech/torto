//! Region planning shared by paged, spread, scrolling, and focus consumers.
//!
//! Region planning is intentionally independent from text shaping and page
//! construction. It turns a viewport plus reader style into the geometry that
//! the flow builder consumes.

use std::ops::Range;

use crate::flow::{FlowItem, FlowScopeKind};
use crate::{LayoutViewport, ReaderStyle, SpreadMode};

/// Minimum width required before a second reading column is introduced.
pub const MIN_COLUMN_WIDTH: f32 = 360.0;
/// Maximum measure of one reading column in device-independent pixels.
pub const MAX_COLUMN_WIDTH: f32 = 800.0;

/// Immutable geometry for one pass through a sequence of flow regions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionPlan {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub bottom: f32,
    /// Number of simultaneously visible flow regions (one or two today).
    pub visible_pages: usize,
    pub continuation_offset_x: f32,
}

impl RegionPlan {
    /// Resolves page/column geometry without shaping content.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "reader viewport dimensions are bounded far below f32's exact integer range"
    )]
    pub fn resolve(viewport: LayoutViewport, reader_style: &ReaderStyle) -> Self {
        Self::resolve_size(viewport.width as f32, viewport.height as f32, reader_style)
    }

    /// Resolves geometry from an already normalized DIP size.
    #[must_use]
    pub fn resolve_size(page_width: f32, page_height: f32, reader_style: &ReaderStyle) -> Self {
        let (content_left, content_width, column_count, continuation_offset_x) =
            horizontal_geometry(page_width, reader_style);
        let max_vertical_margin = page_height.mul_add(0.2, -8.0).max(20.0);
        let top_margin = reader_style.top_margin.min(max_vertical_margin);
        let bottom_margin = reader_style.bottom_margin.min(max_vertical_margin);
        let content_bottom = (page_height - bottom_margin).max(top_margin + 40.0);

        Self {
            left: content_left,
            top: top_margin,
            width: content_width,
            bottom: content_bottom,
            visible_pages: column_count,
            continuation_offset_x,
        }
    }
}

/// Returns `(left, width, region_count, continuation_offset_x)`.
#[must_use]
pub fn horizontal_geometry(page_width: f32, reader_style: &ReaderStyle) -> (f32, f32, usize, f32) {
    let horizontal_margin = reader_style
        .horizontal_margin
        .min(page_width.mul_add(0.2, -8.0).max(20.0));
    let configured_column_gap = reader_style.column_gap.max(0.0);
    let double_available = page_width - horizontal_margin * 2.0 - configured_column_gap;
    let column_count = if reader_style.spread == SpreadMode::Double
        && double_available >= MIN_COLUMN_WIDTH * 2.0
    {
        2
    } else {
        1
    };
    let column_gap = if column_count == 2 {
        configured_column_gap
    } else {
        0.0
    };
    let column_divisor = if column_count == 2 { 2.0 } else { 1.0 };
    let content_width = ((page_width - horizontal_margin * 2.0 - column_gap) / column_divisor)
        .clamp(80.0, MAX_COLUMN_WIDTH);
    let spread_width = content_width * column_divisor + column_gap;
    let content_left = ((page_width - spread_width) / 2.0).max(horizontal_margin);
    (
        content_left,
        content_width,
        column_count,
        content_width + column_gap,
    )
}

/// One contiguous slice of a flow stream assigned to a region.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowRegionFragment {
    pub item_range: Range<usize>,
    pub block_extent: f32,
    /// `true` when the consumer must advance before placing this fragment.
    pub advance_before: bool,
    /// Cost of the breakpoint that ended this fragment.
    pub break_cost: i32,
}

/// Pure region/page breakpoint planner for measured [`FlowItem`] streams.
///
/// The builder does not position or paint payloads. It decides only which
/// consecutive items belong to each region, preserving anchors and penalties
/// in the returned half-open ranges. Opaque splittable blocks must already be
/// expanded into smaller `FlowItem`s by their block compiler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionBuilder {
    region_extent: f32,
    first_available_extent: f32,
    first_region_has_content: bool,
}

impl RegionBuilder {
    #[must_use]
    pub fn new(region_extent: f32) -> Self {
        let region_extent = region_extent.max(1.0);
        Self {
            region_extent,
            first_available_extent: region_extent,
            first_region_has_content: false,
        }
    }

    /// Starts planning inside a partially occupied region.
    #[must_use]
    pub fn with_first_region(mut self, available_extent: f32, has_content: bool) -> Self {
        self.first_available_extent = available_extent.max(0.0);
        self.first_region_has_content = has_content;
        self
    }

    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "the pure pagination state machine keeps breakpoint state transitions together"
    )]
    pub fn build<T>(&self, items: &[FlowItem<T>]) -> Vec<FlowRegionFragment> {
        if items.is_empty() {
            return Vec::new();
        }

        let mut fragments = Vec::new();
        let scope_leading = scope_leading_extents(items);
        let mut region_start = 0;
        let mut index = 0;
        let mut used = 0.0_f32;
        let mut capacity = self.first_available_extent;
        let mut region_has_prior_content = self.first_region_has_content;
        let mut advance_before = false;
        let mut line_run_origin = None;
        let mut line_run_start_extent = 0.0;
        let mut breakpoints = Vec::<BreakCandidate>::new();

        while index < items.len() {
            if let FlowItem::Penalty(penalty) = &items[index] {
                if penalty.forced {
                    let end = index + 1;
                    if end > region_start {
                        fragments.push(FlowRegionFragment {
                            item_range: region_start..end,
                            block_extent: used,
                            advance_before,
                            break_cost: penalty.cost,
                        });
                    }
                    region_start = end;
                    index = end;
                    used = scope_leading[end];
                    capacity = self.region_extent;
                    region_has_prior_content = false;
                    advance_before = true;
                    line_run_origin = None;
                    breakpoints.clear();
                    continue;
                }

                let boundary = index;
                if breakpoints
                    .last()
                    .is_some_and(|candidate| candidate.next_index == boundary)
                {
                    breakpoints.pop();
                }
                if !penalty.prohibited {
                    breakpoints.push(BreakCandidate {
                        next_index: index + 1,
                        used,
                        cost: penalty.cost,
                    });
                }
                index += 1;
                continue;
            }

            if matches!(&items[index], FlowItem::Glue(glue) if glue.collapsible)
                && index > region_start
            {
                // A collapsible boundary is a legal break *before* the glue.
                // Leaving it for the next region lets the consumer discard it
                // at the region edge instead of painting paragraph spacing at
                // the bottom of the previous page.
                breakpoints.push(BreakCandidate {
                    next_index: index,
                    used,
                    cost: 0,
                });
            }

            let candidate_used = match &items[index] {
                FlowItem::Line(line) => {
                    let origin = *line_run_origin.get_or_insert_with(|| {
                        line_run_start_extent = used;
                        line.block_min
                    });
                    line_run_start_extent + (line.block_max - origin).max(line.line_height).max(0.0)
                }
                FlowItem::Glue(glue) => {
                    line_run_origin = None;
                    if glue.collapsible && used <= scope_leading[index] + f32::EPSILON {
                        used
                    } else {
                        used + glue.natural.max(0.0)
                    }
                }
                FlowItem::Block(block) => {
                    line_run_origin = None;
                    used + block.block_extent.max(0.0)
                }
                FlowItem::Anchor(_) => used,
                FlowItem::Scope(scope) => {
                    line_run_origin = None;
                    used + match scope.kind {
                        FlowScopeKind::Enter => scope.initial_extent,
                        FlowScopeKind::Exit => scope.final_extent,
                    }
                }
                FlowItem::Penalty(_) => unreachable!("penalties are handled before measurement"),
            };

            if candidate_used > capacity {
                if used <= f32::EPSILON && region_has_prior_content {
                    capacity = self.region_extent;
                    region_has_prior_content = false;
                    advance_before = true;
                    line_run_origin = None;
                    breakpoints.clear();
                    continue;
                }

                if let Some(candidate) = best_breakpoint(&breakpoints, capacity) {
                    fragments.push(FlowRegionFragment {
                        item_range: region_start..candidate.next_index,
                        block_extent: candidate.used,
                        advance_before,
                        break_cost: candidate.cost,
                    });
                    region_start = candidate.next_index;
                    index = region_start;
                    used = scope_leading[region_start];
                    capacity = self.region_extent;
                    region_has_prior_content = false;
                    advance_before = true;
                    line_run_origin = None;
                    breakpoints.clear();
                    continue;
                }

                // A prohibited keep group or an over-tall first block is kept
                // intact rather than creating an infinite pagination loop.
            }

            used = candidate_used;
            index += 1;
            if !matches!(
                items[index - 1],
                FlowItem::Anchor(_)
                    | FlowItem::Glue(crate::flow::VerticalGlue {
                        collapsible: true,
                        ..
                    })
            ) {
                breakpoints.push(BreakCandidate {
                    next_index: index,
                    used,
                    cost: 0,
                });
            }
        }

        if region_start < items.len() {
            fragments.push(FlowRegionFragment {
                item_range: region_start..items.len(),
                block_extent: used,
                advance_before,
                break_cost: 0,
            });
        }
        fragments
    }
}

fn scope_leading_extents<T>(items: &[FlowItem<T>]) -> Vec<f32> {
    let mut stack = Vec::new();
    let mut current = 0.0_f32;
    let mut leading = Vec::with_capacity(items.len() + 1);
    leading.push(current);
    for item in items {
        if let FlowItem::Scope(scope) = item {
            match scope.kind {
                FlowScopeKind::Enter => {
                    let extent = scope.continuation_extent.max(0.0);
                    stack.push(extent);
                    current += extent;
                }
                FlowScopeKind::Exit => {
                    current -= stack.pop().unwrap_or(0.0);
                    current = current.max(0.0);
                }
            }
        }
        leading.push(current);
    }
    leading
}

#[derive(Clone, Copy, Debug)]
struct BreakCandidate {
    next_index: usize,
    used: f32,
    cost: i32,
}

fn best_breakpoint(candidates: &[BreakCandidate], capacity: f32) -> Option<BreakCandidate> {
    candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.used <= capacity && candidate.next_index > 0)
        .min_by(|left, right| {
            page_break_score(*left, capacity).total_cmp(&page_break_score(*right, capacity))
        })
}

fn page_break_score(candidate: BreakCandidate, capacity: f32) -> f64 {
    let slack = f64::from((capacity - candidate.used).max(0.0));
    slack.mul_add(slack, f64::from(candidate.cost))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{
        BlockBreakability, FlowBlock, FlowLine, FlowScope, PagePenalty, VerticalGlue,
    };
    use rebook_publication::{SourceAnchor, SpineItemId};

    fn line(index: usize, top: f32) -> FlowItem<usize> {
        FlowItem::Line(FlowLine::new(index, top, top + 10.0, 10.0, None))
    }

    fn block(index: usize, extent: f32) -> FlowItem<usize> {
        FlowItem::Block(FlowBlock {
            payload: index,
            block_extent: extent,
            breakability: BlockBreakability::KeepTogether,
            source: None,
        })
    }

    #[test]
    fn narrow_viewport_falls_back_to_one_region() {
        let style = ReaderStyle {
            spread: SpreadMode::Double,
            ..ReaderStyle::default()
        };
        let plan = RegionPlan::resolve(LayoutViewport::new(700, 900).unwrap(), &style);
        assert_eq!(plan.visible_pages, 1);
        assert!(plan.width <= MAX_COLUMN_WIDTH);
    }

    #[test]
    fn wide_viewport_caps_each_region_at_eight_hundred_dip() {
        let style = ReaderStyle {
            spread: SpreadMode::Double,
            ..ReaderStyle::default()
        };
        let plan = RegionPlan::resolve(LayoutViewport::new(2_000, 1_200).unwrap(), &style);
        assert_eq!(plan.visible_pages, 2);
        assert!((plan.width - MAX_COLUMN_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn region_builder_emits_stable_half_open_line_ranges() {
        let items = [line(0, 0.0), line(1, 12.0), line(2, 24.0)];
        let fragments = RegionBuilder::new(23.0).build(&items);
        assert_eq!(
            fragments,
            [
                FlowRegionFragment {
                    item_range: 0..2,
                    block_extent: 22.0,
                    advance_before: false,
                    break_cost: 0,
                },
                FlowRegionFragment {
                    item_range: 2..3,
                    block_extent: 10.0,
                    advance_before: true,
                    break_cost: 0,
                },
            ]
        );
    }

    #[test]
    fn occupied_first_region_advances_before_an_overflowing_line() {
        let items = [line(0, 0.0), line(1, 12.0)];
        let fragments = RegionBuilder::new(30.0)
            .with_first_region(5.0, true)
            .build(&items);
        assert_eq!(fragments.len(), 1);
        assert!(fragments[0].advance_before);
        assert_eq!(fragments[0].item_range, 0..2);
    }

    #[test]
    fn forced_penalty_commits_the_current_region() {
        let items = [
            block(0, 10.0),
            FlowItem::Penalty(PagePenalty::FORCED),
            block(1, 10.0),
        ];
        let fragments = RegionBuilder::new(100.0).build(&items);
        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].item_range, 0..2);
        assert_eq!(fragments[1].item_range, 2..3);
        assert!(fragments[1].advance_before);
    }

    #[test]
    fn prohibited_penalty_keeps_a_heading_with_its_first_body_block() {
        let items = [
            block(0, 15.0),
            block(1, 8.0),
            FlowItem::Penalty(PagePenalty::PROHIBITED),
            block(2, 8.0),
        ];
        let fragments = RegionBuilder::new(25.0).build(&items);
        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].item_range, 0..1);
        assert_eq!(fragments[1].item_range, 1..4);
        assert!((fragments[1].block_extent - 16.0).abs() < f32::EPSILON);
    }

    #[test]
    fn collapsible_leading_glue_and_anchor_do_not_consume_region_extent() {
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p1".into(),
            text_offset: 0,
        };
        let items = [
            FlowItem::Anchor(anchor),
            FlowItem::Glue(VerticalGlue {
                natural: 20.0,
                stretch: 4.0,
                shrink: 2.0,
                collapsible: true,
            }),
            block(0, 10.0),
        ];
        let fragments = RegionBuilder::new(40.0).build(&items);
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].item_range, 0..3);
        assert!((fragments[0].block_extent - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn collapsible_glue_is_discarded_when_it_becomes_a_region_boundary() {
        let items = [
            line(0, 0.0),
            FlowItem::Glue(VerticalGlue {
                natural: 10.0,
                stretch: 0.0,
                shrink: 0.0,
                collapsible: true,
            }),
            line(1, 0.0),
        ];
        let fragments = RegionBuilder::new(25.0).build(&items);
        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].item_range, 0..1);
        assert_eq!(fragments[1].item_range, 1..3);
        assert!((fragments[1].block_extent - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scope_repeats_continuation_extent_after_a_region_break() {
        let items = [
            FlowItem::Scope(FlowScope::enter(7, 5.0, 5.0)),
            FlowItem::Penalty(PagePenalty::PROHIBITED),
            line(0, 0.0),
            line(1, 12.0),
            line(2, 24.0),
            FlowItem::Penalty(PagePenalty::PROHIBITED),
            FlowItem::Scope(FlowScope::exit(7, 5.0)),
        ];
        let fragments = RegionBuilder::new(27.0).build(&items);

        assert_eq!(fragments.len(), 2);
        assert_eq!(fragments[0].item_range, 0..4);
        assert!((fragments[0].block_extent - 27.0).abs() < f32::EPSILON);
        assert_eq!(fragments[1].item_range, 4..7);
        assert!((fragments[1].block_extent - 20.0).abs() < f32::EPSILON);
        assert!(fragments[1].advance_before);
    }
}
