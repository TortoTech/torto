//! Compiles immutable page layouts into cheap-to-replay display lists.

use std::ops::Range;
use std::sync::Arc;

use anyrender::{Glyph, NormalizedCoord, PaintScene};
use kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Shape, Stroke, Vec2};
use parley::editing::{Cursor, Selection};
use parley::layout::{Affinity, BreakReason, Cluster, ClusterSide};
use parley::{FontData, Layout, PositionedLayoutItem};
use peniko::{Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
use rebook_layout::{
    ImagePlacement, PageItem, PageLayout, QuotePlacement, TablePlacement, TextBrush, TextPlacement,
};
use rebook_publication::{Rgba, SourceAnchor, SourceRange, TextBaseline};

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

/// Paint style for connecting one semantic quote across continuously stitched pages.
#[derive(Clone, Copy)]
pub struct PageQuoteBridge {
    pub x: f32,
    pub width: f32,
    pub color: Color,
}

/// Retained drawing commands for one page. No parsing, shaping, or pagination
/// occurs while this list is replayed.
pub struct PageDisplayList {
    width: u32,
    height: u32,
    content_top: Option<f32>,
    content_bottom: Option<f32>,
    leading_gap: f32,
    background: Color,
    commands: Vec<DisplayCommand>,
    text_regions: Vec<TextRegion>,
    table_regions: Vec<TableRegion>,
    quote_regions: Vec<QuoteRegion>,
    footnote_regions: Vec<FootnoteRegion>,
}

struct TableRegion {
    bounds: Rect,
    sources: Vec<SourceRange>,
}

struct QuoteRegion {
    bounds: Rect,
    sources: Vec<SourceRange>,
    continued_before: bool,
    continued_after: bool,
    accent_x: f32,
    accent_width: f32,
    accent: Color,
}

struct FootnoteRegion {
    bounds: Rect,
    source: SourceRange,
}

const FOOTNOTE_ICON_CENTER_ABOVE_BASELINE: f32 = 0.68;

fn footnote_icon_bounds(center_x: f32, baseline: f32, font_size: f32) -> Rect {
    let size = (font_size * 0.78).clamp(8.0, 12.0);
    // Footnote sources are not uniform: some books use a superscript link while
    // others embed a normal-baseline inline note. The replacement icon should
    // occupy one stable optical superscript position regardless of that source
    // encoding. A center slightly over two thirds of its diameter above the
    // text baseline aligns with the upper half of both CJK em boxes and Latin
    // cap height without changing the placement of ordinary superscript text.
    let center_y = baseline - size * FOOTNOTE_ICON_CENTER_ABOVE_BASELINE;
    Rect::new(
        f64::from(center_x - size / 2.0),
        f64::from(center_y - size / 2.0),
        f64::from(center_x + size / 2.0),
        f64::from(center_y + size / 2.0),
    )
}

fn paint_footnote_region(
    scene: &mut impl PaintScene,
    footnote: &FootnoteRegion,
    color: Color,
    transform: Affine,
) {
    let bounds = footnote.bounds;
    let circle = Circle::new(bounds.center(), bounds.width().min(bounds.height()) * 0.5);
    scene.stroke(&Stroke::new(1.15), transform, color, None, &circle);
    let center_x = bounds.center().x;
    let dot = Rect::new(
        center_x - 0.7,
        bounds.y0 + 2.0,
        center_x + 0.7,
        bounds.y0 + 3.4,
    );
    scene.fill(Fill::NonZero, transform, color, None, &dot);
    scene.stroke(
        &Stroke::new(1.2),
        transform,
        color,
        None,
        &Line::new((center_x, bounds.y0 + 5.0), (center_x, bounds.y1 - 2.0)),
    );
}

const HIGHLIGHT_VERTICAL_OVERLAP: f64 = 0.5;

fn source_range_highlight_path(rects: impl IntoIterator<Item = Rect>) -> BezPath {
    let mut path = BezPath::new();
    for rect in rects {
        let rect = Rect::new(
            rect.x0,
            rect.y0 - HIGHLIGHT_VERTICAL_OVERLAP,
            rect.x1,
            rect.y1 + HIGHLIGHT_VERTICAL_OVERLAP,
        );
        path.extend(rect.path_elements(0.0));
    }
    path
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

    /// Semantic spacing removed when the first block was moved to this page.
    pub fn leading_gap(&self) -> f32 {
        self.leading_gap
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
                | DisplayCommand::FillRoundedRect(_)
                | DisplayCommand::Rule(_) => None,
            })
            .reduce(|bounds, next| bounds.union(next))
    }

    /// Raster resources referenced by this retained page.
    ///
    /// The desktop GPU renderer uses these handles to refresh Vello's image
    /// atlas before replaying a scene. Cloning an [`ImageData`] is cheap and
    /// preserves the blob identity encoded into the Vello scene.
    pub fn image_data(&self) -> impl Iterator<Item = &ImageData> {
        self.commands.iter().filter_map(|command| match command {
            DisplayCommand::Image(command) => Some(&command.image.image),
            DisplayCommand::Glyphs(_)
            | DisplayCommand::FillRect(_)
            | DisplayCommand::FillRoundedRect(_)
            | DisplayCommand::Rule(_) => None,
        })
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
                | DisplayCommand::FillRoundedRect(_)
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
                | DisplayCommand::FillRoundedRect(_)
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

    /// Returns the source-backed block nearest a vertical page coordinate.
    /// Continuous readers use this to persist the paragraph, table, or image at
    /// the top of the viewport instead of falling back to the page's first block.
    pub fn source_range_nearest_y(&self, y: f32) -> Option<SourceRange> {
        let mut nearest: Option<(f32, SourceRange)> = None;
        let mut consider = |distance: f32, range: SourceRange| {
            if nearest
                .as_ref()
                .is_none_or(|(current, _)| distance < *current)
            {
                nearest = Some((distance, range));
            }
        };
        for region in &self.text_regions {
            if let Some(range) = region.visible_source_range() {
                consider(region.vertical_distance(y), range);
            }
        }
        for table in &self.table_regions {
            if let Some(range) = table.sources.first() {
                consider(vertical_rect_distance(table.bounds, y), range.clone());
            }
        }
        for command in &self.commands {
            if let DisplayCommand::Image(image) = command
                && let Some(source) = &image.source
            {
                consider(vertical_rect_distance(image.bounds, y), source.clone());
            }
        }
        nearest.map(|(_, range)| range)
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

    /// Resolves table cell source ranges to their complete table-chunk bounds.
    pub fn source_table_bounds(&self, ranges: &[SourceRange]) -> Vec<Rect> {
        self.table_regions
            .iter()
            .filter(|table| {
                table
                    .sources
                    .iter()
                    .any(|source| ranges.iter().any(|range| range == source))
            })
            .map(|table| table.bounds)
            .collect()
    }

    /// Resolves quote child ranges to their complete semantic card bounds.
    pub fn source_quote_bounds(&self, ranges: &[SourceRange]) -> Vec<Rect> {
        self.quote_regions
            .iter()
            .filter(|quote| {
                quote
                    .sources
                    .iter()
                    .any(|source| ranges.iter().any(|range| range == source))
            })
            .map(|quote| quote.bounds)
            .collect()
    }

    /// Resolves a semantic quote or preformatted text range to one rectangular
    /// block suitable for focus-mode activation painting.
    pub fn source_block_bounds(&self, ranges: &[SourceRange]) -> Option<Rect> {
        let quote = self.source_quote_bounds(ranges);
        if !quote.is_empty() {
            return quote.into_iter().reduce(|bounds, next| bounds.union(next));
        }
        self.text_regions
            .iter()
            .filter_map(|region| {
                ranges
                    .iter()
                    .find_map(|range| region.block_bounds_for_source(range))
            })
            .reduce(|bounds, next| bounds.union(next))
            .map(|bounds| {
                Rect::new(
                    bounds.x0 - 8.0,
                    bounds.y0 - 6.0,
                    bounds.x1 + 8.0,
                    bounds.y1 + 6.0,
                )
            })
    }

    /// Paints one opaque rounded-rectangle activation fill below page text.
    pub fn paint_source_block_background(
        &self,
        scene: &mut impl PaintScene,
        ranges: &[SourceRange],
        color: Color,
        offset_x: f32,
    ) {
        let Some(bounds) = self.source_block_bounds(ranges) else {
            return;
        };
        let background = RoundedRect::from_rect(bounds, 7.0);
        scene.fill(
            Fill::NonZero,
            Affine::translate((f64::from(offset_x), 0.0)),
            color,
            None,
            &background,
        );
    }

    /// Returns the accent style when this page and `next` are consecutive
    /// slices of the same semantic quotation.
    pub fn quote_bridge_to(&self, next: &Self) -> Option<PageQuoteBridge> {
        let trailing = self
            .quote_regions
            .iter()
            .rev()
            .find(|quote| quote.continued_after)?;
        let leading = next
            .quote_regions
            .iter()
            .find(|quote| quote.continued_before)?;
        if trailing.sources.is_empty()
            || trailing.sources != leading.sources
            || (trailing.accent_x - leading.accent_x).abs() > 0.5
            || (trailing.accent_width - leading.accent_width).abs() > 0.5
        {
            return None;
        }
        Some(PageQuoteBridge {
            x: trailing.accent_x,
            width: trailing.accent_width,
            color: trailing.accent,
        })
    }

    /// Returns the union of text, image, and table geometry belonging to the
    /// supplied semantic source ranges.
    pub fn source_content_bounds(&self, ranges: &[SourceRange]) -> Option<Rect> {
        self.source_rects(ranges)
            .into_iter()
            .chain(self.image_source_rects(ranges))
            .chain(self.source_table_bounds(ranges))
            .chain(self.source_quote_bounds(ranges))
            .reduce(|bounds, next| bounds.union(next))
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
            if command.paints_below_source_overlays() {
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
        let path = source_range_highlight_path(self.source_rects(ranges));
        if !path.is_empty() {
            // Separate translucent AA rectangles can expose one-pixel conflation
            // seams at shared line edges in Vello. Paint one slightly-overlapped
            // non-zero path so adjacent rows are composited exactly once:
            // https://github.com/linebender/vello/issues/49
            // https://github.com/linebender/vello/issues/417
            scene.fill(Fill::NonZero, transform, color, None, &path);
        }
    }

    /// Paints compact footnote icons for the active focus-mode source ranges.
    pub fn paint_footnote_icons(
        &self,
        scene: &mut impl PaintScene,
        ranges: &[SourceRange],
        color: Color,
        offset_x: f32,
    ) {
        let transform = Affine::translate((f64::from(offset_x), 0.0));
        for footnote in &self.footnote_regions {
            if !ranges.iter().any(|range| range == &footnote.source) {
                continue;
            }
            paint_footnote_region(scene, footnote, color, transform);
        }
    }

    /// Paints every semantic footnote icon on a classic-mode page.
    pub fn paint_all_footnote_icons(
        &self,
        scene: &mut impl PaintScene,
        color: Color,
        offset_x: f32,
    ) {
        let transform = Affine::translate((f64::from(offset_x), 0.0));
        for footnote in &self.footnote_regions {
            paint_footnote_region(scene, footnote, color, transform);
        }
    }

    /// Returns the source-backed paragraph owning a semantic footnote icon.
    pub fn footnote_source_at(&self, x: f32, y: f32) -> Option<SourceRange> {
        let point = Point::new(f64::from(x), f64::from(y));
        self.footnote_regions
            .iter()
            .rev()
            .find(|footnote| footnote.bounds.contains(point))
            .map(|footnote| footnote.source.clone())
    }

    /// Paints block-level outlines for table chunks containing any requested source range.
    pub fn paint_source_table_borders(
        &self,
        scene: &mut impl PaintScene,
        ranges: &[SourceRange],
        color: Color,
        offset_x: f32,
    ) {
        let transform = Affine::translate((f64::from(offset_x), 0.0));
        let first = ranges.first();
        let last = ranges.last();
        for table in &self.table_regions {
            if table
                .sources
                .iter()
                .any(|source| ranges.iter().any(|range| range == source))
            {
                let left = table.bounds.x0;
                let top = table.bounds.y0;
                let right = table.bounds.x1;
                let bottom = table.bounds.y1;
                let contains = |range: Option<&SourceRange>| {
                    range.is_some_and(|range| table.sources.iter().any(|source| source == range))
                };
                let mut edges = vec![
                    Line::new((left, top), (left, bottom)),
                    Line::new((right, top), (right, bottom)),
                ];
                if contains(first) {
                    edges.push(Line::new((left, top), (right, top)));
                }
                if contains(last) {
                    edges.push(Line::new((left, bottom), (right, bottom)));
                }
                for edge in edges {
                    scene.stroke(&Stroke::new(2.0), transform, color, None, &edge);
                }
            }
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

#[allow(
    clippy::cast_possible_truncation,
    reason = "display-list coordinates are viewport-bounded f32 values stored in kurbo f64"
)]
fn vertical_rect_distance(rect: Rect, y: f32) -> f32 {
    let y = f64::from(y);
    if y < rect.y0 {
        (rect.y0 - y) as f32
    } else if y > rect.y1 {
        (y - rect.y1) as f32
    } else {
        0.0
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

    fn block_bounds_for_source(&self, range: &SourceRange) -> Option<Rect> {
        let byte_range = self.byte_range_for_source(range)?;
        match self {
            Self::Shaped(region) => region.block_bounds(),
            Self::Fixed(region) => region
                .selection_rects(byte_range)
                .into_iter()
                .reduce(|bounds, next| bounds.union(next)),
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
    available_width: f32,
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

    fn block_bounds(&self) -> Option<Rect> {
        let (top, bottom) = self.vertical_bounds()?;
        Some(Rect::new(
            f64::from(self.origin_x),
            f64::from(top),
            f64::from(self.origin_x + self.available_width),
            f64::from(bottom),
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
                    // Synthetic list markers precede `source_text_start` and are
                    // intentionally not selectable. A source-backed selection that
                    // starts at the first real character still covers the complete
                    // visual first line, so ignore the marker-only byte prefix when
                    // deciding whether the line should receive full-width geometry.
                    let selectable_line_start = line_text.start.max(self.source_text_start);
                    if selected_text.start <= selectable_line_start
                        && selected_text.end >= line_text.end
                    {
                        let visual_start =
                            f64::from(line.metrics().offset + line.metrics().inline_min_coord);
                        let soft_wrapped = line.break_reason() == BreakReason::Regular;
                        let visual_end = if soft_wrapped {
                            // A soft-wrapped line occupies the full paragraph measure,
                            // even when an unbreakable Latin word leaves visible space at
                            // its end. Keep the final/explicit line content-sized, but make
                            // every wrapped line's active highlight share one right edge.
                            // `inline_max_coord` is already expressed in the line's
                            // paragraph coordinate space. A hanging indent is carried
                            // separately by `offset`; adding it here a second time makes
                            // list highlights protrude past the paragraph's right edge.
                            f64::from(line.metrics().inline_max_coord)
                        } else {
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
                            visual_start + f64::from(text_advance + inline_box_advance)
                        };
                        if soft_wrapped {
                            // The first list line may start with a synthetic marker
                            // before `source_text_start`. Keep Parley's source-backed
                            // selection start so the marker is not highlighted; only
                            // extend the right edge to the shared wrapped-line limit.
                            if line_text.start >= self.source_text_start {
                                x0 = visual_start.min(visual_end);
                            }
                            x1 = visual_start.max(visual_end);
                        } else {
                            x0 = x0.min(visual_start.min(visual_end));
                            x1 = x1.max(visual_start.max(visual_end));
                        }
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
        let source_length = self.source.end.text_offset.checked_sub(source_start)?;
        let source_text = self.text.get(self.source_text_start..)?;
        let text_length = source_text.chars().count();
        let start_chars = self
            .text
            .get(self.source_text_start..byte_range.start)?
            .chars()
            .count();
        let end_chars = self
            .text
            .get(self.source_text_start..byte_range.end)?
            .chars()
            .count();
        let start = source_start
            + scale_text_offset_to_source(start_chars, text_length, source_length, false)?;
        let end = source_start
            + scale_text_offset_to_source(end_chars, text_length, source_length, true)?;
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
        let text_length = source_text.chars().count();
        let source_length = self
            .source
            .end
            .text_offset
            .checked_sub(self.source.start.text_offset)?;
        let start_chars = scale_source_offset_to_text(
            start_offset - self.source.start.text_offset,
            source_length,
            text_length,
            false,
        )?;
        let end_chars = scale_source_offset_to_text(
            end_offset - self.source.start.text_offset,
            source_length,
            text_length,
            true,
        )?;
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

fn scale_text_offset_to_source(
    offset: usize,
    text_length: usize,
    source_length: u64,
    round_up: bool,
) -> Option<u64> {
    if text_length == 0 {
        return Some(0);
    }
    let numerator = u128::try_from(offset).ok()? * u128::from(source_length);
    let denominator = u128::try_from(text_length).ok()?;
    let scaled = if round_up {
        numerator.div_ceil(denominator)
    } else {
        numerator / denominator
    };
    u64::try_from(scaled).ok()
}

fn scale_source_offset_to_text(
    offset: u64,
    source_length: u64,
    text_length: usize,
    round_up: bool,
) -> Option<usize> {
    if source_length == 0 {
        return Some(0);
    }
    let numerator = u128::from(offset) * u128::try_from(text_length).ok()?;
    let denominator = u128::from(source_length);
    let scaled = if round_up {
        numerator.div_ceil(denominator)
    } else {
        numerator / denominator
    };
    usize::try_from(scaled).ok()
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
    FillRoundedRect(FillRoundedRectCommand),
    Rule(RuleCommand),
}

impl DisplayCommand {
    fn paints_below_source_overlays(&self) -> bool {
        matches!(
            self,
            Self::Image(_) | Self::FillRect(_) | Self::FillRoundedRect(_)
        )
    }

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
            Self::FillRoundedRect(command) => scene.fill(
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

struct FillRoundedRectCommand {
    rect: RoundedRect,
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
        let mut table_regions = Vec::new();
        let mut quote_regions = Vec::new();
        let mut footnote_regions = Vec::new();
        for item in &page.items {
            match item {
                PageItem::Text(text) => {
                    if let Some(region) = text_region(text) {
                        text_regions.push(region);
                    }
                    compile_text_commands(&mut commands, &mut footnote_regions, text);
                }
                PageItem::Quote(quote) => {
                    let bounds = quote_bounds(quote);
                    quote_regions.push(QuoteRegion {
                        bounds,
                        sources: quote.sources.clone(),
                        continued_before: quote.continued_before,
                        continued_after: quote.continued_after,
                        accent_x: quote.x + 6.0,
                        accent_width: 4.0,
                        accent: color(quote.accent),
                    });
                    if quote.fill.alpha > 0 {
                        commands.push(DisplayCommand::FillRoundedRect(FillRoundedRectCommand {
                            rect: RoundedRect::from_rect(bounds, 7.0),
                            color: color(quote.fill),
                        }));
                    }
                    let accent_inset = 8.0_f64.min(bounds.height() * 0.2);
                    let accent_top = if quote.continued_before {
                        bounds.y0
                    } else {
                        bounds.y0 + accent_inset
                    };
                    let accent_bottom = if quote.continued_after {
                        bounds.y1
                    } else {
                        bounds.y1 - accent_inset
                    };
                    commands.push(DisplayCommand::FillRoundedRect(FillRoundedRectCommand {
                        rect: RoundedRect::from_rect(
                            Rect::new(bounds.x0 + 6.0, accent_top, bounds.x0 + 10.0, accent_bottom),
                            2.0,
                        ),
                        color: color(quote.accent),
                    }));
                }
                PageItem::Table(table) => {
                    if let Some(region) = table_region(table) {
                        table_regions.push(region);
                    }
                    compile_table_commands(
                        &mut commands,
                        &mut text_regions,
                        &mut footnote_regions,
                        table,
                    );
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
                            compile_text_commands(
                                &mut commands,
                                &mut footnote_regions,
                                &segment.text,
                            );
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
            leading_gap: page.leading_gap.max(0.0),
            background: color(page.background),
            commands,
            text_regions,
            table_regions,
            quote_regions,
            footnote_regions,
        }
    }
}

fn quote_bounds(quote: &QuotePlacement) -> Rect {
    Rect::new(
        f64::from(quote.x),
        f64::from(quote.y),
        f64::from(quote.x + quote.width),
        f64::from(quote.y + quote.height),
    )
}

fn table_region(table: &TablePlacement) -> Option<TableRegion> {
    let bounds = table
        .cells
        .iter()
        .map(|cell| {
            Rect::new(
                f64::from(cell.x),
                f64::from(cell.y),
                f64::from(cell.x + cell.width),
                f64::from(cell.y + cell.height),
            )
        })
        .reduce(|current, next| current.union(next))?;
    let sources = table
        .cells
        .iter()
        .filter_map(|cell| cell.text.as_ref()?.source.clone())
        .collect();
    Some(TableRegion { bounds, sources })
}

fn compile_table_commands(
    commands: &mut Vec<DisplayCommand>,
    text_regions: &mut Vec<TextRegion>,
    footnote_regions: &mut Vec<FootnoteRegion>,
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
            compile_text_commands(commands, footnote_regions, text);
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
        PageItem::Quote(quote) => (!quote.continued_before).then_some(quote.y),
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
        PageItem::Quote(quote) => (!quote.continued_after).then_some(quote.y + quote.height),
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
        available_width: text.available_width,
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

fn compile_text_commands(
    commands: &mut Vec<DisplayCommand>,
    footnote_regions: &mut Vec<FootnoteRegion>,
    text: &TextPlacement,
) {
    let transform = Affine::translate((f64::from(text.origin_x), f64::from(text.origin_y)));
    let mut compiled_footnote_groups = Vec::<u32>::new();
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
            let baseline_offset = match brush.baseline {
                TextBaseline::Normal => 0.0,
                TextBaseline::Superscript => -run.font_size() * 0.35,
                TextBaseline::Subscript => run.font_size() * 0.2,
            };
            if brush.footnote_reference {
                if compiled_footnote_groups.contains(&brush.footnote_reference_group) {
                    continue;
                }
                if let Some(source) = text.source.clone() {
                    let center_x = text.origin_x + glyph_run.offset() + glyph_run.advance() / 2.0;
                    let bounds = footnote_icon_bounds(
                        center_x,
                        text.origin_y + glyph_run.baseline(),
                        run.font_size(),
                    );
                    footnote_regions.push(FootnoteRegion { bounds, source });
                    compiled_footnote_groups.push(brush.footnote_reference_group);
                }
                continue;
            }
            let synthesis = run.synthesis();
            let glyph_transform = synthesis
                .skew()
                .map(|angle| Affine::skew(f64::from(angle.to_radians().tan()), 0.0));
            let glyphs = glyph_run
                .positioned_glyphs()
                .map(|glyph| Glyph {
                    id: glyph.id,
                    x: glyph.x,
                    y: glyph.y + baseline_offset,
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
                    glyph_run.baseline() + baseline_offset - metrics.underline_offset
                        + metrics.underline_size / 2.0,
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
        ImagePlacement, LayoutViewport, PageItem, PageLayout, QuotePlacement, RasterImage,
        SeparatorPlacement, TextBrush, TextPlacement,
    };

    #[test]
    fn rounded_quote_decorations_are_painted_below_source_overlays() {
        let page = PageLayout {
            viewport: LayoutViewport::new(200, 200).unwrap(),
            background: Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            leading_gap: 0.0,
            items: vec![PageItem::Quote(QuotePlacement {
                x: 20.0,
                y: 30.0,
                width: 160.0,
                height: 80.0,
                continued_before: false,
                continued_after: false,
                fill: Rgba {
                    alpha: 0,
                    ..Rgba::BLACK
                },
                accent: Rgba {
                    red: 0xD1,
                    green: 0xD7,
                    blue: 0xDE,
                    alpha: 255,
                },
                sources: Vec::new(),
            })],
        };

        let list = DisplayListCompiler.compile(&page);
        let quote_decorations = list
            .commands
            .iter()
            .filter(|command| matches!(command, DisplayCommand::FillRoundedRect(_)))
            .collect::<Vec<_>>();
        assert_eq!(quote_decorations.len(), 1);
        assert!(
            quote_decorations
                .iter()
                .all(|command| command.paints_below_source_overlays())
        );
    }

    #[test]
    fn continued_quote_segments_trim_page_padding_and_join_the_accent() {
        let page = PageLayout {
            viewport: LayoutViewport::new(200, 200).unwrap(),
            background: Rgba::BLACK,
            leading_gap: 0.0,
            items: vec![
                PageItem::Quote(QuotePlacement {
                    x: 20.0,
                    y: 10.0,
                    width: 160.0,
                    height: 180.0,
                    continued_before: true,
                    continued_after: true,
                    fill: Rgba {
                        alpha: 0,
                        ..Rgba::BLACK
                    },
                    accent: Rgba::BLACK,
                    sources: Vec::new(),
                }),
                PageItem::Separator(SeparatorPlacement {
                    x: 40.0,
                    y: 50.0,
                    width: 80.0,
                }),
            ],
        };

        let list = DisplayListCompiler.compile(&page);
        assert_eq!(list.content_top(), Some(50.0));
        assert_eq!(list.content_bottom(), Some(51.0));
        let accent = list
            .commands
            .iter()
            .find_map(|command| match command {
                DisplayCommand::FillRoundedRect(command) => Some(command.rect.bounding_box()),
                _ => None,
            })
            .expect("continued quote should paint an accent");
        assert_eq!(accent.y0, 10.0);
        assert_eq!(accent.y1, 190.0);
    }

    #[test]
    fn matching_quote_continuations_expose_a_scroll_bridge() {
        let spine = SpineItemId::new("chapter").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "quote".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "quote".into(),
                text_offset: 20,
            },
        };
        let page = |continued_before, continued_after| {
            DisplayListCompiler.compile(&PageLayout {
                viewport: LayoutViewport::new(200, 200).unwrap(),
                background: Rgba::BLACK,
                leading_gap: 0.0,
                items: vec![PageItem::Quote(QuotePlacement {
                    x: 20.0,
                    y: 10.0,
                    width: 160.0,
                    height: 180.0,
                    continued_before,
                    continued_after,
                    fill: Rgba {
                        alpha: 0,
                        ..Rgba::BLACK
                    },
                    accent: Rgba::BLACK,
                    sources: vec![source.clone()],
                })],
            })
        };

        let first = page(false, true);
        let second = page(true, false);
        assert_eq!(
            first.source_block_bounds(std::slice::from_ref(&source)),
            Some(Rect::new(20.0, 10.0, 180.0, 190.0))
        );
        let bridge = first
            .quote_bridge_to(&second)
            .expect("matching continuation slices should expose their accent style");
        assert_eq!(bridge.x, 26.0);
        assert_eq!(bridge.width, 4.0);
    }
    use rebook_publication::{
        FixedPageTextLayer, FixedPageTextRect, FixedPageTextSpan, SourceAnchor, SourceRange,
        SpineItemId,
    };

    #[test]
    fn multiline_highlight_geometry_overlaps_adjacent_antialiased_edges() {
        let path = source_range_highlight_path([
            Rect::new(10.0, 10.0, 80.0, 25.0),
            Rect::new(10.0, 25.0, 60.0, 40.0),
        ]);

        assert_eq!(path.bounding_box(), Rect::new(10.0, 9.5, 80.0, 40.5));
        assert_eq!(
            path.elements()
                .iter()
                .filter(|element| matches!(element, kurbo::PathEl::MoveTo(_)))
                .count(),
            2
        );
    }

    #[test]
    fn empty_page_still_has_a_background() {
        let page = PageLayout {
            viewport: LayoutViewport::new(320, 240).unwrap(),
            background: Rgba::BLACK,
            leading_gap: 0.0,
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
    fn multi_run_footnote_reference_compiles_to_one_icon_region() {
        let text: Arc<str> = "A【3】".into();
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::new();
        let mut builder =
            layout_context.ranged_builder(&mut font_context, text.as_ref(), 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
        }));
        builder.push(
            StyleProperty::Brush(TextBrush {
                color: Rgba::BLACK,
                underline: false,
                baseline: TextBaseline::Superscript,
                footnote_reference: true,
                footnote_reference_group: 1,
            }),
            1..8,
        );
        // Force the semantic marker into several glyph runs independently of the
        // fonts installed on the test machine. All runs must still become one icon.
        builder.push(StyleProperty::FontSize(17.0), 4..5);
        let mut layout = builder.build(text.as_ref());
        layout.break_all_lines(Some(240.0));
        layout.align(Alignment::Start, AlignmentOptions::default());
        let source = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("chapter-1").unwrap(),
                node: "paragraph-1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("chapter-1").unwrap(),
                node: "paragraph-1".into(),
                text_offset: 8,
            },
        };
        let page = PageLayout {
            viewport: LayoutViewport::new(320, 240).unwrap(),
            background: Rgba::BLACK,
            leading_gap: 0.0,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text,
                source_text_start: 0,
                lines: 0..1,
                origin_x: 24.0,
                origin_y: 24.0,
                available_width: 240.0,
                source: Some(source.clone()),
                inline_images: Arc::from([]),
            })],
        };

        let list = DisplayListCompiler.compile(&page);
        assert_eq!(list.footnote_regions.len(), 1);
        assert_eq!(list.footnote_regions[0].source, source);
        let icon_bounds = list.footnote_regions[0].bounds;
        assert_eq!(
            list.footnote_source_at(icon_bounds.center().x as f32, icon_bounds.center().y as f32),
            Some(source.clone())
        );
        assert_eq!(list.footnote_source_at(0.0, 0.0), None);
        let painted_glyphs = list
            .commands
            .iter()
            .filter_map(|command| match command {
                DisplayCommand::Glyphs(command) => Some(command.glyphs.len()),
                _ => None,
            })
            .sum::<usize>();
        assert_eq!(painted_glyphs, 1);
    }

    #[test]
    fn footnote_icon_uses_a_stable_optical_superscript_position() {
        let baseline = 40.0;
        let bounds = footnote_icon_bounds(24.0, baseline, 20.0);

        assert!((bounds.width() - 12.0).abs() < f64::EPSILON);
        assert!((bounds.height() - 12.0).abs() < f64::EPSILON);
        assert!((bounds.center().x - 24.0).abs() < f64::EPSILON);
        assert!((bounds.center().y - 31.84).abs() < 0.001);
        assert!(bounds.y1 < f64::from(baseline));
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
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
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
            leading_gap: 0.0,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text,
                source_text_start: 0,
                lines: 0..1,
                origin_x: 24.0,
                origin_y: 24.0,
                available_width: 240.0,
                source: Some(source.clone()),
                inline_images: Arc::from([]),
            })],
        };
        let list = DisplayListCompiler.compile(&page);
        assert!(
            list.content_bottom()
                .is_some_and(|bottom| bottom > 24.0 && bottom < 80.0),
            "text content should end near its shaped line rather than the 240px page boundary"
        );
        let text_bounds = list
            .source_rects(std::slice::from_ref(&source))
            .into_iter()
            .reduce(|bounds, next| bounds.union(next))
            .unwrap();
        assert_eq!(
            list.source_block_bounds(std::slice::from_ref(&source)),
            Some(Rect::new(
                16.0,
                text_bounds.y0 - 6.0,
                272.0,
                text_bounds.y1 + 6.0,
            ))
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
    fn translated_text_maps_selection_offsets_to_the_original_source_span() {
        let text: Arc<str> = "这是较短的完整译文".into();
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::new();
        let mut builder =
            layout_context.ranged_builder(&mut font_context, text.as_ref(), 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
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
                text_offset: 224,
            },
        };
        let line_count = layout.len();
        let page = PageLayout {
            viewport: LayoutViewport::new(320, 240).unwrap(),
            background: Rgba::BLACK,
            leading_gap: 0.0,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text: Arc::clone(&text),
                source_text_start: 0,
                lines: 0..line_count,
                origin_x: 24.0,
                origin_y: 24.0,
                available_width: 240.0,
                source: Some(source.clone()),
                inline_images: Arc::from([]),
            })],
        };
        let list = DisplayListCompiler.compile(&page);

        let selection = list.selection_fragment(0, 0..text.len()).unwrap();
        assert_eq!(selection.range, source);
        assert!(!list.source_rects(std::slice::from_ref(&source)).is_empty());
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
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
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
                    line.metrics().offset + line.metrics().inline_max_coord,
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
            leading_gap: 0.0,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text: Arc::clone(&text),
                source_text_start: 0,
                lines: 0..line_count,
                origin_x,
                origin_y,
                available_width: 240.0,
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
    fn hanging_indent_highlight_aligns_first_and_continuation_line_right_edges() {
        let text: Arc<str> =
            "•\u{00a0}alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".into();
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::new();
        let mut builder =
            layout_context.ranged_builder(&mut font_context, text.as_ref(), 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
        }));
        let mut layout = builder.build(text.as_ref());
        layout.set_text_indent(
            18.0,
            parley::IndentOptions {
                hanging: true,
                ..parley::IndentOptions::default()
            },
        );
        layout.break_all_lines(Some(150.0));
        layout.align(Alignment::Justify, AlignmentOptions::default());
        let first = layout.get(0).expect("fixture should have a first line");
        let continuation = layout.get(1).expect("fixture should wrap");
        let expected_right = continuation.metrics().inline_max_coord;
        let first_y = first.metrics().block_min_coord;
        let continuation_y = continuation.metrics().block_min_coord;

        let spine = SpineItemId::new("chapter-1").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "list-item".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "list-item".into(),
                text_offset: u64::try_from(text.chars().count()).unwrap(),
            },
        };
        let line_count = layout.len();
        let page = PageLayout {
            viewport: LayoutViewport::new(240, 240).unwrap(),
            background: Rgba::BLACK,
            leading_gap: 0.0,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text: Arc::clone(&text),
                source_text_start: "•\u{00a0}".len(),
                lines: 0..line_count,
                origin_x: 24.0,
                origin_y: 24.0,
                available_width: 180.0,
                source: Some(source),
                inline_images: Arc::from([]),
            })],
        };
        let fragment = DisplayListCompiler
            .compile(&page)
            .selection_fragment(0, "•\u{00a0}".len()..text.len())
            .unwrap();
        let first_rect = fragment
            .rects
            .iter()
            .find(|rect| (rect.y0 - f64::from(first_y + 24.0)).abs() < 0.01)
            .unwrap();
        let continuation_rect = fragment
            .rects
            .iter()
            .find(|rect| (rect.y0 - f64::from(continuation_y + 24.0)).abs() < 0.01)
            .unwrap();
        assert!((first_rect.x1 - f64::from(expected_right + 24.0)).abs() < 0.01);
        assert!(
            first_rect.x0 > 24.0,
            "synthetic list marker must remain outside the source-backed highlight"
        );
        assert!((continuation_rect.x1 - f64::from(expected_right + 24.0)).abs() < 0.01);
    }

    #[test]
    fn wrapped_mixed_text_uses_line_width_while_the_last_line_stays_content_sized() {
        let text: Arc<str> =
            "中文 FitText mixed content with several English words and 中文结尾".into();
        let mut font_context = FontContext::new();
        let mut layout_context = LayoutContext::new();
        let mut builder =
            layout_context.ranged_builder(&mut font_context, text.as_ref(), 1.0, false);
        builder.push_default(StyleProperty::FontSize(18.0));
        builder.push_default(StyleProperty::Brush(TextBrush {
            color: Rgba::BLACK,
            underline: false,
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
        }));
        let mut layout = builder.build(text.as_ref());
        layout.break_all_lines(Some(180.0));
        layout.align(Alignment::Justify, AlignmentOptions::default());
        assert!(layout.len() >= 2);

        let first = layout.get(0).unwrap();
        let last = layout.get(layout.len() - 1).unwrap();
        assert!(matches!(first.break_reason(), BreakReason::Regular));
        assert_eq!(last.break_reason(), BreakReason::None);
        let expected_wrapped_right = first.metrics().offset + first.metrics().inline_max_coord;
        let last_content_right = last.metrics().offset
            + last.metrics().inline_min_coord
            + last
                .runs()
                .map(|run| {
                    run.visual_clusters()
                        .map(|cluster| cluster.advance())
                        .sum::<f32>()
                })
                .sum::<f32>();
        let expected_last_limit = last.metrics().inline_max_coord;

        let spine = SpineItemId::new("chapter-1").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "mixed-paragraph".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "mixed-paragraph".into(),
                text_offset: u64::try_from(text.chars().count()).unwrap(),
            },
        };
        let first_y = first.metrics().block_min_coord;
        let last_y = last.metrics().block_min_coord;
        let line_count = layout.len();
        let origin_x = 24.0;
        let origin_y = 24.0;
        let page = PageLayout {
            viewport: LayoutViewport::new(320, 300).unwrap(),
            background: Rgba::BLACK,
            leading_gap: 0.0,
            items: vec![PageItem::Text(TextPlacement {
                layout: Arc::new(layout),
                text: Arc::clone(&text),
                source_text_start: 0,
                lines: 0..line_count,
                origin_x,
                origin_y,
                available_width: 240.0,
                source: Some(source),
                inline_images: Arc::from([]),
            })],
        };

        let fragment = DisplayListCompiler
            .compile(&page)
            .selection_fragment(0, 0..text.len())
            .unwrap();
        let wrapped_rect = fragment
            .rects
            .iter()
            .find(|rect| (rect.y0 - f64::from(first_y + origin_y)).abs() < 0.01)
            .unwrap();
        let last_rect = fragment
            .rects
            .iter()
            .find(|rect| (rect.y0 - f64::from(last_y + origin_y)).abs() < 0.01)
            .unwrap();

        assert!(
            (wrapped_rect.x1 - f64::from(expected_wrapped_right + origin_x)).abs() < 0.01,
            "wrapped right={} expected={}",
            wrapped_rect.x1,
            expected_wrapped_right + origin_x
        );
        assert!((last_rect.x1 - f64::from(last_content_right + origin_x)).abs() < 0.01);
        assert!(last_rect.x1 < f64::from(expected_last_limit + origin_x));
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
            leading_gap: 0.0,
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
        let image_data = list.image_data().collect::<Vec<_>>();
        assert_eq!(image_data.len(), 1);
        assert_eq!((image_data[0].width, image_data[0].height), (100, 100));
        assert_eq!(image_data[0].data.len(), 100 * 100 * 4);
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
            baseline: TextBaseline::Normal,
            footnote_reference: false,
            footnote_reference_group: 0,
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
            leading_gap: 0.0,
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
                            available_width: 76.0,
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
