//! Backend-neutral retained text geometry.
//!
//! Text shaping implementations live in adapters. Page layout, interaction,
//! and paint consumers exchange only the immutable data in this module.

use std::ops::Range;
use std::sync::Arc;

use rebook_publication::{Rgba, SourceAnchor, SourceRange, TextAlignment, TextBaseline};

pub mod legacy_parley;

/// Store-local identifier for one shaped line.
///
/// IDs are immutable for the lifetime of their [`TextLayoutStore`] and must
/// not be persisted across a re-layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextLineId(u32);

impl TextLineId {
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub(super) fn from_index(index: usize) -> Option<Self> {
        u32::try_from(index).ok().map(Self)
    }
}

/// Half-open line span inside one [`TextLayoutStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextLineSpan {
    pub start: TextLineId,
    pub end: TextLineId,
}

impl TextLineSpan {
    #[must_use]
    pub fn len(self) -> usize {
        self.end.index().saturating_sub(self.start.index())
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start.0 >= self.end.0
    }

    #[must_use]
    pub fn contains(self, id: TextLineId) -> bool {
        id >= self.start && id < self.end
    }

    pub fn iter(self) -> impl Iterator<Item = TextLineId> {
        (self.start.0..self.end.0).map(TextLineId)
    }

    #[must_use]
    pub const fn last(self) -> Option<TextLineId> {
        match self.end.0.checked_sub(1) {
            Some(last) if last >= self.start.0 => Some(TextLineId(last)),
            _ => None,
        }
    }
}

/// Backend-neutral line break classification needed by selection geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextLineBreak {
    Soft,
    Hard,
    End,
}

/// Logical line metrics in device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextLineMetrics {
    pub block_min: f32,
    pub block_max: f32,
    pub line_height: f32,
    pub offset: f32,
    pub inline_min: f32,
    pub inline_max: f32,
    /// Right edge of actual text/inline content before a soft line is expanded
    /// to the paragraph measure for justification/highlight geometry.
    pub content_inline_max: f32,
}

/// First-line indentation passed to a text engine.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TextIndent {
    pub amount: f32,
    pub hanging: bool,
    pub each_line: bool,
}

/// One styled UTF-8 byte range in a text-engine request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextStyleSpan {
    pub range: Range<usize>,
    pub font_size: Option<f32>,
    pub color: Option<Rgba>,
    pub font_weight: Option<f32>,
    pub italic: bool,
    pub underline: bool,
    pub baseline: TextBaseline,
    pub footnote_reference_group: u32,
}

/// A measured inline object inserted at one authored UTF-8 byte boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextInlineObject {
    pub id: u64,
    pub index: usize,
    pub width: f32,
    pub height: f32,
}

/// Paragraph line-breaking policy requested from the transitional text engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextLineBreakStrategy {
    #[default]
    Greedy,
    Optimized,
}

/// Backend-neutral request accepted by text shaping adapters.
#[derive(Debug, Clone)]
pub struct TextLayoutRequest<'a> {
    pub text: &'a str,
    pub font_family: Option<&'a str>,
    pub font_size: f32,
    pub font_weight: Option<f32>,
    pub line_height: Option<f32>,
    pub color: Rgba,
    pub width: Option<f32>,
    pub alignment: TextAlignment,
    pub indent: TextIndent,
    pub spans: &'a [TextStyleSpan],
    pub inline_objects: &'a [TextInlineObject],
    pub line_break_strategy: TextLineBreakStrategy,
}

impl<'a> TextLayoutRequest<'a> {
    #[must_use]
    pub fn plain(text: &'a str, font_size: f32, width: Option<f32>) -> Self {
        Self {
            text,
            font_family: None,
            font_size,
            font_weight: None,
            line_height: None,
            color: Rgba::BLACK,
            width,
            alignment: TextAlignment::Start,
            indent: TextIndent::default(),
            spans: &[],
            inline_objects: &[],
            line_break_strategy: TextLineBreakStrategy::Greedy,
        }
    }
}

/// Text shaping boundary used by layout. Implementations choose fonts, shape
/// glyphs, and realize lines while returning the same retained contract.
pub trait TextEngine {
    /// Registers application-provided font bytes when the backend supports it.
    fn register_font(&mut self, _font: &TextFontBlob) {}

    fn shape(&mut self, request: &TextLayoutRequest<'_>) -> TextLayoutStore;
}

impl<T: TextEngine + ?Sized> TextEngine for Box<T> {
    fn register_font(&mut self, font: &TextFontBlob) {
        (**self).register_font(font);
    }

    fn shape(&mut self, request: &TextLayoutRequest<'_>) -> TextLayoutStore {
        (**self).shape(request)
    }
}

/// Durable mapping between shaped UTF-8 display text and authored source text.
///
/// Translations can have a different character count from the original. In
/// that case offsets are mapped proportionally while preserving exact start and
/// end anchors for a full-range selection.
#[derive(Debug, Clone)]
pub struct TextSourceMap {
    text: Arc<str>,
    source_text_start: usize,
    source: SourceRange,
}

impl TextSourceMap {
    #[must_use]
    pub fn new(text: Arc<str>, source_text_start: usize, source: SourceRange) -> Option<Self> {
        (source_text_start <= text.len()).then_some(Self {
            text,
            source_text_start,
            source,
        })
    }

    #[must_use]
    pub fn text(&self) -> &Arc<str> {
        &self.text
    }

    #[must_use]
    pub const fn source_text_start(&self) -> usize {
        self.source_text_start
    }

    #[must_use]
    pub const fn source(&self) -> &SourceRange {
        &self.source
    }

    #[must_use]
    pub fn source_range_for_bytes(&self, byte_range: Range<usize>) -> Option<SourceRange> {
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

    #[must_use]
    pub fn byte_range_for_source(&self, range: &SourceRange) -> Option<Range<usize>> {
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
        (end > start).then_some(start..end)
    }
}

fn scale_text_offset_to_source(
    text_offset: usize,
    text_length: usize,
    source_length: u64,
    round_up: bool,
) -> Option<u64> {
    if text_length == 0 {
        return Some(0);
    }
    let numerator = (text_offset as u128).checked_mul(u128::from(source_length))?;
    let denominator = text_length as u128;
    let scaled = if round_up {
        numerator.div_ceil(denominator)
    } else {
        numerator / denominator
    };
    u64::try_from(scaled).ok()
}

fn scale_source_offset_to_text(
    source_offset: u64,
    source_length: u64,
    text_length: usize,
    round_up: bool,
) -> Option<usize> {
    if source_length == 0 {
        return Some(0);
    }
    let numerator = u128::from(source_offset).checked_mul(text_length as u128)?;
    let denominator = u128::from(source_length);
    let scaled = if round_up {
        numerator.div_ceil(denominator)
    } else {
        numerator / denominator
    };
    usize::try_from(scaled).ok()
}

fn byte_index_for_char_offset(text: &str, offset: usize) -> usize {
    text.char_indices()
        .nth(offset)
        .map_or(text.len(), |(index, _)| index)
}

/// Positioned glyph detached from the shaping backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextGlyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// One paintable glyph run.
#[derive(Clone)]
pub struct TextGlyphRun {
    pub font: TextFontResource,
    pub font_size: f32,
    pub normalized_coords: Arc<[i16]>,
    pub color: Rgba,
    pub skew_tan: Option<f32>,
    pub glyphs: Arc<[TextGlyph]>,
}

/// Shared font bytes detached from a shaping or painting backend.
///
/// `resource_id` identifies these exact bytes for renderer caches. Adapters
/// that import an existing resource handle should preserve its stable ID;
/// adapters creating new bytes must allocate a unique ID for different data.
#[derive(Clone)]
pub struct TextFontResource {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    resource_id: u64,
    collection_index: u32,
}

/// Application-provided font bytes accepted without exposing a shaping crate.
#[derive(Clone)]
pub struct TextFontBlob {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl TextFontBlob {
    #[must_use]
    pub fn new(data: Arc<dyn AsRef<[u8]> + Send + Sync>) -> Self {
        Self { data }
    }

    #[must_use]
    pub fn from_static(bytes: &'static [u8]) -> Self {
        Self::new(Arc::new(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.data.as_ref().as_ref()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    pub(crate) fn shared_data(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        Arc::clone(&self.data)
    }
}

impl std::fmt::Debug for TextFontBlob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextFontBlob")
            .field("byte_len", &self.as_bytes().len())
            .finish()
    }
}

impl TextFontResource {
    #[must_use]
    pub fn from_raw_parts(
        data: Arc<dyn AsRef<[u8]> + Send + Sync>,
        resource_id: u64,
        collection_index: u32,
    ) -> Self {
        Self {
            data,
            resource_id,
            collection_index,
        }
    }

    #[must_use]
    pub fn shared_data(&self) -> Arc<dyn AsRef<[u8]> + Send + Sync> {
        Arc::clone(&self.data)
    }

    #[must_use]
    pub const fn resource_id(&self) -> u64 {
        self.resource_id
    }

    #[must_use]
    pub const fn collection_index(&self) -> u32 {
        self.collection_index
    }
}

/// Positioned inline object in a shaped line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextInlineBox {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Explicit underline emitted by the text adapter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextRule {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub thickness: f32,
    pub color: Rgba,
}

/// One semantic footnote marker. Several fallback-font runs may share a group.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextFootnoteReference {
    pub group: u32,
    pub center_x: f32,
    pub baseline: f32,
    pub font_size: f32,
}

/// Backend-neutral text paint descriptor used by UI-native text backends.
///
/// It contains authored byte ranges and visual style, never a GPUI/Parley
/// shaping object. A UI backend may reshape this authored slice for paint, or
/// resolve it through a cache that lives entirely outside `LayoutFrame`.
#[derive(Debug, Clone, PartialEq)]
pub struct TextNativeRun {
    pub text_range: Range<usize>,
    pub x: f32,
    pub baseline: f32,
    /// Resolved primary family name used by a native UI text backend.
    pub font_family: Option<Arc<str>>,
    pub font_size: f32,
    pub font_weight: f32,
    pub italic: bool,
    pub underline: bool,
    pub color: Rgba,
}

/// One shaped cluster retained in visual coordinates.
///
/// Keeping this geometry in the neutral store lets hit testing and selection
/// work without retaining a Parley/GPUI layout object.
#[derive(Debug, Clone, PartialEq)]
pub struct TextCluster {
    pub text_range: Range<usize>,
    pub inline_start: f32,
    pub inline_end: f32,
    pub rtl: bool,
}

/// Paint data for one retained line.
#[derive(Clone)]
pub enum TextPaintItem {
    GlyphRun(TextGlyphRun),
    NativeRun(TextNativeRun),
    InlineBox(TextInlineBox),
    Rule(TextRule),
    FootnoteReference(TextFootnoteReference),
}

/// Immutable line snapshot shared by layout, interaction and rendering.
#[derive(Clone)]
pub struct TextLine {
    pub id: TextLineId,
    pub text_range: Range<usize>,
    pub metrics: TextLineMetrics,
    pub break_kind: TextLineBreak,
    pub clusters: Arc<[TextCluster]>,
    pub items: Arc<[TextPaintItem]>,
}

/// Backend-produced line data before a [`TextLayoutStore`] assigns its
/// store-local [`TextLineId`] values.
///
/// Text adapters should construct these snapshots rather than manufacturing
/// identifiers themselves. This keeps identifiers dense, immutable and scoped
/// to one validated store.
#[derive(Clone)]
pub struct TextLineSnapshot {
    pub text_range: Range<usize>,
    pub metrics: TextLineMetrics,
    pub break_kind: TextLineBreak,
    pub clusters: Arc<[TextCluster]>,
    pub items: Arc<[TextPaintItem]>,
}

/// A backend-neutral hit result in UTF-8 byte offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextLayoutHit {
    pub byte_index: usize,
    pub cluster_start: usize,
    pub cluster_end: usize,
}

/// Backend-neutral rectangle in layout coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextLayoutRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Validation failure while constructing a retained text store.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TextLayoutStoreError {
    #[error("text layout contains more lines than TextLineId can represent")]
    TooManyLines,
    #[error("text layout full width must be finite and non-negative")]
    InvalidFullWidth,
    #[error("text line {line} has an inverted UTF-8 byte range {start}..{end}")]
    InvalidTextRange {
        line: usize,
        start: usize,
        end: usize,
    },
    #[error("text line {line} has invalid geometry")]
    InvalidLineMetrics { line: usize },
    #[error("text line {line} cluster {cluster} has invalid geometry or byte range")]
    InvalidCluster { line: usize, cluster: usize },
}

/// Immutable shaped text store. The concrete shaper is deliberately hidden.
#[derive(Clone)]
pub struct TextLayoutStore {
    lines: Arc<[TextLine]>,
    full_width: f32,
}

impl TextLayoutStore {
    /// Builds a store from backend-neutral line snapshots.
    ///
    /// This is the only adapter construction boundary: it validates geometry
    /// and assigns dense store-local line IDs before exposing retained data to
    /// layout, hit testing and rendering.
    pub fn from_snapshots(
        snapshots: Vec<TextLineSnapshot>,
        full_width: f32,
    ) -> Result<Self, TextLayoutStoreError> {
        if !full_width.is_finite() || full_width < 0.0 {
            return Err(TextLayoutStoreError::InvalidFullWidth);
        }
        if snapshots.len() > u32::MAX as usize {
            return Err(TextLayoutStoreError::TooManyLines);
        }

        let mut lines = Vec::with_capacity(snapshots.len());
        for (index, snapshot) in snapshots.into_iter().enumerate() {
            if snapshot.text_range.start > snapshot.text_range.end {
                return Err(TextLayoutStoreError::InvalidTextRange {
                    line: index,
                    start: snapshot.text_range.start,
                    end: snapshot.text_range.end,
                });
            }
            if !valid_line_metrics(snapshot.metrics) {
                return Err(TextLayoutStoreError::InvalidLineMetrics { line: index });
            }
            for (cluster, geometry) in snapshot.clusters.iter().enumerate() {
                if geometry.text_range.start > geometry.text_range.end
                    || !geometry.inline_start.is_finite()
                    || !geometry.inline_end.is_finite()
                {
                    return Err(TextLayoutStoreError::InvalidCluster {
                        line: index,
                        cluster,
                    });
                }
            }
            lines.push(TextLine {
                id: TextLineId::from_index(index).ok_or(TextLayoutStoreError::TooManyLines)?,
                text_range: snapshot.text_range,
                metrics: snapshot.metrics,
                break_kind: snapshot.break_kind,
                clusters: snapshot.clusters,
                items: snapshot.items,
            });
        }

        Ok(Self {
            lines: lines.into(),
            full_width,
        })
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    #[must_use]
    pub fn line(&self, id: TextLineId) -> Option<&TextLine> {
        self.lines.get(id.index())
    }

    #[must_use]
    pub fn line_at(&self, index: usize) -> Option<&TextLine> {
        self.lines.get(index)
    }

    #[must_use]
    pub fn first_line(&self) -> Option<&TextLine> {
        self.lines.first()
    }

    #[must_use]
    pub fn last_line(&self) -> Option<&TextLine> {
        self.lines.last()
    }

    /// Returns the widest shaped content edge across all lines.
    #[must_use]
    pub const fn full_width(&self) -> f32 {
        self.full_width
    }

    #[must_use]
    pub fn line_span(&self, range: Range<usize>) -> Option<TextLineSpan> {
        if range.start > range.end || range.end > self.lines.len() {
            return None;
        }
        Some(TextLineSpan {
            start: TextLineId::from_index(range.start)?,
            end: TextLineId::from_index(range.end)?,
        })
    }

    #[must_use]
    pub fn visible_byte_range(
        &self,
        lines: TextLineSpan,
        source_text_start: usize,
        text_len: usize,
    ) -> Option<Range<usize>> {
        let first = self.line(lines.start)?;
        let last = self.line(lines.last()?)?;
        let start = first.text_range.start.max(source_text_start);
        let end = last.text_range.end.min(text_len);
        (end > start).then_some(start..end)
    }

    #[must_use]
    pub fn vertical_bounds(&self, lines: TextLineSpan) -> Option<(f32, f32)> {
        let first = self.line(lines.start)?;
        let last = self.line(lines.last()?)?;
        Some((first.metrics.block_min, last.metrics.block_max))
    }

    #[must_use]
    pub fn hit_test(
        &self,
        lines: TextLineSpan,
        source_text_start: usize,
        text_len: usize,
        x: f32,
        y: f32,
        exact: bool,
    ) -> Option<TextLayoutHit> {
        let (top, bottom) = self.vertical_bounds(lines)?;
        if exact && !(top..=bottom).contains(&y) {
            return None;
        }
        let y = if exact {
            y
        } else {
            y.clamp(top + 0.01, bottom - 0.01)
        };
        let line = if exact {
            lines
                .iter()
                .filter_map(|id| self.line(id))
                .find(|line| (line.metrics.block_min..=line.metrics.block_max).contains(&y))?
        } else {
            lines
                .iter()
                .filter_map(|id| self.line(id))
                .min_by(|left, right| {
                    distance_to_line(y, left).total_cmp(&distance_to_line(y, right))
                })?
        };
        let cluster = if exact {
            line.clusters.iter().find(|cluster| {
                let start = cluster.inline_start.min(cluster.inline_end);
                let end = cluster.inline_start.max(cluster.inline_end);
                (start..=end).contains(&x)
            })?
        } else {
            line.clusters.iter().min_by(|left, right| {
                distance_to_cluster(x, left).total_cmp(&distance_to_cluster(x, right))
            })?
        };
        let hit_left = x <= (cluster.inline_start + cluster.inline_end) * 0.5;
        let byte_index = if cluster.rtl == hit_left {
            cluster.text_range.end
        } else {
            cluster.text_range.start
        };
        let mut hit = TextLayoutHit {
            byte_index,
            cluster_start: cluster.text_range.start,
            cluster_end: cluster.text_range.end,
        };
        let visible = self.visible_byte_range(lines, source_text_start, text_len)?;
        hit.byte_index = hit.byte_index.clamp(visible.start, visible.end);
        hit.cluster_start = hit.cluster_start.clamp(visible.start, visible.end);
        hit.cluster_end = hit.cluster_end.clamp(visible.start, visible.end);
        Some(hit)
    }

    #[must_use]
    pub fn selection_rects(
        &self,
        lines: TextLineSpan,
        source_text_start: usize,
        byte_range: Range<usize>,
    ) -> Vec<TextLayoutRect> {
        if byte_range.end <= byte_range.start {
            return Vec::new();
        }
        let mut rects: Vec<TextLayoutRect> = Vec::new();
        for id in lines.iter() {
            let Some(line) = self.line(id) else {
                continue;
            };
            let selectable_start = line.text_range.start.max(source_text_start);
            if byte_range.start <= selectable_start
                && byte_range.end >= line.text_range.end
                && line.text_range.end > selectable_start
            {
                let visual_start = line.metrics.offset + line.metrics.inline_min;
                let x0 = if line.text_range.start >= source_text_start {
                    visual_start
                } else {
                    line.clusters
                        .iter()
                        .filter(|cluster| cluster.text_range.end > selectable_start)
                        .map(|cluster| cluster.inline_start.min(cluster.inline_end))
                        .min_by(f32::total_cmp)
                        .unwrap_or(visual_start)
                };
                let x1 = if line.break_kind == TextLineBreak::Soft {
                    line.metrics.inline_max
                } else {
                    line.metrics.content_inline_max
                };
                rects.push(TextLayoutRect {
                    x0: x0.min(x1),
                    y0: line.metrics.block_min,
                    x1: x0.max(x1),
                    y1: line.metrics.block_max,
                });
                continue;
            }

            let mut intervals = line
                .clusters
                .iter()
                .filter(|cluster| {
                    cluster.text_range.end > byte_range.start
                        && cluster.text_range.start < byte_range.end
                        && cluster.text_range.end > source_text_start
                })
                .map(|cluster| {
                    (
                        cluster.inline_start.min(cluster.inline_end),
                        cluster.inline_start.max(cluster.inline_end),
                    )
                })
                .collect::<Vec<_>>();
            intervals.sort_by(|left, right| left.0.total_cmp(&right.0));
            for (x0, x1) in intervals {
                if let Some(last) = rects.last_mut()
                    && (last.y0 - line.metrics.block_min).abs() <= f32::EPSILON
                    && x0 <= last.x1 + 0.5
                {
                    last.x1 = last.x1.max(x1);
                } else {
                    rects.push(TextLayoutRect {
                        x0,
                        y0: line.metrics.block_min,
                        x1,
                        y1: line.metrics.block_max,
                    });
                }
            }
        }
        rects
    }
}

fn distance_to_line(y: f32, line: &TextLine) -> f32 {
    if y < line.metrics.block_min {
        line.metrics.block_min - y
    } else if y > line.metrics.block_max {
        y - line.metrics.block_max
    } else {
        0.0
    }
}

fn distance_to_cluster(x: f32, cluster: &TextCluster) -> f32 {
    let start = cluster.inline_start.min(cluster.inline_end);
    let end = cluster.inline_start.max(cluster.inline_end);
    if x < start {
        start - x
    } else if x > end {
        x - end
    } else {
        0.0
    }
}

fn valid_line_metrics(metrics: TextLineMetrics) -> bool {
    [
        metrics.block_min,
        metrics.block_max,
        metrics.line_height,
        metrics.offset,
        metrics.inline_min,
        metrics.inline_max,
        metrics.content_inline_max,
    ]
    .into_iter()
    .all(f32::is_finite)
        && metrics.block_max >= metrics.block_min
        && metrics.line_height >= 0.0
        && metrics.inline_max >= metrics.inline_min
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::SpineItemId;

    #[test]
    fn line_spans_are_store_local_and_half_open() {
        let lines = (0_u32..3)
            .zip([0.0_f32, 1.0, 2.0])
            .map(|(index, block_min)| TextLineSnapshot {
                text_range: index as usize..index as usize + 1,
                metrics: TextLineMetrics {
                    block_min,
                    block_max: block_min + 1.0,
                    line_height: 1.0,
                    offset: 0.0,
                    inline_min: 0.0,
                    inline_max: 1.0,
                    content_inline_max: 1.0,
                },
                break_kind: TextLineBreak::Soft,
                clusters: Arc::from([]),
                items: Arc::from([]),
            })
            .collect::<Vec<_>>();
        let store = TextLayoutStore::from_snapshots(lines, 1.0).unwrap();
        let span = store.line_span(1..3).unwrap();
        assert_eq!(span.len(), 2);
        assert_eq!(
            span.iter().map(TextLineId::index).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(store.line_span(2..4).is_none());
    }

    #[test]
    fn snapshot_constructor_assigns_dense_ids() {
        let snapshots = [0.0, 1.0]
            .into_iter()
            .map(|block_min| TextLineSnapshot {
                text_range: 0..0,
                metrics: TextLineMetrics {
                    block_min,
                    block_max: block_min + 1.0,
                    line_height: 1.0,
                    offset: 0.0,
                    inline_min: 0.0,
                    inline_max: 0.0,
                    content_inline_max: 0.0,
                },
                break_kind: TextLineBreak::End,
                clusters: Arc::from([]),
                items: Arc::from([]),
            })
            .collect();

        let store = TextLayoutStore::from_snapshots(snapshots, 0.0).unwrap();

        assert_eq!(store.line_at(0).unwrap().id.index(), 0);
        assert_eq!(store.line_at(1).unwrap().id.index(), 1);
    }

    #[test]
    fn snapshot_constructor_rejects_invalid_geometry() {
        let snapshot = TextLineSnapshot {
            text_range: 0..0,
            metrics: TextLineMetrics {
                block_min: 2.0,
                block_max: 1.0,
                line_height: 1.0,
                offset: 0.0,
                inline_min: 0.0,
                inline_max: 0.0,
                content_inline_max: 0.0,
            },
            break_kind: TextLineBreak::End,
            clusters: Arc::from([]),
            items: Arc::from([]),
        };

        let result = TextLayoutStore::from_snapshots(vec![snapshot], 0.0);

        assert_eq!(
            result.err(),
            Some(TextLayoutStoreError::InvalidLineMetrics { line: 0 })
        );
    }

    #[test]
    fn retained_clusters_drive_hits_and_selection_without_a_shaper() {
        let snapshot = TextLineSnapshot {
            text_range: 0..4,
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
            clusters: Arc::from([
                TextCluster {
                    text_range: 0..1,
                    inline_start: 0.0,
                    inline_end: 10.0,
                    rtl: false,
                },
                TextCluster {
                    text_range: 1..4,
                    inline_start: 10.0,
                    inline_end: 20.0,
                    rtl: false,
                },
            ]),
            items: Arc::from([]),
        };
        let store = TextLayoutStore::from_snapshots(vec![snapshot], 20.0).unwrap();
        let lines = store.line_span(0..1).unwrap();

        assert_eq!(
            store.hit_test(lines, 0, 4, 15.0, 10.0, true),
            Some(TextLayoutHit {
                byte_index: 1,
                cluster_start: 1,
                cluster_end: 4,
            })
        );
        assert_eq!(
            store.selection_rects(lines, 0, 1..4),
            [TextLayoutRect {
                x0: 10.0,
                y0: 0.0,
                x1: 20.0,
                y1: 20.0,
            }]
        );
    }

    #[test]
    fn translated_source_mapping_round_trips_the_full_range() {
        let spine = SpineItemId::new("chapter").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph".into(),
                text_offset: 10,
            },
            end: SourceAnchor {
                spine,
                node: "paragraph".into(),
                text_offset: 110,
            },
        };
        let text: Arc<str> = "short translation".into();
        let map = TextSourceMap::new(Arc::clone(&text), 0, source.clone()).unwrap();

        assert_eq!(
            map.source_range_for_bytes(0..text.len()),
            Some(source.clone())
        );
        assert_eq!(map.byte_range_for_source(&source), Some(0..text.len()));
        let middle = map.source_range_for_bytes(6..text.len()).unwrap();
        assert!(middle.start.text_offset > source.start.text_offset);
        assert_eq!(middle.end, source.end);
    }
}
