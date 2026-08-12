//! Compiles immutable page layouts into cheap-to-replay display lists.

use std::ops::Range;
use std::sync::Arc;

use anyrender::{Glyph, NormalizedCoord, PaintScene};
use kurbo::{Affine, Line, Rect, Stroke, Vec2};
use parley::editing::{Cursor, Selection};
use parley::layout::{Affinity, Cluster, ClusterSide};
use parley::{FontData, Layout, PositionedLayoutItem};
use peniko::{Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
use rebook_layout::{
    ImagePlacement, PageItem, PageLayout, TablePlacement, TextBrush, TextPlacement,
};
use rebook_publication::{Rgba, SourceAnchor, SourceRange};

/// Pointer hit inside one retained text placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTextHit {
    pub region_index: usize,
    /// Caret boundary nearest to the pointer, used to determine drag direction.
    pub byte_index: usize,
    /// Logical byte range of the shaped cluster or fixed-page text span under the pointer.
    pub cluster_start: usize,
    pub cluster_end: usize,
}

/// One durable, single-block piece of a visual text selection.
#[derive(Debug, Clone)]
pub struct PageSelectionFragment {
    pub range: SourceRange,
    pub quote: String,
    pub rects: Vec<Rect>,
}

/// Original raster content for the top-most image under a page coordinate.
#[derive(Clone)]
pub struct PageImageHit {
    pub bounds: Rect,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

/// Retained drawing commands for one page. No parsing, shaping, or pagination
/// occurs while this list is replayed.
pub struct PageDisplayList {
    width: u32,
    height: u32,
    content_top: Option<f32>,
    content_bottom: Option<f32>,
    background: Color,
    commands: Vec<DisplayCommand>,
    text_regions: Vec<TextRegion>,
}

impl PageDisplayList {
    /// Logical width of the compiled page.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Logical height of the compiled page.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Top edge of retained page content in logical page coordinates.
    pub fn content_top(&self) -> Option<f32> {
        self.content_top
    }

    /// Bottom edge of the retained page content in logical page coordinates.
    ///
    /// Unlike the page height, this excludes unused pagination space after the
    /// final text line or image.
    pub fn content_bottom(&self) -> Option<f32> {
        self.content_bottom
    }

    /// Number of retained commands, useful for diagnostics.
    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    /// Number of source-backed text placements on this logical page.
    pub fn text_region_count(&self) -> usize {
        self.text_regions.len()
    }

    /// Bounds of retained raster page content in logical page coordinates.
    pub fn image_bounds(&self) -> Option<Rect> {
        self.commands
            .iter()
            .filter_map(|command| match command {
                DisplayCommand::Image(command) => Some(command.bounds),
                DisplayCommand::Glyphs(_)
                | DisplayCommand::FillRect(_)
                | DisplayCommand::Rule(_) => None,
            })
            .reduce(|bounds, next| bounds.union(next))
    }

    /// Returns the top-most retained raster image under the given page coordinate.
    pub fn image_at(&self, x: f32, y: f32) -> Option<PageImageHit> {
        let point = kurbo::Point::new(f64::from(x), f64::from(y));
        self.commands
            .iter()
            .rev()
            .find_map(|command| match command {
                DisplayCommand::Image(command)
                    if command.interactive && command.bounds.contains(point) =>
                {
                    Some(PageImageHit {
                        bounds: command.bounds,
                        width: command.width,
                        height: command.height,
                        pixels: Arc::clone(&command.pixels),
                    })
                }
                DisplayCommand::Glyphs(_)
                | DisplayCommand::Image(_)
                | DisplayCommand::FillRect(_)
                | DisplayCommand::Rule(_) => None,
            })
    }

    /// Resolves source-backed block images to page-coordinate rectangles.
    pub fn image_source_rects(&self, ranges: &[SourceRange]) -> Vec<Rect> {
        self.commands
            .iter()
            .filter_map(|command| match command {
                DisplayCommand::Image(command)
                    if command
                        .source
                        .as_ref()
                        .is_some_and(|source| ranges.iter().any(|range| range == source)) =>
                {
                    Some(command.bounds)
                }
                DisplayCommand::Glyphs(_)
                | DisplayCommand::Image(_)
                | DisplayCommand::FillRect(_)
                | DisplayCommand::Rule(_) => None,
            })
            .collect()
    }

    /// Visible UTF-8 byte range for a retained text placement.
    pub fn text_region_visible_range(&self, region_index: usize) -> Option<Range<usize>> {
        self.text_regions
            .get(region_index)
            .and_then(TextRegion::visible_byte_range)
    }

    /// Full shaped text retained for a source-backed region.
    pub fn text_region_text(&self, region_index: usize) -> Option<&str> {
        self.text_regions.get(region_index).map(TextRegion::text)
    }

    /// Full selectable byte range, including text outside this logical page's
    /// visible line slice when a paragraph continues onto another page.
    pub fn text_region_selectable_range(&self, region_index: usize) -> Option<Range<usize>> {
        self.text_regions
            .get(region_index)
            .map(TextRegion::selectable_byte_range)
    }

    /// Maps a byte range in retained text to its durable authored source range.
    pub fn text_region_source_range(
        &self,
        region_index: usize,
        byte_range: Range<usize>,
    ) -> Option<SourceRange> {
        self.text_regions
            .get(region_index)?
            .source_range_for_bytes(byte_range)
    }

    /// Returns the visible byte intersection of a durable source range in one
    /// retained text region.
    pub fn text_region_byte_range_for_source(
        &self,
        region_index: usize,
        range: &SourceRange,
    ) -> Option<Range<usize>> {
        self.text_regions
            .get(region_index)?
            .byte_range_for_source(range)
    }

    /// Returns the first durable source range visible on this page.
    ///
    /// This is used as the primary reading-position anchor. Unlike page numbers,
    /// the source range survives viewport, font, and pagination changes.
    pub fn leading_source_range(&self) -> Option<SourceRange> {
        self.text_regions
            .iter()
            .find_map(TextRegion::visible_source_range)
    }

    /// Hit-tests source-backed text. Exact mode is used when a drag starts;
    /// nearest mode lets a drag extend naturally through line/column whitespace.
    pub fn hit_test_text(&self, x: f32, y: f32, exact: bool) -> Option<PageTextHit> {
        if exact {
            return self
                .text_regions
                .iter()
                .enumerate()
                .find_map(|(index, region)| {
                    region.hit_test(x, y, true).map(|hit| PageTextHit {
                        region_index: index,
                        byte_index: hit.byte_index,
                        cluster_start: hit.cluster_start,
                        cluster_end: hit.cluster_end,
                    })
                });
        }

        self.text_regions
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                left.vertical_distance(y)
                    .total_cmp(&right.vertical_distance(y))
            })
            .and_then(|(index, region)| {
                region.hit_test(x, y, false).map(|hit| PageTextHit {
                    region_index: index,
                    byte_index: hit.byte_index,
                    cluster_start: hit.cluster_start,
                    cluster_end: hit.cluster_end,
                })
            })
    }

    /// Resolves a byte range in one retained placement to source anchors,
    /// selected text, and page-coordinate rectangles.
    pub fn selection_fragment(
        &self,
        region_index: usize,
        byte_range: Range<usize>,
    ) -> Option<PageSelectionFragment> {
        self.text_regions
            .get(region_index)?
            .selection_fragment(byte_range)
    }

    /// Resolves durable source ranges to page-coordinate highlight rectangles.
    pub fn source_rects(&self, ranges: &[SourceRange]) -> Vec<Rect> {
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

    pub fn contains_source_anchor(&self, anchor: &SourceAnchor) -> bool {
        self.text_regions
            .iter()
            .any(|region| region.contains_source_anchor(anchor))
    }

    pub fn source_ranges_contain_point(&self, ranges: &[SourceRange], x: f32, y: f32) -> bool {
        self.source_rects(ranges)
            .iter()
            .any(|rect| rect.contains(kurbo::Point::new(f64::from(x), f64::from(y))))
    }

    /// Replays this page into any `AnyRender` backend, including Vello GPU and CPU.
    pub fn paint(&self, scene: &mut impl PaintScene) {
        self.paint_scaled(scene, 1.0);
    }

    /// Replays logical page coordinates at the window's device scale.
    pub fn paint_scaled(&self, scene: &mut impl PaintScene, scale_factor: f32) {
        self.paint_scaled_at(scene, scale_factor, 0.0, 0.0);
    }

    /// Replays the page at a logical offset, used to compose reader chrome and
    /// the book surface without re-compiling either display list.
    pub fn paint_scaled_at(
        &self,
        scene: &mut impl PaintScene,
        scale_factor: f32,
        offset_x: f32,
        offset_y: f32,
    ) {
        let scale = Affine::scale(f64::from(scale_factor.max(0.1)))
            * Affine::translate((f64::from(offset_x), f64::from(offset_y)));
        self.paint_background_with_transform(scene, scale);
        self.paint_content_with_transform(scene, scale);
    }

    /// Paints only the page background. Spread composition paints this once,
    /// then overlays one or two logical page display lists.
    pub fn paint_background(&self, scene: &mut impl PaintScene) {
        self.paint_background_with_transform(scene, Affine::IDENTITY);
    }

    /// Paints retained page content without covering content already composed
    /// into the same spread.
    pub fn paint_content_at(&self, scene: &mut impl PaintScene, offset_x: f32) {
        self.paint_content_with_transform(scene, Affine::translate((f64::from(offset_x), 0.0)));
    }

    /// Paints fixed-page raster content below source range overlays.
    pub fn paint_images_at(&self, scene: &mut impl PaintScene, offset_x: f32) {
        let transform = Affine::translate((f64::from(offset_x), 0.0));
        for command in &self.commands {
            if matches!(
                command,
                DisplayCommand::Image(_) | DisplayCommand::FillRect(_)
            ) {
                command.paint(scene, transform);
            }
        }
    }

    /// Paints text and rules above source range overlays.
    pub fn paint_non_image_content_at(&self, scene: &mut impl PaintScene, offset_x: f32) {
        let transform = Affine::translate((f64::from(offset_x), 0.0));
        for command in &self.commands {
            if matches!(command, DisplayCommand::Glyphs(_) | DisplayCommand::Rule(_)) {
                command.paint(scene, transform);
            }
        }
    }

    /// Paints translucent source-backed marks below page content.
    pub fn paint_source_ranges(
        &self,
        scene: &mut impl PaintScene,
        ranges: &[SourceRange],
        color: Color,
        offset_x: f32,
    ) {
        let transform = Affine::translate((f64::from(offset_x), 0.0));
        for rect in self.source_rects(ranges) {
            scene.fill(Fill::NonZero, transform, color, None, &rect);
        }
    }

    /// Paints translucent marks over source-backed block images.
    pub fn paint_image_source_ranges(
        &self,
        scene: &mut impl PaintScene,
        ranges: &[SourceRange],
        color: Color,
        offset_x: f32,
    ) {
        let transform = Affine::translate((f64::from(offset_x), 0.0));
        for rect in self.image_source_rects(ranges) {
            scene.fill(Fill::NonZero, transform, color, None, &rect);
        }
    }

    fn paint_background_with_transform(&self, scene: &mut impl PaintScene, transform: Affine) {
        scene.fill(
            Fill::NonZero,
            transform,
            self.background,
            None,
            &Rect::new(0.0, 0.0, f64::from(self.width), f64::from(self.height)),
        );
    }

    fn paint_content_with_transform(&self, scene: &mut impl PaintScene, transform: Affine) {
        for command in &self.commands {
            command.paint(scene, transform);
        }
    }
}

enum TextRegion {
    Shaped(ShapedTextRegion),
    Fixed(FixedTextRegion),
}

struct TextRegionHit {
    byte_index: usize,
    cluster_start: usize,
    cluster_end: usize,
}

impl TextRegion {
    fn text(&self) -> &str {
        match self {
            Self::Shaped(region) => &region.text,
            Self::Fixed(region) => &region.text,
        }
    }

    fn selectable_byte_range(&self) -> Range<usize> {
        match self {
            Self::Shaped(region) => region.source_text_start..region.text.len(),
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
        match self {
            Self::Shaped(region) => region.source_range_for_bytes(visible),
            Self::Fixed(region) => region.source_range_for_bytes(visible),
        }
    }

    fn source_range_for_bytes(&self, byte_range: Range<usize>) -> Option<SourceRange> {
        match self {
            Self::Shaped(region) => region.source_range_for_bytes(byte_range),
            Self::Fixed(region) => region.source_range_for_bytes(byte_range),
        }
    }

    fn vertical_distance(&self, y: f32) -> f32 {
        match self {
            Self::Shaped(region) => region.vertical_distance(y),
            Self::Fixed(region) => region.vertical_distance(y),
        }
    }

    fn hit_test(&self, x: f32, y: f32, exact: bool) -> Option<TextRegionHit> {
        match self {
            Self::Shaped(region) => region.hit_test(x, y, exact),
            Self::Fixed(region) => region.hit_test(x, y, exact),
        }
    }

    fn selection_fragment(&self, byte_range: Range<usize>) -> Option<PageSelectionFragment> {
        match self {
            Self::Shaped(region) => region.selection_fragment(byte_range),
            Self::Fixed(region) => region.selection_fragment(byte_range),
        }
    }

    fn selection_rects(&self, byte_range: Range<usize>) -> Vec<Rect> {
        match self {
            Self::Shaped(region) => region.selection_rects(byte_range),
            Self::Fixed(region) => region.selection_rects(byte_range),
        }
    }

    fn byte_range_for_source(&self, range: &SourceRange) -> Option<Range<usize>> {
        match self {
            Self::Shaped(region) => region.byte_range_for_source(range),
            Self::Fixed(region) => region.byte_range_for_source(range),
        }
    }

    fn contains_source_anchor(&self, anchor: &SourceAnchor) -> bool {
        match self {
            Self::Shaped(region) => region.contains_source_anchor(anchor),
            Self::Fixed(region) => region.contains_source_anchor(anchor),
        }
    }
}

struct ShapedTextRegion {
    layout: Arc<Layout<TextBrush>>,
    text: Arc<str>,
    source_text_start: usize,
    lines: Range<usize>,
    origin_x: f32,
    origin_y: f32,
    source: SourceRange,
}

impl ShapedTextRegion {
    fn visible_byte_range(&self) -> Option<Range<usize>> {
        let first = self.layout.get(self.lines.start)?;
        let last = self.layout.get(self.lines.end.checked_sub(1)?)?;
        let start = first.text_range().start.max(self.source_text_start);
        let end = last.text_range().end.min(self.text.len());
        (end > start).then_some(start..end)
    }

    fn vertical_bounds(&self) -> Option<(f32, f32)> {
        let first = self.layout.get(self.lines.start)?;
        let last = self.layout.get(self.lines.end.checked_sub(1)?)?;
        Some((
            first.metrics().block_min_coord + self.origin_y,
            last.metrics().block_max_coord + self.origin_y,
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

    fn hit_test(&self, x: f32, y: f32, exact: bool) -> Option<TextRegionHit> {
        let (top, bottom) = self.vertical_bounds()?;
        if exact && !(top..=bottom).contains(&y) {
            return None;
        }
        let local_x = x - self.origin_x;
        let local_y = if exact {
            y - self.origin_y
        } else {
            y.clamp(top + 0.01, bottom - 0.01) - self.origin_y
        };
        let (byte_index, cluster_start, cluster_end) = if exact {
            let (cluster, side) = Cluster::from_point_exact(&self.layout, local_x, local_y)?;
            let range = cluster.text_range();
            let byte_index = if cluster.is_rtl() {
                if side == ClusterSide::Left {
                    range.end
                } else {
                    range.start
                }
            } else if side == ClusterSide::Left {
                range.start
            } else {
                range.end
            };
            (byte_index, range.start, range.end)
        } else {
            let byte_index = Cursor::from_point(&self.layout, local_x, local_y).index();
            (byte_index, byte_index, byte_index)
        };
        let visible = self.visible_byte_range()?;
        Some(TextRegionHit {
            byte_index: byte_index.clamp(visible.start, visible.end),
            cluster_start: cluster_start.clamp(visible.start, visible.end),
            cluster_end: cluster_end.clamp(visible.start, visible.end),
        })
    }

    fn selection_fragment(&self, byte_range: Range<usize>) -> Option<PageSelectionFragment> {
        let visible = self.visible_byte_range()?;
        let start = floor_char_boundary(
            &self.text,
            byte_range.start.clamp(visible.start, visible.end),
        );
        let end = floor_char_boundary(&self.text, byte_range.end.clamp(visible.start, visible.end));
        if end <= start {
            return None;
        }
        let range = self.source_range_for_bytes(start..end)?;
        Some(PageSelectionFragment {
            range,
            quote: self.text.get(start..end)?.to_owned(),
            rects: self.selection_rects(start..end),
        })
    }

    fn selection_rects(&self, byte_range: Range<usize>) -> Vec<Rect> {
        if byte_range.end <= byte_range.start {
            return Vec::new();
        }
        let selection = Selection::new(
            Cursor::from_byte_index(&self.layout, byte_range.start, Affinity::Downstream),
            Cursor::from_byte_index(&self.layout, byte_range.end, Affinity::Upstream),
        );
        let selected_text = selection.text_range();
        selection
            .geometry(&self.layout)
            .into_iter()
            .filter(|(_, line_index)| self.lines.contains(line_index))
            .map(|(rect, line_index)| {
                let mut x0 = rect.x0;
                let mut x1 = rect.x1;

                // Parley 0.10 does not include the extra whitespace advance added by
                // justified alignment in LineMetrics::advance. Its selection geometry
                // uses that stale value for fully selected middle lines, leaving the
                // right-hand end of those lines unpainted. Reconstruct the visual
                // advance from the adjusted clusters until the upstream issue is fixed:
                // https://github.com/linebender/parley/issues/396
                if let Some(line) = self.layout.get(line_index) {
                    let line_text = line.text_range();
                    if selected_text.start <= line_text.start && selected_text.end >= line_text.end
                    {
                        let text_advance = line
                            .runs()
                            .map(|run| {
                                run.visual_clusters()
                                    .map(|cluster| cluster.advance())
                                    .sum::<f32>()
                            })
                            .sum::<f32>();
                        let inline_box_advance = line
                            .items()
                            .filter_map(|item| match item {
                                PositionedLayoutItem::InlineBox(inline_box) => {
                                    Some(inline_box.width)
                                }
                                PositionedLayoutItem::GlyphRun(_) => None,
                            })
                            .sum::<f32>();
                        let visual_start =
                            f64::from(line.metrics().offset + line.metrics().inline_min_coord);
                        let visual_end =
                            visual_start + f64::from(text_advance + inline_box_advance);
                        x0 = x0.min(visual_start.min(visual_end));
                        x1 = x1.max(visual_start.max(visual_end));
                    }
                }

                Rect::new(
                    x0 + f64::from(self.origin_x),
                    rect.y0 + f64::from(self.origin_y),
                    x1 + f64::from(self.origin_x),
                    rect.y1 + f64::from(self.origin_y),
                )
            })
            .collect()
    }

    fn source_range_for_bytes(&self, byte_range: Range<usize>) -> Option<SourceRange> {
        if self.source.start.spine != self.source.end.spine
            || self.source.start.node != self.source.end.node
        {
            return None;
        }
        let source_start = self.source.start.text_offset;
        let start = source_start
            + u64::try_from(
                self.text
                    .get(self.source_text_start..byte_range.start)?
                    .chars()
                    .count(),
            )
            .ok()?;
        let end = source_start
            + u64::try_from(
                self.text
                    .get(self.source_text_start..byte_range.end)?
                    .chars()
                    .count(),
            )
            .ok()?;
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
        let source_text = self.text.get(self.source_text_start..)?;
        let start_chars = usize::try_from(start_offset - self.source.start.text_offset).ok()?;
        let end_chars = usize::try_from(end_offset - self.source.start.text_offset).ok()?;
        let start = self.source_text_start + byte_index_for_char_offset(source_text, start_chars);
        let end = self.source_text_start + byte_index_for_char_offset(source_text, end_chars);
        let visible = self.visible_byte_range()?;
        let start = start.max(visible.start).min(visible.end);
        let end = end.max(visible.start).min(visible.end);
        (end > start).then_some(start..end)
    }

    fn contains_source_anchor(&self, anchor: &SourceAnchor) -> bool {
        self.visible_byte_range()
            .and_then(|range| self.source_range_for_bytes(range))
            .is_some_and(|range| source_range_contains(&range, anchor))
    }
}

struct FixedTextRegion {
    text: Arc<str>,
    spans: Arc<[FixedTextSpan]>,
    source: SourceRange,
}

#[derive(Clone)]
struct FixedTextSpan {
    byte_range: Range<usize>,
    rect: Rect,
}

impl FixedTextRegion {
    fn visible_byte_range(&self) -> Option<Range<usize>> {
        (!self.text.is_empty() && !self.spans.is_empty()).then_some(0..self.text.len())
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "fixed page coordinates originate from bounded f32 layout dimensions"
    )]
    fn vertical_bounds(&self) -> Option<(f32, f32)> {
        let top = self
            .spans
            .iter()
            .map(|span| span.rect.y0 as f32)
            .min_by(f32::total_cmp)?;
        let bottom = self
            .spans
            .iter()
            .map(|span| span.rect.y1 as f32)
            .max_by(f32::total_cmp)?;
        Some((top, bottom))
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

    fn hit_test(&self, x: f32, y: f32, exact: bool) -> Option<TextRegionHit> {
        let point = kurbo::Point::new(f64::from(x), f64::from(y));
        let span = if exact {
            self.spans.iter().find(|span| span.rect.contains(point))?
        } else {
            self.spans.iter().min_by(|left, right| {
                rect_distance_squared(left.rect, point)
                    .total_cmp(&rect_distance_squared(right.rect, point))
            })?
        };
        let vertical = span.rect.height().abs() > span.rect.width().abs() * 1.5;
        let after_middle = if vertical {
            point.y >= span.rect.center().y
        } else {
            point.x >= span.rect.center().x
        };
        let byte_index = if after_middle {
            span.byte_range.end
        } else {
            span.byte_range.start
        };
        Some(TextRegionHit {
            byte_index,
            cluster_start: span.byte_range.start,
            cluster_end: span.byte_range.end,
        })
    }

    fn selection_fragment(&self, byte_range: Range<usize>) -> Option<PageSelectionFragment> {
        let visible = self.visible_byte_range()?;
        let start = floor_char_boundary(
            &self.text,
            byte_range.start.clamp(visible.start, visible.end),
        );
        let end = floor_char_boundary(&self.text, byte_range.end.clamp(visible.start, visible.end));
        if end <= start {
            return None;
        }
        Some(PageSelectionFragment {
            range: self.source_range_for_bytes(start..end)?,
            quote: self.text.get(start..end)?.to_owned(),
            rects: self.selection_rects(start..end),
        })
    }

    fn selection_rects(&self, byte_range: Range<usize>) -> Vec<Rect> {
        let rects = self
            .spans
            .iter()
            .filter(|span| {
                span.byte_range.start < byte_range.end && span.byte_range.end > byte_range.start
            })
            .map(|span| span.rect)
            .collect::<Vec<_>>();
        merge_fixed_text_rects(rects)
    }

    fn source_range_for_bytes(&self, byte_range: Range<usize>) -> Option<SourceRange> {
        if self.source.start.spine != self.source.end.spine
            || self.source.start.node != self.source.end.node
        {
            return None;
        }
        let start = self.source.start.text_offset
            + u64::try_from(self.text.get(..byte_range.start)?.chars().count()).ok()?;
        let end = self.source.start.text_offset
            + u64::try_from(self.text.get(..byte_range.end)?.chars().count()).ok()?;
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
        let start_chars = usize::try_from(start_offset - self.source.start.text_offset).ok()?;
        let end_chars = usize::try_from(end_offset - self.source.start.text_offset).ok()?;
        let start = byte_index_for_char_offset(&self.text, start_chars);
        let end = byte_index_for_char_offset(&self.text, end_chars);
        (end > start).then_some(start..end)
    }

    fn contains_source_anchor(&self, anchor: &SourceAnchor) -> bool {
        source_range_contains(&self.source, anchor)
    }
}

fn rect_distance_squared(rect: Rect, point: kurbo::Point) -> f64 {
    let dx = if point.x < rect.x0 {
        rect.x0 - point.x
    } else if point.x > rect.x1 {
        point.x - rect.x1
    } else {
        0.0
    };
    let dy = if point.y < rect.y0 {
        rect.y0 - point.y
    } else if point.y > rect.y1 {
        point.y - rect.y1
    } else {
        0.0
    };
    dx.mul_add(dx, dy * dy)
}

fn merge_fixed_text_rects(rects: Vec<Rect>) -> Vec<Rect> {
    let mut merged: Vec<Rect> = Vec::new();
    for rect in rects {
        if let Some(previous) = merged.last_mut() {
            let same_line = (previous.center().y - rect.center().y).abs()
                <= previous.height().abs().max(rect.height().abs()) * 0.55;
            let gap = rect.x0 - previous.x1;
            let merge_gap = previous.height().abs().max(rect.height().abs()) * 0.45;
            if same_line && gap <= merge_gap && rect.x1 >= previous.x0 {
                *previous = previous.union(rect);
                continue;
            }
        }
        merged.push(rect);
    }
    merged
}

fn byte_index_for_char_offset(text: &str, offset: usize) -> usize {
    text.char_indices()
        .nth(offset)
        .map_or(text.len(), |(index, _)| index)
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn source_range_contains(range: &SourceRange, anchor: &SourceAnchor) -> bool {
    range.start.spine == anchor.spine
        && range.start.node == anchor.node
        && anchor.text_offset >= range.start.text_offset
        && (anchor.text_offset < range.end.text_offset
            || (range.start.text_offset == range.end.text_offset
                && anchor.text_offset == range.start.text_offset))
}

enum DisplayCommand {
    Glyphs(GlyphCommand),
    Image(ImageCommand),
    FillRect(FillRectCommand),
    Rule(RuleCommand),
}

impl DisplayCommand {
    fn paint(&self, scene: &mut impl PaintScene, page_transform: Affine) {
        match self {
            Self::Glyphs(command) => scene.draw_glyphs(
                &command.font,
                command.font_size,
                true,
                &command.normalized_coords,
                Vec2::ZERO,
                Fill::NonZero,
                command.color,
                1.0,
                page_transform * command.transform,
                command.glyph_transform,
                command.glyphs.iter().copied(),
            ),
            Self::Image(command) => {
                scene.draw_image(command.image.as_ref(), page_transform * command.transform);
            }
            Self::FillRect(command) => scene.fill(
                Fill::NonZero,
                page_transform,
                command.color,
                None,
                &command.rect,
            ),
            Self::Rule(command) => scene.stroke(
                &Stroke::new(command.width),
                page_transform,
                command.color,
                None,
                &Line::new(command.start, command.end),
            ),
        }
    }
}

struct GlyphCommand {
    font: FontData,
    font_size: f32,
    normalized_coords: Arc<[NormalizedCoord]>,
    color: Color,
    transform: Affine,
    glyph_transform: Option<Affine>,
    glyphs: Arc<[Glyph]>,
}

struct ImageCommand {
    image: ImageBrush,
    transform: Affine,
    bounds: Rect,
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
    interactive: bool,
    source: Option<SourceRange>,
}

struct FillRectCommand {
    rect: Rect,
    color: Color,
}

struct RuleCommand {
    start: (f64, f64),
    end: (f64, f64),
    width: f64,
    color: Color,
}

/// Stateless compiler from layout IR to retained paint commands.
#[derive(Debug, Default)]
pub struct DisplayListCompiler;

impl DisplayListCompiler {
    pub fn compile(&self, page: &PageLayout) -> PageDisplayList {
        let content_top = page.items.iter().filter_map(page_item_top).reduce(f32::min);
        let content_bottom = page
            .items
            .iter()
            .filter_map(page_item_bottom)
            .reduce(f32::max);
        let mut commands = Vec::new();
        let mut text_regions = Vec::new();
        for item in &page.items {
            match item {
                PageItem::Text(text) => {
                    if let Some(region) = text_region(text) {
                        text_regions.push(region);
                    }
                    compile_text_commands(&mut commands, text);
                }
                PageItem::Table(table) => {
                    compile_table_commands(&mut commands, &mut text_regions, table);
                }
                PageItem::Image(image) => {
                    let data = ImageData {
                        data: Blob::new(Arc::new(image.image.pixels.clone())),
                        format: ImageFormat::Rgba8,
                        alpha_type: ImageAlphaType::Alpha,
                        width: image.image.width,
                        height: image.image.height,
                    };
                    let transform = Affine::translate((f64::from(image.x), f64::from(image.y)))
                        * Affine::scale_non_uniform(
                            f64::from(image.width) / f64::from(image.image.width.max(1)),
                            f64::from(image.height) / f64::from(image.image.height.max(1)),
                        );
                    commands.push(DisplayCommand::Image(ImageCommand {
                        image: ImageBrush::new(data),
                        transform,
                        bounds: Rect::new(
                            f64::from(image.x),
                            f64::from(image.y),
                            f64::from(image.x + image.width),
                            f64::from(image.y + image.height),
                        ),
                        width: image.image.width,
                        height: image.image.height,
                        pixels: Arc::clone(&image.image.pixels),
                        interactive: true,
                        source: image.source.clone(),
                    }));
                    if let Some(replacement) = &image.replacement {
                        for segment in &replacement.segments {
                            commands.push(DisplayCommand::FillRect(FillRectCommand {
                                rect: Rect::new(
                                    f64::from(segment.rect.x),
                                    f64::from(segment.rect.y),
                                    f64::from(segment.rect.x + segment.rect.width),
                                    f64::from(segment.rect.y + segment.rect.height),
                                ),
                                color: fixed_page_mask_color(image, segment.rect),
                            }));
                            if let Some(region) = text_region(&segment.text) {
                                text_regions.push(region);
                            }
                            compile_text_commands(&mut commands, &segment.text);
                        }
                    } else if let Some(region) = fixed_text_region(image) {
                        text_regions.push(region);
                    }
                }
                PageItem::Separator(separator) => {
                    commands.push(DisplayCommand::Rule(RuleCommand {
                        start: (f64::from(separator.x), f64::from(separator.y)),
                        end: (
                            f64::from(separator.x + separator.width),
                            f64::from(separator.y),
                        ),
                        width: 1.0,
                        color: Color::from_rgba8(120, 116, 108, 160),
                    }));
                }
            }
        }

        PageDisplayList {
            width: page.viewport.width,
            height: page.viewport.height,
            content_top,
            content_bottom,
            background: color(page.background),
            commands,
            text_regions,
        }
    }
}

fn compile_table_commands(
    commands: &mut Vec<DisplayCommand>,
    text_regions: &mut Vec<TextRegion>,
    table: &TablePlacement,
) {
    for cell in &table.cells {
        if cell.header {
            commands.push(DisplayCommand::FillRect(FillRectCommand {
                rect: Rect::new(
                    f64::from(cell.x),
                    f64::from(cell.y),
                    f64::from(cell.x + cell.width),
                    f64::from(cell.y + cell.height),
                ),
                color: color(table.header_fill),
            }));
        }
    }
    for cell in &table.cells {
        if let Some(text) = &cell.text {
            if let Some(region) = text_region(text) {
                text_regions.push(region);
            }
            compile_text_commands(commands, text);
        }
    }
    for cell in &table.cells {
        let left = f64::from(cell.x);
        let top = f64::from(cell.y);
        let right = f64::from(cell.x + cell.width);
        let bottom = f64::from(cell.y + cell.height);
        let border = color(table.border);
        for (start, end) in [
            ((left, top), (right, top)),
            ((right, top), (right, bottom)),
            ((right, bottom), (left, bottom)),
            ((left, bottom), (left, top)),
        ] {
            commands.push(DisplayCommand::Rule(RuleCommand {
                start,
                end,
                width: 1.0,
                color: border,
            }));
        }
    }
}

fn page_item_top(item: &PageItem) -> Option<f32> {
    match item {
        PageItem::Text(text) => text
            .layout
            .get(text.lines.start)
            .map(|line| text.origin_y + line.metrics().block_min_coord),
        PageItem::Image(image) => Some(image.y),
        PageItem::Table(table) => Some(table.y),
        PageItem::Separator(separator) => Some(separator.y),
    }
}

fn page_item_bottom(item: &PageItem) -> Option<f32> {
    match item {
        PageItem::Text(text) => text
            .lines
            .end
            .checked_sub(1)
            .and_then(|line| text.layout.get(line))
            .map(|line| text.origin_y + line.metrics().block_max_coord),
        PageItem::Image(image) => Some(image.y + image.height),
        PageItem::Table(table) => Some(table.y + table.height),
        PageItem::Separator(separator) => Some(separator.y + 1.0),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "fixed-page raster coordinates are clamped to bounded image dimensions before indexing"
)]
fn fixed_page_mask_color(
    image: &ImagePlacement,
    rect: rebook_publication::FixedPageTextRect,
) -> Color {
    let raster_width = image.image.width as usize;
    let raster_height = image.image.height as usize;
    if raster_width == 0
        || raster_height == 0
        || image.width <= 0.0
        || image.height <= 0.0
        || image.image.pixels.len() < raster_width.saturating_mul(raster_height).saturating_mul(4)
    {
        return Color::from_rgba8(255, 255, 255, 255);
    }
    let to_x = |x: f32| {
        (((x - image.x) / image.width) * image.image.width as f32)
            .floor()
            .clamp(0.0, (raster_width - 1) as f32) as usize
    };
    let to_y = |y: f32| {
        (((y - image.y) / image.height) * image.image.height as f32)
            .floor()
            .clamp(0.0, (raster_height - 1) as f32) as usize
    };
    let x0 = to_x(rect.x);
    let x1 = to_x(rect.x + rect.width).max(x0);
    let y0 = to_y(rect.y);
    let y1 = to_y(rect.y + rect.height).max(y0);
    let mut samples = Vec::<[u8; 4]>::new();
    let sample = |samples: &mut Vec<[u8; 4]>, x: usize, y: usize| {
        let offset = (y * raster_width + x) * 4;
        samples.push([
            image.image.pixels[offset],
            image.image.pixels[offset + 1],
            image.image.pixels[offset + 2],
            image.image.pixels[offset + 3],
        ]);
    };
    let steps = 12_usize;
    for step in 0..=steps {
        let x = x0 + (x1 - x0) * step / steps;
        let y = y0 + (y1 - y0) * step / steps;
        sample(&mut samples, x, y0);
        sample(&mut samples, x, y1);
        sample(&mut samples, x0, y);
        sample(&mut samples, x1, y);
    }
    let median = |channel: usize| {
        let mut values = samples
            .iter()
            .map(|sample| sample[channel])
            .collect::<Vec<_>>();
        values.sort_unstable();
        values[values.len() / 2]
    };
    Color::from_rgba8(median(0), median(1), median(2), median(3))
}

fn text_region(text: &TextPlacement) -> Option<TextRegion> {
    Some(TextRegion::Shaped(ShapedTextRegion {
        layout: Arc::clone(&text.layout),
        text: Arc::clone(&text.text),
        source_text_start: text.source_text_start,
        lines: text.lines.clone(),
        origin_x: text.origin_x,
        origin_y: text.origin_y,
        source: text.source.clone()?,
    }))
}

fn fixed_text_region(image: &ImagePlacement) -> Option<TextRegion> {
    let layer = image.text_layer.as_ref()?;
    let source = image.source.clone()?;
    if layer.text.is_empty() || layer.spans.is_empty() || layer.width <= 0.0 || layer.height <= 0.0
    {
        return None;
    }
    let scale_x = f64::from(image.width / layer.width);
    let scale_y = f64::from(image.height / layer.height);
    let spans = layer
        .spans
        .iter()
        .filter_map(|span| {
            let start_chars = usize::try_from(span.char_range.start).ok()?;
            let end_chars = usize::try_from(span.char_range.end).ok()?;
            let byte_start = byte_index_for_char_offset(&layer.text, start_chars);
            let byte_end = byte_index_for_char_offset(&layer.text, end_chars);
            if byte_end <= byte_start {
                return None;
            }
            let rect = &span.rect;
            Some(FixedTextSpan {
                byte_range: byte_start..byte_end,
                rect: Rect::new(
                    f64::from(image.x) + f64::from(rect.x) * scale_x,
                    f64::from(image.y) + f64::from(rect.y) * scale_y,
                    f64::from(image.x) + f64::from(rect.x + rect.width) * scale_x,
                    f64::from(image.y) + f64::from(rect.y + rect.height) * scale_y,
                ),
            })
        })
        .collect::<Vec<_>>();
    (!spans.is_empty()).then(|| {
        TextRegion::Fixed(FixedTextRegion {
            text: Arc::from(layer.text.as_str()),
            spans: spans.into(),
            source,
        })
    })
}

fn compile_text_commands(commands: &mut Vec<DisplayCommand>, text: &TextPlacement) {
    let transform = Affine::translate((f64::from(text.origin_x), f64::from(text.origin_y)));
    for line in text
        .layout
        .lines()
        .skip(text.lines.start)
        .take(text.lines.len())
    {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                let PositionedLayoutItem::InlineBox(inline_box) = item else {
                    continue;
                };
                let Some(image) = text
                    .inline_images
                    .iter()
                    .find(|image| image.id == inline_box.id)
                else {
                    continue;
                };
                let x = text.origin_x + inline_box.x;
                let y = text.origin_y + inline_box.y;
                let image_transform = Affine::translate((f64::from(x), f64::from(y)))
                    * Affine::scale_non_uniform(
                        f64::from(image.width) / f64::from(image.image.width.max(1)),
                        f64::from(image.height) / f64::from(image.image.height.max(1)),
                    );
                let data = ImageData {
                    data: Blob::new(Arc::new(image.image.pixels.clone())),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width: image.image.width,
                    height: image.image.height,
                };
                commands.push(DisplayCommand::Image(ImageCommand {
                    image: ImageBrush::new(data),
                    transform: image_transform,
                    bounds: Rect::new(
                        f64::from(x),
                        f64::from(y),
                        f64::from(x + image.width),
                        f64::from(y + image.height),
                    ),
                    width: image.image.width,
                    height: image.image.height,
                    pixels: Arc::clone(&image.image.pixels),
                    interactive: false,
                    source: None,
                }));
                continue;
            };
            let run = glyph_run.run();
            let brush = glyph_run.style().brush;
            let synthesis = run.synthesis();
            let glyph_transform = synthesis
                .skew()
                .map(|angle| Affine::skew(f64::from(angle.to_radians().tan()), 0.0));
            let glyphs = glyph_run
                .positioned_glyphs()
                .map(|glyph| Glyph {
                    id: glyph.id,
                    x: glyph.x,
                    y: glyph.y,
                })
                .collect::<Vec<_>>()
                .into();
            commands.push(DisplayCommand::Glyphs(GlyphCommand {
                font: run.font().clone(),
                font_size: run.font_size(),
                normalized_coords: run.normalized_coords().to_vec().into(),
                color: color(brush.color),
                transform,
                glyph_transform,
                glyphs,
            }));

            if brush.underline {
                let metrics = run.metrics();
                let y = f64::from(
                    glyph_run.baseline() - metrics.underline_offset + metrics.underline_size / 2.0,
                ) + f64::from(text.origin_y);
                let x = f64::from(glyph_run.offset() + text.origin_x);
                commands.push(DisplayCommand::Rule(RuleCommand {
                    start: (x, y),
                    end: (x + f64::from(glyph_run.advance()), y),
                    width: f64::from(metrics.underline_size.max(1.0)),
                    color: color(brush.color),
                }));
            }
        }
    }
}

fn color(value: Rgba) -> Color {
    Color::from_rgba8(value.red, value.green, value.blue, value.alpha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley::{Alignment, AlignmentOptions, FontContext, LayoutContext, StyleProperty};
    use rebook_layout::{
        FixedPageTextReplacementPlacement, FixedPageTextReplacementSegmentPlacement,
        ImagePlacement, LayoutViewport, PageItem, PageLayout, RasterImage, TextBrush,
        TextPlacement,
    };
    use rebook_publication::{
        FixedPageTextLayer, FixedPageTextRect, FixedPageTextSpan, SourceAnchor, SourceRange,
        SpineItemId,
    };

    #[test]
    fn empty_page_still_has_a_background() {
        let page = PageLayout {
            viewport: LayoutViewport::new(320, 240).unwrap(),
            background: Rgba::BLACK,
            items: Vec::new(),
        };
        let list = DisplayListCompiler.compile(&page);
        assert_eq!(list.width(), 320);
        assert_eq!(list.height(), 240);
        assert_eq!(list.content_top(), None);
        assert_eq!(list.content_bottom(), None);
        assert_eq!(list.command_count(), 0);
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the test uses small, bounded logical page coordinates"
    )]
    fn text_hits_and_source_ranges_round_trip_through_retained_geometry() {
        let text: Arc<str> = "hello world".into();
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::new();
        let mut builder =
            layout_context.ranged_builder(&mut font_context, text.as_ref(), 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
        }));
        let mut layout = builder.build(text.as_ref());
        layout.break_all_lines(Some(240.0));
        layout.align(Alignment::Start, AlignmentOptions::default());
        let spine = SpineItemId::new("chapter-1").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "paragraph-1".into(),
                text_offset: 11,
            },
        };
        let page = PageLayout {
            viewport: LayoutViewport::new(320, 240).unwrap(),
            background: Rgba::BLACK,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text,
                source_text_start: 0,
                lines: 0..1,
                origin_x: 24.0,
                origin_y: 24.0,
                source: Some(source),
                inline_images: Arc::from([]),
            })],
        };
        let list = DisplayListCompiler.compile(&page);
        assert!(
            list.content_bottom()
                .is_some_and(|bottom| bottom > 24.0 && bottom < 80.0),
            "text content should end near its shaped line rather than the 240px page boundary"
        );
        let selected_source = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("chapter-1").unwrap(),
                node: "paragraph-1".into(),
                text_offset: 1,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("chapter-1").unwrap(),
                node: "paragraph-1".into(),
                text_offset: 5,
            },
        };
        let rects = list.source_rects(std::slice::from_ref(&selected_source));
        assert!(!rects.is_empty());
        let point = rects[0].center();
        assert!(
            list.hit_test_text(point.x as f32, point.y as f32, true)
                .is_some()
        );

        let fragment = list.selection_fragment(0, 1..5).unwrap();
        assert_eq!(fragment.quote, "ello");
        assert_eq!(fragment.range, selected_source);
        assert!(!fragment.rects.is_empty());
    }

    #[test]
    fn selection_covers_the_visual_width_of_justified_middle_lines() {
        let text: Arc<str> =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".into();
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::new();
        let mut builder =
            layout_context.ranged_builder(&mut font_context, text.as_ref(), 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
        }));
        let mut layout = builder.build(text.as_ref());
        layout.break_all_lines(Some(150.0));
        layout.align(Alignment::Justify, AlignmentOptions::default());

        let (line_y, expected_right) = layout
            .lines()
            .skip(1)
            .take(layout.len().saturating_sub(2))
            .find_map(|line| {
                let visual_advance = line
                    .runs()
                    .map(|run| {
                        run.visual_clusters()
                            .map(|cluster| cluster.advance())
                            .sum::<f32>()
                    })
                    .sum::<f32>();
                (visual_advance > line.metrics().advance + 1.0).then_some((
                    line.metrics().block_min_coord,
                    line.metrics().offset + line.metrics().inline_min_coord + visual_advance,
                ))
            })
            .expect("the fixture should contain a justified middle line");

        let spine = SpineItemId::new("chapter-1").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "paragraph-1".into(),
                text_offset: u64::try_from(text.chars().count()).unwrap(),
            },
        };
        let line_count = layout.len();
        let origin_x = 24.0;
        let origin_y = 24.0;
        let page = PageLayout {
            viewport: LayoutViewport::new(320, 240).unwrap(),
            background: Rgba::BLACK,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text: Arc::clone(&text),
                source_text_start: 0,
                lines: 0..line_count,
                origin_x,
                origin_y,
                source: Some(source),
                inline_images: Arc::from([]),
            })],
        };

        let fragment = DisplayListCompiler
            .compile(&page)
            .selection_fragment(0, 0..text.len())
            .unwrap();
        let rect = fragment
            .rects
            .iter()
            .find(|rect| (rect.y0 - f64::from(line_y + origin_y)).abs() < 0.01)
            .expect("the justified line should have selection geometry");

        assert!((rect.x1 - f64::from(expected_right + origin_x)).abs() < 0.01);
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the test uses small, bounded logical page coordinates"
    )]
    fn fixed_page_text_geometry_supports_hit_testing_and_source_ranges() {
        let spine = SpineItemId::new("pdf-page-1").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "pdf-page-text".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "pdf-page-text".into(),
                text_offset: 3,
            },
        };
        let spans = (0_u16..3)
            .map(|index| FixedPageTextSpan {
                char_range: u64::from(index)..u64::from(index + 1),
                rect: FixedPageTextRect {
                    x: 10.0 + f32::from(index) * 10.0,
                    y: 20.0,
                    width: 9.0,
                    height: 12.0,
                },
            })
            .collect();
        let page = PageLayout {
            viewport: LayoutViewport::new(200, 200).unwrap(),
            background: Rgba::BLACK,
            items: vec![PageItem::Image(ImagePlacement {
                image: RasterImage {
                    width: 100,
                    height: 100,
                    pixels: vec![255; 100 * 100 * 4].into(),
                },
                x: 50.0,
                y: 40.0,
                width: 100.0,
                height: 100.0,
                source: Some(source.clone()),
                text_layer: Some(FixedPageTextLayer {
                    width: 100.0,
                    height: 100.0,
                    text: "PDF".into(),
                    spans,
                    replacement: None,
                }),
                replacement: None,
            })],
        };

        let list = DisplayListCompiler.compile(&page);
        assert_eq!(list.text_region_count(), 1);
        assert_eq!(
            list.image_bounds(),
            Some(Rect::new(50.0, 40.0, 150.0, 140.0))
        );
        assert_eq!(
            list.image_source_rects(std::slice::from_ref(&source)),
            [Rect::new(50.0, 40.0, 150.0, 140.0)]
        );
        let rects = list.source_rects(std::slice::from_ref(&source));
        assert_eq!(rects.len(), 1);
        let point = rects[0].center();
        let image = list
            .image_at(point.x as f32, point.y as f32)
            .expect("fixed image should be hit-testable");
        assert_eq!((image.width, image.height), (100, 100));
        assert_eq!(image.pixels.len(), 100 * 100 * 4);
        let hit = list
            .hit_test_text(point.x as f32, point.y as f32, true)
            .expect("fixed text should be hit-testable");
        let fragment = list.selection_fragment(hit.region_index, 0..3).unwrap();
        assert_eq!(fragment.quote, "PDF");
        assert_eq!(fragment.range, source);
    }

    #[test]
    fn fixed_page_replacement_compiles_image_mask_and_translated_glyphs() {
        let text: Arc<str> = "译文".into();
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::new();
        let mut builder =
            layout_context.ranged_builder(&mut font_context, text.as_ref(), 1.0, false);
        builder.push_default(StyleProperty::FontSize(14.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
        }));
        let mut layout = builder.build(text.as_ref());
        layout.break_all_lines(Some(80.0));
        layout.align(Alignment::Start, AlignmentOptions::default());
        let line_count = layout.len();
        let spine = SpineItemId::new("pdf-page-1").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "pdf-page-text".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "pdf-page-text".into(),
                text_offset: 2,
            },
        };
        let page = PageLayout {
            viewport: LayoutViewport::new(200, 200).unwrap(),
            background: Rgba::BLACK,
            items: vec![PageItem::Image(ImagePlacement {
                image: RasterImage {
                    width: 100,
                    height: 100,
                    pixels: [200, 201, 202, 255].repeat(100 * 100).into(),
                },
                x: 50.0,
                y: 40.0,
                width: 100.0,
                height: 100.0,
                source: Some(source.clone()),
                text_layer: None,
                replacement: Some(FixedPageTextReplacementPlacement {
                    segments: vec![FixedPageTextReplacementSegmentPlacement {
                        rect: FixedPageTextRect {
                            x: 60.0,
                            y: 60.0,
                            width: 80.0,
                            height: 30.0,
                        },
                        text: TextPlacement {
                            layout: Arc::new(layout),
                            text,
                            source_text_start: 0,
                            lines: 0..line_count,
                            origin_x: 64.0,
                            origin_y: 64.0,
                            source: Some(source.clone()),
                            inline_images: Arc::from([]),
                        },
                    }],
                }),
            })],
        };

        let list = DisplayListCompiler.compile(&page);

        assert!(matches!(
            list.commands.first(),
            Some(DisplayCommand::Image(_))
        ));
        assert!(matches!(
            list.commands.get(1),
            Some(DisplayCommand::FillRect(_))
        ));
        let Some(DisplayCommand::FillRect(mask)) = list.commands.get(1) else {
            unreachable!();
        };
        assert_eq!(mask.color, Color::from_rgba8(200, 201, 202, 255));
        assert!(
            list.commands
                .iter()
                .skip(2)
                .any(|command| matches!(command, DisplayCommand::Glyphs(_)))
        );
        assert_eq!(list.text_region_count(), 1);
        let fragment = list.selection_fragment(0, 0.."译文".len()).unwrap();
        assert_eq!(fragment.quote, "译文");
        assert_eq!(fragment.range, source);
    }
}
