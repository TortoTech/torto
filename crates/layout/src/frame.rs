//! Backend-neutral interaction data attached to immutable layout frames.

use std::ops::{Deref, Range};
use std::sync::Arc;

use rebook_publication::{SourceAnchor, SourceRange};

use crate::text::{TextLayoutStore, TextLineSpan, TextSourceMap};
use crate::{ImagePlacement, PageItem, PageLayout, TextPlacement};

/// Immutable ownership boundary for one completed logical page.
///
/// Store-local text line identifiers and interaction indexes are rebuilt for
/// every layout pass; durable source anchors remain stable across passes.
pub struct LayoutFrame {
    page: PageLayout,
    source_anchors: Option<PageSourceAnchors>,
    interaction: FrameInteractionMap,
}

impl LayoutFrame {
    #[must_use]
    pub fn freeze(page: PageLayout) -> Self {
        let source_anchors = page.source_anchors();
        let interaction = FrameInteractionMap::from_page(&page);
        Self {
            page,
            source_anchors,
            interaction,
        }
    }

    #[must_use]
    pub const fn page(&self) -> &PageLayout {
        &self.page
    }

    #[must_use]
    pub const fn source_anchors(&self) -> Option<&PageSourceAnchors> {
        self.source_anchors.as_ref()
    }

    #[must_use]
    pub const fn interaction(&self) -> &FrameInteractionMap {
        &self.interaction
    }

    /// Top and bottom of retained content in logical frame coordinates.
    ///
    /// Continuous/focus readers use this neutral geometry when stitching
    /// immutable frames. It deliberately derives from layout items rather than
    /// a renderer's retained command list.
    #[must_use]
    pub fn content_vertical_bounds(&self) -> Option<(f32, f32)> {
        let content = self
            .page
            .items
            .iter()
            .filter(|item| !matches!(item, PageItem::Quote(_)))
            .filter_map(item_vertical_bounds)
            .reduce(|(top, bottom), (next_top, next_bottom)| {
                (top.min(next_top), bottom.max(next_bottom))
            });
        content.or_else(|| {
            self.page
                .items
                .iter()
                .filter_map(|item| match item {
                    PageItem::Quote(_) => item_vertical_bounds(item),
                    PageItem::Text(_)
                    | PageItem::Table(_)
                    | PageItem::Image(_)
                    | PageItem::Separator(_) => None,
                })
                .reduce(|(top, bottom), (next_top, next_bottom)| {
                    (top.min(next_top), bottom.max(next_bottom))
                })
        })
    }

    /// Returns the union of text and semantic block geometry belonging to the
    /// supplied durable source ranges.
    #[must_use]
    pub fn source_content_bounds(&self, ranges: &[SourceRange]) -> Option<FrameRect> {
        self.interaction
            .source_rects(ranges)
            .into_iter()
            .chain(
                self.page
                    .items
                    .iter()
                    .filter_map(|item| item_source_bounds(item, ranges)),
            )
            .reduce(FrameRect::union)
    }

    /// Returns rectangular semantic-block geometry when the supplied ranges
    /// belong to a quote, table, image, or one shaped text block.
    #[must_use]
    pub fn source_block_bounds(&self, ranges: &[SourceRange]) -> Option<FrameRect> {
        self.page
            .items
            .iter()
            .filter_map(|item| item_source_bounds(item, ranges))
            .reduce(FrameRect::union)
            .or_else(|| self.interaction.source_block_bounds(ranges))
    }

    /// Returns the visible geometry containing one durable source anchor.
    ///
    /// This is used to restore a scroll/focus viewport after repagination
    /// without persisting generation-local line or frame identifiers.
    #[must_use]
    pub fn source_anchor_bounds(&self, anchor: &SourceAnchor) -> Option<FrameRect> {
        self.interaction
            .source_anchor_bounds(anchor)
            .into_iter()
            .chain(
                self.page
                    .items
                    .iter()
                    .filter_map(|item| item_anchor_bounds(item, anchor)),
            )
            .reduce(FrameRect::union)
    }

    /// Returns every visible text line's authored range in visual order.
    ///
    /// This is intentionally independent of line IDs and shaping backends so
    /// migration probes can compare source coverage even when two text engines
    /// choose different soft breaks.
    #[must_use]
    pub fn line_source_ranges(&self) -> Vec<SourceRange> {
        self.page.line_source_ranges()
    }
}

fn item_vertical_bounds(item: &PageItem) -> Option<(f32, f32)> {
    let bounds = match item {
        PageItem::Text(text) => text
            .lines
            .iter()
            .filter_map(|line| text.layout.line(line))
            .map(|line| {
                (
                    text.origin_y + line.metrics.block_min,
                    text.origin_y + line.metrics.block_max,
                )
            })
            .reduce(|(top, bottom), (next_top, next_bottom)| {
                (top.min(next_top), bottom.max(next_bottom))
            })?,
        PageItem::Quote(quote) => (quote.y, quote.y + quote.height),
        PageItem::Table(table) => (table.y, table.y + table.height),
        PageItem::Image(image) => (image.y, image.y + image.height),
        PageItem::Separator(separator) => (separator.y, separator.y + 1.0),
    };
    (bounds.0.is_finite() && bounds.1.is_finite() && bounds.1 >= bounds.0).then_some(bounds)
}

fn item_source_bounds(item: &PageItem, ranges: &[SourceRange]) -> Option<FrameRect> {
    let matches = |source: &SourceRange| ranges.iter().any(|range| range == source);
    match item {
        PageItem::Quote(quote) if quote.sources.iter().any(&matches) => Some(FrameRect::new(
            quote.x,
            quote.y,
            quote.x + quote.width,
            quote.y + quote.height,
        )),
        PageItem::Table(table)
            if table.cells.iter().any(|cell| {
                cell.text
                    .as_ref()
                    .and_then(|text| text.source.as_ref())
                    .is_some_and(&matches)
            }) =>
        {
            let (x0, x1) = table
                .cells
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(x0, x1), cell| {
                    (x0.min(cell.x), x1.max(cell.x + cell.width))
                });
            (x0.is_finite() && x1.is_finite())
                .then(|| FrameRect::new(x0, table.y, x1, table.y + table.height))
        }
        PageItem::Image(image) if image.source.as_ref().is_some_and(matches) => {
            Some(FrameRect::new(
                image.x,
                image.y,
                image.x + image.width,
                image.y + image.height,
            ))
        }
        PageItem::Text(_)
        | PageItem::Quote(_)
        | PageItem::Table(_)
        | PageItem::Image(_)
        | PageItem::Separator(_) => None,
    }
}

fn item_anchor_bounds(item: &PageItem, anchor: &SourceAnchor) -> Option<FrameRect> {
    let contains = |source: &SourceRange| source_range_contains(source, anchor);
    match item {
        PageItem::Quote(quote) if quote.sources.iter().any(&contains) => Some(FrameRect::new(
            quote.x,
            quote.y,
            quote.x + quote.width,
            quote.y + quote.height,
        )),
        PageItem::Table(table)
            if table.cells.iter().any(|cell| {
                cell.text
                    .as_ref()
                    .and_then(|text| text.source.as_ref())
                    .is_some_and(&contains)
            }) =>
        {
            let (x0, x1) = table
                .cells
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(x0, x1), cell| {
                    (x0.min(cell.x), x1.max(cell.x + cell.width))
                });
            (x0.is_finite() && x1.is_finite())
                .then(|| FrameRect::new(x0, table.y, x1, table.y + table.height))
        }
        PageItem::Image(image) if image.source.as_ref().is_some_and(contains) => {
            Some(FrameRect::new(
                image.x,
                image.y,
                image.x + image.width,
                image.y + image.height,
            ))
        }
        PageItem::Text(_)
        | PageItem::Quote(_)
        | PageItem::Table(_)
        | PageItem::Image(_)
        | PageItem::Separator(_) => None,
    }
}

impl Deref for LayoutFrame {
    type Target = PageLayout;

    fn deref(&self) -> &Self::Target {
        self.page()
    }
}

/// First and last durable source anchors visible in one logical page frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSourceAnchors {
    pub first: SourceAnchor,
    pub last: SourceAnchor,
}

impl PageLayout {
    /// Returns every visible text line's authored range in visual order.
    #[must_use]
    pub fn line_source_ranges(&self) -> Vec<SourceRange> {
        let mut ranges = Vec::new();
        for item in &self.items {
            match item {
                PageItem::Text(text) => append_text_line_ranges(text, &mut ranges),
                PageItem::Table(table) => {
                    for text in table.cells.iter().filter_map(|cell| cell.text.as_ref()) {
                        append_text_line_ranges(text, &mut ranges);
                    }
                }
                PageItem::Image(_) | PageItem::Quote(_) | PageItem::Separator(_) => {}
            }
        }
        ranges
    }

    /// Returns the authored source extent in visual item order.
    #[must_use]
    pub fn source_anchors(&self) -> Option<PageSourceAnchors> {
        let mut first = None;
        let mut last = None;
        let mut include = |range: SourceRange| {
            first.get_or_insert_with(|| range.start.clone());
            last = Some(range.end);
        };
        for item in &self.items {
            match item {
                PageItem::Text(text) => {
                    for line in text.lines.iter() {
                        if let Some(range) = text.line_source_range(line) {
                            include(range);
                        }
                    }
                }
                PageItem::Table(table) => {
                    for text in table.cells.iter().filter_map(|cell| cell.text.as_ref()) {
                        for line in text.lines.iter() {
                            if let Some(range) = text.line_source_range(line) {
                                include(range);
                            }
                        }
                    }
                }
                PageItem::Image(image) => {
                    if let Some(range) = image.source.clone() {
                        include(range);
                    }
                }
                // Quote ranges duplicate child text placements. Decorations
                // and separators do not own authored reading order.
                PageItem::Quote(_) | PageItem::Separator(_) => {}
            }
        }
        Some(PageSourceAnchors {
            first: first?,
            last: last?,
        })
    }
}

fn append_text_line_ranges(text: &TextPlacement, ranges: &mut Vec<SourceRange>) {
    ranges.extend(
        text.lines
            .iter()
            .filter_map(|line| text.line_source_range(line)),
    );
}

/// Rectangle in logical frame coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl FrameRect {
    #[must_use]
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    #[must_use]
    pub fn contains(self, x: f32, y: f32) -> bool {
        (self.x0..=self.x1).contains(&x) && (self.y0..=self.y1).contains(&y)
    }

    #[must_use]
    pub fn center(self) -> (f32, f32) {
        ((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }
}

/// Pointer hit inside one frame text region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTextHit {
    pub region_index: usize,
    pub byte_index: usize,
    pub cluster_start: usize,
    pub cluster_end: usize,
}

/// Logical caret at an authored UTF-8 cluster boundary inside one frame.
///
/// Region indexes and byte offsets are generation-local. Persisted reading
/// state must continue to use [`SourceRange`] and resolve a fresh pair of
/// cursors after re-layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameTextCursor {
    pub region_index: usize,
    pub byte_index: usize,
}

impl FrameTextCursor {
    #[must_use]
    pub const fn new(region_index: usize, byte_index: usize) -> Self {
        Self {
            region_index,
            byte_index,
        }
    }
}

/// Source-backed selected text and its frame-coordinate geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameSelectionFragment {
    pub range: SourceRange,
    pub quote: String,
    pub rects: Vec<FrameRect>,
}

/// Complete source-backed selection inside one immutable frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameTextSelection {
    pub ranges: Vec<SourceRange>,
    pub quote: String,
    pub rects: Vec<FrameRect>,
}

/// Immutable source map and hit regions for one page frame.
#[derive(Clone, Default)]
pub struct FrameInteractionMap {
    text_regions: Arc<[FrameTextRegion]>,
}

impl FrameInteractionMap {
    #[must_use]
    pub fn from_page(page: &PageLayout) -> Self {
        let mut regions = Vec::new();
        for item in &page.items {
            match item {
                PageItem::Text(text) => push_shaped_region(&mut regions, text),
                PageItem::Table(table) => {
                    for text in table.cells.iter().filter_map(|cell| cell.text.as_ref()) {
                        push_shaped_region(&mut regions, text);
                    }
                }
                PageItem::Image(image) => {
                    if let Some(replacement) = &image.replacement {
                        for segment in &replacement.segments {
                            push_shaped_region(&mut regions, &segment.text);
                        }
                    } else if let Some(region) = fixed_text_region(image) {
                        regions.push(FrameTextRegion::Fixed(region));
                    }
                }
                PageItem::Quote(_) | PageItem::Separator(_) => {}
            }
        }
        Self {
            text_regions: regions.into(),
        }
    }

    #[must_use]
    pub fn text_region_count(&self) -> usize {
        self.text_regions.len()
    }

    #[must_use]
    pub fn text(&self, region: usize) -> Option<&str> {
        self.text_regions.get(region).map(FrameTextRegion::text)
    }

    #[must_use]
    pub fn selectable_byte_range(&self, region: usize) -> Option<Range<usize>> {
        self.text_regions
            .get(region)
            .map(FrameTextRegion::selectable_byte_range)
    }

    #[must_use]
    pub fn visible_byte_range(&self, region: usize) -> Option<Range<usize>> {
        self.text_regions
            .get(region)
            .and_then(FrameTextRegion::visible_byte_range)
    }

    #[must_use]
    pub fn source_range_for_bytes(
        &self,
        region: usize,
        bytes: Range<usize>,
    ) -> Option<SourceRange> {
        self.text_regions.get(region)?.source_range_for_bytes(bytes)
    }

    /// Converts a pointer hit to its nearest generation-local caret.
    #[must_use]
    pub const fn cursor_for_hit(hit: FrameTextHit) -> FrameTextCursor {
        FrameTextCursor::new(hit.region_index, hit.byte_index)
    }

    #[must_use]
    pub fn first_cursor(&self) -> Option<FrameTextCursor> {
        self.text_regions
            .iter()
            .enumerate()
            .find_map(|(region_index, region)| {
                region
                    .caret_boundaries()
                    .first()
                    .copied()
                    .map(|byte_index| FrameTextCursor::new(region_index, byte_index))
            })
    }

    #[must_use]
    pub fn last_cursor(&self) -> Option<FrameTextCursor> {
        self.text_regions
            .iter()
            .enumerate()
            .rev()
            .find_map(|(region_index, region)| {
                region
                    .caret_boundaries()
                    .last()
                    .copied()
                    .map(|byte_index| FrameTextCursor::new(region_index, byte_index))
            })
    }

    /// Returns the previous logical shaped-cluster boundary, crossing text
    /// regions when necessary. This is intentionally a logical-order API;
    /// visual `BiDi` navigation remains a later text-engine capability.
    #[must_use]
    pub fn previous_cursor(&self, cursor: FrameTextCursor) -> Option<FrameTextCursor> {
        let region = cursor
            .region_index
            .min(self.text_regions.len().saturating_sub(1));
        for region_index in (0..=region).rev() {
            let boundaries = self.text_regions.get(region_index)?.caret_boundaries();
            let before = if region_index == cursor.region_index {
                boundaries
                    .into_iter()
                    .rev()
                    .find(|boundary| *boundary < cursor.byte_index)
            } else {
                boundaries.last().copied()
            };
            if let Some(byte_index) = before {
                return Some(FrameTextCursor::new(region_index, byte_index));
            }
        }
        None
    }

    /// Returns the next logical shaped-cluster boundary, crossing text regions
    /// when necessary.
    #[must_use]
    pub fn next_cursor(&self, cursor: FrameTextCursor) -> Option<FrameTextCursor> {
        for region_index in cursor.region_index..self.text_regions.len() {
            let boundaries = self.text_regions.get(region_index)?.caret_boundaries();
            let after = if region_index == cursor.region_index {
                boundaries
                    .into_iter()
                    .find(|boundary| *boundary > cursor.byte_index)
            } else {
                boundaries.first().copied()
            };
            if let Some(byte_index) = after {
                return Some(FrameTextCursor::new(region_index, byte_index));
            }
        }
        None
    }

    /// Resolves durable selected ranges to the first and last visible caret in
    /// this frame generation.
    #[must_use]
    pub fn cursors_for_source_ranges(
        &self,
        ranges: &[SourceRange],
    ) -> Option<(FrameTextCursor, FrameTextCursor)> {
        let start = self
            .text_regions
            .iter()
            .enumerate()
            .find_map(|(region_index, region)| {
                ranges
                    .iter()
                    .filter_map(|range| region.byte_range_for_source(range))
                    .map(|bytes| bytes.start)
                    .min()
                    .map(|byte_index| FrameTextCursor::new(region_index, byte_index))
            })?;
        let end =
            self.text_regions
                .iter()
                .enumerate()
                .rev()
                .find_map(|(region_index, region)| {
                    ranges
                        .iter()
                        .filter_map(|range| region.byte_range_for_source(range))
                        .map(|bytes| bytes.end)
                        .max()
                        .map(|byte_index| FrameTextCursor::new(region_index, byte_index))
                })?;
        (end > start).then_some((start, end))
    }

    #[must_use]
    pub fn byte_range_for_source(
        &self,
        region: usize,
        range: &SourceRange,
    ) -> Option<Range<usize>> {
        self.text_regions.get(region)?.byte_range_for_source(range)
    }

    #[must_use]
    pub fn leading_source_range(&self) -> Option<SourceRange> {
        self.text_regions
            .iter()
            .find_map(FrameTextRegion::visible_source_range)
    }

    #[must_use]
    pub fn source_range_nearest_y(&self, y: f32) -> Option<(f32, SourceRange)> {
        self.text_regions
            .iter()
            .filter_map(|region| {
                Some((region.vertical_distance(y), region.visible_source_range()?))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
    }

    #[must_use]
    pub fn hit_test_text(&self, x: f32, y: f32, exact: bool) -> Option<FrameTextHit> {
        let candidate = if exact {
            self.text_regions
                .iter()
                .enumerate()
                .find_map(|(index, region)| Some((index, region.hit_test(x, y, true)?)))
        } else {
            self.text_regions
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| {
                    left.vertical_distance(y)
                        .total_cmp(&right.vertical_distance(y))
                })
                .and_then(|(index, region)| Some((index, region.hit_test(x, y, false)?)))
        }?;
        Some(FrameTextHit {
            region_index: candidate.0,
            byte_index: candidate.1.byte_index,
            cluster_start: candidate.1.cluster_start,
            cluster_end: candidate.1.cluster_end,
        })
    }

    #[must_use]
    pub fn selection_fragment(
        &self,
        region: usize,
        bytes: Range<usize>,
    ) -> Option<FrameSelectionFragment> {
        self.text_regions.get(region)?.selection_fragment(bytes)
    }

    /// Builds one logical-order selection between generation-local cursors.
    /// The returned ranges remain durable after the frame is discarded.
    #[must_use]
    pub fn selection_between(
        &self,
        anchor: FrameTextCursor,
        focus: FrameTextCursor,
    ) -> Option<FrameTextSelection> {
        let (start, end) = if anchor <= focus {
            (anchor, focus)
        } else {
            (focus, anchor)
        };
        if start == end || end.region_index >= self.text_regions.len() {
            return None;
        }

        let mut ranges = Vec::new();
        let mut quote = String::new();
        let mut rects = Vec::new();
        for region_index in start.region_index..=end.region_index {
            let region = self.text_regions.get(region_index)?;
            let visible = region.visible_byte_range()?;
            let byte_start = if region_index == start.region_index {
                start.byte_index.clamp(visible.start, visible.end)
            } else {
                visible.start
            };
            let byte_end = if region_index == end.region_index {
                end.byte_index.clamp(visible.start, visible.end)
            } else {
                visible.end
            };
            let Some(fragment) = region.selection_fragment(byte_start..byte_end) else {
                continue;
            };
            let source_continues = ranges.last().is_some_and(|previous: &SourceRange| {
                previous.end.spine == fragment.range.start.spine
                    && previous.end.node == fragment.range.start.node
                    && previous.end.text_offset == fragment.range.start.text_offset
            });
            append_selection_quote(&mut quote, &fragment.quote, source_continues);
            push_source_range(&mut ranges, fragment.range);
            rects.extend(fragment.rects);
        }
        (!ranges.is_empty() && !quote.trim().is_empty() && !rects.is_empty()).then_some(
            FrameTextSelection {
                ranges,
                quote,
                rects,
            },
        )
    }

    #[must_use]
    pub fn source_rects(&self, ranges: &[SourceRange]) -> Vec<FrameRect> {
        self.text_regions
            .iter()
            .flat_map(|region| {
                ranges
                    .iter()
                    .filter_map(|range| region.byte_range_for_source(range))
                    .flat_map(|range| region.selection_rects(range))
            })
            .collect()
    }

    #[must_use]
    pub fn source_block_bounds(&self, ranges: &[SourceRange]) -> Option<FrameRect> {
        self.text_regions
            .iter()
            .filter_map(|region| {
                ranges
                    .iter()
                    .find_map(|range| region.block_bounds_for_source(range))
            })
            .reduce(FrameRect::union)
            .map(|bounds| {
                FrameRect::new(
                    bounds.x0 - 8.0,
                    bounds.y0 - 6.0,
                    bounds.x1 + 8.0,
                    bounds.y1 + 6.0,
                )
            })
    }

    /// Resolves a durable anchor to its complete visible text-region bounds.
    #[must_use]
    pub fn source_anchor_bounds(&self, anchor: &SourceAnchor) -> Option<FrameRect> {
        self.text_regions
            .iter()
            .filter(|region| region.contains_source_anchor(anchor))
            .filter_map(FrameTextRegion::bounds)
            .reduce(FrameRect::union)
    }

    #[must_use]
    pub fn contains_source_anchor(&self, anchor: &SourceAnchor) -> bool {
        self.text_regions
            .iter()
            .any(|region| region.contains_source_anchor(anchor))
    }
}

#[derive(Clone)]
enum FrameTextRegion {
    Shaped(ShapedTextRegion),
    Fixed(FixedTextRegion),
}

impl FrameTextRegion {
    fn bounds(&self) -> Option<FrameRect> {
        match self {
            Self::Shaped(region) => region.block_bounds(),
            Self::Fixed(region) => region.bounds(),
        }
    }

    fn text(&self) -> &str {
        match self {
            Self::Shaped(region) => region.source_map.text(),
            Self::Fixed(region) => &region.text,
        }
    }

    fn selectable_byte_range(&self) -> Range<usize> {
        match self {
            Self::Shaped(region) => {
                region.source_map.source_text_start()..region.source_map.text().len()
            }
            Self::Fixed(region) => 0..region.text.len(),
        }
    }

    fn visible_byte_range(&self) -> Option<Range<usize>> {
        match self {
            Self::Shaped(region) => region.visible_byte_range(),
            Self::Fixed(region) => region.visible_byte_range(),
        }
    }

    fn visible_source_range(&self) -> Option<SourceRange> {
        let visible = self.visible_byte_range()?;
        self.source_range_for_bytes(visible)
    }

    fn caret_boundaries(&self) -> Vec<usize> {
        match self {
            Self::Shaped(region) => region.caret_boundaries(),
            Self::Fixed(region) => region.caret_boundaries(),
        }
    }

    fn source_range_for_bytes(&self, bytes: Range<usize>) -> Option<SourceRange> {
        match self {
            Self::Shaped(region) => region.source_range_for_bytes(bytes),
            Self::Fixed(region) => region.source_range_for_bytes(bytes),
        }
    }

    fn byte_range_for_source(&self, range: &SourceRange) -> Option<Range<usize>> {
        match self {
            Self::Shaped(region) => region.byte_range_for_source(range),
            Self::Fixed(region) => region.byte_range_for_source(range),
        }
    }

    fn vertical_distance(&self, y: f32) -> f32 {
        match self {
            Self::Shaped(region) => region.vertical_distance(y),
            Self::Fixed(region) => region.vertical_distance(y),
        }
    }

    fn hit_test(&self, x: f32, y: f32, exact: bool) -> Option<RawTextHit> {
        match self {
            Self::Shaped(region) => region.hit_test(x, y, exact),
            Self::Fixed(region) => region.hit_test(x, y, exact),
        }
    }

    fn selection_fragment(&self, bytes: Range<usize>) -> Option<FrameSelectionFragment> {
        match self {
            Self::Shaped(region) => region.selection_fragment(bytes),
            Self::Fixed(region) => region.selection_fragment(bytes),
        }
    }

    fn selection_rects(&self, bytes: Range<usize>) -> Vec<FrameRect> {
        match self {
            Self::Shaped(region) => region.selection_rects(bytes),
            Self::Fixed(region) => region.selection_rects(bytes),
        }
    }

    fn block_bounds_for_source(&self, range: &SourceRange) -> Option<FrameRect> {
        let bytes = self.byte_range_for_source(range)?;
        match self {
            Self::Shaped(region) => region.block_bounds(),
            Self::Fixed(region) => region
                .selection_rects(bytes)
                .into_iter()
                .reduce(FrameRect::union),
        }
    }

    fn contains_source_anchor(&self, anchor: &SourceAnchor) -> bool {
        self.visible_source_range()
            .is_some_and(|range| source_range_contains(&range, anchor))
    }
}

#[derive(Debug, Clone, Copy)]
struct RawTextHit {
    byte_index: usize,
    cluster_start: usize,
    cluster_end: usize,
}

#[derive(Clone)]
struct ShapedTextRegion {
    layout: TextLayoutStore,
    source_map: TextSourceMap,
    lines: TextLineSpan,
    origin_x: f32,
    origin_y: f32,
    available_width: f32,
}

impl ShapedTextRegion {
    fn visible_byte_range(&self) -> Option<Range<usize>> {
        self.layout.visible_byte_range(
            self.lines,
            self.source_map.source_text_start(),
            self.source_map.text().len(),
        )
    }

    fn caret_boundaries(&self) -> Vec<usize> {
        let Some(visible) = self.visible_byte_range() else {
            return Vec::new();
        };
        let mut boundaries = vec![visible.start, visible.end];
        for line in self.lines.iter().filter_map(|line| self.layout.line(line)) {
            for cluster in &*line.clusters {
                if cluster.text_range.end <= visible.start
                    || cluster.text_range.start >= visible.end
                {
                    continue;
                }
                boundaries.push(cluster.text_range.start.clamp(visible.start, visible.end));
                boundaries.push(cluster.text_range.end.clamp(visible.start, visible.end));
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        boundaries
    }

    fn vertical_bounds(&self) -> Option<(f32, f32)> {
        let (top, bottom) = self.layout.vertical_bounds(self.lines)?;
        Some((top + self.origin_y, bottom + self.origin_y))
    }

    fn block_bounds(&self) -> Option<FrameRect> {
        let (top, bottom) = self.vertical_bounds()?;
        Some(FrameRect::new(
            self.origin_x,
            top,
            self.origin_x + self.available_width,
            bottom,
        ))
    }

    fn vertical_distance(&self, y: f32) -> f32 {
        let Some((top, bottom)) = self.vertical_bounds() else {
            return f32::MAX;
        };
        if y < top {
            top - y
        } else if y > bottom {
            y - bottom
        } else {
            0.0
        }
    }

    fn hit_test(&self, x: f32, y: f32, exact: bool) -> Option<RawTextHit> {
        let hit = self.layout.hit_test(
            self.lines,
            self.source_map.source_text_start(),
            self.source_map.text().len(),
            x - self.origin_x,
            y - self.origin_y,
            exact,
        )?;
        Some(RawTextHit {
            byte_index: hit.byte_index,
            cluster_start: hit.cluster_start,
            cluster_end: hit.cluster_end,
        })
    }

    fn selection_fragment(&self, bytes: Range<usize>) -> Option<FrameSelectionFragment> {
        let visible = self.visible_byte_range()?;
        let text = self.source_map.text();
        let start = floor_char_boundary(text, bytes.start.clamp(visible.start, visible.end));
        let end = floor_char_boundary(text, bytes.end.clamp(visible.start, visible.end));
        (end > start).then_some(FrameSelectionFragment {
            range: self.source_range_for_bytes(start..end)?,
            quote: text.get(start..end)?.to_owned(),
            rects: self.selection_rects(start..end),
        })
    }

    fn selection_rects(&self, bytes: Range<usize>) -> Vec<FrameRect> {
        self.layout
            .selection_rects(self.lines, self.source_map.source_text_start(), bytes)
            .into_iter()
            .map(|rect| {
                FrameRect::new(
                    rect.x0 + self.origin_x,
                    rect.y0 + self.origin_y,
                    rect.x1 + self.origin_x,
                    rect.y1 + self.origin_y,
                )
            })
            .collect()
    }

    fn source_range_for_bytes(&self, bytes: Range<usize>) -> Option<SourceRange> {
        self.source_map.source_range_for_bytes(bytes)
    }

    fn byte_range_for_source(&self, range: &SourceRange) -> Option<Range<usize>> {
        let range = self.source_map.byte_range_for_source(range)?;
        let visible = self.visible_byte_range()?;
        let start = range.start.max(visible.start).min(visible.end);
        let end = range.end.max(visible.start).min(visible.end);
        (end > start).then_some(start..end)
    }
}

#[derive(Clone)]
struct FixedTextRegion {
    text: Arc<str>,
    spans: Arc<[FixedTextSpan]>,
    source: SourceRange,
}

#[derive(Debug, Clone)]
struct FixedTextSpan {
    byte_range: Range<usize>,
    rect: FrameRect,
}

impl FixedTextRegion {
    fn bounds(&self) -> Option<FrameRect> {
        self.spans
            .iter()
            .map(|span| span.rect)
            .reduce(FrameRect::union)
    }

    fn caret_boundaries(&self) -> Vec<usize> {
        let Some(visible) = self.visible_byte_range() else {
            return Vec::new();
        };
        let mut boundaries = self
            .text
            .char_indices()
            .map(|(index, _)| index)
            .filter(|index| *index >= visible.start && *index <= visible.end)
            .collect::<Vec<_>>();
        boundaries.push(visible.start);
        boundaries.push(visible.end);
        boundaries.sort_unstable();
        boundaries.dedup();
        boundaries
    }

    fn visible_byte_range(&self) -> Option<Range<usize>> {
        (!self.text.is_empty() && !self.spans.is_empty()).then_some(0..self.text.len())
    }

    fn vertical_distance(&self, y: f32) -> f32 {
        self.spans
            .iter()
            .map(|span| vertical_rect_distance(span.rect, y))
            .fold(f32::MAX, f32::min)
    }

    fn hit_test(&self, x: f32, y: f32, exact: bool) -> Option<RawTextHit> {
        let span = if exact {
            self.spans.iter().find(|span| span.rect.contains(x, y))?
        } else {
            self.spans.iter().min_by(|left, right| {
                rect_distance_squared(left.rect, x, y)
                    .total_cmp(&rect_distance_squared(right.rect, x, y))
            })?
        };
        let (center_x, center_y) = span.rect.center();
        let vertical =
            (span.rect.y1 - span.rect.y0).abs() > (span.rect.x1 - span.rect.x0).abs() * 1.5;
        let after_middle = if vertical {
            y >= center_y
        } else {
            x >= center_x
        };
        let byte_index = if after_middle {
            span.byte_range.end
        } else {
            span.byte_range.start
        };
        Some(RawTextHit {
            byte_index,
            cluster_start: span.byte_range.start,
            cluster_end: span.byte_range.end,
        })
    }

    fn selection_fragment(&self, bytes: Range<usize>) -> Option<FrameSelectionFragment> {
        let visible = self.visible_byte_range()?;
        let start = floor_char_boundary(&self.text, bytes.start.clamp(visible.start, visible.end));
        let end = floor_char_boundary(&self.text, bytes.end.clamp(visible.start, visible.end));
        (end > start).then_some(FrameSelectionFragment {
            range: self.source_range_for_bytes(start..end)?,
            quote: self.text.get(start..end)?.to_owned(),
            rects: self.selection_rects(start..end),
        })
    }

    fn selection_rects(&self, bytes: Range<usize>) -> Vec<FrameRect> {
        merge_fixed_text_rects(
            self.spans
                .iter()
                .filter(|span| {
                    span.byte_range.start < bytes.end && span.byte_range.end > bytes.start
                })
                .map(|span| span.rect)
                .collect(),
        )
    }

    fn source_range_for_bytes(&self, bytes: Range<usize>) -> Option<SourceRange> {
        if self.source.start.spine != self.source.end.spine
            || self.source.start.node != self.source.end.node
        {
            return None;
        }
        let start = self.source.start.text_offset
            + u64::try_from(self.text.get(..bytes.start)?.chars().count()).ok()?;
        let end = self.source.start.text_offset
            + u64::try_from(self.text.get(..bytes.end)?.chars().count()).ok()?;
        Some(SourceRange {
            start: SourceAnchor {
                spine: self.source.start.spine.clone(),
                node: self.source.start.node.clone(),
                text_offset: start,
            },
            end: SourceAnchor {
                spine: self.source.start.spine.clone(),
                node: self.source.start.node.clone(),
                text_offset: end,
            },
        })
    }

    fn byte_range_for_source(&self, range: &SourceRange) -> Option<Range<usize>> {
        if self.source.start.spine != self.source.end.spine
            || self.source.start.node != self.source.end.node
            || range.start.spine != range.end.spine
            || range.start.node != range.end.node
            || self.source.start.spine != range.start.spine
            || self.source.start.node != range.start.node
        {
            return None;
        }
        let start_offset = range
            .start
            .text_offset
            .max(self.source.start.text_offset)
            .min(self.source.end.text_offset);
        let end_offset = range
            .end
            .text_offset
            .max(self.source.start.text_offset)
            .min(self.source.end.text_offset);
        if end_offset <= start_offset {
            return None;
        }
        let start = byte_index_for_char_offset(
            &self.text,
            usize::try_from(start_offset - self.source.start.text_offset).ok()?,
        );
        let end = byte_index_for_char_offset(
            &self.text,
            usize::try_from(end_offset - self.source.start.text_offset).ok()?,
        );
        (end > start).then_some(start..end)
    }
}

fn push_shaped_region(regions: &mut Vec<FrameTextRegion>, text: &TextPlacement) {
    if let Some(source_map) = text.source_map() {
        regions.push(FrameTextRegion::Shaped(ShapedTextRegion {
            layout: text.layout.clone(),
            source_map,
            lines: text.lines,
            origin_x: text.origin_x,
            origin_y: text.origin_y,
            available_width: text.available_width,
        }));
    }
}

fn fixed_text_region(image: &ImagePlacement) -> Option<FixedTextRegion> {
    let layer = image.text_layer.as_ref()?;
    let source = image.source.clone()?;
    if layer.text.is_empty() || layer.spans.is_empty() || layer.width <= 0.0 || layer.height <= 0.0
    {
        return None;
    }
    let scale_x = image.width / layer.width;
    let scale_y = image.height / layer.height;
    let spans = layer
        .spans
        .iter()
        .filter_map(|span| {
            let byte_start = byte_index_for_char_offset(
                &layer.text,
                usize::try_from(span.char_range.start).ok()?,
            );
            let byte_end =
                byte_index_for_char_offset(&layer.text, usize::try_from(span.char_range.end).ok()?);
            (byte_end > byte_start).then_some(FixedTextSpan {
                byte_range: byte_start..byte_end,
                rect: FrameRect::new(
                    image.x + span.rect.x * scale_x,
                    image.y + span.rect.y * scale_y,
                    image.x + (span.rect.x + span.rect.width) * scale_x,
                    image.y + (span.rect.y + span.rect.height) * scale_y,
                ),
            })
        })
        .collect::<Vec<_>>();
    (!spans.is_empty()).then_some(FixedTextRegion {
        text: Arc::from(layer.text.as_str()),
        spans: spans.into(),
        source,
    })
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn byte_index_for_char_offset(text: &str, offset: usize) -> usize {
    text.char_indices()
        .nth(offset)
        .map_or(text.len(), |(index, _)| index)
}

fn vertical_rect_distance(rect: FrameRect, y: f32) -> f32 {
    if y < rect.y0 {
        rect.y0 - y
    } else if y > rect.y1 {
        y - rect.y1
    } else {
        0.0
    }
}

fn rect_distance_squared(rect: FrameRect, x: f32, y: f32) -> f32 {
    let dx = if x < rect.x0 {
        rect.x0 - x
    } else if x > rect.x1 {
        x - rect.x1
    } else {
        0.0
    };
    let dy = vertical_rect_distance(rect, y);
    dx.mul_add(dx, dy * dy)
}

fn merge_fixed_text_rects(mut rects: Vec<FrameRect>) -> Vec<FrameRect> {
    rects.sort_by(|left, right| {
        left.y0
            .total_cmp(&right.y0)
            .then_with(|| left.x0.total_cmp(&right.x0))
    });
    let mut merged: Vec<FrameRect> = Vec::new();
    for rect in rects {
        if let Some(previous) = merged.last_mut() {
            let same_line =
                (previous.y0 - rect.y0).abs() <= 2.0 && (previous.y1 - rect.y1).abs() <= 2.0;
            if same_line && rect.x0 <= previous.x1 + 2.0 {
                *previous = previous.union(rect);
                continue;
            }
        }
        merged.push(rect);
    }
    merged
}

fn append_selection_quote(output: &mut String, value: &str, source_continues: bool) {
    if value.is_empty() {
        return;
    }
    if !source_continues && !output.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(value);
}

fn push_source_range(ranges: &mut Vec<SourceRange>, range: SourceRange) {
    if let Some(previous) = ranges.last_mut()
        && previous.end.spine == range.start.spine
        && previous.end.node == range.start.node
        && previous.end.text_offset == range.start.text_offset
    {
        previous.end = range.end;
    } else {
        ranges.push(range);
    }
}

fn source_range_contains(range: &SourceRange, anchor: &SourceAnchor) -> bool {
    range.start.spine == anchor.spine
        && range.start.node == anchor.node
        && anchor.text_offset >= range.start.text_offset
        && (anchor.text_offset < range.end.text_offset
            || (range.start.text_offset == range.end.text_offset
                && anchor.text_offset == range.start.text_offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{
        TextCluster, TextLayoutStore, TextLineBreak, TextLineMetrics, TextLineSnapshot,
    };
    use crate::{LayoutViewport, PageLayout, QuotePlacement, TextPlacement};
    use rebook_publication::{Rgba, SpineItemId};

    #[test]
    fn logical_cursors_and_selection_cross_text_regions() {
        let frame = LayoutFrame::freeze(PageLayout {
            viewport: LayoutViewport::new(200, 200).unwrap(),
            background: Rgba::default(),
            leading_gap: 0.0,
            items: vec![text_item("ab", "a", 0.0), text_item("cd", "b", 30.0)],
        });
        let interaction = frame.interaction();

        assert_eq!(interaction.first_cursor(), Some(FrameTextCursor::new(0, 0)));
        assert_eq!(interaction.last_cursor(), Some(FrameTextCursor::new(1, 2)));
        assert_eq!(
            interaction.next_cursor(FrameTextCursor::new(0, 0)),
            Some(FrameTextCursor::new(0, 1))
        );
        assert_eq!(
            interaction.next_cursor(FrameTextCursor::new(0, 2)),
            Some(FrameTextCursor::new(1, 0))
        );
        assert_eq!(
            interaction.previous_cursor(FrameTextCursor::new(1, 0)),
            Some(FrameTextCursor::new(0, 2))
        );

        let selection = interaction
            .selection_between(FrameTextCursor::new(0, 1), FrameTextCursor::new(1, 1))
            .unwrap();
        assert_eq!(selection.quote, "b\n\nc");
        assert_eq!(selection.ranges.len(), 2);
        assert_eq!(selection.rects.len(), 2);
        assert_eq!(
            interaction.cursors_for_source_ranges(&selection.ranges),
            Some((FrameTextCursor::new(0, 1), FrameTextCursor::new(1, 1)))
        );
    }

    #[test]
    fn frame_content_bounds_are_derived_without_a_renderer() {
        let quoted_source = source_range("b");
        let frame = LayoutFrame::freeze(PageLayout {
            viewport: LayoutViewport::new(200, 200).unwrap(),
            background: Rgba::default(),
            leading_gap: 12.0,
            items: vec![
                text_item("ab", "a", 10.0),
                text_item("cd", "b", 50.0),
                PageItem::Quote(QuotePlacement {
                    x: 5.0,
                    y: 90.0,
                    width: 100.0,
                    height: 30.0,
                    continued_before: false,
                    continued_after: false,
                    fill: Rgba::default(),
                    accent: Rgba::default(),
                    sources: vec![quoted_source.clone()],
                }),
            ],
        });

        let (top, bottom) = frame.content_vertical_bounds().unwrap();
        assert!((top - 10.0).abs() < f32::EPSILON);
        assert!((bottom - 70.0).abs() < f32::EPSILON);
        assert!((frame.leading_gap - 12.0).abs() < f32::EPSILON);

        let content = frame
            .source_content_bounds(std::slice::from_ref(&quoted_source))
            .unwrap();
        assert!(content.x0.abs() < f32::EPSILON);
        assert!((content.y0 - 50.0).abs() < f32::EPSILON);
        assert!((content.x1 - 105.0).abs() < f32::EPSILON);
        assert!((content.y1 - 120.0).abs() < f32::EPSILON);
        let block = frame.source_block_bounds(&[quoted_source]).unwrap();
        assert!((block.x0 - 5.0).abs() < f32::EPSILON);
        assert!((block.y0 - 90.0).abs() < f32::EPSILON);
        assert!((block.x1 - 105.0).abs() < f32::EPSILON);
        assert!((block.y1 - 120.0).abs() < f32::EPSILON);

        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "b".into(),
            text_offset: 0,
        };
        let anchored = frame.source_anchor_bounds(&anchor).unwrap();
        assert!(anchored.x0.abs() < f32::EPSILON);
        assert!((anchored.y0 - 50.0).abs() < f32::EPSILON);
        assert!((anchored.x1 - 105.0).abs() < f32::EPSILON);
        assert!((anchored.y1 - 120.0).abs() < f32::EPSILON);
    }

    fn text_item(text: &str, node: &str, origin_y: f32) -> PageItem {
        let text: Arc<str> = Arc::from(text);
        let clusters = text
            .char_indices()
            .enumerate()
            .map(|(cluster, (start, character))| TextCluster {
                text_range: start..start + character.len_utf8(),
                inline_start: f32::from(u16::try_from(cluster).unwrap()) * 10.0,
                inline_end: f32::from(u16::try_from(cluster + 1).unwrap()) * 10.0,
                rtl: false,
            })
            .collect::<Vec<_>>();
        let layout = TextLayoutStore::from_snapshots(
            vec![TextLineSnapshot {
                text_range: 0..text.len(),
                metrics: TextLineMetrics {
                    block_min: 0.0,
                    block_max: 20.0,
                    line_height: 20.0,
                    offset: 0.0,
                    inline_min: 0.0,
                    inline_max: 20.0,
                    content_inline_max: 20.0,
                },
                break_kind: TextLineBreak::End,
                clusters: clusters.into(),
                items: Arc::from([]),
            }],
            20.0,
        )
        .unwrap();
        let lines = layout.line_span(0..1).unwrap();
        PageItem::Text(TextPlacement {
            layout,
            text,
            source_text_start: 0,
            lines,
            origin_x: 0.0,
            origin_y,
            available_width: 20.0,
            source: Some(source_range(node)),
            inline_images: Arc::from([]),
        })
    }

    fn source_range(node: &str) -> SourceRange {
        let spine = SpineItemId::new("chapter").unwrap();
        SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: node.into(),
                text_offset: 2,
            },
        }
    }
}
