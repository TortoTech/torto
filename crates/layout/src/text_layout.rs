//! Product-owned line geometry over upstream Parley. Source offsets never contain
//! synthetic bidi controls; independently directed paragraphs remain separate.
// Parley editing returns f64 rectangles; product layout uses finite f32 coordinates.
#![allow(clippy::cast_possible_truncation)]
pub use parley::layout::{Affinity, BreakReason, ClusterSide};
use parley::{Alignment, AlignmentOptions, Brush, IndentOptions};
use std::ops::{Deref, Range};

#[derive(Clone, Debug)]
pub struct Layout<B: Brush> {
    parts: Vec<Part<B>>,
    paint: Vec<(f32, f32)>,
    spacing: Vec<LineSpacing>,
}
#[derive(Clone, Debug)]
struct Part<B: Brush> {
    layout: parley::Layout<B>,
    start: usize,
    translated: bool,
    omit_terminal: bool,
}
impl<B: Brush> Default for Layout<B> {
    fn default() -> Self {
        Self::from(parley::Layout::new())
    }
}
impl<B: Brush> From<parley::Layout<B>> for Layout<B> {
    fn from(layout: parley::Layout<B>) -> Self {
        Self {
            parts: vec![Part {
                layout,
                start: 0,
                translated: false,
                omit_terminal: false,
            }],
            paint: vec![],
            spacing: vec![],
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineMetrics {
    pub line_height: f32,
    pub baseline: f32,
    pub offset: f32,
    pub advance: f32,
    pub hanging_advance: f32,
    pub indent: f32,
    pub inline_min_coord: f32,
    pub inline_max_coord: f32,
    pub block_min_coord: f32,
    pub block_max_coord: f32,
    pub content_block_min_coord: f32,
    pub content_block_max_coord: f32,
}
#[derive(Clone, Debug)]
struct Gap {
    range: Range<usize>,
    start: f32,
    end: f32,
    delta: f32,
}
#[derive(Clone, Debug, Default)]
struct LineSpacing {
    gaps: Vec<Gap>,
    hanging: f32,
}
static EMPTY_SPACING: LineSpacing = LineSpacing {
    gaps: Vec::new(),
    hanging: 0.,
};
impl LineSpacing {
    fn delta_before(&self, x: f32) -> f32 {
        self.gaps
            .iter()
            .take_while(|g| g.end <= x + 0.001)
            .map(|g| g.delta)
            .sum()
    }
    fn glyph_x(&self, x: f32) -> f32 {
        x + self.delta_before(x)
    }
    fn caret_x(&self, x: f32) -> f32 {
        let mut shift = 0.;
        for g in &self.gaps {
            if x >= g.end {
                shift += g.delta;
            } else {
                if x > g.start && g.end > g.start {
                    shift += g.delta * (x - g.start) / (g.end - g.start);
                }
                break;
            }
        }
        x + shift
    }
    fn source_x(&self, x: f32) -> f32 {
        let mut shift = 0.;
        for g in &self.gaps {
            if x < g.start + shift {
                break;
            }
            if x < g.end + shift + g.delta && g.end - g.start + g.delta > 0. {
                return g.start
                    + (x - g.start - shift) * (g.end - g.start) / (g.end - g.start + g.delta);
            }
            shift += g.delta;
        }
        x - shift
    }
    fn delta(&self, range: Range<usize>) -> f32 {
        self.gaps
            .binary_search_by_key(&range.start, |g| g.range.start)
            .ok()
            .map_or(0., |i| self.gaps[i].delta)
    }
    fn total(&self) -> f32 {
        self.gaps.iter().map(|g| g.delta).sum()
    }
}
impl<B: Brush> Layout<B> {
    pub fn new() -> Self {
        Self::default()
    }
    pub(crate) fn paragraphs(parts: Vec<(parley::Layout<B>, usize, bool, bool)>) -> Self {
        Self {
            parts: parts
                .into_iter()
                .map(|(layout, start, translated, omit_terminal)| Part {
                    layout,
                    start,
                    translated,
                    omit_terminal,
                })
                .collect(),
            paint: vec![],
            spacing: vec![],
        }
    }
    fn part_len(part: &Part<B>) -> usize {
        part.layout.len()
            - usize::from(
                part.omit_terminal
                    && part
                        .layout
                        .lines()
                        .next_back()
                        .is_some_and(|line| line.text_range().is_empty()),
            )
    }
    fn part_height(part: &Part<B>) -> f32 {
        if Self::part_len(part) < part.layout.len() {
            part.layout
                .get(Self::part_len(part))
                .map_or(part.layout.height(), |l| l.metrics().block_min_coord)
        } else {
            part.layout.height()
        }
    }
    pub fn len(&self) -> usize {
        self.parts.iter().map(Self::part_len).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_rtl(&self) -> bool {
        self.parts.first().is_some_and(|p| p.layout.is_rtl())
    }
    pub fn width(&self) -> f32 {
        if self.spacing.is_empty() {
            self.parts
                .iter()
                .map(|p| p.layout.width())
                .fold(0., f32::max)
        } else {
            self.lines()
                .map(|l| l.metrics.advance - l.metrics.hanging_advance)
                .fold(0., f32::max)
        }
    }
    pub fn full_width(&self) -> f32 {
        if self.spacing.is_empty() {
            self.parts
                .iter()
                .map(|p| p.layout.full_width())
                .fold(0., f32::max)
        } else {
            self.lines().map(|l| l.metrics.advance).fold(0., f32::max)
        }
    }
    pub fn height(&self) -> f32 {
        self.parts.iter().map(Self::part_height).sum::<f32>()
            + self.paint.iter().map(|(a, b)| a + b).sum::<f32>()
    }
    pub fn calculate_content_widths(&self) -> parley::ContentWidths {
        self.parts
            .iter()
            .fold(parley::ContentWidths { min: 0., max: 0. }, |mut w, p| {
                let next = p.layout.calculate_content_widths();
                w.min = w.min.max(next.min);
                w.max = w.max.max(next.max);
                w
            })
    }
    pub fn inline_boxes(&self) -> impl Iterator<Item = parley::InlineBox> + '_ {
        self.parts.iter().flat_map(|p| {
            p.layout.inline_boxes().map(move |b| {
                let mut b = b.clone();
                b.index += p.start;
                b
            })
        })
    }
    pub fn set_text_indent(&mut self, amount: f32, options: IndentOptions) {
        for p in &mut self.parts {
            p.layout.set_text_indent(amount, options);
        }
    }
    pub fn break_all_lines(&mut self, width: Option<f32>) {
        self.paint.clear();
        self.spacing.clear();
        for p in &mut self.parts {
            p.layout.break_all_lines(width);
        }
    }
    /// The custom paragraph optimizer is restricted to one LTR paragraph.
    ///
    /// # Panics
    /// Panics for a composed layout containing multiple logical paragraphs.
    pub fn break_lines(&mut self) -> parley::layout::BreakLines<'_, B> {
        assert_eq!(
            self.parts.len(),
            1,
            "custom line breaks require one paragraph"
        );
        self.paint.clear();
        self.spacing.clear();
        self.parts[0].layout.break_lines()
    }
    /// Aligns logical paragraphs, keeping translated paragraphs left aligned.
    ///
    /// # Panics
    /// Panics if native justification is requested after optimizer spacing.
    pub fn align(&mut self, alignment: Alignment, options: AlignmentOptions) {
        let old_offsets: Vec<_> = if self.spacing.is_empty() {
            vec![]
        } else {
            self.lines().map(|l| l.metrics().offset).collect()
        };
        for part in &mut self.parts {
            part.layout.align(
                if part.translated {
                    Alignment::Left
                } else {
                    alignment
                },
                options,
            );
        }
        if !old_offsets.is_empty() {
            assert_ne!(
                alignment,
                Alignment::Justify,
                "optimized spacing already supplies justification"
            );
            for ((line, old), spacing) in self.parts[0]
                .layout
                .lines()
                .zip(old_offsets)
                .zip(&mut self.spacing)
            {
                let shift = line.metrics().offset - old;
                for gap in &mut spacing.gaps {
                    gap.start += shift;
                    gap.end += shift;
                }
            }
        }
    }
    /// Apply the optimizer's spacing in line geometry, keeping shaped glyphs and
    /// font fallback intact. Unsafe adjustments within ligatures use reshaping.
    pub(crate) fn apply_spacing(
        &mut self,
        adjustments: &[crate::linebreak::SpacingAdjustment],
    ) -> Option<()> {
        if adjustments.is_empty() {
            self.spacing.clear();
            return Some(());
        }
        if self.parts.len() != 1 || self.is_rtl() {
            return None;
        }
        let raw = &self.parts[0].layout;
        let mut spacing = Vec::with_capacity(raw.len());
        for line in raw.lines() {
            let mut gaps = Vec::new();
            let mut hanging = 0.;
            let visible_end =
                line.metrics().offset + line.metrics().inline_min_coord + line.metrics().advance
                    - line.metrics().hanging_advance;
            for run in line.runs() {
                if run.is_rtl() {
                    return None;
                }
                for cluster in run.clusters() {
                    let range = cluster.text_range();
                    let delta = adjustments
                        .iter()
                        .find(|a| a.range.contains(&range.start))
                        .map_or(0., |a| a.amount);
                    if delta.abs() < 0.00001 {
                        continue;
                    }
                    if !delta.is_finite()
                        || cluster.is_ligature_start()
                        || cluster.is_ligature_continuation()
                        // Advancing a base glyph must not move attached marks.
                        || cluster.glyphs().count() != 1
                    {
                        return None;
                    }
                    let start = cluster.visual_offset()? + line.metrics().inline_min_coord;
                    let end = start + cluster.advance();
                    if end - start + delta <= 0.0001 {
                        return None;
                    }
                    if start >= visible_end - 0.001 {
                        hanging += delta;
                    }
                    gaps.push(Gap {
                        range,
                        start,
                        end,
                        delta,
                    });
                }
            }
            spacing.push(LineSpacing { gaps, hanging });
        }
        self.spacing = spacing;
        Some(())
    }
    pub fn get(&self, index: usize) -> Option<Line<'_, B>> {
        let mut local = index;
        let mut y = 0.;
        for part in &self.parts {
            if local < Self::part_len(part) {
                let raw = part.layout.get(local)?;
                let m = *raw.metrics();
                let (above, below) = self.paint.get(index).copied().unwrap_or_default();
                let spacing = self.spacing.get(index).unwrap_or(&EMPTY_SPACING);
                let added = spacing.total();
                let shift = y + self
                    .paint
                    .iter()
                    .take(index)
                    .map(|(a, b)| a + b)
                    .sum::<f32>();
                return Some(Line {
                    raw,
                    start: part.start,
                    index,
                    y: shift + above,
                    spacing,
                    metrics: LineMetrics {
                        line_height: m.line_height + above + below,
                        baseline: m.baseline + shift + above,
                        offset: m.offset,
                        advance: m.advance + added,
                        hanging_advance: m.hanging_advance + spacing.hanging,
                        indent: m.indent,
                        inline_min_coord: m.inline_min_coord,
                        inline_max_coord: m.inline_max_coord,
                        block_min_coord: m.block_min_coord + shift,
                        block_max_coord: m.block_max_coord + shift + above + below,
                        content_block_min_coord: m.content_block_min_coord + shift + above,
                        content_block_max_coord: m.content_block_max_coord + shift + above,
                    },
                });
            }
            local -= Self::part_len(part);
            y += Self::part_height(part);
        }
        None
    }
    /// Iterates the displayed lines.
    ///
    /// # Panics
    /// Panics only if the internal paragraph line counts are inconsistent.
    pub fn lines(
        &self,
    ) -> impl ExactSizeIterator<Item = Line<'_, B>> + DoubleEndedIterator + Clone {
        (0..self.len()).map(|index| self.get(index).expect("valid line index"))
    }
    /// Ruby stays anchored to the base text's actual UTF-8 range after wrapping.
    pub fn reserve_text_paint_bounds(&mut self, bounds: &[(Range<usize>, f32, f32)]) {
        let extra = self
            .lines()
            .map(|line| {
                let m = line.metrics();
                let mut a = 0_f32;
                let mut b = 0_f32;
                for (range, offset, height) in bounds {
                    if range.start < line.text_range().end
                        && range.end > line.text_range().start
                        && offset.is_finite()
                        && height.is_finite()
                        && *height >= 0.
                    {
                        a = a.max(m.block_min_coord - m.baseline - offset);
                        b = b.max(m.baseline + offset + height - m.block_max_coord);
                    }
                }
                (a, b)
            })
            .collect::<Vec<_>>();
        self.paint.resize(extra.len(), (0., 0.));
        for (existing, (a, b)) in self.paint.iter_mut().zip(extra) {
            existing.0 += a;
            existing.1 += b;
        }
    }
    fn locate(&self, index: usize, affinity: Affinity) -> Option<(&Part<B>, usize)> {
        let mut line_start = 0;
        for (i, p) in self.parts.iter().enumerate() {
            if index < p.start + p.layout.text_len()
                || (index == p.start + p.layout.text_len()
                    && (affinity == Affinity::Upstream || i + 1 == self.parts.len()))
            {
                return Some((p, line_start));
            }
            line_start += Self::part_len(p);
        }
        self.parts
            .last()
            .map(|p| (p, line_start.saturating_sub(Self::part_len(p))))
    }
    fn line_at_y(&self, y: f32) -> Option<Line<'_, B>> {
        self.lines().min_by(|a, b| {
            let distance = |l: &Line<'_, B>| {
                let m = l.metrics();
                if y < m.block_min_coord {
                    m.block_min_coord - y
                } else {
                    (y - m.block_max_coord).max(0.)
                }
            };
            distance(a).total_cmp(&distance(b))
        })
    }
}

#[derive(Clone)]
pub struct Line<'a, B: Brush> {
    raw: parley::layout::Line<'a, B>,
    start: usize,
    index: usize,
    y: f32,
    metrics: LineMetrics,
    spacing: &'a LineSpacing,
}
impl<B: Brush + Copy> Copy for Line<'_, B> {}
impl<'a, B: Brush> Line<'a, B> {
    fn source_y(&self, y: f32) -> f32 {
        let m = self.raw.metrics();
        let epsilon = ((m.block_max_coord - m.block_min_coord) * 0.25).min(0.001);
        (y - self.y).clamp(m.block_min_coord + epsilon, m.block_max_coord - epsilon)
    }
    pub fn metrics(&self) -> LineMetrics {
        self.metrics
    }
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn break_reason(&self) -> BreakReason {
        self.raw.break_reason()
    }
    pub fn text_range(&self) -> Range<usize> {
        let r = self.raw.text_range();
        r.start + self.start..r.end + self.start
    }
    pub fn runs(&self) -> impl Iterator<Item = Run<'a, B>> + 'a + use<'a, B> {
        let start = self.start;
        let spacing = self.spacing;
        self.raw.runs().map(move |raw| Run {
            raw,
            start,
            spacing,
        })
    }
    pub fn items(&self) -> impl Iterator<Item = PositionedLayoutItem<'a, B>> + 'a + use<'a, B> {
        let start = self.start;
        let y = self.y;
        let spacing = self.spacing;
        self.raw.items().map(move |item| match item {
            parley::PositionedLayoutItem::GlyphRun(raw) => {
                PositionedLayoutItem::GlyphRun(GlyphRun {
                    raw,
                    start,
                    y,
                    spacing,
                })
            }
            parley::PositionedLayoutItem::InlineBox(mut b) => {
                b.y += y;
                b.x = spacing.glyph_x(b.x);
                PositionedLayoutItem::InlineBox(b)
            }
        })
    }
}
pub struct Run<'a, B: Brush> {
    raw: parley::layout::Run<'a, B>,
    start: usize,
    spacing: &'a LineSpacing,
}
impl<B: Brush> Copy for Run<'_, B> {}
#[allow(clippy::expl_impl_clone_on_copy)]
impl<B: Brush> Clone for Run<'_, B> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<'a, B: Brush> Deref for Run<'a, B> {
    type Target = parley::layout::Run<'a, B>;
    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}
impl<'a, B: Brush> Run<'a, B> {
    pub fn text_range(&self) -> Range<usize> {
        let r = self.raw.text_range();
        r.start + self.start..r.end + self.start
    }
    pub fn font(&self) -> &parley::FontData {
        self.raw.font()
    }
    pub fn normalized_coords(&self) -> &[parley_engine::NormalizedCoord] {
        self.raw.normalized_coords()
    }
    pub fn font_metrics(&self) -> &parley_engine::FontMetrics {
        self.raw.font_metrics()
    }
    pub fn clusters(&self) -> impl Iterator<Item = Cluster<'a, B>> + 'a + use<'a, B> {
        let start = self.start;
        let spacing = self.spacing;
        self.raw.clusters().map(move |raw| Cluster {
            raw,
            start,
            spacing,
        })
    }
    pub fn visual_clusters(&self) -> impl Iterator<Item = Cluster<'a, B>> + 'a + use<'a, B> {
        let start = self.start;
        let spacing = self.spacing;
        self.raw.visual_clusters().map(move |raw| Cluster {
            raw,
            start,
            spacing,
        })
    }
}
pub struct Cluster<'a, B: Brush> {
    raw: parley::layout::Cluster<'a, B>,
    start: usize,
    spacing: &'a LineSpacing,
}
impl<'a, B: Brush> Deref for Cluster<'a, B> {
    type Target = parley::layout::Cluster<'a, B>;
    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}
impl<'a, B: Brush> Cluster<'a, B> {
    pub fn text_range(&self) -> Range<usize> {
        let r = self.raw.text_range();
        r.start + self.start..r.end + self.start
    }
    pub fn advance(&self) -> f32 {
        self.raw.advance() + self.spacing.delta(self.text_range())
    }
    pub fn from_point_exact(layout: &'a Layout<B>, x: f32, y: f32) -> Option<(Self, ClusterSide)> {
        let l = layout.line_at_y(y)?;
        if y < l.metrics.block_min_coord || y > l.metrics.block_max_coord {
            return None;
        }
        let (part, _) = layout.locate(l.text_range().start, Affinity::Downstream)?;
        let (raw, side) = parley::layout::Cluster::from_point_exact(
            &part.layout,
            l.spacing.source_x(x),
            l.source_y(y),
        )?;
        Some((
            Self {
                raw,
                start: part.start,
                spacing: l.spacing,
            },
            side,
        ))
    }
}
pub enum PositionedLayoutItem<'a, B: Brush> {
    GlyphRun(GlyphRun<'a, B>),
    InlineBox(parley::PositionedInlineBox),
}
pub struct GlyphRun<'a, B: Brush> {
    raw: parley::layout::GlyphRun<'a, B>,
    start: usize,
    y: f32,
    spacing: &'a LineSpacing,
}
impl<'a, B: Brush> GlyphRun<'a, B> {
    pub fn run(&self) -> Run<'a, B> {
        Run {
            raw: *self.raw.run(),
            start: self.start,
            spacing: self.spacing,
        }
    }
    pub fn style(&self) -> &parley::layout::Style<B> {
        self.raw.style()
    }
    pub fn offset(&self) -> f32 {
        self.spacing.glyph_x(self.raw.offset())
    }
    pub fn advance(&self) -> f32 {
        self.spacing.glyph_x(self.raw.offset() + self.raw.advance()) - self.offset()
    }
    pub fn baseline(&self) -> f32 {
        self.raw.baseline() + self.y
    }
    pub fn glyphs(&'a self) -> impl Iterator<Item = parley::Glyph> + 'a {
        let spacing = self.spacing;
        self.raw
            .glyphs()
            .scan(self.raw.offset(), move |pen, mut g| {
                let end = *pen + g.advance;
                g.advance += spacing.delta_before(end) - spacing.delta_before(*pen);
                *pen = end;
                Some(g)
            })
    }
    pub fn positioned_glyphs(&'a self) -> impl Iterator<Item = parley::Glyph> + 'a {
        let baseline = self.baseline();
        let spacing = self.spacing;
        self.raw
            .glyphs()
            .scan(self.raw.offset(), move |pen, mut g| {
                let end = *pen + g.advance;
                g.x += spacing.glyph_x(*pen);
                g.y += baseline;
                g.advance += spacing.delta_before(end) - spacing.delta_before(*pen);
                *pen = end;
                Some(g)
            })
    }
}

#[derive(Clone, Copy)]
pub struct Cursor {
    index: usize,
    affinity: Affinity,
}
impl Cursor {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn from_byte_index<B: Brush>(layout: &Layout<B>, index: usize, affinity: Affinity) -> Self {
        let index = layout.locate(index, affinity).map_or(0, |(p, _)| {
            p.start
                + parley::editing::Cursor::from_byte_index(
                    &p.layout,
                    index.saturating_sub(p.start),
                    affinity,
                )
                .index()
        });
        Self { index, affinity }
    }
    pub fn from_point<B: Brush>(layout: &Layout<B>, x: f32, y: f32) -> Self {
        let Some(line) = layout.line_at_y(y) else {
            return Self {
                index: 0,
                affinity: Affinity::Downstream,
            };
        };
        let Some((part, _)) = layout.locate(line.text_range().start, Affinity::Downstream) else {
            return Self {
                index: 0,
                affinity: Affinity::Downstream,
            };
        };
        let cursor = parley::editing::Cursor::from_point(
            &part.layout,
            line.spacing.source_x(x),
            line.source_y(y),
        );
        Self {
            index: part.start + cursor.index(),
            affinity: cursor.affinity(),
        }
    }
    pub fn geometry<B: Brush>(&self, layout: &Layout<B>, width: f32) -> parley::BoundingBox {
        let Some((p, first)) = layout.locate(self.index, self.affinity) else {
            return parley::BoundingBox {
                x0: 0.,
                x1: 0.,
                y0: 0.,
                y1: 0.,
            };
        };
        // A terminal newline belongs to the next displayed paragraph. Native
        // RTL cursor geometry can otherwise attach this boundary to the prior
        // visual cluster instead of its omitted terminal line.
        if p.omit_terminal && self.index == p.start + p.layout.text_len() {
            return Self {
                index: self.index,
                affinity: Affinity::Downstream,
            }
            .geometry(layout, width);
        }
        let c = parley::editing::Cursor::from_byte_index(
            &p.layout,
            self.index.saturating_sub(p.start),
            self.affinity,
        );
        let mut r = c.geometry(&p.layout, width);
        let center = ((r.y0 + r.y1) * 0.5) as f32;
        let local = p
            .layout
            .lines()
            .position(|l| {
                l.metrics().block_min_coord <= center && center <= l.metrics().block_max_coord
            })
            .unwrap_or_else(|| Self::part_len_for_cursor(p).saturating_sub(1));
        if let Some(line) = layout.get(first + local) {
            r.y0 += f64::from(line.y);
            r.y1 += f64::from(line.y);
            r.x0 = f64::from(line.spacing.caret_x(r.x0 as f32));
            r.x1 = f64::from(line.spacing.caret_x(r.x1 as f32));
        }
        r
    }
    fn part_len_for_cursor<B: Brush>(p: &Part<B>) -> usize {
        Layout::part_len(p)
    }
}
pub struct Selection {
    anchor: Cursor,
    focus: Cursor,
}
impl Selection {
    pub fn new(anchor: Cursor, focus: Cursor) -> Self {
        Self { anchor, focus }
    }
    pub fn text_range(&self) -> Range<usize> {
        self.anchor.index.min(self.focus.index)..self.anchor.index.max(self.focus.index)
    }
    pub fn geometry<B: Brush>(&self, layout: &Layout<B>) -> Vec<(parley::BoundingBox, usize)> {
        let selected = self.text_range();
        let mut result = vec![];
        let mut first = 0;
        for p in &layout.parts {
            let start = selected.start.max(p.start);
            let end = selected.end.min(p.start + p.layout.text_len());
            if start < end {
                let s = parley::editing::Selection::new(
                    parley::editing::Cursor::from_byte_index(
                        &p.layout,
                        start - p.start,
                        Affinity::Downstream,
                    ),
                    parley::editing::Cursor::from_byte_index(
                        &p.layout,
                        end - p.start,
                        Affinity::Upstream,
                    ),
                );
                for (mut r, index) in s.geometry(&p.layout) {
                    if index >= Layout::part_len(p) {
                        continue;
                    }
                    if let Some(line) = layout.get(first + index) {
                        r.x0 = f64::from(line.spacing.caret_x(r.x0 as f32));
                        r.x1 = f64::from(line.spacing.caret_x(r.x1 as f32));
                        r.y0 = f64::from(
                            line.metrics
                                .block_min_coord
                                .min(line.metrics.content_block_min_coord),
                        );
                        r.y1 = f64::from(
                            line.metrics
                                .block_max_coord
                                .max(line.metrics.content_block_max_coord),
                        );
                        result.push((r, first + index));
                    }
                }
            }
            first += Layout::part_len(p);
        }
        result
    }
}

#[cfg(test)]
#[path = "text_layout_tests.rs"]
mod tests;
