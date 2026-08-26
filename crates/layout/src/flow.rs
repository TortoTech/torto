//! Backend-neutral vertical flow primitives.
//!
//! Flow is the boundary between measured content and region/page construction.
//! It deliberately contains no Parley, renderer, EPUB, or GPUI types.

use rebook_publication::{SourceAnchor, SourceRange};

/// One measured item in reading order.
#[derive(Debug, Clone, PartialEq)]
pub enum FlowItem<T> {
    Line(FlowLine<T>),
    Glue(VerticalGlue),
    Block(FlowBlock<T>),
    Penalty(PagePenalty),
    Anchor(SourceAnchor),
    Scope(FlowScope<T>),
}

/// A shaped line with vertical metrics and an opaque backend payload.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowLine<T> {
    pub payload: T,
    pub block_min: f32,
    pub block_max: f32,
    pub line_height: f32,
    pub source: Option<SourceRange>,
}

impl<T> FlowLine<T> {
    #[must_use]
    pub fn new(
        payload: T,
        block_min: f32,
        block_max: f32,
        line_height: f32,
        source: Option<SourceRange>,
    ) -> Self {
        Self {
            payload,
            block_min,
            block_max,
            line_height,
            source,
        }
    }
}

/// Vertical spacing between flow items.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VerticalGlue {
    pub natural: f32,
    pub stretch: f32,
    pub shrink: f32,
    pub collapsible: bool,
}

impl VerticalGlue {
    #[must_use]
    pub fn fixed(natural: f32) -> Self {
        Self {
            natural: natural.max(0.0),
            stretch: 0.0,
            shrink: 0.0,
            collapsible: false,
        }
    }
}

/// Whether a measured block may split across regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockBreakability {
    KeepTogether,
    Splittable,
}

/// An opaque measured non-line block such as an image or table.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowBlock<T> {
    pub payload: T,
    pub block_extent: f32,
    pub breakability: BlockBreakability,
    pub source: Option<SourceRange>,
}

/// A balanced vertical-flow scope with repeatable continuation inset.
///
/// Quote decorations are the first consumer: entering contributes top padding,
/// every continuation region starts with the same inset, and exiting contributes
/// bottom padding. The payload remains opaque to the pure region builder.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowScope<T> {
    pub payload: T,
    pub kind: FlowScopeKind,
    pub initial_extent: f32,
    pub continuation_extent: f32,
    pub final_extent: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowScopeKind {
    Enter,
    Exit,
}

impl<T> FlowScope<T> {
    #[must_use]
    pub fn enter(payload: T, initial_extent: f32, continuation_extent: f32) -> Self {
        Self {
            payload,
            kind: FlowScopeKind::Enter,
            initial_extent: initial_extent.max(0.0),
            continuation_extent: continuation_extent.max(0.0),
            final_extent: 0.0,
        }
    }

    #[must_use]
    pub fn exit(payload: T, final_extent: f32) -> Self {
        Self {
            payload,
            kind: FlowScopeKind::Exit,
            initial_extent: 0.0,
            continuation_extent: 0.0,
            final_extent: final_extent.max(0.0),
        }
    }
}

/// A legal page/region break and its desirability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagePenalty {
    pub cost: i32,
    pub forced: bool,
    pub prohibited: bool,
}

impl PagePenalty {
    pub const FORCED: Self = Self {
        cost: i32::MIN,
        forced: true,
        prohibited: false,
    };

    pub const PROHIBITED: Self = Self {
        cost: i32::MAX,
        forced: false,
        prohibited: true,
    };

    #[must_use]
    pub const fn allowed(cost: i32) -> Self {
        Self {
            cost,
            forced: false,
            prohibited: false,
        }
    }
}

/// The result of fitting the next line fragment into the current region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineFit {
    /// The first remaining line only belongs in a fresh region.
    AdvanceRegion,
    /// A non-empty half-open range of lines fits in this region.
    Fragment(FlowLineFragment),
}

/// A consecutive line range selected for one region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowLineFragment {
    pub start: usize,
    pub end: usize,
    pub block_extent: f32,
}

/// Fits the longest line prefix in `available_block_extent`.
///
/// An over-tall first line is allowed in an empty region so malformed metrics
/// cannot cause an infinite pagination loop. In a non-empty region the caller
/// is asked to advance first, matching the existing Torto pagination behavior.
#[must_use]
pub fn fit_line_prefix<T>(
    lines: &[FlowLine<T>],
    start: usize,
    available_block_extent: f32,
    region_has_content: bool,
) -> Option<LineFit> {
    let first = lines.get(start)?;
    let first_top = first.block_min;
    let mut end = start;
    let mut block_extent = 0.0;

    while let Some(line) = lines.get(end) {
        let candidate_extent = line.block_max - first_top;
        if candidate_extent > available_block_extent && end > start {
            break;
        }
        if candidate_extent > available_block_extent && region_has_content {
            return Some(LineFit::AdvanceRegion);
        }
        block_extent = candidate_extent.max(line.line_height);
        end += 1;
    }

    Some(LineFit::Fragment(FlowLineFragment {
        start,
        end,
        block_extent,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::SpineItemId;

    fn line(index: usize, top: f32) -> FlowLine<usize> {
        FlowLine::new(index, top, top + 10.0, 12.0, None)
    }

    #[test]
    fn line_fitting_selects_a_stable_half_open_fragment() {
        let lines = [line(0, 0.0), line(1, 12.0), line(2, 24.0)];
        assert_eq!(
            fit_line_prefix(&lines, 0, 23.0, false),
            Some(LineFit::Fragment(FlowLineFragment {
                start: 0,
                end: 2,
                block_extent: 22.0,
            }))
        );
    }

    #[test]
    fn over_tall_first_line_advances_only_when_region_has_content() {
        let lines = [line(0, 0.0)];
        assert_eq!(
            fit_line_prefix(&lines, 0, 5.0, true),
            Some(LineFit::AdvanceRegion)
        );
        assert_eq!(
            fit_line_prefix(&lines, 0, 5.0, false),
            Some(LineFit::Fragment(FlowLineFragment {
                start: 0,
                end: 1,
                block_extent: 12.0,
            }))
        );
    }

    #[test]
    fn flow_contract_carries_glue_penalties_blocks_and_anchors() {
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p1".into(),
            text_offset: 0,
        };
        let items = [
            FlowItem::<()>::Glue(VerticalGlue::fixed(12.0)),
            FlowItem::Block(FlowBlock {
                payload: (),
                block_extent: 80.0,
                breakability: BlockBreakability::KeepTogether,
                source: None,
            }),
            FlowItem::Penalty(PagePenalty::FORCED),
            FlowItem::Anchor(anchor.clone()),
        ];
        assert!(matches!(items[0], FlowItem::Glue(_)));
        assert!(matches!(items[1], FlowItem::Block(_)));
        assert!(matches!(items[2], FlowItem::Penalty(PagePenalty::FORCED)));
        assert_eq!(items[3], FlowItem::Anchor(anchor));
    }
}
