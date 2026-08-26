//! Reader session with section and renderer-independent layout-frame caches.

mod structure;

pub use structure::{ParagraphStructureKey, ParagraphStructureSource};

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};

use rebook_layout::{
    ImagePlacement, LayoutEngine, LayoutError, LayoutFrame, LayoutViewport, PageItem,
    ReaderFontBlob, ReaderStyle, SpreadMode, TypesettingMode,
    frame::{FrameInteractionMap, FrameRect, FrameTextCursor, FrameTextHit},
    text::{TextEngine, legacy_parley::LegacyParleyTextEngine},
};
use rebook_publication::{
    Block, Book, BookSource, Inline, InlineRole, LinkRole, LocatorV1, PublicationError,
    PublicationUrl, RenditionLayout, Section, SectionAnchor, SourceAnchor, SourceRange,
    TableOfContentsOrigin, TextBaseline, TextBlock, TextBlockKind, TextRun, TocEntry,
};
#[cfg(feature = "legacy-renderer")]
use rebook_renderer::{DisplayListCompiler, PageDisplayList};
use thiserror::Error;
use unicode_segmentation::UnicodeSegmentation;

const PREFETCH_DISTANCE: usize = 2;
const DEFAULT_SEGMENT_CACHE_CAPACITY: usize = PREFETCH_DISTANCE * 2 + 3;
const FRAGMENT_TEXT_BUDGET: usize = 4_096;
const LARGE_SECTION_TEXT_BUDGET: usize = FRAGMENT_TEXT_BUDGET * 8;
const FRAGMENT_BLOCK_BUDGET: usize = 64;
const FOCUS_MINIMUM_PARAGRAPH_GAP: f32 = 12.0;

/// Direction requested by keyboard, pointer, or command navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDirection {
    Next,
    Previous,
}

/// Application-level reading presentation independent of a particular UI
/// toolkit. Layout still emits immutable page frames; frontends decide how to
/// animate, scroll, or center those frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadingMode {
    Classic,
    #[default]
    Focus,
}

/// Resolved layout-facing policy for a requested reading presentation.
///
/// Keeping this decision beside [`ReaderSession`] prevents each frontend from
/// independently translating focus mode into scroll pagination, footnote
/// affordances, and compatibility fallbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderPresentationPolicy {
    pub mode: ReadingMode,
    pub spread: SpreadMode,
}

impl ReaderPresentationPolicy {
    #[must_use]
    pub const fn resolve(
        requested_mode: ReadingMode,
        classic_spread: SpreadMode,
        focus_supported: bool,
    ) -> Self {
        let mode = if matches!(requested_mode, ReadingMode::Focus) && !focus_supported {
            ReadingMode::Classic
        } else {
            requested_mode
        };
        let spread = if matches!(mode, ReadingMode::Focus) {
            SpreadMode::Scroll
        } else {
            classic_spread
        };
        Self { mode, spread }
    }

    /// Applies the layout-visible portion of this presentation policy.
    /// Frontend-only concerns such as sidebar motion remain outside the core.
    pub fn apply_to_style(self, style: &mut ReaderStyle) {
        style.spread = self.spread;
        style.focus_footnote_icons = matches!(self.mode, ReadingMode::Focus);
        style.minimum_paragraph_gap = if matches!(self.mode, ReadingMode::Focus)
            && style.typesetting.mode == TypesettingMode::Book
        {
            FOCUS_MINIMUM_PARAGRAPH_GAP
        } else {
            0.0
        };
    }
}

/// Semantic unit used to expand pointer-driven text selections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SelectionGranularity {
    #[default]
    Free,
    Word,
    Sentence,
    Paragraph,
}

/// Returns the contiguous Unicode sentence ranges in `text`.
///
/// Ranges are byte offsets and retain the boundary whitespace supplied by
/// `unicode-segmentation`, so consumers can reconstruct the source text
/// without rewriting it. Semantic selection trims a chosen range only when it
/// presents that range to the user.
pub fn sentence_byte_ranges(text: &str) -> Vec<Range<usize>> {
    text.split_sentence_bound_indices()
        .map(|(start, sentence)| start..start + sentence.len())
        .collect()
}

/// Stable current position exposed to the application shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderLocation {
    pub section_index: usize,
    pub segment_index: usize,
    pub segment_count: usize,
    pub page_index: usize,
    pub page_count: usize,
}

/// Resolved random-access destination in the current pagination generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReaderPosition {
    pub section_index: usize,
    pub segment_index: usize,
    pub page_index: usize,
}

/// A pointer-resolved text position tied to the current pagination generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderTextHit {
    position: ReaderPosition,
    region_index: usize,
    byte_index: usize,
    cluster_start: usize,
    cluster_end: usize,
}

/// Logical text caret tied to one reader pagination generation.
///
/// Like [`ReaderTextHit`], this value is transient: applications must persist
/// the [`SourceRange`] values of a selection and resolve fresh cursors after a
/// resize, style change, source refresh, or other repagination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReaderTextCursor {
    position: ReaderPosition,
    cursor: FrameTextCursor,
}

/// Logical direction used to move a generation-local text caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextCursorDirection {
    Previous,
    Next,
}

impl ReaderTextHit {
    /// Returns the exact caret nearest the pointer inside the hit cluster.
    #[must_use]
    pub const fn cursor(&self) -> ReaderTextCursor {
        ReaderTextCursor {
            position: self.position,
            cursor: FrameTextCursor::new(self.region_index, self.byte_index),
        }
    }

    /// Returns the authored cluster boundaries enclosing this pointer hit.
    #[must_use]
    pub const fn cluster_cursors(&self) -> (ReaderTextCursor, ReaderTextCursor) {
        (
            ReaderTextCursor {
                position: self.position,
                cursor: FrameTextCursor::new(self.region_index, self.cluster_start),
            },
            ReaderTextCursor {
                position: self.position,
                cursor: FrameTextCursor::new(self.region_index, self.cluster_end),
            },
        )
    }
}

impl ReaderTextCursor {
    #[must_use]
    pub const fn position(self) -> ReaderPosition {
        self.position
    }
}

/// Page-coordinate rectangle used to paint a native selection and anchor its
/// floating action toolbar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderSelectionRect {
    pub position: ReaderPosition,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Durable source ranges plus transient geometry for the active native text
/// selection. Each range belongs to one source-backed text block.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderSelection {
    pub ranges: Vec<SourceRange>,
    pub text: String,
    pub rects: Vec<ReaderSelectionRect>,
}

/// Original image pixels resolved from a point in the visible reader spread.
#[derive(Clone)]
pub struct ReaderImage {
    pub position: ReaderPosition,
    /// Left edge in the coordinate space used for the image query.
    pub x: f32,
    /// Top edge in the coordinate space used for the image query.
    pub y: f32,
    pub display_width: f32,
    pub display_height: f32,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

/// One source-backed text fragment retained on a logical page in the current
/// visible spread. The source range remains stable while `position` identifies
/// the page that supplied the visible quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderVisibleTextFragment {
    pub position: ReaderPosition,
    pub range: SourceRange,
    pub text: String,
}

/// One visual reader surface assembled from adjacent logical pages. In double
/// mode the secondary page may come from the next layout segment or authored
/// spine section.
#[cfg(feature = "legacy-renderer")]
pub struct ReaderSpread {
    pub primary: Arc<PageDisplayList>,
    pub secondary: Option<Arc<PageDisplayList>>,
    pub primary_offset_x: f32,
    pub secondary_offset_x: f32,
}

/// One compiled logical page in the active authored section.
#[derive(Clone)]
#[cfg(feature = "legacy-renderer")]
pub struct ReaderSectionPage {
    pub position: ReaderPosition,
    pub page: Arc<PageDisplayList>,
    /// Whether this page only reserves fixed-page geometry and still needs its
    /// real raster/display list to be materialized near the viewport.
    pub placeholder: bool,
    /// Optional top crop in logical page coordinates for semantic reading views.
    pub visible_top: Option<f32>,
    /// Optional bottom crop in logical page coordinates for semantic reading views.
    pub visible_bottom: Option<f32>,
}

/// One renderer-independent reader surface assembled from adjacent logical
/// pages. Frontends should consume this frame-first contract; legacy display
/// lists remain available behind the `legacy-renderer` compatibility feature.
pub struct ReaderFrameSpread {
    pub primary: Arc<LayoutFrame>,
    pub secondary: Option<Arc<LayoutFrame>>,
    pub primary_offset_x: f32,
    pub secondary_offset_x: f32,
}

/// Renderer-independent geometry for one semantic focus-reading unit.
///
/// The bounds live in the stitched coordinate space of [`ReaderScrollLayout`]
/// rather than in egui/GPUI window coordinates. Frontends may animate toward
/// these bounds and paint activation affordances without rebuilding source
/// geometry from renderer-specific display lists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderFocusGeometry {
    pub position: ReaderPosition,
    pub bounds: FrameRect,
    pub activation_bounds: Option<FrameRect>,
}

/// Durable identity and immutable geometry for one focus-reading step.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderFocusUnit {
    pub range: SourceRange,
    pub paint_ranges: Vec<SourceRange>,
    pub geometry: ReaderFocusGeometry,
    pub content_kind: ReaderFocusContentKind,
    pub activation: ReaderFocusActivation,
    pub footnotes: Vec<ReaderFocusFootnote>,
}

/// Resolved footnote payload attached to a focus unit. Missing linked content
/// remains explicit so each frontend can localize its unavailable-state text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderFocusFootnote {
    pub marker: Option<String>,
    pub target: Option<PublicationUrl>,
    pub text: Option<String>,
}

/// Reading-IR footnote source before a linked target is resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReaderFocusFootnoteSource {
    Inline(String),
    Reference {
        marker: String,
        target: PublicationUrl,
    },
}

/// Complete frontend-neutral focus presentation for one semantic reading unit.
///
/// `first_unit_after_anchor` preserves heading restoration semantics: headings
/// are not independent focus steps, so an anchor inside one resolves to the
/// first following readable unit after candidates without frame geometry have
/// been discarded.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderFocusLayout {
    pub units: Vec<ReaderFocusUnit>,
    pub first_unit_after_anchor: Option<usize>,
}

/// Geometry adjustments shared by every focus-mode frontend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderFocusLayoutPolicy {
    pub table_bottom_margin: f32,
}

impl Default for ReaderFocusLayoutPolicy {
    fn default() -> Self {
        Self {
            table_bottom_margin: 24.0,
        }
    }
}

impl ReaderFocusUnit {
    #[must_use]
    pub fn contains_anchor(&self, anchor: &SourceAnchor) -> bool {
        source_range_contains_anchor(&self.range, anchor)
            || self
                .paint_ranges
                .iter()
                .any(|range| source_range_contains_anchor(range, anchor))
    }

    #[must_use]
    pub const fn is_image(&self) -> bool {
        matches!(self.content_kind, ReaderFocusContentKind::Image)
    }

    #[must_use]
    pub const fn is_table(&self) -> bool {
        matches!(self.content_kind, ReaderFocusContentKind::Table)
    }
}

/// Semantic content class of one focus-reading step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderFocusContentKind {
    Text,
    Image,
    Table,
}

/// Backend-neutral activation treatment. Frontends choose colors and animation,
/// while the reader decides whether authored geometry is inline or rectangular.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderFocusActivation {
    Inline,
    Rectangular,
    Structured,
}

impl ReaderFocusActivation {
    #[must_use]
    pub const fn is_rectangular(self) -> bool {
        !matches!(self, Self::Inline)
    }
}

/// Reading-IR result before immutable frame geometry is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderFocusCandidate {
    pub block_indices: Vec<usize>,
    pub range: SourceRange,
    pub paint_ranges: Vec<SourceRange>,
    pub geometry_ranges: Vec<SourceRange>,
    pub text: String,
    pub content_kind: ReaderFocusContentKind,
    pub activation: ReaderFocusActivation,
}

impl ReaderFocusCandidate {
    #[must_use]
    pub fn into_unit(self, geometry: ReaderFocusGeometry) -> ReaderFocusUnit {
        self.into_unit_with_footnotes(geometry, Vec::new())
    }

    #[must_use]
    pub fn into_unit_with_footnotes(
        self,
        geometry: ReaderFocusGeometry,
        footnotes: Vec<ReaderFocusFootnote>,
    ) -> ReaderFocusUnit {
        ReaderFocusUnit {
            range: self.range,
            paint_ranges: self.paint_ranges,
            geometry,
            content_kind: self.content_kind,
            activation: self.activation,
            footnotes,
        }
    }
}

enum FocusBlockCandidate {
    Heading(SourceRange),
    Unit {
        candidate: ReaderFocusCandidate,
        list_depth: Option<u8>,
    },
    Reset,
    Skip,
}

/// Compiles normalized Reading IR into ordered focus-reading candidates.
///
/// This stage owns semantic inclusion, list-tree grouping, heading attachment,
/// and source identity. It deliberately does not depend on layout frames or a UI
/// toolkit. The caller supplies only the external structured-paragraph state.
pub fn compile_focus_candidates(
    section: &Section,
    reading_ranges: &[SourceRange],
    is_structured: impl FnMut(&SourceRange) -> bool,
) -> Vec<ReaderFocusCandidate> {
    compile_focus_candidates_from_blocks(section.blocks.iter(), reading_ranges, is_structured)
}

fn compile_focus_candidates_from_blocks<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    reading_ranges: &[SourceRange],
    mut is_structured: impl FnMut(&SourceRange) -> bool,
) -> Vec<ReaderFocusCandidate> {
    let mut candidates: Vec<ReaderFocusCandidate> = Vec::new();
    let mut leading_heading_ranges = Vec::new();
    let mut active_list_root: Option<(usize, u8)> = None;

    for (block_index, block) in blocks.into_iter().enumerate() {
        if focus_block_source_range(block).is_some_and(|range| !reading_ranges.contains(range))
            || focus_block_is_footnote_definition(block)
        {
            active_list_root = None;
            continue;
        }

        match classify_focus_block(block, block_index, &mut is_structured) {
            FocusBlockCandidate::Heading(range) => {
                active_list_root = None;
                if candidates.is_empty() {
                    leading_heading_ranges.push(range);
                }
            }
            FocusBlockCandidate::Reset => active_list_root = None,
            FocusBlockCandidate::Skip => {}
            FocusBlockCandidate::Unit {
                mut candidate,
                list_depth,
            } => {
                if candidate.text.trim().is_empty()
                    && candidate.content_kind != ReaderFocusContentKind::Image
                {
                    if list_depth.is_none_or(|depth| depth == 0) {
                        active_list_root = None;
                    }
                    continue;
                }
                if candidates.is_empty() && !leading_heading_ranges.is_empty() {
                    candidate.geometry_ranges = leading_heading_ranges
                        .iter()
                        .chain(&candidate.paint_ranges)
                        .cloned()
                        .collect();
                }
                if let Some(depth) = list_depth {
                    if let Some(root_index) = focus_list_descendant_root(active_list_root, depth)
                        && candidates[root_index].range.start.spine == candidate.range.start.spine
                    {
                        merge_focus_candidate(&mut candidates[root_index], candidate);
                        continue;
                    }
                    let root_index = candidates.len();
                    candidates.push(candidate);
                    active_list_root = Some((root_index, depth));
                } else {
                    active_list_root = None;
                    candidates.push(candidate);
                }
            }
        }
    }
    candidates
}

fn classify_focus_block(
    block: &Block,
    block_index: usize,
    is_structured: &mut impl FnMut(&SourceRange) -> bool,
) -> FocusBlockCandidate {
    if let Block::Text(text) = block {
        return classify_focus_text_block(text, block_index, is_structured);
    }
    let (range, paint_ranges, text, content_kind, activation, list_depth) = match block {
        Block::Text(_) => unreachable!("text blocks return before non-text classification"),
        Block::Quote(quote) => {
            let Some(range) = quote.source.clone() else {
                return FocusBlockCandidate::Skip;
            };
            (
                range.clone(),
                focus_block_paint_ranges(block, &range),
                focus_block_text(block),
                ReaderFocusContentKind::Text,
                ReaderFocusActivation::Rectangular,
                None,
            )
        }
        Block::Table(table) => {
            let Some(range) = table.source.clone() else {
                return FocusBlockCandidate::Skip;
            };
            (
                range.clone(),
                focus_block_paint_ranges(block, &range),
                focus_block_text(block),
                ReaderFocusContentKind::Table,
                ReaderFocusActivation::Inline,
                None,
            )
        }
        Block::Image(image) => {
            if image.text_layer.is_some() {
                return FocusBlockCandidate::Reset;
            }
            let Some(range) = image.source.clone() else {
                return FocusBlockCandidate::Skip;
            };
            (
                range.clone(),
                vec![range],
                image.alt.clone(),
                ReaderFocusContentKind::Image,
                ReaderFocusActivation::Inline,
                None,
            )
        }
        Block::Figure(figure) => {
            let Some(range) = figure.source.clone() else {
                return FocusBlockCandidate::Skip;
            };
            (
                range.clone(),
                focus_block_paint_ranges(block, &range),
                focus_block_text(block),
                ReaderFocusContentKind::Image,
                ReaderFocusActivation::Inline,
                None,
            )
        }
        Block::Separator | Block::LineBreak | Block::PageBreak => {
            return FocusBlockCandidate::Reset;
        }
    };
    let geometry_ranges = paint_ranges.clone();
    FocusBlockCandidate::Unit {
        candidate: ReaderFocusCandidate {
            block_indices: vec![block_index],
            range,
            paint_ranges,
            geometry_ranges,
            text,
            content_kind,
            activation,
        },
        list_depth,
    }
}

fn classify_focus_text_block(
    text: &TextBlock,
    block_index: usize,
    is_structured: &mut impl FnMut(&SourceRange) -> bool,
) -> FocusBlockCandidate {
    if matches!(text.kind, TextBlockKind::Heading(_)) {
        return text
            .source
            .clone()
            .map_or(FocusBlockCandidate::Skip, FocusBlockCandidate::Heading);
    }
    let Some(range) = text.source.clone() else {
        return FocusBlockCandidate::Skip;
    };
    let list_depth = match text.kind {
        TextBlockKind::ListItem { depth, .. } => Some(depth),
        _ => None,
    };
    let activation = if text.kind == TextBlockKind::Paragraph && is_structured(&range) {
        ReaderFocusActivation::Structured
    } else if text.kind == TextBlockKind::Preformatted {
        ReaderFocusActivation::Rectangular
    } else {
        ReaderFocusActivation::Inline
    };
    FocusBlockCandidate::Unit {
        candidate: ReaderFocusCandidate {
            block_indices: vec![block_index],
            geometry_ranges: vec![range.clone()],
            paint_ranges: vec![range.clone()],
            range,
            text: focus_text_block_text(text),
            content_kind: ReaderFocusContentKind::Text,
            activation,
        },
        list_depth,
    }
}

fn merge_focus_candidate(root: &mut ReaderFocusCandidate, descendant: ReaderFocusCandidate) {
    root.range.end = descendant.range.end;
    root.paint_ranges.extend(descendant.paint_ranges);
    root.geometry_ranges.extend(descendant.geometry_ranges);
    root.block_indices.extend(descendant.block_indices);
    if !descendant.text.trim().is_empty() {
        if !root.text.is_empty() {
            root.text.push('\n');
        }
        root.text.push_str(&descendant.text);
    }
}

fn focus_list_descendant_root(active_root: Option<(usize, u8)>, depth: u8) -> Option<usize> {
    active_root.and_then(|(root_index, root_depth)| (depth > root_depth).then_some(root_index))
}

/// Returns the candidate containing, or immediately following, the source block
/// addressed by `anchor`. Heading anchors resolve to the first following prose
/// candidate because headings are intentionally not independent focus steps.
#[must_use]
pub fn focus_candidate_index_for_anchor(
    section: &Section,
    candidates: &[ReaderFocusCandidate],
    anchor: Option<&SourceAnchor>,
) -> Option<usize> {
    let block_index = focus_anchor_block_index(&section.blocks, anchor)?;
    candidates.iter().position(|candidate| {
        candidate
            .block_indices
            .last()
            .is_some_and(|last| *last >= block_index)
    })
}

#[must_use]
pub fn focus_anchor_block_index(blocks: &[Block], anchor: Option<&SourceAnchor>) -> Option<usize> {
    focus_anchor_block_index_from_blocks(blocks.iter(), anchor)
}

fn focus_anchor_block_index_from_blocks<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    anchor: Option<&SourceAnchor>,
) -> Option<usize> {
    let anchor = anchor?;
    blocks.into_iter().position(|block| {
        let Some(range) = focus_block_source_range(block) else {
            return false;
        };
        source_range_contains_anchor(range, anchor)
            || focus_block_paint_ranges(block, range)
                .iter()
                .any(|range| source_range_contains_anchor(range, anchor))
    })
}

#[must_use]
pub fn focus_block_source_range(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(block) => block.source.as_ref(),
        Block::Quote(quote) => quote.source.as_ref(),
        Block::Table(table) => table.source.as_ref(),
        Block::Image(image) => image.source.as_ref(),
        Block::Figure(figure) => figure.source.as_ref(),
        Block::Separator | Block::LineBreak | Block::PageBreak => None,
    }
}

#[must_use]
pub fn focus_block_paint_ranges(block: &Block, range: &SourceRange) -> Vec<SourceRange> {
    let nested = match block {
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(&quote.attribution)
            .filter_map(|block| block.source.clone())
            .collect::<Vec<_>>(),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .filter_map(|cell| cell.text.source.clone())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if nested.is_empty() {
        vec![range.clone()]
    } else {
        nested
    }
}

#[must_use]
pub fn focus_block_text(block: &Block) -> String {
    match block {
        Block::Text(block) => focus_text_block_text(block),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(&quote.attribution)
            .map(focus_text_block_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .map(|cell| focus_text_block_text(&cell.text))
            .collect::<Vec<_>>()
            .join(" "),
        Block::Image(image) => image.alt.clone(),
        Block::Figure(figure) => {
            let captions = figure
                .captions
                .iter()
                .map(focus_text_block_text)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if captions.is_empty() {
                figure
                    .images
                    .iter()
                    .map(|image| image.alt.trim())
                    .filter(|alt| !alt.is_empty())
                    .collect::<Vec<_>>()
                    .join("; ")
            } else {
                captions
            }
        }
        Block::Separator | Block::LineBreak | Block::PageBreak => String::new(),
    }
}

fn focus_text_block_text(block: &TextBlock) -> String {
    block
        .content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(run) if run.style.inline_role == InlineRole::Footnote => None,
            Inline::Text(run) => Some(run.text.as_str()),
            Inline::Math(run) => Some(run.latex.as_str()),
            Inline::Break => Some("\n"),
        })
        .collect()
}

fn text_block_has_link_role(block: &TextBlock, role: LinkRole) -> bool {
    block.content.iter().any(|inline| {
        matches!(inline, Inline::Text(run) if run.link.is_some() && run.style.link_role == role)
    })
}

fn text_block_focus_footnote_sources(block: &TextBlock) -> Vec<ReaderFocusFootnoteSource> {
    fn flush_inline_note(notes: &mut Vec<ReaderFocusFootnoteSource>, text: &mut String) {
        let note = text.trim();
        if !note.is_empty() {
            notes.push(ReaderFocusFootnoteSource::Inline(note.to_owned()));
        }
        text.clear();
    }

    let mut notes = Vec::new();
    let mut inline_note = String::new();
    for inline in &block.content {
        let Inline::Text(run) = inline else {
            flush_inline_note(&mut notes, &mut inline_note);
            continue;
        };
        if run.style.inline_role == InlineRole::Footnote {
            inline_note.push_str(&run.text);
            continue;
        }
        flush_inline_note(&mut notes, &mut inline_note);
        if (run.style.link_role == LinkRole::FootnoteReference
            || (run.style.link_role == LinkRole::Normal
                && run.style.baseline == TextBaseline::Superscript))
            && let Some(target) = run
                .link
                .clone()
                .filter(|target| target.fragment().is_some())
            && !run.text.trim().is_empty()
        {
            notes.push(ReaderFocusFootnoteSource::Reference {
                marker: run.text.trim().to_owned(),
                target,
            });
        }
    }
    flush_inline_note(&mut notes, &mut inline_note);
    notes
}

/// Extracts inline and linked footnote sources from any focus-readable block.
#[must_use]
pub fn focus_block_footnote_sources(block: &Block) -> Vec<ReaderFocusFootnoteSource> {
    match block {
        Block::Text(block) => text_block_focus_footnote_sources(block),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(&quote.attribution)
            .flat_map(text_block_focus_footnote_sources)
            .collect(),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .flat_map(|cell| text_block_focus_footnote_sources(&cell.text))
            .collect(),
        Block::Figure(figure) => figure
            .captions
            .iter()
            .flat_map(text_block_focus_footnote_sources)
            .collect(),
        Block::Image(_) | Block::Separator | Block::LineBreak | Block::PageBreak => Vec::new(),
    }
}

#[must_use]
pub fn focus_block_is_footnote_definition(block: &Block) -> bool {
    match block {
        Block::Text(block) => text_block_has_link_role(block, LinkRole::FootnoteBacklink),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(&quote.attribution)
            .any(|block| text_block_has_link_role(block, LinkRole::FootnoteBacklink)),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| text_block_has_link_role(&cell.text, LinkRole::FootnoteBacklink)),
        Block::Figure(figure) => figure
            .captions
            .iter()
            .any(|caption| text_block_has_link_role(caption, LinkRole::FootnoteBacklink)),
        Block::Image(_) | Block::Separator | Block::LineBreak | Block::PageBreak => false,
    }
}

impl AsRef<Self> for ReaderFocusUnit {
    fn as_ref(&self) -> &Self {
        self
    }
}

/// Toolkit-neutral outcome of an Up/Down focus-navigation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderFocusMove {
    Empty,
    Boundary,
    Select(usize),
}

/// Focus-only overlays that replace the active unit's inline affordance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReaderFocusOverlay {
    #[default]
    None,
    Actions,
    Footnotes,
}

/// Frontend-neutral command accepted by [`ReaderFocusState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderFocusCommand {
    Select(usize),
    Move(PageDirection),
}

/// Semantic effect emitted after reducing a focus command. Frontends attach
/// animation, persistence, and adjacent-section navigation to these effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderFocusTransition {
    Unchanged,
    Empty,
    Boundary(PageDirection),
    Selected(usize),
}

/// Durable focus selection plus mutually-exclusive focus affordance state.
///
/// No toolkit coordinates or animation clocks are retained here. The source
/// anchor survives candidate/frame regeneration; active indexes are resolved
/// again against each fresh unit generation.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderFocusState {
    active_index: usize,
    anchor: Option<SourceAnchor>,
    overlay: ReaderFocusOverlay,
    footnote_scroll_delta: f32,
}

impl Default for ReaderFocusState {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ReaderFocusState {
    #[must_use]
    pub const fn new(anchor: Option<SourceAnchor>) -> Self {
        Self {
            active_index: 0,
            anchor,
            overlay: ReaderFocusOverlay::None,
            footnote_scroll_delta: 0.0,
        }
    }

    #[must_use]
    pub const fn active_index(&self) -> usize {
        self.active_index
    }

    #[must_use]
    pub fn anchor(&self) -> Option<&SourceAnchor> {
        self.anchor.as_ref()
    }

    #[must_use]
    pub const fn overlay(&self) -> ReaderFocusOverlay {
        self.overlay
    }

    #[must_use]
    pub const fn actions_visible(&self) -> bool {
        matches!(self.overlay, ReaderFocusOverlay::Actions)
    }

    #[must_use]
    pub const fn footnotes_visible(&self) -> bool {
        matches!(self.overlay, ReaderFocusOverlay::Footnotes)
    }

    pub fn reset(&mut self, anchor: Option<SourceAnchor>) {
        self.active_index = 0;
        self.anchor = anchor;
        self.overlay = ReaderFocusOverlay::None;
        self.footnote_scroll_delta = 0.0;
    }

    pub fn set_anchor(&mut self, anchor: Option<SourceAnchor>) {
        self.anchor = anchor;
    }

    pub fn resolve<T: AsRef<ReaderFocusUnit>>(
        &mut self,
        units: &[T],
        first_unit_after_anchor: Option<usize>,
        current: ReaderPosition,
    ) {
        self.active_index = resolve_focus_unit_index(
            units,
            self.anchor.as_ref(),
            first_unit_after_anchor,
            current,
        );
        self.anchor = units
            .get(self.active_index)
            .map(|unit| unit.as_ref().range.start.clone());
    }

    pub fn apply<T: AsRef<ReaderFocusUnit>>(
        &mut self,
        units: &[T],
        command: ReaderFocusCommand,
    ) -> ReaderFocusTransition {
        let requested = match command {
            ReaderFocusCommand::Select(index) => ReaderFocusMove::Select(index),
            ReaderFocusCommand::Move(direction) => {
                match focus_move(self.active_index, units.len(), direction) {
                    ReaderFocusMove::Empty => return ReaderFocusTransition::Empty,
                    ReaderFocusMove::Boundary => {
                        return ReaderFocusTransition::Boundary(direction);
                    }
                    selected @ ReaderFocusMove::Select(_) => selected,
                }
            }
        };
        let ReaderFocusMove::Select(index) = requested else {
            unreachable!("explicit selection is the only remaining focus move")
        };
        let Some(unit) = units.get(index) else {
            return ReaderFocusTransition::Unchanged;
        };
        let anchor = unit.as_ref().range.start.clone();
        let changed = index != self.active_index || self.anchor.as_ref() != Some(&anchor);
        self.active_index = index;
        self.anchor = Some(anchor);
        self.hide_footnotes();
        if changed {
            ReaderFocusTransition::Selected(index)
        } else {
            ReaderFocusTransition::Unchanged
        }
    }

    pub fn show_actions(&mut self) {
        self.overlay = ReaderFocusOverlay::Actions;
        self.footnote_scroll_delta = 0.0;
    }

    pub fn hide_actions(&mut self) {
        if self.overlay == ReaderFocusOverlay::Actions {
            self.overlay = ReaderFocusOverlay::None;
        }
    }

    pub fn show_footnotes(&mut self) {
        self.overlay = ReaderFocusOverlay::Footnotes;
        self.footnote_scroll_delta = 0.0;
    }

    pub fn hide_footnotes(&mut self) {
        if self.overlay == ReaderFocusOverlay::Footnotes {
            self.overlay = ReaderFocusOverlay::None;
        }
        self.footnote_scroll_delta = 0.0;
    }

    pub fn add_footnote_scroll_delta(&mut self, delta: f32) {
        self.footnote_scroll_delta += delta;
    }

    pub fn take_footnote_scroll_delta(&mut self) -> f32 {
        std::mem::take(&mut self.footnote_scroll_delta)
    }
}

#[must_use]
pub fn focus_move(current: usize, unit_count: usize, direction: PageDirection) -> ReaderFocusMove {
    if unit_count == 0 {
        return ReaderFocusMove::Empty;
    }
    match direction {
        PageDirection::Previous if current == 0 => ReaderFocusMove::Boundary,
        PageDirection::Next if current + 1 >= unit_count => ReaderFocusMove::Boundary,
        PageDirection::Previous => ReaderFocusMove::Select(current - 1),
        PageDirection::Next => ReaderFocusMove::Select(current + 1),
    }
}

#[must_use]
pub fn resolve_focus_unit_index<T: AsRef<ReaderFocusUnit>>(
    units: &[T],
    anchor: Option<&SourceAnchor>,
    first_unit_after_anchor: Option<usize>,
    current: ReaderPosition,
) -> usize {
    anchor
        .and_then(|anchor| {
            units
                .iter()
                .position(|unit| unit.as_ref().contains_anchor(anchor))
        })
        .or(first_unit_after_anchor)
        .or_else(|| {
            units
                .iter()
                .position(|unit| unit.as_ref().geometry.position == current)
        })
        .unwrap_or(0)
}

#[must_use]
pub fn source_range_contains_anchor(range: &SourceRange, anchor: &SourceAnchor) -> bool {
    if range.start.spine != anchor.spine || range.start.node != anchor.node {
        return false;
    }
    if range.start.spine != range.end.spine || range.start.node != range.end.node {
        return range.start == *anchor;
    }
    anchor.text_offset >= range.start.text_offset
        && (anchor.text_offset < range.end.text_offset
            || (range.start.text_offset == range.end.text_offset
                && anchor.text_offset == range.start.text_offset))
}

/// Shared viewport policy for centering and manually scrolling focus units.
/// Animation curves and clocks remain frontend-owned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderFocusViewportPolicy {
    pub minimum_unit_height: f32,
}

/// Explicit content-to-viewport alignment for a focus navigation target.
/// This avoids assuming that a frontend inserted synthetic half-viewport
/// padding around its scroll surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReaderFocusViewportTarget {
    pub content_y: f32,
    pub viewport_y: f32,
}

impl Default for ReaderFocusViewportPolicy {
    fn default() -> Self {
        Self {
            minimum_unit_height: 240.0,
        }
    }
}

impl ReaderFocusViewportPolicy {
    #[must_use]
    pub fn unit_viewport_target(
        self,
        bounds: FrameRect,
        viewport_height: f32,
    ) -> ReaderFocusViewportTarget {
        let height = (bounds.y1 - bounds.y0).max(0.0);
        if height > viewport_height {
            return ReaderFocusViewportTarget {
                content_y: bounds.y0,
                viewport_y: 0.0,
            };
        }
        ReaderFocusViewportTarget {
            content_y: bounds.y0 + height * 0.5,
            viewport_y: viewport_height * 0.5,
        }
    }

    #[must_use]
    pub fn unit_scroll_bounds(
        self,
        bounds: FrameRect,
        viewport_height: f32,
        content_padding: f32,
    ) -> (f32, f32) {
        let top = (bounds.y0 + content_padding).max(0.0);
        let bottom = (bounds.y1 + content_padding - viewport_height).max(top);
        (top, bottom)
    }

    #[must_use]
    pub fn unit_target_offset(self, bounds: FrameRect, viewport_height: f32) -> f32 {
        let content_padding = viewport_height * 0.5;
        let (top, bottom) = self.unit_scroll_bounds(bounds, viewport_height, content_padding);
        if bottom > top {
            return top;
        }
        let unit_height = (bounds.y1 - bounds.y0).max(self.minimum_unit_height);
        (bounds.y0 + unit_height * 0.5 + content_padding - viewport_height * 0.5).max(0.0)
    }

    #[must_use]
    pub fn nearest_unit_for_offset(
        self,
        bounds: impl IntoIterator<Item = FrameRect>,
        offset: f32,
        viewport_height: f32,
    ) -> Option<usize> {
        bounds
            .into_iter()
            .enumerate()
            .map(|(index, bounds)| {
                let target = self.unit_target_offset(bounds, viewport_height);
                (index, (target - offset).abs())
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index)
    }
}

/// One immutable logical frame in an authored section or semantic reading
/// unit. Crop bounds are expressed in logical frame coordinates.
#[derive(Clone)]
pub struct ReaderSectionFrame {
    pub position: ReaderPosition,
    pub frame: Arc<LayoutFrame>,
    pub placeholder: bool,
    pub visible_top: Option<f32>,
    pub visible_bottom: Option<f32>,
}

/// One immutable logical frame positioned inside a continuous reading unit.
#[derive(Clone)]
pub struct ReaderScrollFrame {
    pub position: ReaderPosition,
    pub frame: Arc<LayoutFrame>,
    pub placeholder: bool,
    pub top: f32,
    pub origin_y: f32,
    pub height: f32,
}

/// Backend-neutral vertical composition of the frames in one semantic reading
/// unit. This is shared by scroll and focus presentations; renderers only paint
/// the retained frames at the resulting offsets.
pub struct ReaderScrollLayout {
    pub section_index: usize,
    pub reading_unit_index: usize,
    frames: Vec<ReaderScrollFrame>,
    content_height: f32,
}

impl ReaderScrollLayout {
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "layout viewport dimensions are bounded logical pixels composed as f32"
    )]
    pub fn new(
        section_index: usize,
        reading_unit_index: usize,
        frames: Vec<ReaderSectionFrame>,
        preserve_physical_pages: bool,
    ) -> Self {
        let mut cursor = 0.0;
        let mut composed: Vec<ReaderScrollFrame> = Vec::with_capacity(frames.len());
        for (index, entry) in frames.into_iter().enumerate() {
            let logical_height = entry.frame.viewport.height as f32;
            let (retained_top, retained_bottom) = entry
                .frame
                .content_vertical_bounds()
                .unwrap_or((0.0, logical_height));
            let content_top = entry
                .visible_top
                .unwrap_or(retained_top)
                .clamp(0.0, logical_height);
            let content_bottom = entry
                .visible_bottom
                .unwrap_or(retained_bottom)
                .clamp(content_top, logical_height);
            let leading_gap =
                if preserve_physical_pages || entry.visible_top.is_some() || index == 0 {
                    0.0
                } else {
                    entry.frame.leading_gap
                };
            let origin_y = if preserve_physical_pages || entry.visible_top.is_some() || index > 0 {
                content_top
            } else {
                0.0
            };
            let height = if preserve_physical_pages {
                content_bottom - content_top
            } else {
                content_bottom - origin_y
            };
            if leading_gap > 0.0 {
                if let Some(previous) = composed.last_mut() {
                    previous.height += leading_gap;
                }
                cursor += leading_gap;
            }
            composed.push(ReaderScrollFrame {
                position: entry.position,
                frame: entry.frame,
                placeholder: entry.placeholder,
                top: cursor,
                origin_y,
                height,
            });
            cursor += height;
        }
        Self {
            section_index,
            reading_unit_index,
            frames: composed,
            content_height: cursor,
        }
    }

    #[must_use]
    pub fn frames(&self) -> &[ReaderScrollFrame] {
        &self.frames
    }

    #[must_use]
    pub const fn content_height(&self) -> f32 {
        self.content_height
    }

    #[must_use]
    pub fn frame_index(&self, position: ReaderPosition) -> Option<usize> {
        self.frames
            .iter()
            .position(|entry| entry.position == position)
    }

    #[must_use]
    pub fn frame_top(&self, position: ReaderPosition) -> Option<f32> {
        self.frame_index(position)
            .map(|index| self.frames[index].top)
    }

    /// Maps one continuous-content coordinate to a frame and page-local y.
    #[must_use]
    pub fn frame_at_content_y(&self, y: f32) -> Option<(usize, f32)> {
        self.frames.iter().enumerate().find_map(|(index, frame)| {
            let local_y = y - frame.top;
            (local_y >= 0.0 && local_y < frame.height).then_some((index, local_y + frame.origin_y))
        })
    }

    #[must_use]
    pub fn content_y(&self, index: usize, frame_y: f32) -> Option<f32> {
        let frame = self.frames.get(index)?;
        Some(frame.top + frame_y - frame.origin_y)
    }

    #[must_use]
    pub fn content_y_for_position(&self, position: ReaderPosition, frame_y: f32) -> Option<f32> {
        self.content_y(self.frame_index(position)?, frame_y)
    }

    #[must_use]
    pub fn source_top(&self, range: &SourceRange) -> Option<f32> {
        self.frames.iter().enumerate().find_map(|(index, entry)| {
            entry
                .frame
                .source_content_bounds(std::slice::from_ref(range))
                .and_then(|bounds| self.content_y(index, bounds.y0))
        })
    }

    /// Resolves a durable anchor to a vertical coordinate in the stitched
    /// surface. Generation-local frame IDs never leave this presentation model.
    #[must_use]
    pub fn source_anchor_top(&self, anchor: &SourceAnchor) -> Option<f32> {
        self.frames.iter().enumerate().find_map(|(index, entry)| {
            entry
                .frame
                .source_anchor_bounds(anchor)
                .and_then(|bounds| self.content_y(index, bounds.y0))
        })
    }

    /// Resolves durable ranges to their union in stitched reading-unit
    /// coordinates and reports the first logical frame that contributes.
    #[must_use]
    pub fn source_content_bounds(
        &self,
        ranges: &[SourceRange],
    ) -> Option<(FrameRect, ReaderPosition)> {
        let mut bounds = None;
        let mut position = None;
        for (index, entry) in self.frames.iter().enumerate() {
            let Some(local) = entry.frame.source_content_bounds(ranges) else {
                continue;
            };
            let stitched = self.stitched_rect(index, local)?;
            bounds = Some(bounds.map_or(stitched, |current: FrameRect| current.union(stitched)));
            position.get_or_insert(entry.position);
        }
        bounds.zip(position)
    }

    /// Resolves rectangular semantic-block geometry into stitched coordinates.
    #[must_use]
    pub fn source_block_bounds(&self, ranges: &[SourceRange]) -> Option<FrameRect> {
        self.frames
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let local = entry.frame.source_block_bounds(ranges)?;
                self.stitched_rect(index, local)
            })
            .reduce(FrameRect::union)
    }

    /// Builds the neutral geometry consumed by focus-mode frontends. The
    /// optional activation ranges are used for quote/preformatted/structured
    /// rectangular affordances; ordinary prose can omit them.
    #[must_use]
    pub fn focus_geometry(
        &self,
        content_ranges: &[SourceRange],
        activation_ranges: Option<&[SourceRange]>,
    ) -> Option<ReaderFocusGeometry> {
        let (bounds, position) = self.source_content_bounds(content_ranges)?;
        let activation_bounds =
            activation_ranges.and_then(|ranges| self.source_block_bounds(ranges));
        Some(ReaderFocusGeometry {
            position,
            bounds,
            activation_bounds,
        })
    }

    fn stitched_rect(&self, index: usize, local: FrameRect) -> Option<FrameRect> {
        Some(FrameRect::new(
            local.x0,
            self.content_y(index, local.y0)?,
            local.x1,
            self.content_y(index, local.y1)?,
        ))
    }

    /// Resolves text inside one displayed scroll-frame crop. `y` is relative to
    /// the cropped frame, not the original logical page.
    #[must_use]
    pub fn hit_test_text(
        &self,
        frame_index: usize,
        x: f32,
        y: f32,
        exact: bool,
    ) -> Option<ReaderTextHit> {
        let entry = self.frames.get(frame_index)?;
        entry
            .frame
            .interaction()
            .hit_test_text(x, y + entry.origin_y, exact)
            .map(|hit| reader_text_hit(entry.position, hit))
    }

    /// First and last generation-local text carets in this reading unit.
    #[must_use]
    pub fn cursor_range(&self) -> Option<(ReaderTextCursor, ReaderTextCursor)> {
        let first = self.frames.iter().find_map(|entry| {
            entry
                .frame
                .interaction()
                .first_cursor()
                .map(|cursor| ReaderTextCursor {
                    position: entry.position,
                    cursor,
                })
        });
        let last = self.frames.iter().rev().find_map(|entry| {
            entry
                .frame
                .interaction()
                .last_cursor()
                .map(|cursor| ReaderTextCursor {
                    position: entry.position,
                    cursor,
                })
        });
        first.zip(last)
    }

    /// Restores generation-local carets for durable ranges across every frame
    /// in this stitched reading unit.
    #[must_use]
    pub fn cursors_for_source_ranges(
        &self,
        ranges: &[SourceRange],
    ) -> Option<(ReaderTextCursor, ReaderTextCursor)> {
        if ranges.is_empty() {
            return None;
        }
        let mut first = None;
        let mut last = None;
        for entry in &self.frames {
            if let Some((start, end)) = entry.frame.interaction().cursors_for_source_ranges(ranges)
            {
                first.get_or_insert(ReaderTextCursor {
                    position: entry.position,
                    cursor: start,
                });
                last = Some(ReaderTextCursor {
                    position: entry.position,
                    cursor: end,
                });
            }
        }
        first.zip(last)
    }

    /// Moves one caret by a shaped cluster, crossing stitched frame boundaries.
    #[must_use]
    pub fn move_text_cursor(
        &self,
        cursor: ReaderTextCursor,
        direction: TextCursorDirection,
    ) -> Option<ReaderTextCursor> {
        let frame_index = self.frame_index(cursor.position)?;
        let local = match direction {
            TextCursorDirection::Previous => self.frames[frame_index]
                .frame
                .interaction()
                .previous_cursor(cursor.cursor),
            TextCursorDirection::Next => self.frames[frame_index]
                .frame
                .interaction()
                .next_cursor(cursor.cursor),
        };
        if let Some(local) = local {
            return Some(ReaderTextCursor {
                position: cursor.position,
                cursor: local,
            });
        }
        match direction {
            TextCursorDirection::Previous => {
                self.frames[..frame_index].iter().rev().find_map(|entry| {
                    entry
                        .frame
                        .interaction()
                        .last_cursor()
                        .map(|cursor| ReaderTextCursor {
                            position: entry.position,
                            cursor,
                        })
                })
            }
            TextCursorDirection::Next => self.frames[frame_index + 1..].iter().find_map(|entry| {
                entry
                    .frame
                    .interaction()
                    .first_cursor()
                    .map(|cursor| ReaderTextCursor {
                        position: entry.position,
                        cursor,
                    })
            }),
        }
    }

    #[must_use]
    pub fn first_visible_frame(&self, offset_y: f32) -> Option<ReaderPosition> {
        self.frames
            .iter()
            .find(|entry| entry.top + entry.height > offset_y)
            .map(|entry| entry.position)
    }

    #[must_use]
    pub fn visible_frames(&self, offset_y: f32, viewport_height: f32) -> Vec<ReaderPosition> {
        let bottom = offset_y + viewport_height;
        self.frames
            .iter()
            .filter(|entry| entry.top + entry.height > offset_y && entry.top < bottom)
            .map(|entry| entry.position)
            .collect()
    }
}

/// Position inside the semantic table-of-contents units of the current view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadingUnitLocation {
    pub index: usize,
    pub count: usize,
}

/// Flattened, presentation-ready table-of-contents item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocViewItem {
    pub id: String,
    pub label: String,
    pub target: Option<PublicationUrl>,
    pub depth: usize,
    pub ancestors: Vec<String>,
    pub has_children: bool,
}

/// Complete reader state after a command has been applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderSnapshot {
    pub location: ReaderLocation,
    pub total_progression: f64,
    pub active_toc_id: Option<String>,
    pub active_toc_path: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationOutcome {
    Moved,
    Boundary,
}

/// Navigation always returns the resulting state, including at book boundaries.
#[derive(Debug, Clone, PartialEq)]
pub struct NavigationResult {
    pub outcome: NavigationOutcome,
    pub snapshot: ReaderSnapshot,
}

/// Result of an interactive navigation attempt. A pending result means the
/// destination is being prepared by the background pagination worker and the
/// caller should retry without blocking its event loop.
#[derive(Debug, Clone, PartialEq)]
pub enum NavigationAttempt {
    Ready(NavigationResult),
    Pending,
}

enum PositionAttempt {
    Ready(Option<ReaderPosition>),
    Pending,
}

struct CachedSegment {
    section: Arc<PreparedSection>,
    frames: Vec<Arc<LayoutFrame>>,
    #[cfg(feature = "legacy-renderer")]
    pages: Vec<Arc<PageDisplayList>>,
    anchor_pages: HashMap<String, usize>,
    visible_pages: usize,
    continuation_offset_x: f32,
}

struct PreparedSection {
    fragments: Vec<ContentFragment>,
    segments: Vec<LayoutSegment>,
    anchor_segments: HashMap<String, usize>,
    reading_units: Vec<ReadingUnit>,
}

struct ContentFragment {
    blocks: Vec<Block>,
    anchors: Vec<rebook_publication::SectionAnchor>,
}

struct LayoutSegment {
    fragment_range: Range<usize>,
}

struct ReadingUnit {
    fragment_range: Range<usize>,
    start: Option<SourceAnchor>,
}

/// Semantic reading units for fixed-layout publications. A PDF physical page
/// is represented by one spine section, so a TOC leaf unit can span several
/// authored sections without cropping any of those pages.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FixedReadingUnit {
    section_range: Range<usize>,
}

struct SectionRepository {
    source: Arc<dyn BookSource>,
    sections: Vec<SectionSlot>,
}

struct SectionSlot {
    state: Mutex<SectionSlotState>,
    ready: Condvar,
}

enum SectionSlotState {
    Empty,
    Loading,
    Ready(Weak<PreparedSection>),
}

impl SectionRepository {
    fn new(source: Arc<dyn BookSource>) -> Self {
        let section_count = source.book().sections.len();
        Self {
            source,
            sections: (0..section_count)
                .map(|_| SectionSlot {
                    state: Mutex::new(SectionSlotState::Empty),
                    ready: Condvar::new(),
                })
                .collect(),
        }
    }

    fn get(&self, index: usize) -> Option<Arc<PreparedSection>> {
        let slot = self.sections.get(index)?;
        let state = slot.state.lock().ok()?;
        match &*state {
            SectionSlotState::Ready(section) => section.upgrade(),
            SectionSlotState::Empty | SectionSlotState::Loading => None,
        }
    }

    fn load(&self, index: usize) -> Result<Arc<PreparedSection>, ReaderError> {
        let slot = self
            .sections
            .get(index)
            .ok_or(ReaderError::SectionOutOfBounds(index))?;
        loop {
            let mut state = slot
                .state
                .lock()
                .map_err(|_| ReaderError::SectionRepositoryPoisoned)?;
            match &*state {
                SectionSlotState::Ready(section) => {
                    if let Some(section) = section.upgrade() {
                        return Ok(section);
                    }
                    *state = SectionSlotState::Loading;
                }
                SectionSlotState::Empty => *state = SectionSlotState::Loading,
                SectionSlotState::Loading => {
                    drop(
                        slot.ready
                            .wait(state)
                            .map_err(|_| ReaderError::SectionRepositoryPoisoned)?,
                    );
                    continue;
                }
            }
            drop(state);

            let layout_boundaries = top_level_toc_fragments_for_section(self.source.book(), index);
            let reading_boundaries = semantic_toc_boundaries_for_section(self.source.book(), index);
            let parsed = self
                .source
                .parse_section(index)
                .map(|section| prepare_section(section, &layout_boundaries, &reading_boundaries));
            let mut state = slot
                .state
                .lock()
                .map_err(|_| ReaderError::SectionRepositoryPoisoned)?;
            match parsed {
                Ok(section) => {
                    let section = Arc::new(section);
                    *state = SectionSlotState::Ready(Arc::downgrade(&section));
                    slot.ready.notify_all();
                    return Ok(section);
                }
                Err(error) => {
                    *state = SectionSlotState::Empty;
                    slot.ready.notify_all();
                    return Err(error.into());
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SegmentKey {
    section_index: usize,
    segment_index: usize,
}

struct PrefetchRequest {
    key: SegmentKey,
    viewport: LayoutViewport,
    style: ReaderStyle,
    generation: u64,
}

struct PrefetchResult {
    key: SegmentKey,
    generation: u64,
    segment: Result<Arc<CachedSegment>, ReaderError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PrefetchKey {
    generation: u64,
    segment: SegmentKey,
}

struct PrefetchWorker {
    requests: Option<Sender<PrefetchRequest>>,
    results: Mutex<Receiver<PrefetchResult>>,
    active_generation: Arc<AtomicU64>,
    cancelled: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PrefetchWorker {
    fn spawn(
        source: Arc<dyn BookSource>,
        repository: Arc<SectionRepository>,
        fonts: Arc<[ReaderFontBlob]>,
    ) -> Result<Self, ReaderError> {
        let (request_sender, request_receiver) = mpsc::channel::<PrefetchRequest>();
        let (result_sender, result_receiver) = mpsc::channel::<PrefetchResult>();
        let active_generation = Arc::new(AtomicU64::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_generation = Arc::clone(&active_generation);
        let worker_cancelled = Arc::clone(&cancelled);
        let handle = thread::Builder::new()
            .name("rebook-prefetch".into())
            .spawn(move || {
                let mut layout_engine = LayoutEngine::with_fonts(fonts.iter().cloned());
                #[cfg(feature = "legacy-renderer")]
                let display_compiler = DisplayListCompiler;
                while let Ok(request) = request_receiver.recv() {
                    if worker_cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    if worker_generation.load(Ordering::Acquire) != request.generation {
                        continue;
                    }
                    let segment = repository
                        .load(request.key.section_index)
                        .and_then(|section| {
                            compile_segment(
                                source.as_ref(),
                                section,
                                request.key,
                                request.viewport,
                                &request.style,
                                &mut layout_engine,
                                #[cfg(feature = "legacy-renderer")]
                                &display_compiler,
                            )
                            .map(Arc::new)
                        });
                    if worker_cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    if worker_generation.load(Ordering::Acquire) != request.generation {
                        continue;
                    }
                    if result_sender
                        .send(PrefetchResult {
                            key: request.key,
                            generation: request.generation,
                            segment,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(ReaderError::PrefetchWorkerStart)?;
        Ok(Self {
            requests: Some(request_sender),
            results: Mutex::new(result_receiver),
            active_generation,
            cancelled,
            handle: Some(handle),
        })
    }

    fn generation(&self) -> u64 {
        self.active_generation.load(Ordering::Acquire)
    }

    fn invalidate(&self) -> u64 {
        self.active_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn send(&self, request: PrefetchRequest) -> Result<(), ReaderError> {
        self.requests
            .as_ref()
            .ok_or(ReaderError::PrefetchWorkerStopped)?
            .send(request)
            .map_err(|_| ReaderError::PrefetchWorkerStopped)
    }

    fn recv(&self) -> Result<PrefetchResult, ReaderError> {
        self.results
            .lock()
            .map_err(|_| ReaderError::PrefetchWorkerStopped)?
            .recv()
            .map_err(|_| ReaderError::PrefetchWorkerStopped)
    }

    fn try_recv(&self) -> Result<PrefetchResult, TryRecvError> {
        self.results
            .lock()
            .map_or(Err(TryRecvError::Disconnected), |results| {
                results.try_recv()
            })
    }
}

fn spawn_optional_prefetch_worker(
    enabled: bool,
    source: &Arc<dyn BookSource>,
    repository: &Arc<SectionRepository>,
    fonts: &Arc<[ReaderFontBlob]>,
) -> Result<Option<PrefetchWorker>, ReaderError> {
    enabled
        .then(|| {
            PrefetchWorker::spawn(
                Arc::clone(source),
                Arc::clone(repository),
                Arc::clone(fonts),
            )
        })
        .transpose()
}

impl Drop for PrefetchWorker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.requests.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct TocIndex {
    items_by_section: Vec<Vec<usize>>,
    preceding_section_by_section: Vec<Option<usize>>,
}

impl TocIndex {
    fn new(
        items: &[TocViewItem],
        section_indices_by_path: &HashMap<String, usize>,
        section_count: usize,
    ) -> Self {
        let mut items_by_section = vec![Vec::new(); section_count];
        for (item_index, item) in items.iter().enumerate() {
            let Some(section_index) = item
                .target
                .as_ref()
                .and_then(|target| section_indices_by_path.get(target.path()))
                .copied()
            else {
                continue;
            };
            items_by_section[section_index].push(item_index);
        }

        let mut preceding_section_by_section = Vec::with_capacity(section_count);
        let mut preceding_section = None;
        for (section_index, section_items) in items_by_section.iter().enumerate() {
            preceding_section_by_section.push(preceding_section);
            if !section_items.is_empty() {
                preceding_section = Some(section_index);
            }
        }

        Self {
            items_by_section,
            preceding_section_by_section,
        }
    }
}

/// Single-owner reader orchestration over publication and immutable layout
/// frames. Paint backends are optional compatibility consumers of those frames.
pub struct ReaderSession {
    source: Arc<dyn BookSource>,
    repository: Arc<SectionRepository>,
    fonts: Arc<[ReaderFontBlob]>,
    layout_engine: LayoutEngine<Box<dyn TextEngine>>,
    #[cfg(feature = "legacy-renderer")]
    display_compiler: DisplayListCompiler,
    viewport: LayoutViewport,
    style: ReaderStyle,
    toc_items: Arc<[TocViewItem]>,
    toc_index: TocIndex,
    section_indices_by_path: HashMap<String, usize>,
    cache_capacity: usize,
    cache: HashMap<SegmentKey, Arc<CachedSegment>>,
    lru: VecDeque<SegmentKey>,
    prefetch_worker: Option<PrefetchWorker>,
    prefetch_inflight: HashSet<PrefetchKey>,
    prefetch_failures: HashMap<SegmentKey, ReaderError>,
    current_section: usize,
    current_segment: usize,
    current_page: usize,
    current_reading_unit: usize,
    fixed_reading_units: Option<Vec<FixedReadingUnit>>,
}

impl ReaderSession {
    /// Opens the first section and compiles its pages once.
    pub fn open(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
    ) -> Result<Self, ReaderError> {
        Self::open_with_fonts(source, viewport, style, Arc::default())
    }

    /// Opens a reader with application-provided fonts registered in both the
    /// foreground and background pagination engines.
    pub fn open_with_fonts(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
        fonts: Arc<[ReaderFontBlob]>,
    ) -> Result<Self, ReaderError> {
        let mut session = Self::new_unpositioned(source, viewport, style, fonts)?;
        session.ensure_segment(SegmentKey {
            section_index: 0,
            segment_index: 0,
        })?;
        Ok(session)
    }

    /// Opens a reader whose foreground layout is shaped by an application-owned
    /// text engine. This is the migration boundary used by the GPUI shell: the
    /// reader still owns pagination and immutable frames, while GPUI supplies
    /// only font fallback and shaping.
    ///
    /// Application-owned engines may be tied to the UI thread, so background
    /// prefetch is disabled for this session. Missing segments are compiled by
    /// the same injected engine on the caller thread until an engine-factory
    /// based prefetch path is introduced.
    pub fn open_with_text_engine<E: TextEngine + 'static>(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
        text_engine: E,
    ) -> Result<Self, ReaderError> {
        Self::open_with_fonts_and_text_engine(source, viewport, style, Arc::default(), text_engine)
    }

    /// Variant of [`Self::open_with_text_engine`] that registers application
    /// font bytes with both the neutral layout catalog and the injected engine.
    pub fn open_with_fonts_and_text_engine<E: TextEngine + 'static>(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
        fonts: Arc<[ReaderFontBlob]>,
        text_engine: E,
    ) -> Result<Self, ReaderError> {
        let mut session = Self::new_unpositioned_with_text_engine(
            source,
            viewport,
            style,
            fonts,
            Box::new(text_engine),
            false,
        )?;
        session.ensure_segment(SegmentKey {
            section_index: 0,
            segment_index: 0,
        })?;
        Ok(session)
    }

    /// Opens directly at a durable locator while using an application-owned
    /// text engine. Like [`Self::open_with_fonts_at_locator`], this avoids
    /// compiling the first section when the saved position belongs elsewhere.
    pub fn open_with_text_engine_at_locator<E: TextEngine + 'static>(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
        text_engine: E,
        locator: &LocatorV1,
    ) -> Result<Self, ReaderError> {
        Self::open_with_fonts_and_text_engine_at_locator(
            source,
            viewport,
            style,
            Arc::default(),
            text_engine,
            locator,
        )
    }

    /// Variant of [`Self::open_with_text_engine_at_locator`] that registers
    /// application-provided font bytes before restoring the locator.
    pub fn open_with_fonts_and_text_engine_at_locator<E: TextEngine + 'static>(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
        fonts: Arc<[ReaderFontBlob]>,
        text_engine: E,
        locator: &LocatorV1,
    ) -> Result<Self, ReaderError> {
        let mut session = Self::new_unpositioned_with_text_engine(
            source,
            viewport,
            style,
            fonts,
            Box::new(text_engine),
            false,
        )?;
        session.restore_locator(locator)?;
        Ok(session)
    }

    /// Opens directly at a durable locator without first compiling the first
    /// section. This avoids duplicate parsing and pagination when resuming a
    /// book away from its beginning.
    pub fn open_with_fonts_at_locator(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
        fonts: Arc<[ReaderFontBlob]>,
        locator: &LocatorV1,
    ) -> Result<Self, ReaderError> {
        let mut session = Self::new_unpositioned(source, viewport, style, fonts)?;
        session.restore_locator(locator)?;
        Ok(session)
    }

    fn new_unpositioned(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        style: ReaderStyle,
        fonts: Arc<[ReaderFontBlob]>,
    ) -> Result<Self, ReaderError> {
        Self::new_unpositioned_with_text_engine(
            source,
            viewport,
            style,
            fonts,
            Box::new(LegacyParleyTextEngine::default()),
            true,
        )
    }

    fn new_unpositioned_with_text_engine(
        source: Arc<dyn BookSource>,
        viewport: LayoutViewport,
        mut style: ReaderStyle,
        fonts: Arc<[ReaderFontBlob]>,
        text_engine: Box<dyn TextEngine>,
        enable_background_prefetch: bool,
    ) -> Result<Self, ReaderError> {
        if source.book().sections.is_empty() {
            return Err(ReaderError::EmptyBook);
        }
        style.writing_system = source.book().metadata.writing_system();
        let toc_items: Arc<[TocViewItem]> = flatten_toc(&source.book().table_of_contents).into();
        let mut section_indices_by_path = HashMap::with_capacity(source.book().sections.len());
        for (index, section) in source.book().sections.iter().enumerate() {
            section_indices_by_path
                .entry(section.href.path().to_owned())
                .or_insert(index);
        }
        let toc_index = TocIndex::new(
            &toc_items,
            &section_indices_by_path,
            source.book().sections.len(),
        );
        let fixed_reading_units =
            build_fixed_reading_units(source.as_ref(), &toc_items, &section_indices_by_path);
        let repository = Arc::new(SectionRepository::new(Arc::clone(&source)));
        let prefetch_worker = spawn_optional_prefetch_worker(
            enable_background_prefetch,
            &source,
            &repository,
            &fonts,
        )?;
        let mut layout_engine = LayoutEngine::with_text_engine(text_engine);
        layout_engine.register_fonts(fonts.iter().cloned());
        Ok(Self {
            source,
            repository,
            layout_engine,
            fonts,
            #[cfg(feature = "legacy-renderer")]
            display_compiler: DisplayListCompiler,
            viewport,
            style,
            toc_items,
            toc_index,
            section_indices_by_path,
            cache_capacity: DEFAULT_SEGMENT_CACHE_CAPACITY,
            cache: HashMap::new(),
            lru: VecDeque::new(),
            prefetch_worker,
            prefetch_inflight: HashSet::new(),
            prefetch_failures: HashMap::new(),
            current_section: 0,
            current_segment: 0,
            current_page: 0,
            current_reading_unit: 0,
            fixed_reading_units,
        })
    }

    pub fn book(&self) -> &Book {
        self.source.book()
    }

    /// Returns the lazy normalized publication source for frontend-neutral
    /// services such as assistant search. Consumers must keep durable
    /// navigation in [`SourceAnchor`] / [`SourceRange`] rather than retaining
    /// layout-relative positions.
    #[must_use]
    pub fn book_source(&self) -> Arc<dyn BookSource> {
        Arc::clone(&self.source)
    }

    pub fn viewport(&self) -> LayoutViewport {
        self.viewport
    }

    pub fn style(&self) -> ReaderStyle {
        self.style.clone()
    }

    pub fn available_font_families(&mut self) -> Vec<String> {
        self.layout_engine.available_font_families()
    }

    pub fn toc_items(&self) -> &[TocViewItem] {
        &self.toc_items
    }

    pub fn section_count(&self) -> usize {
        self.source.book().sections.len()
    }

    pub fn location(&self) -> ReaderLocation {
        let segment_count = self.current_section_data().segments.len();
        ReaderLocation {
            section_index: self.current_section,
            segment_index: self.current_segment,
            segment_count,
            page_index: self.current_page,
            page_count: self.current_page_count(),
        }
    }

    pub fn snapshot(&self) -> ReaderSnapshot {
        let location = self.location();
        let active_toc = active_toc_item_for_location(
            &self.toc_items,
            &self.toc_index.items_by_section[location.section_index],
            self.toc_index.preceding_section_by_section[location.section_index]
                .map_or(&[], |section_index| {
                    self.toc_index.items_by_section[section_index].as_slice()
                }),
            location.section_index,
            location.segment_index,
            location.page_index,
            |target| self.position_for_href(target),
        );
        let (active_toc_id, active_toc_path) = active_toc.map_or_else(
            || (None, Vec::new()),
            |item| {
                let mut path = item.ancestors.clone();
                if item.has_children {
                    path.push(item.id.clone());
                }
                (Some(item.id.clone()), path)
            },
        );
        ReaderSnapshot {
            location,
            total_progression: total_progression(location, self.source.book().sections.len()),
            active_toc_id,
            active_toc_path,
        }
    }

    /// Captures a durable, versioned locator for the first visible content.
    #[allow(clippy::cast_precision_loss)]
    pub fn current_locator(&self) -> LocatorV1 {
        let location = self.location();
        let segment_count = location.segment_count.max(1);
        let page_progression = if location.page_count <= 1 {
            0.0
        } else {
            location.page_index as f64 / (location.page_count - 1) as f64
        };
        let progression = (location.segment_index as f64 + page_progression) / segment_count as f64;
        let section = &self.source.book().sections[location.section_index];
        LocatorV1 {
            version: LocatorV1::VERSION,
            publication_id: self.source.book().id.clone(),
            href: section.href.clone(),
            progression: Some(progression.clamp(0.0, 1.0)),
            total_progression: Some(self.snapshot().total_progression),
            position: None,
            source: self
                .current_layout_frame()
                .interaction()
                .leading_source_range(),
            partial_cfi: None,
            text: None,
        }
    }

    /// Restores a durable locator, preferring source anchors over layout-relative
    /// progression so typography and viewport changes do not move the reader.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    pub fn restore_locator(
        &mut self,
        locator: &LocatorV1,
    ) -> Result<NavigationResult, ReaderError> {
        locator.validate()?;
        if locator.publication_id != self.source.book().id {
            return Err(ReaderError::NavigationTargetNotFound(
                locator.publication_id.to_string(),
            ));
        }
        if let Some(source) = &locator.source
            && let Ok(result) = self.go_to_source(&source.start)
        {
            return Ok(result);
        }

        let (section_index, progression) =
            if let Some(index) = self.section_index_for_href(&locator.href) {
                (index, locator.progression.unwrap_or(0.0))
            } else if let Some(total) = locator.total_progression {
                let section_count = self.source.book().sections.len();
                let scaled = total.clamp(0.0, 1.0) * section_count as f64;
                let index = (scaled.floor() as usize).min(section_count.saturating_sub(1));
                (index, if total >= 1.0 { 1.0 } else { scaled.fract() })
            } else {
                return self.go_to_href(&locator.href);
            };
        let section = self.repository.load(section_index)?;
        let segment_count = section.segments.len().max(1);
        let scaled = progression.clamp(0.0, 1.0) * segment_count as f64;
        let segment_index = if progression >= 1.0 {
            segment_count - 1
        } else {
            (scaled.floor() as usize).min(segment_count - 1)
        };
        let key = SegmentKey {
            section_index,
            segment_index,
        };
        self.ensure_segment(key)?;
        let page_count = self
            .cache
            .get(&key)
            .map_or(1, |segment| segment.frames.len().max(1));
        let within_segment = if progression >= 1.0 {
            1.0
        } else {
            scaled.fract()
        };
        let page_index = if page_count <= 1 {
            0
        } else {
            (within_segment * (page_count - 1) as f64).round() as usize
        };
        self.install_position(ReaderPosition {
            section_index,
            segment_index,
            page_index,
        });
        Ok(self.moved())
    }

    /// Returns the compiled display list for the current page.
    ///
    /// # Panics
    ///
    /// Panics if the reader's internal invariant is broken and the current
    /// section or page is missing from the cache.
    #[cfg(feature = "legacy-renderer")]
    pub fn current_page(&self) -> &PageDisplayList {
        self.cache
            .get(&self.current_key())
            .expect("current layout segment must remain cached")
            .pages[self.current_page]
            .as_ref()
    }

    /// Returns the immutable layout frame for the current page.
    ///
    /// # Panics
    ///
    /// Panics if the reader's internal invariant is broken and the current
    /// section or page is missing from the cache.
    pub fn current_layout_frame(&self) -> &LayoutFrame {
        self.cache
            .get(&self.current_key())
            .expect("current layout segment must remain cached")
            .frames[self.current_page]
            .as_ref()
    }

    /// Returns the immutable frames currently composed into the reader
    /// viewport. This is the primary frontend contract and does not require a
    /// paint backend.
    pub fn current_frame_spread(&mut self) -> Result<ReaderFrameSpread, ReaderError> {
        self.poll_prefetch()?;
        let position = self.current_position();
        let (primary, visible_pages, default_secondary_offset_x) = self
            .cache
            .get(&self.current_key())
            .and_then(|segment| {
                segment.frames.get(self.current_page).map(|frame| {
                    (
                        Arc::clone(frame),
                        segment.visible_pages,
                        segment.continuation_offset_x,
                    )
                })
            })
            .ok_or(ReaderError::PageOutOfBounds(position))?;
        let secondary = if visible_pages > 1 {
            self.next_position(position)?
                .map(|position| self.layout_frame_at(position))
                .transpose()?
        } else {
            None
        };
        let (primary_offset_x, secondary_offset_x) = resolve_frame_spread_offsets(
            &primary,
            secondary.as_deref(),
            default_secondary_offset_x,
            self.style.column_gap == 0.0,
        );
        Ok(ReaderFrameSpread {
            primary,
            secondary,
            primary_offset_x,
            secondary_offset_x,
        })
    }

    /// Returns the logical pages visible in the current reader viewport.
    /// Adjacent content is resolved across layout-segment and spine-section
    /// boundaries so those implementation boundaries never create a blank
    /// right page.
    #[cfg(feature = "legacy-renderer")]
    pub fn current_spread(&mut self) -> Result<ReaderSpread, ReaderError> {
        let position = self.current_position();
        let frames = self.current_frame_spread()?;
        let primary = self.page_at(position)?;
        let secondary = if frames.secondary.is_some() {
            self.next_position(position)?
                .map(|position| self.page_at(position))
                .transpose()?
        } else {
            None
        };
        Ok(ReaderSpread {
            primary,
            secondary,
            primary_offset_x: frames.primary_offset_x,
            secondary_offset_x: frames.secondary_offset_x,
        })
    }

    /// Compiles and returns every logical page in the active authored section.
    /// Frames are retained by the returned `Arc`s even when the segment LRU
    /// later evicts their owning compiled segment.
    pub fn current_section_frames(&mut self) -> Result<Vec<ReaderSectionFrame>, ReaderError> {
        self.section_frames(self.current_section)
    }

    /// Compiles and returns every legacy display list in the active authored
    /// section.
    /// Pages are retained by the returned `Arc`s even when the segment LRU later
    /// evicts their owning compiled segment.
    #[cfg(feature = "legacy-renderer")]
    pub fn current_section_pages(&mut self) -> Result<Vec<ReaderSectionPage>, ReaderError> {
        self.section_pages(self.current_section)
    }

    /// Returns the renderer-independent frames intersecting the active
    /// semantic table-of-contents unit.
    pub fn current_reading_unit_frames(&mut self) -> Result<Vec<ReaderSectionFrame>, ReaderError> {
        if let Some(unit) = self
            .fixed_reading_units
            .as_ref()
            .and_then(|units| units.get(self.current_reading_unit))
            .cloned()
        {
            let mut frames = Vec::new();
            for section_index in unit.section_range {
                if section_index == self.current_section {
                    frames.extend(self.section_frames(section_index)?);
                    continue;
                }
                let Some(dimensions) = self.source.fixed_page_dimensions(section_index)? else {
                    frames.extend(self.section_frames(section_index)?);
                    continue;
                };
                if let Some(cached) = self.cached_fixed_section_frames(section_index) {
                    frames.extend(cached);
                } else {
                    let layout = self.layout_engine.layout_fixed_page_placeholder(
                        dimensions,
                        self.viewport,
                        &self.style,
                    );
                    frames.extend(layout.into_frames().into_iter().enumerate().map(
                        |(page_index, frame)| ReaderSectionFrame {
                            position: ReaderPosition {
                                section_index,
                                segment_index: 0,
                                page_index,
                            },
                            frame: Arc::new(frame),
                            placeholder: true,
                            visible_top: None,
                            visible_bottom: None,
                        },
                    ));
                }
            }
            return Ok(frames);
        }
        let section = self.repository.load(self.current_section)?;
        let Some(unit) = section
            .reading_units
            .get(self.current_reading_unit)
            .or_else(|| section.reading_units.first())
        else {
            return self.section_frames(self.current_section);
        };
        let ranges = section.fragments[unit.fragment_range.clone()]
            .iter()
            .flat_map(|fragment| &fragment.blocks)
            .filter_map(block_source)
            .cloned()
            .collect::<Vec<_>>();
        let mut frames = self.section_frames(self.current_section)?;
        let visible = frames
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .frame
                    .source_content_bounds(&ranges)
                    .map(|bounds| (index, bounds.y0, bounds.y1))
            })
            .collect::<Vec<_>>();
        let (Some((first, first_top, _)), Some((last, _, last_bottom))) =
            (visible.first().copied(), visible.last().copied())
        else {
            return Ok(frames);
        };
        frames = frames.drain(first..=last).collect();
        if let Some(frame) = frames.first_mut() {
            frame.visible_top = Some(first_top);
        }
        if let Some(frame) = frames.last_mut() {
            frame.visible_bottom = Some(last_bottom);
        }
        Ok(frames)
    }

    /// Composes the active semantic reading unit into one renderer-independent
    /// vertical surface for scroll and focus presentations.
    pub fn current_scroll_layout(
        &mut self,
        preserve_physical_pages: bool,
    ) -> Result<ReaderScrollLayout, ReaderError> {
        let section_index = self.current_section;
        let reading_unit_index = self.current_reading_unit;
        let frames = self.current_reading_unit_frames()?;
        Ok(ReaderScrollLayout::new(
            section_index,
            reading_unit_index,
            frames,
            preserve_physical_pages,
        ))
    }

    /// Resolves every footnote referenced by one semantic focus candidate.
    /// Linked targets remain source-backed and unresolved content is retained as
    /// `None` rather than replaced with frontend-localized copy.
    #[must_use]
    pub fn resolve_focus_candidate_footnotes(
        &self,
        section: &Section,
        candidate: &ReaderFocusCandidate,
    ) -> Vec<ReaderFocusFootnote> {
        self.resolve_focus_footnotes_from_blocks(
            candidate
                .block_indices
                .iter()
                .filter_map(|index| section.blocks.get(*index)),
        )
    }

    fn resolve_focus_footnotes_from_blocks<'a>(
        &self,
        blocks: impl IntoIterator<Item = &'a Block>,
    ) -> Vec<ReaderFocusFootnote> {
        let mut seen = HashSet::new();
        blocks
            .into_iter()
            .flat_map(focus_block_footnote_sources)
            .filter_map(|source| match source {
                ReaderFocusFootnoteSource::Inline(text) => seen
                    .insert(format!("inline:{text}"))
                    .then_some(ReaderFocusFootnote {
                        marker: None,
                        target: None,
                        text: Some(text),
                    }),
                ReaderFocusFootnoteSource::Reference { marker, target } => seen
                    .insert(target.to_string())
                    .then(|| ReaderFocusFootnote {
                        text: self.focus_footnote_text(&target, &marker),
                        marker: Some(marker),
                        target: Some(target),
                    }),
            })
            .collect()
    }

    fn focus_footnote_text(&self, target: &PublicationUrl, marker: &str) -> Option<String> {
        let section_index = self
            .source
            .book()
            .sections
            .iter()
            .position(|section| section.href.path() == target.path())?;
        let section = self.source.parse_section(section_index).ok()?;
        let fragment = target.fragment()?;
        let anchor = section
            .anchors
            .iter()
            .find(|anchor| anchor.fragment == fragment)?
            .source
            .clone();
        let block = section.blocks.iter().find(|block| {
            focus_block_source_range(block).is_some_and(|range| {
                source_range_contains_anchor(range, &anchor)
                    || focus_block_paint_ranges(block, range)
                        .iter()
                        .any(|range| source_range_contains_anchor(range, &anchor))
            })
        })?;
        let text = focus_block_text(block);
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let without_marker = text.strip_prefix(marker.trim()).map_or(text, |rest| {
            rest.trim_start_matches(|character: char| {
                character.is_whitespace()
                    || matches!(character, '.' | '．' | '、' | ')' | '）' | ']' | '】')
            })
        });
        Some(without_marker.to_owned())
    }

    /// Compiles the active Reading IR into immutable focus units and attaches
    /// their geometry from an already composed scroll layout.
    ///
    /// This keeps semantic inclusion and source restoration in the reader while
    /// allowing egui and GPUI to own only input, animation, and final paint.
    pub fn current_focus_layout(
        &mut self,
        scroll_layout: &ReaderScrollLayout,
        anchor: Option<&SourceAnchor>,
        policy: ReaderFocusLayoutPolicy,
        mut is_structured: impl FnMut(&SourceRange) -> bool,
    ) -> Result<ReaderFocusLayout, ReaderError> {
        let reading_ranges = self.current_reading_unit_source_ranges()?;
        let mut section_indices = Vec::new();
        for frame in scroll_layout.frames() {
            if section_indices.last().copied() != Some(frame.position.section_index) {
                section_indices.push(frame.position.section_index);
            }
        }
        if section_indices.is_empty() {
            section_indices.push(self.current_section);
        }

        let mut units = Vec::new();
        let mut first_unit_after_anchor = None;
        for section_index in section_indices {
            let section = self.repository.load(section_index)?;
            let blocks = section
                .fragments
                .iter()
                .flat_map(|fragment| fragment.blocks.iter())
                .collect::<Vec<_>>();
            let candidates = compile_focus_candidates_from_blocks(
                blocks.iter().copied(),
                &reading_ranges,
                &mut is_structured,
            );
            let focus_candidate_index =
                focus_anchor_block_index_from_blocks(blocks.iter().copied(), anchor).and_then(
                    |block_index| {
                        candidates.iter().position(|candidate| {
                            candidate
                                .block_indices
                                .last()
                                .is_some_and(|last| *last >= block_index)
                        })
                    },
                );

            for (candidate_index, candidate) in candidates.into_iter().enumerate() {
                let footnotes = self.resolve_focus_footnotes_from_blocks(
                    candidate
                        .block_indices
                        .iter()
                        .filter_map(|index| blocks.get(*index).copied()),
                );
                let activation_ranges = candidate
                    .activation
                    .is_rectangular()
                    .then_some(candidate.paint_ranges.as_slice());
                let Some(mut geometry) =
                    scroll_layout.focus_geometry(&candidate.geometry_ranges, activation_ranges)
                else {
                    continue;
                };
                if candidate.content_kind == ReaderFocusContentKind::Table {
                    geometry.bounds.y1 += policy.table_bottom_margin;
                }
                if first_unit_after_anchor.is_none()
                    && focus_candidate_index.is_some_and(|target| candidate_index >= target)
                {
                    first_unit_after_anchor = Some(units.len());
                }
                units.push(candidate.into_unit_with_footnotes(geometry, footnotes));
            }
        }

        Ok(ReaderFocusLayout {
            units,
            first_unit_after_anchor,
        })
    }

    /// Returns the pages intersecting the active semantic table-of-contents unit.
    /// The first and last page carry semantic crop bounds so continuous views do
    /// not expose neighboring units that share those physical pages.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "renderer page coordinates are viewport-bounded f32 values stored in kurbo f64"
    )]
    #[cfg(feature = "legacy-renderer")]
    pub fn current_reading_unit_pages(&mut self) -> Result<Vec<ReaderSectionPage>, ReaderError> {
        if let Some(unit) = self
            .fixed_reading_units
            .as_ref()
            .and_then(|units| units.get(self.current_reading_unit))
            .cloned()
        {
            let mut pages = Vec::new();
            for section_index in unit.section_range {
                if section_index == self.current_section {
                    pages.extend(self.section_pages(section_index)?);
                    continue;
                }
                let Some(dimensions) = self.source.fixed_page_dimensions(section_index)? else {
                    pages.extend(self.section_pages(section_index)?);
                    continue;
                };
                if let Some(cached) = self.cached_fixed_section_pages(section_index) {
                    pages.extend(cached);
                } else {
                    let layout = self.layout_engine.layout_fixed_page_placeholder(
                        dimensions,
                        self.viewport,
                        &self.style,
                    );
                    pages.extend(layout.pages.iter().enumerate().map(|(page_index, page)| {
                        ReaderSectionPage {
                            position: ReaderPosition {
                                section_index,
                                segment_index: 0,
                                page_index,
                            },
                            page: Arc::new(self.display_compiler.compile(page)),
                            placeholder: true,
                            visible_top: None,
                            visible_bottom: None,
                        }
                    }));
                }
            }
            return Ok(pages);
        }
        let section = self.repository.load(self.current_section)?;
        let Some(unit) = section
            .reading_units
            .get(self.current_reading_unit)
            .or_else(|| section.reading_units.first())
        else {
            return self.section_pages(self.current_section);
        };
        let ranges = section.fragments[unit.fragment_range.clone()]
            .iter()
            .flat_map(|fragment| &fragment.blocks)
            .filter_map(block_source)
            .cloned()
            .collect::<Vec<_>>();
        let mut pages = self.section_pages(self.current_section)?;
        let visible = pages
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .page
                    .source_content_bounds(&ranges)
                    .map(|bounds| (index, bounds.y0 as f32, bounds.y1 as f32))
            })
            .collect::<Vec<_>>();
        let (Some((first, first_top, _)), Some((last, _, last_bottom))) =
            (visible.first().copied(), visible.last().copied())
        else {
            return Ok(pages);
        };
        pages = pages.drain(first..=last).collect();
        if let Some(page) = pages.first_mut() {
            page.visible_top = Some(first_top);
        }
        if let Some(page) = pages.last_mut() {
            page.visible_bottom = Some(last_bottom);
        }
        Ok(pages)
    }

    /// Durable block ranges belonging to the active semantic TOC unit.
    pub fn current_reading_unit_source_ranges(&mut self) -> Result<Vec<SourceRange>, ReaderError> {
        if let Some(unit) = self
            .fixed_reading_units
            .as_ref()
            .and_then(|units| units.get(self.current_reading_unit))
            .cloned()
        {
            let mut ranges = Vec::new();
            for section_index in unit.section_range {
                let section = self.repository.load(section_index)?;
                ranges.extend(
                    section
                        .fragments
                        .iter()
                        .flat_map(|fragment| &fragment.blocks)
                        .filter_map(block_source)
                        .cloned(),
                );
            }
            return Ok(ranges);
        }
        let section = self.repository.load(self.current_section)?;
        let Some(unit) = section
            .reading_units
            .get(self.current_reading_unit)
            .or_else(|| section.reading_units.first())
        else {
            return Ok(Vec::new());
        };
        Ok(section.fragments[unit.fragment_range.clone()]
            .iter()
            .flat_map(|fragment| &fragment.blocks)
            .filter_map(block_source)
            .cloned()
            .collect())
    }

    pub fn reading_unit_location(&mut self) -> ReadingUnitLocation {
        if let Some(units) = &self.fixed_reading_units {
            let count = units.len().max(1);
            return ReadingUnitLocation {
                index: self.current_reading_unit.min(count - 1),
                count,
            };
        }
        let count = self
            .repository
            .load(self.current_section)
            .map_or(1, |section| section.reading_units.len().max(1));
        ReadingUnitLocation {
            index: self.current_reading_unit.min(count - 1),
            count,
        }
    }

    pub fn current_reading_unit_anchor(&self) -> Option<SourceAnchor> {
        if self.fixed_reading_units.is_some() {
            return None;
        }
        self.repository
            .get(self.current_section)?
            .reading_units
            .get(self.current_reading_unit)?
            .start
            .clone()
    }

    /// Navigates between semantic TOC units, crossing a spine boundary only
    /// after the active section's first or last unit.
    pub fn go_to_adjacent_reading_unit(
        &mut self,
        direction: PageDirection,
    ) -> Result<NavigationResult, ReaderError> {
        if let Some(units) = &self.fixed_reading_units {
            let target = match direction {
                PageDirection::Previous => self.current_reading_unit.checked_sub(1),
                PageDirection::Next => (self.current_reading_unit + 1 < units.len())
                    .then_some(self.current_reading_unit + 1),
            };
            let Some(unit_index) = target else {
                return Ok(self.boundary());
            };
            let section_index = units[unit_index].section_range.start;
            let result = self.go_to_section(section_index)?;
            self.current_reading_unit = unit_index;
            return Ok(result);
        }
        let count = self
            .repository
            .load(self.current_section)?
            .reading_units
            .len();
        let target = match direction {
            PageDirection::Previous if self.current_reading_unit > 0 => {
                Some((self.current_section, self.current_reading_unit - 1))
            }
            PageDirection::Next if self.current_reading_unit + 1 < count => {
                Some((self.current_section, self.current_reading_unit + 1))
            }
            PageDirection::Previous => {
                if let Some(section) = self.current_section.checked_sub(1) {
                    let count = self.repository.load(section)?.reading_units.len().max(1);
                    Some((section, count - 1))
                } else {
                    None
                }
            }
            PageDirection::Next => (self.current_section + 1 < self.section_count())
                .then_some((self.current_section + 1, 0)),
        };
        let Some((section_index, unit_index)) = target else {
            return Ok(self.boundary());
        };
        self.go_to_reading_unit(section_index, unit_index)
    }

    fn section_frames(
        &mut self,
        section_index: usize,
    ) -> Result<Vec<ReaderSectionFrame>, ReaderError> {
        self.poll_prefetch()?;
        let segment_count = self.repository.load(section_index)?.segments.len();
        let mut frames = Vec::new();
        for segment_index in 0..segment_count {
            let key = SegmentKey {
                section_index,
                segment_index,
            };
            self.ensure_segment(key)?;
            let segment_frames = self
                .cache
                .get(&key)
                .ok_or(ReaderError::SegmentOutOfBounds {
                    section: section_index,
                    segment: segment_index,
                })?
                .frames
                .clone();
            frames.extend(
                segment_frames
                    .into_iter()
                    .enumerate()
                    .map(|(page_index, frame)| ReaderSectionFrame {
                        position: ReaderPosition {
                            section_index,
                            segment_index,
                            page_index,
                        },
                        frame,
                        placeholder: false,
                        visible_top: None,
                        visible_bottom: None,
                    }),
            );
        }
        Ok(frames)
    }

    fn cached_fixed_section_frames(&self, section_index: usize) -> Option<Vec<ReaderSectionFrame>> {
        let key = SegmentKey {
            section_index,
            segment_index: 0,
        };
        let segment = self.cache.get(&key)?;
        Some(
            segment
                .frames
                .iter()
                .enumerate()
                .map(|(page_index, frame)| ReaderSectionFrame {
                    position: ReaderPosition {
                        section_index,
                        segment_index: 0,
                        page_index,
                    },
                    frame: Arc::clone(frame),
                    placeholder: false,
                    visible_top: None,
                    visible_bottom: None,
                })
                .collect(),
        )
    }

    #[cfg(feature = "legacy-renderer")]
    fn section_pages(
        &mut self,
        section_index: usize,
    ) -> Result<Vec<ReaderSectionPage>, ReaderError> {
        self.poll_prefetch()?;
        let segment_count = self.repository.load(section_index)?.segments.len();
        let mut pages = Vec::new();
        for segment_index in 0..segment_count {
            let key = SegmentKey {
                section_index,
                segment_index,
            };
            self.ensure_segment(key)?;
            let segment_pages = self
                .cache
                .get(&key)
                .ok_or(ReaderError::SegmentOutOfBounds {
                    section: section_index,
                    segment: segment_index,
                })?
                .pages
                .clone();
            pages.extend(
                segment_pages
                    .into_iter()
                    .enumerate()
                    .map(|(page_index, page)| ReaderSectionPage {
                        position: ReaderPosition {
                            section_index,
                            segment_index,
                            page_index,
                        },
                        page,
                        placeholder: false,
                        visible_top: None,
                        visible_bottom: None,
                    }),
            );
        }
        Ok(pages)
    }

    #[cfg(feature = "legacy-renderer")]
    fn cached_fixed_section_pages(&self, section_index: usize) -> Option<Vec<ReaderSectionPage>> {
        let key = SegmentKey {
            section_index,
            segment_index: 0,
        };
        let segment = self.cache.get(&key)?;
        Some(
            segment
                .pages
                .iter()
                .enumerate()
                .map(|(page_index, page)| ReaderSectionPage {
                    position: ReaderPosition {
                        section_index,
                        segment_index: 0,
                        page_index,
                    },
                    page: Arc::clone(page),
                    placeholder: false,
                    visible_top: None,
                    visible_bottom: None,
                })
                .collect(),
        )
    }

    /// Returns the logical positions currently composed into the fixed-page
    /// spread, in visual order from left to right.
    pub fn current_spread_positions(&mut self) -> Result<Vec<ReaderPosition>, ReaderError> {
        Ok(self
            .current_spread_frames()?
            .into_iter()
            .map(|(position, _, _)| position)
            .collect())
    }

    /// Updates the durable reading position to the first page visible in a
    /// continuous section view without changing layout or authored section.
    pub fn set_visible_position(
        &mut self,
        position: ReaderPosition,
    ) -> Result<ReaderSnapshot, ReaderError> {
        let section_is_visible = position.section_index == self.current_section
            || self
                .fixed_reading_units
                .as_ref()
                .and_then(|units| units.get(self.current_reading_unit))
                .is_some_and(|unit| unit.section_range.contains(&position.section_index));
        if !section_is_visible {
            return Err(ReaderError::SectionOutOfBounds(position.section_index));
        }
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        self.ensure_segment(key)?;
        let page_exists = self
            .cache
            .get(&key)
            .is_some_and(|segment| position.page_index < segment.frames.len());
        if !page_exists {
            return Err(ReaderError::PageOutOfBounds(position));
        }
        self.install_position(position);
        self.sync_reading_unit_to_position();
        Ok(self.snapshot())
    }

    /// Polls or queues the compiled segment for a fixed-page placeholder
    /// without ever parsing, laying out, or waiting on the caller thread.
    pub fn try_materialize_position(
        &mut self,
        position: ReaderPosition,
    ) -> Result<bool, ReaderError> {
        self.poll_prefetch()?;
        let section_is_visible = position.section_index == self.current_section
            || self
                .fixed_reading_units
                .as_ref()
                .and_then(|units| units.get(self.current_reading_unit))
                .is_some_and(|unit| unit.section_range.contains(&position.section_index));
        if !section_is_visible {
            return Err(ReaderError::SectionOutOfBounds(position.section_index));
        }
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        if !self.try_ensure_segment(key)? {
            return Ok(false);
        }
        Ok(self
            .cache
            .get(&key)
            .is_some_and(|segment| position.page_index < segment.frames.len()))
    }

    pub fn hit_test_page(
        &mut self,
        position: ReaderPosition,
        x: f32,
        y: f32,
        exact: bool,
    ) -> Result<Option<ReaderTextHit>, ReaderError> {
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        self.ensure_segment(key)?;
        Ok(self
            .layout_frame_at(position)?
            .interaction()
            .hit_test_text(x, y, exact)
            .map(|hit| reader_text_hit(position, hit)))
    }

    pub fn image_at_page(
        &mut self,
        position: ReaderPosition,
        x: f32,
        y: f32,
    ) -> Result<Option<ReaderImage>, ReaderError> {
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        self.ensure_segment(key)?;
        let frame = self.layout_frame_at(position)?;
        Ok(frame_image_at(frame.as_ref(), x, y).map(|image| reader_image(position, image, 0.0)))
    }

    pub fn source_ranges_contain_point_on_page(
        &mut self,
        position: ReaderPosition,
        ranges: &[SourceRange],
        x: f32,
        y: f32,
    ) -> Result<bool, ReaderError> {
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        self.ensure_segment(key)?;
        Ok(self
            .layout_frame_at(position)?
            .interaction()
            .source_rects(ranges)
            .iter()
            .any(|rect| rect.contains(x, y)))
    }

    /// Returns the authored spine sections represented by the currently
    /// visible logical pages, in visual order and without duplicates.
    pub fn current_spread_section_indices(&mut self) -> Result<Vec<usize>, ReaderError> {
        let mut indices = Vec::with_capacity(self.current_visible_pages());
        for (position, _, _) in self.current_spread_frames()? {
            if indices.last().copied() != Some(position.section_index) {
                indices.push(position.section_index);
            }
        }
        Ok(indices)
    }

    /// Returns the source-backed text actually retained on the currently
    /// visible logical pages. In double-page mode this includes both pages in
    /// visual order and excludes text outside the displayed spread.
    pub fn current_visible_text_fragments(
        &mut self,
    ) -> Result<Vec<ReaderVisibleTextFragment>, ReaderError> {
        let mut fragments = Vec::new();
        for (position, _, _) in self.current_spread_frames()? {
            let frame = self.layout_frame_at(position)?;
            append_visible_text_fragments(&mut fragments, position, frame.interaction());
        }
        Ok(fragments)
    }

    /// Returns source-backed text retained on the requested cached logical
    /// pages. This lets continuous-scroll consumers describe their actual
    /// viewport instead of falling back to the session's leading page.
    pub fn visible_text_fragments_for_pages(
        &self,
        positions: &[ReaderPosition],
    ) -> Result<Vec<ReaderVisibleTextFragment>, ReaderError> {
        let mut fragments = Vec::new();
        for &position in positions {
            let frame = self.layout_frame_at(position)?;
            append_visible_text_fragments(&mut fragments, position, frame.interaction());
        }
        Ok(fragments)
    }

    /// Resolves a canvas point against the currently visible logical pages.
    /// Exact hits start a selection; nearest hits extend an active drag through
    /// whitespace in the same page or across a two-page spread.
    pub fn hit_test_current_spread(
        &mut self,
        x: f32,
        y: f32,
        exact: bool,
    ) -> Result<Option<ReaderTextHit>, ReaderError> {
        let pages = self.current_spread_frames()?;
        if pages.is_empty() {
            return Ok(None);
        }
        if exact {
            for (position, _, offset_x) in &pages {
                if let Some(hit) = self
                    .layout_frame_at(*position)?
                    .interaction()
                    .hit_test_text(x - *offset_x, y, true)
                {
                    return Ok(Some(reader_text_hit(*position, hit)));
                }
            }
            return Ok(None);
        }
        let page_index = usize::from(pages.len() > 1 && x >= pages[1].2);
        let (position, _, offset_x) = &pages[page_index];
        Ok(self
            .layout_frame_at(*position)?
            .interaction()
            .hit_test_text(x - *offset_x, y, false)
            .map(|hit| reader_text_hit(*position, hit)))
    }

    /// Returns the first and last logical carets in the currently visible
    /// spread. These are generation-local values and must not be persisted.
    pub fn current_spread_cursor_range(
        &mut self,
    ) -> Result<Option<(ReaderTextCursor, ReaderTextCursor)>, ReaderError> {
        let pages = self.current_spread_frames()?;
        let first = pages.iter().find_map(|(position, frame, _)| {
            frame
                .interaction()
                .first_cursor()
                .map(|cursor| ReaderTextCursor {
                    position: *position,
                    cursor,
                })
        });
        let last = pages.iter().rev().find_map(|(position, frame, _)| {
            frame
                .interaction()
                .last_cursor()
                .map(|cursor| ReaderTextCursor {
                    position: *position,
                    cursor,
                })
        });
        Ok(first.zip(last))
    }

    /// Moves a logical caret by one shaped cluster within the currently
    /// visible spread, crossing its page boundary when necessary.
    pub fn move_text_cursor_current_spread(
        &mut self,
        cursor: ReaderTextCursor,
        direction: TextCursorDirection,
    ) -> Result<Option<ReaderTextCursor>, ReaderError> {
        let pages = self.current_spread_frames()?;
        let Some(page_index) = pages
            .iter()
            .position(|(position, _, _)| *position == cursor.position)
        else {
            return Ok(None);
        };
        let local = match direction {
            TextCursorDirection::Previous => pages[page_index]
                .1
                .interaction()
                .previous_cursor(cursor.cursor),
            TextCursorDirection::Next => {
                pages[page_index].1.interaction().next_cursor(cursor.cursor)
            }
        };
        if let Some(local) = local {
            return Ok(Some(ReaderTextCursor {
                position: cursor.position,
                cursor: local,
            }));
        }
        let adjacent = match direction {
            TextCursorDirection::Previous => {
                pages[..page_index]
                    .iter()
                    .rev()
                    .find_map(|(position, frame, _)| {
                        frame
                            .interaction()
                            .last_cursor()
                            .map(|cursor| ReaderTextCursor {
                                position: *position,
                                cursor,
                            })
                    })
            }
            TextCursorDirection::Next => {
                pages[page_index + 1..]
                    .iter()
                    .find_map(|(position, frame, _)| {
                        frame
                            .interaction()
                            .first_cursor()
                            .map(|cursor| ReaderTextCursor {
                                position: *position,
                                cursor,
                            })
                    })
            }
        };
        Ok(adjacent)
    }

    /// Resolves durable source ranges to fresh generation-local carets in the
    /// currently visible spread. This is the required restoration path after
    /// repagination.
    pub fn cursors_for_current_spread_source_ranges(
        &mut self,
        ranges: &[SourceRange],
    ) -> Result<Option<(ReaderTextCursor, ReaderTextCursor)>, ReaderError> {
        if ranges.is_empty() {
            return Ok(None);
        }
        let pages = self.current_spread_frames()?;
        let mut first = None;
        let mut last = None;
        for (position, frame, _) in pages {
            if let Some((start, end)) = frame.interaction().cursors_for_source_ranges(ranges) {
                first.get_or_insert(ReaderTextCursor {
                    position,
                    cursor: start,
                });
                last = Some(ReaderTextCursor {
                    position,
                    cursor: end,
                });
            }
        }
        Ok(first.zip(last))
    }

    /// Resolves the top-most raster image under a visible spread coordinate.
    pub fn image_at_current_spread(
        &mut self,
        x: f32,
        y: f32,
    ) -> Result<Option<ReaderImage>, ReaderError> {
        Ok(self
            .current_spread_frames()?
            .iter()
            .rev()
            .find_map(|(position, frame, offset_x)| {
                frame_image_at(frame, x - *offset_x, y)
                    .map(|image| reader_image(*position, image, *offset_x))
            }))
    }

    /// Builds a source-backed selection between two generation-local carets.
    /// The returned ranges are durable even though the carets are not.
    pub fn selection_between_cursors(
        &mut self,
        anchor: ReaderTextCursor,
        focus: ReaderTextCursor,
    ) -> Result<Option<ReaderSelection>, ReaderError> {
        self.selection_between(
            &reader_text_hit_for_cursor(anchor),
            &reader_text_hit_for_cursor(focus),
        )
    }

    /// Builds a source-backed selection between two pointer hits. Native
    /// The returned per-block ranges remain stable after repagination. A
    /// continuous section view may select across multiple logical pages.
    pub fn selection_between(
        &mut self,
        anchor: &ReaderTextHit,
        focus: &ReaderTextHit,
    ) -> Result<Option<ReaderSelection>, ReaderError> {
        self.selection_between_with_granularity(anchor, focus, SelectionGranularity::Free)
    }

    /// Builds a source-backed selection whose endpoints expand to semantic
    /// text units. Word, sentence, and paragraph ranges may continue across
    /// logical page boundaries within the current authored section.
    pub fn selection_between_with_granularity(
        &mut self,
        anchor: &ReaderTextHit,
        focus: &ReaderTextHit,
        granularity: SelectionGranularity,
    ) -> Result<Option<ReaderSelection>, ReaderError> {
        let pages = if granularity == SelectionGranularity::Free
            || anchor.position.section_index != focus.position.section_index
        {
            self.selection_pages(anchor, focus)?
        } else {
            let visible_offsets = self.current_spread_frames()?;
            let entries = self.section_frames(anchor.position.section_index)?;
            let mut frames = Vec::with_capacity(entries.len());
            for entry in entries {
                let offset_x = visible_offsets
                    .iter()
                    .find(|(position, _, _)| *position == entry.position)
                    .map_or(0.0, |(_, _, offset_x)| *offset_x);
                frames.push((
                    entry.position,
                    self.layout_frame_at(entry.position)?,
                    offset_x,
                ));
            }
            frames
        };
        let Some(anchor_page) = pages
            .iter()
            .position(|(position, _, _)| *position == anchor.position)
        else {
            return Ok(None);
        };
        let Some(focus_page) = pages
            .iter()
            .position(|(position, _, _)| *position == focus.position)
        else {
            return Ok(None);
        };
        let anchor_order = (anchor_page, anchor.region_index, anchor.byte_index);
        let focus_order = (focus_page, focus.region_index, focus.byte_index);
        let (start_hit, end_hit, mut start, mut end) = if anchor_order <= focus_order {
            (
                anchor,
                focus,
                SelectionBoundary::new(anchor_page, anchor.region_index, anchor.cluster_start),
                SelectionBoundary::new(focus_page, focus.region_index, focus.byte_index),
            )
        } else {
            (
                focus,
                anchor,
                SelectionBoundary::new(focus_page, focus.region_index, focus.byte_index),
                SelectionBoundary::new(anchor_page, anchor.region_index, anchor.cluster_end),
            )
        };
        if granularity != SelectionGranularity::Free {
            let start_range = semantic_source_range(
                pages[start.page].1.interaction(),
                start_hit,
                granularity,
                false,
            );
            let end_range =
                semantic_source_range(pages[end.page].1.interaction(), end_hit, granularity, false);
            let (Some(start_range), Some(end_range)) = (start_range, end_range) else {
                return Ok(None);
            };
            let end_range = if granularity == SelectionGranularity::Word && start_range != end_range
            {
                semantic_source_range(pages[end.page].1.interaction(), end_hit, granularity, true)
                    .unwrap_or(end_range)
            } else {
                end_range
            };
            let start_range = if granularity == SelectionGranularity::Paragraph {
                expand_paragraph_source_range(&pages, start_range)
            } else {
                start_range
            };
            let end_range = if granularity == SelectionGranularity::Paragraph {
                expand_paragraph_source_range(&pages, end_range)
            } else {
                end_range
            };
            let Some(expanded_start) = first_source_boundary(&pages, &start_range) else {
                return Ok(None);
            };
            let Some(expanded_end) = last_source_boundary(&pages, &end_range) else {
                return Ok(None);
            };
            start = expanded_start;
            end = expanded_end;
        }
        Ok(build_reader_selection(&pages, start, end))
    }

    /// Returns whether a canvas point falls inside the resolved geometry for a
    /// set of durable source ranges on the current spread.
    pub fn source_ranges_contain_point(
        &mut self,
        ranges: &[SourceRange],
        x: f32,
        y: f32,
    ) -> Result<bool, ReaderError> {
        for (position, _, offset_x) in self.current_spread_frames()? {
            if self
                .layout_frame_at(position)?
                .interaction()
                .source_rects(ranges)
                .iter()
                .any(|rect| rect.contains(x - offset_x, y))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Navigates to a durable source anchor, resolving its page again under
    /// the current viewport and reader style.
    pub fn go_to_source(&mut self, anchor: &SourceAnchor) -> Result<NavigationResult, ReaderError> {
        let position = self.position_for_source_anchor(anchor)?;
        self.install_position(position);
        self.sync_reading_unit_to_anchor(anchor);
        Ok(self.moved())
    }

    /// Resolves a publication URL to its containing spine section.
    pub fn section_index_for_href(&self, href: &PublicationUrl) -> Option<usize> {
        self.section_indices_by_path.get(href.path()).copied()
    }

    /// Resolves a publication URL to the layout segment and page containing its
    /// authored anchor.
    ///
    /// A missing or unknown fragment falls back to the beginning of the section,
    /// matching [`Self::go_to_href`]. Page indexes are available for compiled
    /// segments; the current segment is always compiled.
    pub fn position_for_href(&self, href: &PublicationUrl) -> Option<ReaderPosition> {
        let section_index = self.section_index_for_href(href)?;
        let section = self.repository.get(section_index);
        let segment_index = href
            .fragment()
            .and_then(|fragment| {
                section
                    .as_ref()
                    .and_then(|section| section.anchor_segments.get(fragment))
            })
            .copied()
            .unwrap_or(0);
        let key = SegmentKey {
            section_index,
            segment_index,
        };
        let page_index = href
            .fragment()
            .and_then(|fragment| {
                self.cache
                    .get(&key)
                    .and_then(|cached| cached.anchor_pages.get(fragment))
            })
            .copied()
            .unwrap_or(0);
        Some(ReaderPosition {
            section_index,
            segment_index,
            page_index,
        })
    }

    /// Resolves an authored URL fragment to its exact durable source anchor.
    ///
    /// Unlike [`Self::position_for_href`], this keeps paragraph-level precision
    /// when multiple contents entries share one laid-out page.
    pub fn source_anchor_for_href(&self, href: &PublicationUrl) -> Option<SourceAnchor> {
        let section_index = self.section_index_for_href(href)?;
        let fragment = href.fragment()?;
        self.repository
            .get(section_index)?
            .fragments
            .iter()
            .flat_map(|content| &content.anchors)
            .find(|anchor| anchor.fragment == fragment)
            .map(|anchor| anchor.source.clone())
    }

    /// Navigates to the beginning of a spine section.
    pub fn go_to_section(&mut self, index: usize) -> Result<NavigationResult, ReaderError> {
        self.poll_prefetch()?;
        if index >= self.source.book().sections.len() {
            return Err(ReaderError::SectionOutOfBounds(index));
        }
        let key = SegmentKey {
            section_index: index,
            segment_index: 0,
        };
        self.ensure_segment(key)?;
        self.current_section = index;
        self.current_segment = 0;
        self.current_page = 0;
        self.sync_reading_unit_to_position();
        self.touch(key);
        Ok(self.moved())
    }

    /// Navigates a TOC or link target to its authored anchor when available.
    pub fn go_to_href(&mut self, href: &PublicationUrl) -> Result<NavigationResult, ReaderError> {
        let index = self
            .section_index_for_href(href)
            .ok_or_else(|| ReaderError::NavigationTargetNotFound(href.to_string()))?;
        let section = self.repository.load(index)?;
        let segment_index = href
            .fragment()
            .and_then(|fragment| section.anchor_segments.get(fragment))
            .copied()
            .unwrap_or(0);
        let key = SegmentKey {
            section_index: index,
            segment_index,
        };
        self.ensure_segment(key)?;
        self.current_section = index;
        self.current_segment = segment_index;
        self.current_page = href
            .fragment()
            .and_then(|fragment| {
                self.cache
                    .get(&key)
                    .and_then(|cached| cached.anchor_pages.get(fragment))
            })
            .copied()
            .unwrap_or(0);
        if let Some(anchor) = href.fragment().and_then(|fragment| {
            section
                .fragments
                .iter()
                .flat_map(|content| &content.anchors)
                .find(|anchor| anchor.fragment == fragment)
                .map(|anchor| anchor.source.clone())
        }) {
            self.sync_reading_unit_to_anchor(&anchor);
        } else {
            self.sync_reading_unit_to_position();
        }
        self.touch(key);
        Ok(self.moved_to_toc_target(href))
    }

    /// Moves in constant time while pages are cached. Section boundaries compile
    /// only the destination section, never the previous one again.
    pub fn turn_page(&mut self, direction: PageDirection) -> Result<NavigationResult, ReaderError> {
        self.poll_prefetch()?;
        match direction {
            PageDirection::Next => self.next_page(),
            PageDirection::Previous => self.previous_page(),
        }
    }

    /// Attempts to turn a page without parsing, laying out, or waiting on the
    /// caller thread. When the destination is not cached, this queues exactly
    /// the required segment and returns [`NavigationAttempt::Pending`].
    pub fn try_turn_page(
        &mut self,
        direction: PageDirection,
    ) -> Result<NavigationAttempt, ReaderError> {
        self.poll_prefetch()?;
        match direction {
            PageDirection::Next => self.try_next_page(),
            PageDirection::Previous => self.try_previous_page(),
        }
    }

    /// Queues a small layout-segment window around the current position for background
    /// pagination and display-list compilation. Crossing forward over an
    /// authored section boundary queues the start of the following sections.
    /// This method never performs layout on the caller thread.
    pub fn prefetch_adjacent(&mut self) -> Result<(), ReaderError> {
        self.poll_prefetch()?;
        let section_count = self.source.book().sections.len();
        let segment_count = self.current_section_data().segments.len();
        // A double spread advances by two logical pages. Queue the whole next
        // spread plus the normal lookahead so a turn never leaves its second
        // page for synchronous layout on the UI thread.
        let forward_distance = PREFETCH_DISTANCE + self.current_visible_pages().saturating_sub(1);
        for distance in 1..=forward_distance {
            if let Some(segment_index) = self.current_segment.checked_add(distance)
                && segment_index < segment_count
            {
                self.queue_prefetch(SegmentKey {
                    section_index: self.current_section,
                    segment_index,
                })?;
            } else {
                let overflow = self.current_segment + distance - segment_count;
                let section_index = self.current_section + overflow + 1;
                if section_index < section_count {
                    self.queue_prefetch(SegmentKey {
                        section_index,
                        segment_index: 0,
                    })?;
                }
            }
        }
        for distance in 1..=PREFETCH_DISTANCE {
            if let Some(segment_index) = self.current_segment.checked_sub(distance) {
                self.queue_prefetch(SegmentKey {
                    section_index: self.current_section,
                    segment_index,
                })?;
            }
        }
        self.touch(self.current_key());
        Ok(())
    }

    /// Blocks until all currently queued prefetch work has been collected.
    /// Intended for diagnostics and deterministic tests, not interactive shells.
    pub fn wait_for_prefetch(&mut self) -> Result<(), ReaderError> {
        let Some(generation) = self
            .prefetch_worker
            .as_ref()
            .map(PrefetchWorker::generation)
        else {
            return Ok(());
        };
        while self
            .prefetch_inflight
            .iter()
            .any(|key| key.generation == generation)
        {
            let Some(worker) = self.prefetch_worker.as_ref() else {
                return Ok(());
            };
            let result = worker.recv()?;
            self.install_prefetch(result);
        }
        if let Some(key) = self.prefetch_failures.keys().next().copied()
            && let Some(error) = self.prefetch_failures.remove(&key)
        {
            return Err(error);
        }
        Ok(())
    }

    /// Invalidates layout/display caches while preserving approximate progress
    /// inside the active section.
    pub fn resize(&mut self, viewport: LayoutViewport) -> Result<ReaderSnapshot, ReaderError> {
        if self.viewport == viewport {
            return Ok(self.snapshot());
        }
        let old_count = self.current_page_count();
        let fraction = page_fraction(self.current_page, old_count);
        self.viewport = viewport;
        self.invalidate_layout(fraction)?;
        Ok(self.snapshot())
    }

    pub fn set_style(&mut self, mut style: ReaderStyle) -> Result<ReaderSnapshot, ReaderError> {
        style.writing_system = self.source.book().metadata.writing_system();
        if self.style == style {
            return Ok(self.snapshot());
        }
        let fraction = page_fraction(self.current_page, self.current_page_count());
        self.style = style;
        self.invalidate_layout(fraction)?;
        Ok(self.snapshot())
    }

    /// Reparses the publication through its current source layer, invalidates
    /// every parsed/layout cache, and preserves approximate progress inside the
    /// active section. This is used by non-persistent document overlays such as
    /// AI-assisted block rewrites.
    pub fn refresh_source(&mut self) -> Result<ReaderSnapshot, ReaderError> {
        self.refresh_source_with_style(self.style.clone())
    }

    /// Rebuilds the active source and applies pagination-affecting style changes
    /// in one pass. This avoids compiling the current section twice when a
    /// source mode and its page geometry change together.
    pub fn refresh_source_with_style(
        &mut self,
        style: ReaderStyle,
    ) -> Result<ReaderSnapshot, ReaderError> {
        self.refresh_source_with_style_at_href(style, None)
    }

    /// Rebuilds the active source and optionally restores a destination in the
    /// new publication structure. This is used when an overlay changes the
    /// number or identity of spine sections, such as PDF OCR reflow.
    pub fn refresh_source_with_style_at_href(
        &mut self,
        mut style: ReaderStyle,
        target: Option<&PublicationUrl>,
    ) -> Result<ReaderSnapshot, ReaderError> {
        style.writing_system = self.source.book().metadata.writing_system();
        let fraction = page_fraction(self.current_page, self.current_page_count());
        let visible_source_anchor = target
            .is_none()
            .then(|| {
                self.current_layout_frame()
                    .interaction()
                    .leading_source_range()
            })
            .flatten()
            .map(|range| range.start);
        let toc_items: Arc<[TocViewItem]> =
            flatten_toc(&self.source.book().table_of_contents).into();
        let mut section_indices_by_path = HashMap::with_capacity(self.source.book().sections.len());
        for (index, section) in self.source.book().sections.iter().enumerate() {
            section_indices_by_path
                .entry(section.href.path().to_owned())
                .or_insert(index);
        }
        let toc_index = TocIndex::new(
            &toc_items,
            &section_indices_by_path,
            self.source.book().sections.len(),
        );
        let fixed_reading_units =
            build_fixed_reading_units(self.source.as_ref(), &toc_items, &section_indices_by_path);
        let repository = Arc::new(SectionRepository::new(Arc::clone(&self.source)));
        let section_index = target.map_or_else(
            || {
                self.current_section
                    .min(self.source.book().sections.len().saturating_sub(1))
            },
            |href| {
                section_indices_by_path
                    .get(href.path())
                    .copied()
                    .unwrap_or(0)
            },
        );
        let section = repository.load(section_index)?;
        let segment_index = target
            .and_then(PublicationUrl::fragment)
            .and_then(|fragment| section.anchor_segments.get(fragment))
            .copied()
            .unwrap_or_else(|| {
                self.current_segment
                    .min(section.segments.len().saturating_sub(1))
            });
        let key = SegmentKey {
            section_index,
            segment_index,
        };
        let segment = compile_segment(
            self.source.as_ref(),
            section,
            key,
            self.viewport,
            &style,
            &mut self.layout_engine,
            #[cfg(feature = "legacy-renderer")]
            &self.display_compiler,
        )?;
        let prefetch_worker = spawn_optional_prefetch_worker(
            self.prefetch_worker.is_some(),
            &self.source,
            &repository,
            &self.fonts,
        )?;
        let target_page = target
            .and_then(PublicationUrl::fragment)
            .and_then(|fragment| segment.anchor_pages.get(fragment))
            .copied()
            .or_else(|| {
                visible_source_anchor.as_ref().and_then(|anchor| {
                    segment
                        .frames
                        .iter()
                        .position(|frame| frame.interaction().contains_source_anchor(anchor))
                })
            });

        self.repository = repository;
        self.prefetch_worker = prefetch_worker;
        self.toc_items = toc_items;
        self.toc_index = toc_index;
        self.section_indices_by_path = section_indices_by_path;
        self.fixed_reading_units = fixed_reading_units;
        self.prefetch_inflight.clear();
        self.prefetch_failures.clear();
        self.cache.clear();
        self.lru.clear();
        self.style = style;
        self.current_section = section_index;
        self.current_segment = segment_index;
        self.cache.insert(key, Arc::new(segment));
        self.touch(key);
        self.current_page =
            target_page.unwrap_or_else(|| page_for_fraction(fraction, self.current_page_count()));
        self.current_reading_unit = 0;
        self.sync_reading_unit_to_position();
        Ok(self.snapshot())
    }

    /// Returns the closest authored anchor at or before the current page whose
    /// fragment starts with `prefix`.
    pub fn current_preceding_anchor(&self, prefix: &str) -> Option<String> {
        let section = self.current_section_data();
        let current_segment = section.segments.get(self.current_segment)?;
        let cached = self.cache.get(&SegmentKey {
            section_index: self.current_section,
            segment_index: self.current_segment,
        })?;
        section
            .fragments
            .iter()
            .enumerate()
            .take(current_segment.fragment_range.end)
            .flat_map(|(fragment_index, fragment)| {
                fragment
                    .anchors
                    .iter()
                    .filter(move |anchor| {
                        anchor.fragment.starts_with(prefix)
                            && (fragment_index < current_segment.fragment_range.start
                                || cached
                                    .anchor_pages
                                    .get(&anchor.fragment)
                                    .is_some_and(|page| *page <= self.current_page))
                    })
                    .map(|anchor| anchor.fragment.clone())
            })
            .next_back()
    }

    pub fn cached_segment_count(&self) -> usize {
        self.cache.len()
    }

    fn current_spread_frames(
        &mut self,
    ) -> Result<Vec<(ReaderPosition, Arc<LayoutFrame>, f32)>, ReaderError> {
        let position = self.current_position();
        let spread = self.current_frame_spread()?;
        let mut pages = vec![(position, spread.primary, spread.primary_offset_x)];
        if let Some(secondary) = spread.secondary
            && let Some(position) = self.next_position(position)?
        {
            pages.push((position, secondary, spread.secondary_offset_x));
        }
        Ok(pages)
    }

    fn selection_pages(
        &mut self,
        anchor: &ReaderTextHit,
        focus: &ReaderTextHit,
    ) -> Result<Vec<SelectionPage>, ReaderError> {
        let visible_pages = self.current_spread_frames()?;
        let mut pages = Vec::with_capacity(visible_pages.len());
        for (position, _, offset_x) in visible_pages {
            pages.push((position, self.layout_frame_at(position)?, offset_x));
        }
        let spread_contains_both = pages
            .iter()
            .any(|(position, _, _)| *position == anchor.position)
            && pages
                .iter()
                .any(|(position, _, _)| *position == focus.position);
        if spread_contains_both || anchor.position.section_index != focus.position.section_index {
            return Ok(pages);
        }
        self.current_section_frames()?
            .into_iter()
            .map(|entry| {
                self.layout_frame_at(entry.position)
                    .map(|frame| (entry.position, frame, 0.0))
            })
            .collect::<Result<Vec<_>, _>>()
    }

    fn next_page(&mut self) -> Result<NavigationResult, ReaderError> {
        let mut destination = self.current_position();
        for _ in 0..self.current_visible_pages() {
            let Some(next) = self.next_position(destination)? else {
                return Ok(self.boundary());
            };
            destination = next;
        }
        self.install_position(destination);
        Ok(self.moved())
    }

    fn previous_page(&mut self) -> Result<NavigationResult, ReaderError> {
        let original = self.current_position();
        let mut destination = original;
        for _ in 0..self.current_visible_pages() {
            let Some(previous) = self.previous_position(destination)? else {
                break;
            };
            destination = previous;
        }
        if destination == original {
            return Ok(self.boundary());
        }
        self.install_position(destination);
        Ok(self.moved())
    }

    fn try_next_page(&mut self) -> Result<NavigationAttempt, ReaderError> {
        let mut destination = self.current_position();
        for _ in 0..self.current_visible_pages() {
            match self.try_next_position(destination)? {
                PositionAttempt::Ready(Some(next)) => destination = next,
                PositionAttempt::Ready(None) => {
                    return Ok(NavigationAttempt::Ready(self.boundary()));
                }
                PositionAttempt::Pending => return Ok(NavigationAttempt::Pending),
            }
        }
        if !self.try_spread_ready_at(destination)? {
            return Ok(NavigationAttempt::Pending);
        }
        self.install_position(destination);
        Ok(NavigationAttempt::Ready(self.moved()))
    }

    fn try_previous_page(&mut self) -> Result<NavigationAttempt, ReaderError> {
        let original = self.current_position();
        let mut destination = original;
        for _ in 0..self.current_visible_pages() {
            match self.try_previous_position(destination)? {
                PositionAttempt::Ready(Some(previous)) => destination = previous,
                PositionAttempt::Ready(None) => break,
                PositionAttempt::Pending => return Ok(NavigationAttempt::Pending),
            }
        }
        if destination == original {
            return Ok(NavigationAttempt::Ready(self.boundary()));
        }
        if !self.try_spread_ready_at(destination)? {
            return Ok(NavigationAttempt::Pending);
        }
        self.install_position(destination);
        Ok(NavigationAttempt::Ready(self.moved()))
    }

    fn try_spread_ready_at(&mut self, position: ReaderPosition) -> Result<bool, ReaderError> {
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        if !self.try_ensure_segment(key)? {
            return Ok(false);
        }
        let visible_pages = self
            .cache
            .get(&key)
            .expect("ready layout segment must remain cached")
            .visible_pages;
        if visible_pages <= 1 {
            return Ok(true);
        }
        Ok(matches!(
            self.try_next_position(position)?,
            PositionAttempt::Ready(_)
        ))
    }

    fn try_next_position(
        &mut self,
        position: ReaderPosition,
    ) -> Result<PositionAttempt, ReaderError> {
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        if !self.try_ensure_segment(key)? {
            return Ok(PositionAttempt::Pending);
        }
        let segment = self
            .cache
            .get(&key)
            .expect("ready layout segment must remain cached");
        if position.page_index + 1 < segment.frames.len() {
            return Ok(PositionAttempt::Ready(Some(ReaderPosition {
                page_index: position.page_index + 1,
                ..position
            })));
        }
        let next_key = if position.segment_index + 1 < segment.section.segments.len() {
            SegmentKey {
                section_index: position.section_index,
                segment_index: position.segment_index + 1,
            }
        } else {
            let section_index = position.section_index + 1;
            if section_index >= self.source.book().sections.len() {
                return Ok(PositionAttempt::Ready(None));
            }
            SegmentKey {
                section_index,
                segment_index: 0,
            }
        };
        if !self.try_ensure_segment(next_key)? {
            return Ok(PositionAttempt::Pending);
        }
        Ok(PositionAttempt::Ready(Some(ReaderPosition {
            section_index: next_key.section_index,
            segment_index: next_key.segment_index,
            page_index: 0,
        })))
    }

    fn try_previous_position(
        &mut self,
        position: ReaderPosition,
    ) -> Result<PositionAttempt, ReaderError> {
        if position.page_index > 0 {
            return Ok(PositionAttempt::Ready(Some(ReaderPosition {
                page_index: position.page_index - 1,
                ..position
            })));
        }
        let previous_key = if let Some(segment_index) = position.segment_index.checked_sub(1) {
            SegmentKey {
                section_index: position.section_index,
                segment_index,
            }
        } else {
            let Some(section_index) = position.section_index.checked_sub(1) else {
                return Ok(PositionAttempt::Ready(None));
            };
            let first_key = SegmentKey {
                section_index,
                segment_index: 0,
            };
            if !self.try_ensure_segment(first_key)? {
                return Ok(PositionAttempt::Pending);
            }
            let segment_index = self
                .cache
                .get(&first_key)
                .expect("ready layout segment must remain cached")
                .section
                .segments
                .len()
                .saturating_sub(1);
            SegmentKey {
                section_index,
                segment_index,
            }
        };
        if !self.try_ensure_segment(previous_key)? {
            return Ok(PositionAttempt::Pending);
        }
        let page_index = self
            .cache
            .get(&previous_key)
            .expect("ready layout segment must remain cached")
            .frames
            .len()
            .saturating_sub(1);
        Ok(PositionAttempt::Ready(Some(ReaderPosition {
            section_index: previous_key.section_index,
            segment_index: previous_key.segment_index,
            page_index,
        })))
    }

    fn next_position(
        &mut self,
        position: ReaderPosition,
    ) -> Result<Option<ReaderPosition>, ReaderError> {
        let key = SegmentKey {
            section_index: position.section_index,
            segment_index: position.segment_index,
        };
        self.ensure_segment(key)?;
        let segment = self
            .cache
            .get(&key)
            .expect("ensured layout segment must remain cached");
        if position.page_index + 1 < segment.frames.len() {
            return Ok(Some(ReaderPosition {
                page_index: position.page_index + 1,
                ..position
            }));
        }
        let next_key = if position.segment_index + 1 < segment.section.segments.len() {
            SegmentKey {
                section_index: position.section_index,
                segment_index: position.segment_index + 1,
            }
        } else {
            let section_index = position.section_index + 1;
            if section_index >= self.source.book().sections.len() {
                return Ok(None);
            }
            SegmentKey {
                section_index,
                segment_index: 0,
            }
        };
        self.ensure_segment(next_key)?;
        Ok(Some(ReaderPosition {
            section_index: next_key.section_index,
            segment_index: next_key.segment_index,
            page_index: 0,
        }))
    }

    fn previous_position(
        &mut self,
        position: ReaderPosition,
    ) -> Result<Option<ReaderPosition>, ReaderError> {
        if position.page_index > 0 {
            return Ok(Some(ReaderPosition {
                page_index: position.page_index - 1,
                ..position
            }));
        }
        let previous_key = if let Some(segment_index) = position.segment_index.checked_sub(1) {
            SegmentKey {
                section_index: position.section_index,
                segment_index,
            }
        } else {
            let Some(section_index) = position.section_index.checked_sub(1) else {
                return Ok(None);
            };
            let section = self.repository.load(section_index)?;
            SegmentKey {
                section_index,
                segment_index: section.segments.len().saturating_sub(1),
            }
        };
        self.ensure_segment(previous_key)?;
        let page_index = self
            .cache
            .get(&previous_key)
            .expect("ensured layout segment must remain cached")
            .frames
            .len()
            .saturating_sub(1);
        Ok(Some(ReaderPosition {
            section_index: previous_key.section_index,
            segment_index: previous_key.segment_index,
            page_index,
        }))
    }

    #[cfg(feature = "legacy-renderer")]
    fn page_at(&self, position: ReaderPosition) -> Result<Arc<PageDisplayList>, ReaderError> {
        self.cache
            .get(&SegmentKey {
                section_index: position.section_index,
                segment_index: position.segment_index,
            })
            .and_then(|segment| segment.pages.get(position.page_index))
            .cloned()
            .ok_or(ReaderError::PageOutOfBounds(position))
    }

    /// Returns the immutable layout frame behind a cached painted page.
    pub fn layout_frame_at(
        &self,
        position: ReaderPosition,
    ) -> Result<Arc<LayoutFrame>, ReaderError> {
        self.cache
            .get(&SegmentKey {
                section_index: position.section_index,
                segment_index: position.segment_index,
            })
            .and_then(|segment| segment.frames.get(position.page_index))
            .cloned()
            .ok_or(ReaderError::PageOutOfBounds(position))
    }

    fn current_position(&self) -> ReaderPosition {
        ReaderPosition {
            section_index: self.current_section,
            segment_index: self.current_segment,
            page_index: self.current_page,
        }
    }

    fn sync_reading_unit_to_position(&mut self) {
        if let Some(units) = &self.fixed_reading_units {
            self.current_reading_unit = units
                .partition_point(|unit| unit.section_range.start <= self.current_section)
                .saturating_sub(1)
                .min(units.len().saturating_sub(1));
            return;
        }
        let Ok(section) = self.repository.load(self.current_section) else {
            self.current_reading_unit = 0;
            return;
        };
        let position = self.current_position();
        let starts = section
            .reading_units
            .iter()
            .map(|unit| {
                unit.start
                    .as_ref()
                    .and_then(|anchor| self.position_for_source_anchor(anchor).ok())
                    .unwrap_or(ReaderPosition {
                        section_index: self.current_section,
                        segment_index: 0,
                        page_index: 0,
                    })
            })
            .collect::<Vec<_>>();
        self.current_reading_unit = starts
            .partition_point(|start| *start <= position)
            .saturating_sub(1);
    }

    fn sync_reading_unit_to_anchor(&mut self, anchor: &SourceAnchor) {
        if self.fixed_reading_units.is_some() {
            if let Some(section_index) = self
                .source
                .book()
                .sections
                .iter()
                .position(|section| section.id == anchor.spine)
            {
                self.current_section = section_index;
                self.sync_reading_unit_to_position();
            }
            return;
        }
        let Ok(section) = self.repository.load(self.current_section) else {
            self.current_reading_unit = 0;
            return;
        };
        self.current_reading_unit = section
            .reading_units
            .iter()
            .position(|unit| {
                section.fragments[unit.fragment_range.clone()]
                    .iter()
                    .flat_map(|fragment| &fragment.blocks)
                    .filter_map(block_source)
                    .any(|range| source_range_contains(range, anchor))
            })
            .unwrap_or(0);
    }

    fn position_for_source_anchor(
        &mut self,
        anchor: &SourceAnchor,
    ) -> Result<ReaderPosition, ReaderError> {
        let section_index = self
            .source
            .book()
            .sections
            .iter()
            .position(|section| section.id == anchor.spine)
            .ok_or_else(|| ReaderError::NavigationTargetNotFound(anchor.node.clone()))?;
        let section = self.repository.load(section_index)?;
        let fragment_index = section
            .fragments
            .iter()
            .position(|fragment| {
                fragment.blocks.iter().any(|block| {
                    block_source(block).is_some_and(|range| source_range_contains(range, anchor))
                })
            })
            .unwrap_or(0);
        let segment_index = section
            .segments
            .iter()
            .position(|segment| segment.fragment_range.contains(&fragment_index))
            .unwrap_or(0);
        let key = SegmentKey {
            section_index,
            segment_index,
        };
        self.ensure_segment(key)?;
        let page_index = self
            .cache
            .get(&key)
            .and_then(|segment| {
                segment
                    .frames
                    .iter()
                    .position(|frame| frame.interaction().contains_source_anchor(anchor))
            })
            .unwrap_or(0);
        Ok(ReaderPosition {
            section_index,
            segment_index,
            page_index,
        })
    }

    fn go_to_reading_unit(
        &mut self,
        section_index: usize,
        unit_index: usize,
    ) -> Result<NavigationResult, ReaderError> {
        let section = self.repository.load(section_index)?;
        let unit = section
            .reading_units
            .get(unit_index)
            .ok_or(ReaderError::SectionOutOfBounds(section_index))?;
        let start = unit.start.clone();
        let result = if unit_index == 0 {
            self.go_to_section(section_index)?
        } else if let Some(start) = start {
            self.go_to_source(&start)?
        } else {
            self.go_to_section(section_index)?
        };
        self.current_reading_unit = unit_index;
        Ok(result)
    }

    fn current_visible_pages(&self) -> usize {
        self.cache
            .get(&self.current_key())
            .map_or(1, |segment| segment.visible_pages.max(1))
    }

    fn install_position(&mut self, position: ReaderPosition) {
        self.current_section = position.section_index;
        self.current_segment = position.segment_index;
        self.current_page = position.page_index;
        self.touch(self.current_key());
    }

    fn ensure_segment(&mut self, key: SegmentKey) -> Result<(), ReaderError> {
        if self.cache.contains_key(&key) {
            self.touch(key);
            return Ok(());
        }
        if let Some(error) = self.prefetch_failures.remove(&key) {
            return Err(error);
        }
        if let Some(generation) = self
            .prefetch_worker
            .as_ref()
            .map(PrefetchWorker::generation)
        {
            let prefetch_key = PrefetchKey {
                generation,
                segment: key,
            };
            if self.prefetch_inflight.contains(&prefetch_key) {
                self.wait_for_segment(key)?;
                if self.cache.contains_key(&key) {
                    self.touch(key);
                    return Ok(());
                }
            }
        }
        let section = self.repository.load(key.section_index)?;
        let segment = compile_segment(
            self.source.as_ref(),
            section,
            key,
            self.viewport,
            &self.style,
            &mut self.layout_engine,
            #[cfg(feature = "legacy-renderer")]
            &self.display_compiler,
        )?;
        self.cache.insert(key, Arc::new(segment));
        self.touch(key);
        self.evict();
        Ok(())
    }

    fn try_ensure_segment(&mut self, key: SegmentKey) -> Result<bool, ReaderError> {
        if self.cache.contains_key(&key) {
            self.touch(key);
            return Ok(true);
        }
        if let Some(error) = self.prefetch_failures.remove(&key) {
            return Err(error);
        }
        let Some(generation) = self
            .prefetch_worker
            .as_ref()
            .map(PrefetchWorker::generation)
        else {
            self.ensure_segment(key)?;
            return Ok(true);
        };
        let prefetch_key = PrefetchKey {
            generation,
            segment: key,
        };
        if !self.prefetch_inflight.contains(&prefetch_key) {
            self.queue_prefetch(key)?;
        }
        Ok(false)
    }

    fn invalidate_layout(&mut self, fraction: f32) -> Result<(), ReaderError> {
        if let Some(worker) = self.prefetch_worker.as_ref() {
            worker.invalidate();
        }
        self.prefetch_inflight.clear();
        self.prefetch_failures.clear();
        let current_section = Arc::clone(self.current_section_data());
        self.cache.clear();
        self.lru.clear();
        let key = self.current_key();
        let segment = compile_segment(
            self.source.as_ref(),
            current_section,
            key,
            self.viewport,
            &self.style,
            &mut self.layout_engine,
            #[cfg(feature = "legacy-renderer")]
            &self.display_compiler,
        )?;
        self.cache.insert(key, Arc::new(segment));
        self.touch(key);
        let count = self.current_page_count();
        self.current_page = page_for_fraction(fraction, count);
        Ok(())
    }

    fn queue_prefetch(&mut self, segment: SegmentKey) -> Result<(), ReaderError> {
        let Some(worker) = self.prefetch_worker.as_ref() else {
            return Ok(());
        };
        let key = PrefetchKey {
            generation: worker.generation(),
            segment,
        };
        if self.cache.contains_key(&segment) || self.prefetch_inflight.contains(&key) {
            return Ok(());
        }
        worker.send(PrefetchRequest {
            key: segment,
            viewport: self.viewport,
            style: self.style.clone(),
            generation: key.generation,
        })?;
        self.prefetch_inflight.insert(key);
        Ok(())
    }

    fn poll_prefetch(&mut self) -> Result<(), ReaderError> {
        if self.prefetch_worker.is_none() {
            return Ok(());
        }
        loop {
            let Some(worker) = self.prefetch_worker.as_ref() else {
                return Ok(());
            };
            let result = worker.try_recv();
            match result {
                Ok(result) => self.install_prefetch(result),
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) if self.prefetch_inflight.is_empty() => {
                    return Ok(());
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(ReaderError::PrefetchWorkerStopped);
                }
            }
        }
    }

    fn wait_for_segment(&mut self, segment: SegmentKey) -> Result<(), ReaderError> {
        let Some(generation) = self
            .prefetch_worker
            .as_ref()
            .map(PrefetchWorker::generation)
        else {
            return Ok(());
        };
        let key = PrefetchKey {
            generation,
            segment,
        };
        while self.prefetch_inflight.contains(&key) {
            let Some(worker) = self.prefetch_worker.as_ref() else {
                return Ok(());
            };
            let result = worker.recv()?;
            self.install_prefetch(result);
        }
        if let Some(error) = self.prefetch_failures.remove(&segment) {
            return Err(error);
        }
        Ok(())
    }

    fn install_prefetch(&mut self, result: PrefetchResult) {
        let Some(generation) = self
            .prefetch_worker
            .as_ref()
            .map(PrefetchWorker::generation)
        else {
            return;
        };
        let key = PrefetchKey {
            generation: result.generation,
            segment: result.key,
        };
        self.prefetch_inflight.remove(&key);
        if result.generation != generation {
            return;
        }
        let segment = match result.segment {
            Ok(segment) => segment,
            Err(error) => {
                self.prefetch_failures.insert(result.key, error);
                return;
            }
        };
        if self.cache.insert(result.key, segment).is_none() {
            self.touch(result.key);
            self.evict();
        }
        self.touch(self.current_key());
    }

    fn current_page_count(&self) -> usize {
        self.cache
            .get(&self.current_key())
            .map_or(0, |segment| segment.frames.len())
    }

    fn current_key(&self) -> SegmentKey {
        SegmentKey {
            section_index: self.current_section,
            segment_index: self.current_segment,
        }
    }

    fn current_section_data(&self) -> &Arc<PreparedSection> {
        &self
            .cache
            .get(&self.current_key())
            .expect("current layout segment must remain cached")
            .section
    }

    fn touch(&mut self, key: SegmentKey) {
        self.lru.retain(|cached| *cached != key);
        self.lru.push_back(key);
    }

    fn evict(&mut self) {
        while self.cache.len() > self.cache_capacity {
            let Some(candidate) = self.lru.pop_front() else {
                break;
            };
            if candidate == self.current_key() {
                self.lru.push_back(candidate);
                continue;
            }
            self.cache.remove(&candidate);
        }
    }

    fn moved(&self) -> NavigationResult {
        NavigationResult {
            outcome: NavigationOutcome::Moved,
            snapshot: self.snapshot(),
        }
    }

    fn moved_to_toc_target(&self, target: &PublicationUrl) -> NavigationResult {
        let mut snapshot = self.snapshot();
        if let Some(item) = self
            .toc_items
            .iter()
            .find(|item| item.target.as_ref() == Some(target))
        {
            snapshot.active_toc_id = Some(item.id.clone());
            snapshot.active_toc_path.clone_from(&item.ancestors);
            if item.has_children {
                snapshot.active_toc_path.push(item.id.clone());
            }
        }
        NavigationResult {
            outcome: NavigationOutcome::Moved,
            snapshot,
        }
    }

    fn boundary(&self) -> NavigationResult {
        NavigationResult {
            outcome: NavigationOutcome::Boundary,
            snapshot: self.snapshot(),
        }
    }
}

fn compile_segment<E: TextEngine>(
    source: &dyn BookSource,
    section: Arc<PreparedSection>,
    key: SegmentKey,
    viewport: LayoutViewport,
    style: &ReaderStyle,
    layout_engine: &mut LayoutEngine<E>,
    #[cfg(feature = "legacy-renderer")] display_compiler: &DisplayListCompiler,
) -> Result<CachedSegment, ReaderError> {
    let segment =
        section
            .segments
            .get(key.segment_index)
            .ok_or(ReaderError::SegmentOutOfBounds {
                section: key.section_index,
                segment: key.segment_index,
            })?;
    let fragments = section.fragments[segment.fragment_range.clone()]
        .iter()
        .map(|fragment| fragment.blocks.as_slice())
        .collect::<Vec<_>>();
    let layout = layout_engine.layout_fragments(source, &fragments, viewport, style)?;
    let visible_pages = layout.visible_pages;
    let continuation_offset_x = layout.continuation_offset_x;
    let anchor_pages = section.fragments[segment.fragment_range.clone()]
        .iter()
        .flat_map(|fragment| &fragment.anchors)
        .filter_map(|anchor| {
            layout
                .pages
                .iter()
                .position(|page| {
                    page.items.iter().any(|item| match item {
                        PageItem::Text(placement) => placement
                            .source
                            .as_ref()
                            .is_some_and(|range| source_range_contains(range, &anchor.source)),
                        PageItem::Quote(placement) => placement
                            .sources
                            .iter()
                            .any(|range| source_range_contains(range, &anchor.source)),
                        PageItem::Table(placement) => placement.cells.iter().any(|cell| {
                            cell.text
                                .as_ref()
                                .and_then(|text| text.source.as_ref())
                                .is_some_and(|range| source_range_contains(range, &anchor.source))
                        }),
                        PageItem::Image(placement) => placement
                            .source
                            .as_ref()
                            .is_some_and(|range| source_range_contains(range, &anchor.source)),
                        PageItem::Separator(_) => false,
                    })
                })
                .map(|page| (anchor.fragment.clone(), page))
        })
        .collect();
    let frames = layout
        .pages
        .into_iter()
        .map(LayoutFrame::freeze)
        .map(Arc::new)
        .collect::<Vec<_>>();
    #[cfg(feature = "legacy-renderer")]
    let pages = frames
        .iter()
        .map(|frame| Arc::new(display_compiler.compile_frame(frame)))
        .collect();
    Ok(CachedSegment {
        section,
        frames,
        #[cfg(feature = "legacy-renderer")]
        pages,
        anchor_pages,
        visible_pages,
        continuation_offset_x,
    })
}

fn prepare_section(
    section: Section,
    layout_boundaries: &HashSet<String>,
    reading_boundaries: &[Option<String>],
) -> PreparedSection {
    let Section {
        blocks, anchors, ..
    } = section;
    let section_text = blocks.iter().map(block_text_len).sum::<usize>();
    let use_layout_boundaries = section_text >= LARGE_SECTION_TEXT_BUDGET;
    let boundary_sources = anchors
        .iter()
        .filter(|anchor| {
            (use_layout_boundaries && layout_boundaries.contains(&anchor.fragment))
                || reading_boundaries
                    .iter()
                    .any(|fragment| fragment.as_deref() == Some(&anchor.fragment))
        })
        .map(|anchor| anchor.source.clone())
        .collect::<Vec<_>>();
    let fragments = fragment_section_blocks(blocks, &boundary_sources);
    let (fragments, resolved_anchors) = resolve_fragment_anchors(fragments, anchors);
    let reading_units = build_reading_units(&resolved_anchors, reading_boundaries, &fragments);
    let reading_unit_starts = reading_units
        .iter()
        .map(|unit| unit.fragment_range.start)
        .collect::<HashSet<_>>();
    let (segments, anchor_segments) = build_layout_segments(
        &resolved_anchors,
        layout_boundaries,
        use_layout_boundaries,
        &reading_unit_starts,
        fragments.len(),
    );

    PreparedSection {
        fragments,
        segments,
        anchor_segments,
        reading_units,
    }
}

fn build_reading_units(
    resolved_anchors: &[(String, usize)],
    reading_boundaries: &[Option<String>],
    fragments: &[ContentFragment],
) -> Vec<ReadingUnit> {
    let resolved = resolved_anchors
        .iter()
        .map(|(fragment, index)| (fragment.as_str(), *index))
        .collect::<HashMap<_, _>>();
    let mut starts = reading_boundaries
        .iter()
        .filter_map(|fragment| {
            let Some(fragment) = fragment else {
                return Some((0, None));
            };
            let index = resolved.get(fragment.as_str()).copied()?;
            let source = fragments
                .get(index)?
                .anchors
                .iter()
                .find(|anchor| anchor.fragment == *fragment)?
                .source
                .clone();
            Some((index, Some(source)))
        })
        .collect::<Vec<_>>();
    starts.sort_by_key(|(index, _)| *index);
    starts.dedup_by(|left, right| left.0 == right.0 || left.1 == right.1);

    if starts.is_empty() {
        return vec![ReadingUnit {
            fragment_range: 0..fragments.len(),
            start: None,
        }];
    }

    let mut units = starts
        .iter()
        .enumerate()
        .map(|(index, (start_fragment, start))| ReadingUnit {
            fragment_range: if index == 0 { 0 } else { *start_fragment }
                ..starts
                    .get(index + 1)
                    .map_or(fragments.len(), |(next, _)| *next),
            start: start.clone(),
        })
        .collect::<Vec<_>>();

    // A navigable chapter/parent anchor can precede its first child while only
    // contributing headings. It is not a useful standalone reading step: fold
    // it into the following subsection. Images and tables remain activatable,
    // so illustrated preludes are deliberately kept independent.
    let mut index = 0;
    while index + 1 < units.len() {
        if reading_unit_has_activatable_content(&units[index], fragments) {
            index += 1;
            continue;
        }
        let start = units[index].fragment_range.start;
        units[index + 1].fragment_range.start = start;
        units.remove(index);
    }
    units
}

fn reading_unit_has_activatable_content(unit: &ReadingUnit, fragments: &[ContentFragment]) -> bool {
    fragments[unit.fragment_range.clone()]
        .iter()
        .flat_map(|fragment| &fragment.blocks)
        .any(|block| match block {
            Block::Text(text) => {
                !matches!(text.kind, rebook_publication::TextBlockKind::Heading(_))
                    && block_text_len(block) > 0
            }
            Block::Quote(_) | Block::Image(_) | Block::Figure(_) | Block::Table(_) => true,
            Block::Separator | Block::LineBreak | Block::PageBreak => false,
        })
}

fn fragment_section_blocks(
    blocks: Vec<Block>,
    boundary_sources: &[SourceAnchor],
) -> Vec<ContentFragment> {
    let mut block_groups = Vec::<Vec<Block>>::new();
    let mut current = Vec::new();
    let mut current_text = 0_usize;

    let flush =
        |current: &mut Vec<Block>, current_text: &mut usize, block_groups: &mut Vec<Vec<Block>>| {
            if !current.is_empty() {
                block_groups.push(std::mem::take(current));
                *current_text = 0;
            }
        };

    for block in blocks {
        let pieces = match block {
            Block::Text(block) => split_text_block(block)
                .into_iter()
                .map(Block::Text)
                .collect::<Vec<_>>(),
            block => vec![block],
        };
        for piece in pieces {
            let starts_layout_segment = !current.is_empty()
                && block_source(&piece).is_some_and(|range| {
                    boundary_sources
                        .iter()
                        .any(|anchor| source_range_contains(range, anchor))
                });
            if starts_layout_segment {
                flush(&mut current, &mut current_text, &mut block_groups);
            }
            let text_len = block_text_len(&piece);
            if !current.is_empty()
                && (current.len() >= FRAGMENT_BLOCK_BUDGET
                    || current_text.saturating_add(text_len) > FRAGMENT_TEXT_BUDGET)
            {
                flush(&mut current, &mut current_text, &mut block_groups);
            }
            current_text = current_text.saturating_add(text_len);
            current.push(piece);
            if current.len() >= FRAGMENT_BLOCK_BUDGET || current_text >= FRAGMENT_TEXT_BUDGET {
                flush(&mut current, &mut current_text, &mut block_groups);
            }
        }
    }
    flush(&mut current, &mut current_text, &mut block_groups);
    if block_groups.is_empty() {
        block_groups.push(Vec::new());
    }

    block_groups
        .into_iter()
        .map(|blocks| ContentFragment {
            blocks,
            anchors: Vec::new(),
        })
        .collect()
}

fn resolve_fragment_anchors(
    mut fragments: Vec<ContentFragment>,
    anchors: Vec<SectionAnchor>,
) -> (Vec<ContentFragment>, Vec<(String, usize)>) {
    let mut resolved_anchors = Vec::with_capacity(anchors.len());
    for anchor in anchors {
        let fragment_index = fragments
            .iter()
            .position(|fragment| {
                fragment.blocks.iter().any(|block| {
                    block_source(block)
                        .is_some_and(|range| source_range_contains(range, &anchor.source))
                })
            })
            .unwrap_or(0);
        resolved_anchors.push((anchor.fragment.clone(), fragment_index));
        fragments[fragment_index].anchors.push(anchor);
    }
    (fragments, resolved_anchors)
}

fn build_layout_segments(
    resolved_anchors: &[(String, usize)],
    layout_boundaries: &HashSet<String>,
    use_layout_boundaries: bool,
    reading_unit_starts: &HashSet<usize>,
    fragment_count: usize,
) -> (Vec<LayoutSegment>, HashMap<String, usize>) {
    let mut segment_starts = resolved_anchors
        .iter()
        .filter(|(fragment, _)| use_layout_boundaries && layout_boundaries.contains(fragment))
        .map(|(_, fragment_index)| *fragment_index)
        .collect::<Vec<_>>();
    segment_starts.extend(reading_unit_starts.iter().copied());
    segment_starts.push(0);
    segment_starts.sort_unstable();
    segment_starts.dedup();
    let segments = segment_starts
        .iter()
        .enumerate()
        .map(|(index, start)| LayoutSegment {
            fragment_range: *start
                ..segment_starts
                    .get(index + 1)
                    .copied()
                    .unwrap_or(fragment_count),
        })
        .collect::<Vec<_>>();
    let anchor_segments = resolved_anchors
        .iter()
        .map(|(fragment, fragment_index)| {
            let segment_index = segment_starts
                .partition_point(|start| *start <= *fragment_index)
                .saturating_sub(1);
            (fragment.clone(), segment_index)
        })
        .collect();
    (segments, anchor_segments)
}

fn top_level_toc_fragments_for_section(book: &Book, section_index: usize) -> HashSet<String> {
    let Some(section) = book.sections.get(section_index) else {
        return HashSet::new();
    };
    book.table_of_contents
        .iter()
        .filter_map(|entry| entry.href.as_ref())
        .filter(|href| href.path() == section.href.path())
        .filter_map(PublicationUrl::fragment)
        .map(str::to_owned)
        .collect()
}

fn semantic_toc_boundaries_for_section(book: &Book, section_index: usize) -> Vec<Option<String>> {
    fn append(entries: &[TocEntry], path: &str, fragments: &mut Vec<Option<String>>) {
        for entry in entries {
            // Navigable parents mark chapter preludes. Leaf entries mark the
            // actual subsection units. Equal anchors are deduplicated below,
            // so a parent and its first child never create an empty unit.
            if let Some(href) = &entry.href
                && href.path() == path
            {
                let fragment = href.fragment().map(str::to_owned);
                if !fragments.contains(&fragment) {
                    fragments.push(fragment);
                }
            }
            append(&entry.children, path, fragments);
        }
    }

    let Some(section) = book.sections.get(section_index) else {
        return Vec::new();
    };
    let mut fragments = Vec::new();
    append(&book.table_of_contents, section.href.path(), &mut fragments);
    fragments
}

fn build_fixed_reading_units(
    source: &dyn BookSource,
    toc_items: &[TocViewItem],
    section_indices_by_path: &HashMap<String, usize>,
) -> Option<Vec<FixedReadingUnit>> {
    if source.book().metadata.layout != RenditionLayout::PrePaginated {
        return None;
    }
    let section_count = source.book().sections.len();
    let mut starts = if source.table_of_contents_origin() == TableOfContentsOrigin::Fallback
        || toc_items.is_empty()
    {
        // A mechanical PDF TOC contains one entry per physical page. Preserve
        // that behavior when no authored/generated semantic outline exists.
        (0..section_count).collect::<Vec<_>>()
    } else {
        toc_items
            .iter()
            .filter_map(|item| item.target.as_ref())
            .filter_map(|target| section_indices_by_path.get(target.path()))
            .copied()
            .collect::<Vec<_>>()
    };
    starts.push(0);
    starts.sort_unstable();
    starts.dedup();
    Some(
        starts
            .iter()
            .enumerate()
            .filter_map(|(index, start)| {
                let end = starts.get(index + 1).copied().unwrap_or(section_count);
                (*start < end).then_some(FixedReadingUnit {
                    section_range: *start..end,
                })
            })
            .collect(),
    )
}

fn split_text_block(block: TextBlock) -> Vec<TextBlock> {
    let content_len = inline_content_len(&block.content);
    if content_len <= FRAGMENT_TEXT_BUDGET {
        return vec![block];
    }

    let TextBlock {
        kind,
        content,
        style,
        source,
    } = block;
    let content_parts = split_inline_content(content);
    let part_count = content_parts.len();
    let mut source_offset = 0_usize;
    content_parts
        .into_iter()
        .enumerate()
        .map(|(index, (content, length))| {
            let mut part_style = style;
            let part_kind = if index > 0
                && matches!(kind, rebook_publication::TextBlockKind::ListItem { .. })
            {
                rebook_publication::TextBlockKind::Paragraph
            } else {
                kind
            };
            if index > 0 {
                part_style.margin_before = 0.0;
                part_style.indent = 0.0;
            }
            if index + 1 < part_count {
                part_style.margin_after = 0.0;
            }
            let part_source = source
                .as_ref()
                .map(|range| slice_source_range(range, source_offset, source_offset + length));
            source_offset += length;
            TextBlock {
                kind: part_kind,
                content,
                style: part_style,
                source: part_source,
            }
        })
        .collect()
}

fn split_inline_content(content: Vec<Inline>) -> Vec<(Vec<Inline>, usize)> {
    let mut parts = Vec::new();
    let mut current = Vec::new();
    let mut current_len = 0_usize;

    let flush = |current: &mut Vec<Inline>,
                 current_len: &mut usize,
                 parts: &mut Vec<(Vec<Inline>, usize)>| {
        if !current.is_empty() {
            parts.push((std::mem::take(current), *current_len));
            *current_len = 0;
        }
    };

    for inline in content {
        match inline {
            Inline::Break => {
                if current_len == FRAGMENT_TEXT_BUDGET {
                    flush(&mut current, &mut current_len, &mut parts);
                }
                current.push(Inline::Break);
                current_len += 1;
            }
            Inline::Text(run) => {
                let TextRun { text, style, link } = run;
                let mut remaining = text.as_str();
                while !remaining.is_empty() {
                    if current_len == FRAGMENT_TEXT_BUDGET {
                        flush(&mut current, &mut current_len, &mut parts);
                    }
                    let capacity = FRAGMENT_TEXT_BUDGET - current_len;
                    let split_at = byte_index_after_chars(remaining, capacity);
                    let (slice, rest) = remaining.split_at(split_at);
                    current.push(Inline::Text(TextRun {
                        text: slice.to_owned(),
                        style,
                        link: link.clone(),
                    }));
                    current_len += slice.chars().count();
                    remaining = rest;
                }
            }
            Inline::Math(run) => current.push(Inline::Math(run)),
        }
    }
    flush(&mut current, &mut current_len, &mut parts);
    parts
}

fn byte_index_after_chars(text: &str, count: usize) -> usize {
    text.char_indices()
        .nth(count)
        .map_or(text.len(), |(index, _)| index)
}

fn slice_source_range(range: &SourceRange, start: usize, end: usize) -> SourceRange {
    if range.start.spine == range.end.spine && range.start.node == range.end.node {
        let offset = |value: usize| {
            range
                .start
                .text_offset
                .saturating_add(u64::try_from(value).unwrap_or(u64::MAX))
                .min(range.end.text_offset)
        };
        SourceRange {
            start: SourceAnchor {
                spine: range.start.spine.clone(),
                node: range.start.node.clone(),
                text_offset: offset(start),
            },
            end: SourceAnchor {
                spine: range.end.spine.clone(),
                node: range.end.node.clone(),
                text_offset: offset(end),
            },
        }
    } else {
        range.clone()
    }
}

fn inline_content_len(content: &[Inline]) -> usize {
    content
        .iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.chars().count(),
            Inline::Math(_) => 0,
            Inline::Break => 1,
        })
        .sum()
}

fn block_text_len(block: &Block) -> usize {
    match block {
        Block::Text(block) => inline_content_len(&block.content),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(quote.attribution.iter())
            .map(|block| inline_content_len(&block.content))
            .sum(),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .map(|cell| inline_content_len(&cell.text.content))
            .sum(),
        Block::Figure(figure) => figure
            .captions
            .iter()
            .map(|caption| inline_content_len(&caption.content))
            .sum(),
        Block::Image(_) | Block::Separator | Block::LineBreak | Block::PageBreak => 0,
    }
}

fn append_visible_text_fragments(
    fragments: &mut Vec<ReaderVisibleTextFragment>,
    position: ReaderPosition,
    interaction: &FrameInteractionMap,
) {
    for region_index in 0..interaction.text_region_count() {
        let Some(visible_range) = interaction.visible_byte_range(region_index) else {
            continue;
        };
        let Some(fragment) = interaction.selection_fragment(region_index, visible_range) else {
            continue;
        };
        if fragment.quote.trim().is_empty() {
            continue;
        }
        fragments.push(ReaderVisibleTextFragment {
            position,
            range: fragment.range,
            text: fragment.quote,
        });
    }
}

fn block_source(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(block) => block.source.as_ref(),
        Block::Quote(block) => block.source.as_ref(),
        Block::Table(block) => block.source.as_ref(),
        Block::Image(block) => block.source.as_ref(),
        Block::Figure(block) => block.source.as_ref(),
        Block::Separator | Block::LineBreak | Block::PageBreak => None,
    }
}

fn source_range_contains(range: &SourceRange, anchor: &SourceAnchor) -> bool {
    if range.start.spine != anchor.spine || range.start.node != anchor.node {
        return false;
    }
    if range.start.spine != range.end.spine || range.start.node != range.end.node {
        return range.start == *anchor;
    }
    anchor.text_offset >= range.start.text_offset
        && (anchor.text_offset < range.end.text_offset
            || (range.start.text_offset == range.end.text_offset
                && anchor.text_offset == range.start.text_offset))
}

fn reader_text_hit(position: ReaderPosition, hit: FrameTextHit) -> ReaderTextHit {
    ReaderTextHit {
        position,
        region_index: hit.region_index,
        byte_index: hit.byte_index,
        cluster_start: hit.cluster_start,
        cluster_end: hit.cluster_end,
    }
}

fn reader_text_hit_for_cursor(cursor: ReaderTextCursor) -> ReaderTextHit {
    ReaderTextHit {
        position: cursor.position,
        region_index: cursor.cursor.region_index,
        byte_index: cursor.cursor.byte_index,
        cluster_start: cursor.cursor.byte_index,
        cluster_end: cursor.cursor.byte_index,
    }
}

type SelectionPage = (ReaderPosition, Arc<LayoutFrame>, f32);

#[derive(Clone, Copy)]
struct SelectionBoundary {
    page: usize,
    region: usize,
    byte: usize,
}

impl SelectionBoundary {
    const fn new(page_index: usize, region_index: usize, byte_index: usize) -> Self {
        Self {
            page: page_index,
            region: region_index,
            byte: byte_index,
        }
    }
}

fn semantic_source_range(
    interaction: &FrameInteractionMap,
    hit: &ReaderTextHit,
    granularity: SelectionGranularity,
    include_trailing_boundary_punctuation: bool,
) -> Option<SourceRange> {
    let text = interaction.text(hit.region_index)?;
    let selectable = interaction.selectable_byte_range(hit.region_index)?;
    let mut byte_range = semantic_byte_range(text, selectable.clone(), hit, granularity)?;
    if include_trailing_boundary_punctuation && granularity == SelectionGranularity::Word {
        byte_range = extend_word_to_sentence_or_paragraph_end(text, selectable, byte_range);
    }
    interaction.source_range_for_bytes(hit.region_index, byte_range)
}

fn extend_word_to_sentence_or_paragraph_end(
    text: &str,
    selectable: Range<usize>,
    word: Range<usize>,
) -> Range<usize> {
    let Some(source_text) = text.get(selectable.clone()) else {
        return word;
    };
    let mut punctuation_end = word.end;
    let mut has_sentence_terminal = false;
    for character in text
        .get(word.end..selectable.end)
        .unwrap_or_default()
        .chars()
    {
        if !is_selection_trailing_punctuation(character) {
            break;
        }
        punctuation_end += character.len_utf8();
        has_sentence_terminal |= is_sentence_terminal(character);
    }
    if punctuation_end > word.end && has_sentence_terminal {
        return word.start..punctuation_end;
    }
    let sentence_end = sentence_byte_ranges(source_text)
        .into_iter()
        .find_map(|range| {
            let sentence_range = selectable.start + range.start..selectable.start + range.end;
            let trimmed = trim_whitespace_range(text, sentence_range)?;
            range_ends_with_word(text, &trimmed, &word).then_some(trimmed.end)
        });
    let paragraph_end = trim_whitespace_range(text, selectable)
        .filter(|range| range_ends_with_word(text, range, &word))
        .map(|range| range.end);
    let Some(end) = sentence_end.or(paragraph_end) else {
        return word;
    };
    let Some(trailing) = text.get(word.end..end) else {
        return word;
    };
    if trailing.is_empty() || !trailing.chars().all(is_selection_trailing_punctuation) {
        return word;
    }
    word.start..end
}

fn range_ends_with_word(text: &str, range: &Range<usize>, word: &Range<usize>) -> bool {
    text.get(range.clone())
        .and_then(|value| value.unicode_word_indices().next_back())
        .is_some_and(|(start, value)| {
            range.start + start == word.start && range.start + start + value.len() == word.end
        })
}

/// Returns whether a character terminates a sentence for semantic selection.
///
/// OCR reflow also uses this rule so page-boundary merging and sentence
/// selection agree on CJK and Latin terminal punctuation.
pub fn is_sentence_terminal(character: char) -> bool {
    matches!(character, '.' | '!' | '?' | '。' | '！' | '？' | '…' | '‥')
}

fn is_selection_trailing_punctuation(character: char) -> bool {
    matches!(
        character,
        '.' | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | '\''
            | '"'
            | ')'
            | ']'
            | '}'
            | '。'
            | '，'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | '…'
            | '‥'
            | '’'
            | '”'
            | '»'
            | '›'
            | '）'
            | '】'
            | '》'
            | '〉'
            | '」'
            | '』'
            | '〕'
            | '〗'
            | '〙'
            | '〛'
    )
}

fn semantic_byte_range(
    text: &str,
    selectable: Range<usize>,
    hit: &ReaderTextHit,
    granularity: SelectionGranularity,
) -> Option<Range<usize>> {
    let source_text = text.get(selectable.clone())?;
    if source_text.is_empty() {
        return None;
    }
    if granularity == SelectionGranularity::Paragraph {
        return Some(selectable);
    }
    let ranges = match granularity {
        SelectionGranularity::Word => source_text
            .unicode_word_indices()
            .map(|(start, word)| selectable.start + start..selectable.start + start + word.len())
            .collect::<Vec<_>>(),
        SelectionGranularity::Sentence => sentence_byte_ranges(source_text)
            .into_iter()
            .filter_map(|range| {
                trim_whitespace_range(
                    text,
                    selectable.start + range.start..selectable.start + range.end,
                )
            })
            .collect::<Vec<_>>(),
        SelectionGranularity::Free | SelectionGranularity::Paragraph => return None,
    };
    nearest_semantic_range(
        &ranges,
        hit.cluster_start.max(selectable.start)..hit.cluster_end.min(selectable.end),
        hit.byte_index.clamp(selectable.start, selectable.end),
    )
}

fn nearest_semantic_range(
    ranges: &[Range<usize>],
    cluster: Range<usize>,
    caret: usize,
) -> Option<Range<usize>> {
    ranges
        .iter()
        .min_by_key(|range| {
            if range.start < cluster.end && range.end > cluster.start {
                0
            } else if caret < range.start {
                range.start - caret
            } else {
                caret.saturating_sub(range.end)
            }
        })
        .cloned()
}

fn trim_whitespace_range(text: &str, mut range: Range<usize>) -> Option<Range<usize>> {
    while range.start < range.end {
        let character = text.get(range.start..range.end)?.chars().next()?;
        if !character.is_whitespace() {
            break;
        }
        range.start += character.len_utf8();
    }
    while range.start < range.end {
        let character = text.get(range.start..range.end)?.chars().next_back()?;
        if !character.is_whitespace() {
            break;
        }
        range.end -= character.len_utf8();
    }
    (range.start < range.end).then_some(range)
}

fn first_source_boundary(
    pages: &[SelectionPage],
    range: &SourceRange,
) -> Option<SelectionBoundary> {
    pages
        .iter()
        .enumerate()
        .find_map(|(page_index, (_, frame, _))| {
            let interaction = frame.interaction();
            (0..interaction.text_region_count()).find_map(|region_index| {
                interaction
                    .byte_range_for_source(region_index, range)
                    .map(|bytes| SelectionBoundary::new(page_index, region_index, bytes.start))
            })
        })
}

fn expand_paragraph_source_range(
    pages: &[SelectionPage],
    mut paragraph: SourceRange,
) -> SourceRange {
    for (_, frame, _) in pages {
        let interaction = frame.interaction();
        for region_index in 0..interaction.text_region_count() {
            let Some(selectable) = interaction.selectable_byte_range(region_index) else {
                continue;
            };
            let Some(candidate) = interaction.source_range_for_bytes(region_index, selectable)
            else {
                continue;
            };
            if candidate.start.spine != paragraph.start.spine
                || candidate.start.node != paragraph.start.node
            {
                continue;
            }
            if candidate.start.text_offset < paragraph.start.text_offset {
                paragraph.start = candidate.start;
            }
            if candidate.end.text_offset > paragraph.end.text_offset {
                paragraph.end = candidate.end;
            }
        }
    }
    paragraph
}

fn last_source_boundary(pages: &[SelectionPage], range: &SourceRange) -> Option<SelectionBoundary> {
    pages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(page_index, (_, frame, _))| {
            let interaction = frame.interaction();
            (0..interaction.text_region_count())
                .rev()
                .find_map(|region_index| {
                    interaction
                        .byte_range_for_source(region_index, range)
                        .map(|bytes| SelectionBoundary::new(page_index, region_index, bytes.end))
                })
        })
}

fn build_reader_selection(
    pages: &[SelectionPage],
    start: SelectionBoundary,
    end: SelectionBoundary,
) -> Option<ReaderSelection> {
    let mut ranges = Vec::new();
    let mut quote = String::new();
    let mut rects = Vec::new();
    for (page_index, (position, frame, offset_x)) in
        pages.iter().enumerate().take(end.page + 1).skip(start.page)
    {
        let first_region = if page_index == start.page {
            start.region
        } else {
            0
        };
        let last_region = if page_index == end.page {
            end.region
        } else {
            frame.interaction().text_region_count().saturating_sub(1)
        };
        for region_index in first_region..=last_region {
            let interaction = frame.interaction();
            let Some(visible) = interaction.visible_byte_range(region_index) else {
                continue;
            };
            let byte_start = if page_index == start.page && region_index == start.region {
                start.byte
            } else {
                visible.start
            };
            let byte_end = if page_index == end.page && region_index == end.region {
                end.byte
            } else {
                visible.end
            };
            let Some(fragment) = interaction.selection_fragment(region_index, byte_start..byte_end)
            else {
                continue;
            };
            let source_continues = ranges.last().is_some_and(|previous: &SourceRange| {
                previous.end.spine == fragment.range.start.spine
                    && previous.end.node == fragment.range.start.node
                    && previous.end.text_offset == fragment.range.start.text_offset
            });
            append_selection_quote(&mut quote, &fragment.quote, source_continues);
            push_source_range(&mut ranges, fragment.range);
            rects.extend(fragment.rects.into_iter().map(|rect| ReaderSelectionRect {
                position: *position,
                x: rect.x0 + *offset_x,
                y: rect.y0,
                width: rect.x1 - rect.x0,
                height: rect.y1 - rect.y0,
            }));
        }
    }
    (!ranges.is_empty() && !quote.trim().is_empty() && !rects.is_empty()).then_some(
        ReaderSelection {
            ranges,
            text: quote,
            rects,
        },
    )
}

fn frame_image_at(frame: &LayoutFrame, x: f32, y: f32) -> Option<&ImagePlacement> {
    frame.items.iter().rev().find_map(|item| {
        let PageItem::Image(image) = item else {
            return None;
        };
        (x >= image.x && x <= image.x + image.width && y >= image.y && y <= image.y + image.height)
            .then_some(image)
    })
}

fn reader_image(position: ReaderPosition, image: &ImagePlacement, offset_x: f32) -> ReaderImage {
    ReaderImage {
        position,
        x: image.x + offset_x,
        y: image.y,
        display_width: image.width,
        display_height: image.height,
        width: image.image.width,
        height: image.image.height,
        pixels: Arc::clone(&image.image.pixels),
    }
}

fn append_selection_quote(output: &mut String, value: &str, source_continues: bool) {
    if value.is_empty() {
        return;
    }
    if !source_continues
        && !output.is_empty()
        && output
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
        && value.chars().next().is_some_and(char::is_alphanumeric)
    {
        output.push(' ');
    }
    output.push_str(value);
}

fn push_source_range(ranges: &mut Vec<SourceRange>, range: SourceRange) {
    if let Some(previous) = ranges.last_mut()
        && previous.end.spine == range.start.spine
        && previous.end.node == range.start.node
        && previous.end.text_offset >= range.start.text_offset
    {
        if range.end.text_offset > previous.end.text_offset {
            previous.end = range.end;
        }
        return;
    }
    ranges.push(range);
}

fn flatten_toc(entries: &[TocEntry]) -> Vec<TocViewItem> {
    fn append(
        entries: &[TocEntry],
        depth: usize,
        ancestors: &[String],
        items: &mut Vec<TocViewItem>,
    ) {
        for (index, entry) in entries.iter().enumerate() {
            let id = ancestors
                .last()
                .map_or_else(|| index.to_string(), |parent| format!("{parent}/{index}"));
            items.push(TocViewItem {
                id: id.clone(),
                label: entry.label.clone(),
                target: entry.href.clone(),
                depth,
                ancestors: ancestors.to_vec(),
                has_children: !entry.children.is_empty(),
            });
            let mut child_ancestors = ancestors.to_vec();
            child_ancestors.push(id);
            append(&entry.children, depth + 1, &child_ancestors, items);
        }
    }

    let mut items = Vec::new();
    append(entries, 0, &[], &mut items);
    items
}

fn active_toc_item_for_location<'a>(
    items: &'a [TocViewItem],
    current_section_items: &[usize],
    preceding_section_items: &[usize],
    current_section: usize,
    current_segment: usize,
    current_page: usize,
    mut resolve: impl FnMut(&PublicationUrl) -> Option<ReaderPosition>,
) -> Option<&'a TocViewItem> {
    let mut first_in_current_section = None;
    let mut best = None;

    for &order in preceding_section_items {
        let Some(item) = items.get(order) else {
            continue;
        };
        let Some(position) = item.target.as_ref().and_then(&mut resolve) else {
            continue;
        };
        if position.section_index >= current_section {
            continue;
        }
        let key = (position.segment_index, position.page_index, order);
        if best.is_none_or(|(best_key, _)| key > best_key) {
            best = Some((key, item));
        }
    }

    for &order in current_section_items {
        let Some(item) = items.get(order) else {
            continue;
        };
        let Some(position) = item.target.as_ref().and_then(&mut resolve) else {
            continue;
        };
        let ReaderPosition {
            section_index,
            segment_index,
            page_index,
        } = position;
        if section_index != current_section {
            continue;
        }
        if first_in_current_section.is_none() {
            first_in_current_section = Some(item);
        }
        if (segment_index, page_index) > (current_segment, current_page) {
            continue;
        }
        let key = (segment_index, page_index, order);
        if best.is_none_or(|(best_key, _)| key > best_key) {
            best = Some((key, item));
        }
    }

    best.map(|(_, item)| item).or(first_in_current_section)
}

fn total_progression(location: ReaderLocation, section_count: usize) -> f64 {
    let to_f64 = |value: usize| f64::from(u32::try_from(value).unwrap_or(u32::MAX));
    let section_count = to_f64(section_count.max(1));
    let segment_count = to_f64(location.segment_count.max(1));
    let page_count = to_f64(location.page_count.max(1));
    let segment_progress = (to_f64(location.segment_index)
        + to_f64(location.page_index + 1) / page_count)
        / segment_count;
    ((to_f64(location.section_index) + segment_progress) / section_count).clamp(0.0, 1.0)
}

fn frame_image_bounds(frame: &LayoutFrame) -> Option<(f32, f32)> {
    frame
        .items
        .iter()
        .filter_map(|item| {
            let PageItem::Image(image) = item else {
                return None;
            };
            Some((image.x, image.x + image.width))
        })
        .reduce(|left, right| (left.0.min(right.0), left.1.max(right.1)))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "layout viewports are bounded logical dimensions represented as f32 during composition"
)]
fn resolve_frame_spread_offsets(
    primary: &LayoutFrame,
    secondary: Option<&LayoutFrame>,
    default_secondary_offset_x: f32,
    compact_images: bool,
) -> (f32, f32) {
    let Some(secondary) = secondary.filter(|_| compact_images) else {
        return (0.0, default_secondary_offset_x);
    };
    let (Some(primary_bounds), Some(secondary_bounds)) =
        (frame_image_bounds(primary), frame_image_bounds(secondary))
    else {
        return (0.0, default_secondary_offset_x);
    };
    let viewport_width = primary.viewport.width as f32;
    let primary_offset_x = f32::midpoint(
        viewport_width - primary_bounds.0 - primary_bounds.1 - secondary_bounds.1,
        secondary_bounds.0,
    );
    let secondary_offset_x = primary_bounds.1 + primary_offset_x - secondary_bounds.0;
    if !primary_offset_x.is_finite() || !secondary_offset_x.is_finite() {
        return (0.0, default_secondary_offset_x);
    }
    (primary_offset_x, secondary_offset_x)
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
fn logical_coordinate(value: impl Into<f64>) -> f32 {
    value.into() as f32
}

// Page counts are bounded by the pages that fit in memory, so they remain far
// below f32's exact-integer limit. These conversions intentionally map the
// discrete page index to and from a normalized viewport-resize progress value.
#[allow(clippy::cast_precision_loss)]
fn page_fraction(page: usize, count: usize) -> f32 {
    if count <= 1 {
        0.0
    } else {
        page as f32 / (count - 1) as f32
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn page_for_fraction(fraction: f32, count: usize) -> usize {
    if count <= 1 {
        0
    } else {
        (fraction.clamp(0.0, 1.0) * (count - 1) as f32).round() as usize
    }
}

#[derive(Debug, Error)]
pub enum ReaderError {
    #[error("publication has no readable sections")]
    EmptyBook,
    #[error("section index is outside the reading order: {0}")]
    SectionOutOfBounds(usize),
    #[error("layout segment {segment} is outside section {section}")]
    SegmentOutOfBounds { section: usize, segment: usize },
    #[error("logical page is outside the compiled reader cache: {0:?}")]
    PageOutOfBounds(ReaderPosition),
    #[error("navigation target is not in the reading order: {0}")]
    NavigationTargetNotFound(String),
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error("failed to start the section prefetch worker: {0}")]
    PrefetchWorkerStart(std::io::Error),
    #[error("section prefetch worker stopped unexpectedly")]
    PrefetchWorkerStopped,
    #[error("parsed section repository lock is poisoned")]
    SectionRepositoryPoisoned,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use rebook_layout::{ReaderDefaultFont, SpreadMode};
    use rebook_publication::{
        Block, BlockStyle, FixedPageDimensions, ImageBlock, ImageStyle, Inline, LinkRole, Metadata,
        PublicationId, PublicationUrl, RasterResource, Resource, Section, SectionAnchor,
        SourceAnchor, SourceRange, SpineItem, SpineItemId, TextBlock, TextBlockKind, TextRun,
        TextStyle, TocEntry,
    };

    use super::*;

    #[test]
    fn focus_presentation_applies_the_shared_layout_contract() {
        let policy =
            ReaderPresentationPolicy::resolve(ReadingMode::Focus, SpreadMode::Double, true);
        let mut style = ReaderStyle::default();

        policy.apply_to_style(&mut style);

        assert_eq!(policy.mode, ReadingMode::Focus);
        assert_eq!(policy.spread, SpreadMode::Scroll);
        assert_eq!(style.spread, SpreadMode::Scroll);
        assert!(style.focus_footnote_icons);
        assert!((style.minimum_paragraph_gap - FOCUS_MINIMUM_PARAGRAPH_GAP).abs() < f32::EPSILON);
    }

    #[test]
    fn unsupported_focus_falls_back_to_the_requested_classic_spread() {
        let policy =
            ReaderPresentationPolicy::resolve(ReadingMode::Focus, SpreadMode::Double, false);
        let mut style = ReaderStyle::default();

        policy.apply_to_style(&mut style);

        assert_eq!(policy.mode, ReadingMode::Classic);
        assert_eq!(style.spread, SpreadMode::Double);
        assert!(!style.focus_footnote_icons);
        assert!(style.minimum_paragraph_gap.abs() < f32::EPSILON);
    }

    #[test]
    fn unified_focus_keeps_flow_owned_paragraph_spacing() {
        let policy =
            ReaderPresentationPolicy::resolve(ReadingMode::Focus, SpreadMode::Single, true);
        let mut style = ReaderStyle::default();
        style.typesetting.mode = TypesettingMode::Unified;

        policy.apply_to_style(&mut style);

        assert_eq!(style.spread, SpreadMode::Scroll);
        assert!(style.minimum_paragraph_gap.abs() < f32::EPSILON);
    }

    struct CountingTextEngine {
        shape_calls: Arc<AtomicUsize>,
        inner: LegacyParleyTextEngine,
    }

    impl TextEngine for CountingTextEngine {
        fn register_font(&mut self, font: &rebook_layout::text::TextFontBlob) {
            self.inner.register_font(font);
        }

        fn shape(
            &mut self,
            request: &rebook_layout::text::TextLayoutRequest<'_>,
        ) -> rebook_layout::text::TextLayoutStore {
            self.shape_calls.fetch_add(1, Ordering::Relaxed);
            self.inner.shape(request)
        }
    }

    struct CountingSource {
        book: Book,
        sections: Vec<Section>,
        parse_counts: Vec<AtomicUsize>,
        background_delay: Duration,
    }

    struct LazyFixedSource {
        book: Book,
        parse_counts: Vec<AtomicUsize>,
        raster_counts: Vec<AtomicUsize>,
    }

    impl LazyFixedSource {
        fn new(section_count: usize) -> Arc<Self> {
            let sections = (0..section_count)
                .map(|index| SpineItem {
                    id: SpineItemId::new(format!("page-{index}")).unwrap(),
                    href: PublicationUrl::parse(&format!("page-{index}.xhtml")).unwrap(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                })
                .collect::<Vec<_>>();
            let first = sections[0].href.clone();
            Arc::new(Self {
                book: Book {
                    id: PublicationId::new("lazy-fixed-reader-test").unwrap(),
                    metadata: Metadata {
                        title: "Lazy fixed pages".into(),
                        authors: Vec::new(),
                        languages: Vec::new(),
                        layout: RenditionLayout::PrePaginated,
                    },
                    sections,
                    table_of_contents: vec![TocEntry {
                        label: "Whole chapter".into(),
                        href: Some(first),
                        children: Vec::new(),
                    }],
                    cover: None,
                },
                parse_counts: (0..section_count).map(|_| AtomicUsize::new(0)).collect(),
                raster_counts: (0..section_count).map(|_| AtomicUsize::new(0)).collect(),
            })
        }

        fn href(index: usize) -> PublicationUrl {
            PublicationUrl::parse(&format!("images/page-{index}.rgba")).unwrap()
        }
    }

    impl BookSource for LazyFixedSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            self.parse_counts[index].fetch_add(1, Ordering::Relaxed);
            let descriptor = &self.book.sections[index];
            Ok(Section {
                id: descriptor.id.clone(),
                href: descriptor.href.clone(),
                blocks: vec![Block::Image(ImageBlock {
                    href: Self::href(index),
                    alt: format!("Page {}", index + 1),
                    style: ImageStyle::default(),
                    source: None,
                    text_layer: None,
                })],
                anchors: Vec::new(),
            })
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }

        fn raster_resource(
            &self,
            href: &PublicationUrl,
        ) -> Result<Option<RasterResource>, PublicationError> {
            let index = href
                .path()
                .strip_prefix("images/page-")
                .and_then(|value| value.strip_suffix(".rgba"))
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| PublicationError::ResourceNotFound(href.to_string()))?;
            self.raster_counts[index].fetch_add(1, Ordering::Relaxed);
            Ok(Some(RasterResource {
                width: 2,
                height: 3,
                pixels: vec![255; 2 * 3 * 4].into(),
            }))
        }

        fn fixed_page_dimensions(
            &self,
            section_index: usize,
        ) -> Result<Option<FixedPageDimensions>, PublicationError> {
            self.book
                .sections
                .get(section_index)
                .ok_or(PublicationError::ResourceNotFound(format!(
                    "section {section_index}"
                )))?;
            Ok(Some(FixedPageDimensions {
                width: 2,
                height: 3,
            }))
        }
    }

    impl CountingSource {
        fn new(texts: &[String]) -> Arc<Self> {
            Self::with_background_delay(texts, Duration::ZERO)
        }

        fn with_background_delay(texts: &[String], background_delay: Duration) -> Arc<Self> {
            let mut descriptors = Vec::with_capacity(texts.len());
            let mut sections = Vec::with_capacity(texts.len());
            for (index, text) in texts.iter().enumerate() {
                let id = SpineItemId::new(format!("section-{index}")).unwrap();
                let href = PublicationUrl::parse(&format!("section-{index}.xhtml")).unwrap();
                let text_len = u64::try_from(text.chars().count()).unwrap();
                descriptors.push(SpineItem {
                    id: id.clone(),
                    href: href.clone(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                });
                sections.push(Section {
                    id: id.clone(),
                    href,
                    blocks: vec![Block::Text(TextBlock {
                        kind: TextBlockKind::Paragraph,
                        content: vec![Inline::Text(TextRun {
                            text: text.clone(),
                            style: TextStyle::default(),
                            link: None,
                        })],
                        style: BlockStyle::default(),
                        source: Some(SourceRange {
                            start: SourceAnchor {
                                spine: id.clone(),
                                node: "paragraph-0".into(),
                                text_offset: 0,
                            },
                            end: SourceAnchor {
                                spine: id.clone(),
                                node: "paragraph-0".into(),
                                text_offset: text_len,
                            },
                        }),
                    })],
                    anchors: Vec::new(),
                });
            }

            Arc::new(Self {
                book: Book {
                    id: PublicationId::new("reader-test").unwrap(),
                    metadata: Metadata::default(),
                    cover: None,
                    sections: descriptors,
                    table_of_contents: Vec::new(),
                },
                parse_counts: (0..sections.len()).map(|_| AtomicUsize::new(0)).collect(),
                sections,
                background_delay,
            })
        }

        fn parse_count(&self, index: usize) -> usize {
            self.parse_counts[index].load(Ordering::Relaxed)
        }
    }

    impl BookSource for CountingSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            if index > 0 {
                thread::sleep(self.background_delay);
            }
            let section =
                self.sections.get(index).cloned().ok_or_else(|| {
                    PublicationError::ResourceNotFound(format!("section {index}"))
                })?;
            self.parse_counts[index].fetch_add(1, Ordering::Relaxed);
            Ok(section)
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    #[test]
    fn application_text_engine_shapes_every_synchronously_loaded_segment() {
        let source = CountingSource::new(&[
            "The first section uses the application-owned shaper.".repeat(12),
            "The second section must use that same shaper after navigation.".repeat(12),
        ]);
        let shape_calls = Arc::new(AtomicUsize::new(0));
        let text_engine = CountingTextEngine {
            shape_calls: Arc::clone(&shape_calls),
            inner: LegacyParleyTextEngine::default(),
        };
        let mut reader = ReaderSession::open_with_text_engine(
            source,
            viewport(600, 400),
            ReaderStyle::default(),
            text_engine,
        )
        .unwrap();

        let first_section_calls = shape_calls.load(Ordering::Relaxed);
        assert!(first_section_calls > 0);
        assert!(reader.prefetch_worker.is_none());

        reader.go_to_section(1).unwrap();
        assert!(shape_calls.load(Ordering::Relaxed) > first_section_calls);
        assert_eq!(reader.location().section_index, 1);
    }

    struct SwitchingSource {
        original_book: Book,
        original_sections: Vec<Section>,
        derived_book: Book,
        derived_sections: Vec<Section>,
        derived: AtomicBool,
    }

    impl SwitchingSource {
        fn new(original: &Arc<CountingSource>, derived: &Arc<CountingSource>) -> Arc<Self> {
            Arc::new(Self {
                original_book: original.book.clone(),
                original_sections: original.sections.clone(),
                derived_book: derived.book.clone(),
                derived_sections: derived.sections.clone(),
                derived: AtomicBool::new(false),
            })
        }

        fn set_derived(&self, derived: bool) {
            self.derived.store(derived, Ordering::Release);
        }

        fn active(&self) -> (&Book, &[Section]) {
            if self.derived.load(Ordering::Acquire) {
                (&self.derived_book, &self.derived_sections)
            } else {
                (&self.original_book, &self.original_sections)
            }
        }
    }

    impl BookSource for SwitchingSource {
        fn book(&self) -> &Book {
            self.active().0
        }

        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            self.active()
                .1
                .get(index)
                .cloned()
                .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    fn viewport(width: u32, height: u32) -> LayoutViewport {
        LayoutViewport::new(width, height).unwrap()
    }

    fn image_frame(image_x: f32) -> LayoutFrame {
        LayoutFrame::freeze(rebook_layout::PageLayout {
            viewport: viewport(1_200, 700),
            background: rebook_publication::Rgba::BLACK,
            leading_gap: 0.0,
            items: vec![rebook_layout::PageItem::Image(
                rebook_layout::ImagePlacement {
                    image: rebook_layout::RasterImage {
                        width: 400,
                        height: 600,
                        pixels: vec![255; 400 * 600 * 4].into(),
                    },
                    x: image_x,
                    y: 0.0,
                    width: 400.0,
                    height: 600.0,
                    source: None,
                    text_layer: None,
                    replacement: None,
                },
            )],
        })
    }

    fn scroll_image_frame(page_index: usize, leading_gap: f32) -> ReaderSectionFrame {
        let frame = LayoutFrame::freeze(rebook_layout::PageLayout {
            viewport: viewport(600, 400),
            background: rebook_publication::Rgba::BLACK,
            leading_gap,
            items: vec![rebook_layout::PageItem::Image(
                rebook_layout::ImagePlacement {
                    image: rebook_layout::RasterImage {
                        width: 100,
                        height: 100,
                        pixels: vec![255; 100 * 100 * 4].into(),
                    },
                    x: 50.0,
                    y: 40.0,
                    width: 100.0,
                    height: 100.0,
                    source: None,
                    text_layer: None,
                    replacement: None,
                },
            )],
        });
        ReaderSectionFrame {
            position: ReaderPosition {
                section_index: 0,
                segment_index: 0,
                page_index,
            },
            frame: Arc::new(frame),
            placeholder: false,
            visible_top: None,
            visible_bottom: None,
        }
    }

    #[test]
    fn compact_image_spread_touches_and_centers_page_edges() {
        let primary = image_frame(150.0);
        let secondary = image_frame(150.0);
        let (primary_offset, secondary_offset) =
            resolve_frame_spread_offsets(&primary, Some(&secondary), 600.0, true);
        let primary_bounds = frame_image_bounds(&primary).unwrap();
        let secondary_bounds = frame_image_bounds(&secondary).unwrap();
        let primary_left = primary_bounds.0 + primary_offset;
        let primary_right = primary_bounds.1 + primary_offset;
        let secondary_left = secondary_bounds.0 + secondary_offset;
        let secondary_right = secondary_bounds.1 + secondary_offset;

        assert!((primary_right - secondary_left).abs() < f32::EPSILON);
        assert!((f32::midpoint(primary_left, secondary_right) - 600.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scroll_layout_stitches_neutral_frames_and_restores_leading_gap() {
        let layout = ReaderScrollLayout::new(
            0,
            0,
            vec![scroll_image_frame(0, 0.0), scroll_image_frame(1, 20.0)],
            false,
        );

        assert!((layout.content_height() - 260.0).abs() < f32::EPSILON);
        assert!(layout.frames()[0].top.abs() < f32::EPSILON);
        assert!(layout.frames()[0].origin_y.abs() < f32::EPSILON);
        assert!((layout.frames()[0].height - 160.0).abs() < f32::EPSILON);
        assert!((layout.frames()[1].top - 160.0).abs() < f32::EPSILON);
        assert!((layout.frames()[1].origin_y - 40.0).abs() < f32::EPSILON);
        assert!((layout.frames()[1].height - 100.0).abs() < f32::EPSILON);
        let (index, frame_y) = layout.frame_at_content_y(170.0).unwrap();
        assert_eq!(index, 1);
        assert!((frame_y - 50.0).abs() < f32::EPSILON);
        assert_eq!(
            layout.visible_frames(150.0, 20.0),
            [
                ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index: 0,
                },
                ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index: 1,
                },
            ]
        );
    }

    #[test]
    fn cached_page_turns_and_boundaries_do_not_reparse() {
        let source = CountingSource::new(&["缓存翻页测试。".repeat(600)]);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        assert!(reader.location().page_count > 2);
        assert_eq!(source.parse_count(0), 1);

        assert!(matches!(
            reader.turn_page(PageDirection::Previous).unwrap().outcome,
            NavigationOutcome::Boundary
        ));
        let mut moved = 0;
        loop {
            let result = reader.turn_page(PageDirection::Next).unwrap();
            if result.outcome == NavigationOutcome::Boundary {
                break;
            }
            moved += 1;
            assert!(moved < 10_000);
        }
        assert!(moved > 2);
        assert_eq!(source.parse_count(0), 1);
    }

    #[test]
    fn continuous_section_pages_cover_every_segment_and_update_visible_position() {
        let source = CountingSource::new(&["连续章节滑动测试。".repeat(1_500)]);
        let mut reader = ReaderSession::open(
            source.clone(),
            viewport(600, 400),
            ReaderStyle {
                spread: SpreadMode::Scroll,
                ..ReaderStyle::default()
            },
        )
        .unwrap();
        let initial = reader.location();
        let pages = reader.current_section_frames().unwrap();

        assert!(pages.len() >= initial.page_count);
        assert!(pages.len() > 1);
        assert_eq!(
            pages
                .iter()
                .map(|entry| entry.position.segment_index)
                .collect::<HashSet<_>>()
                .len(),
            initial.segment_count,
        );
        assert!(pages.windows(2).all(|pair| {
            let left = pair[0].position;
            let right = pair[1].position;
            (left.section_index, left.segment_index, left.page_index)
                < (right.section_index, right.segment_index, right.page_index)
        }));
        assert_eq!(source.parse_count(0), 1);

        let last = pages.last().unwrap().position;
        let snapshot = reader.set_visible_position(last).unwrap();
        assert_eq!(snapshot.location.section_index, last.section_index);
        assert_eq!(snapshot.location.segment_index, last.segment_index);
        assert_eq!(snapshot.location.page_index, last.page_index);
    }

    #[test]
    fn native_selection_round_trips_to_source_ranges_and_geometry() {
        let source = CountingSource::new(&["选择文字行为".into()]);
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let selected_source = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 4,
            },
        };
        let rect = reader
            .current_layout_frame()
            .interaction()
            .source_rects(std::slice::from_ref(&selected_source))[0];
        let first_character = SourceRange {
            start: selected_source.start.clone(),
            end: SourceAnchor {
                text_offset: 1,
                ..selected_source.start.clone()
            },
        };
        let last_character = SourceRange {
            start: SourceAnchor {
                text_offset: 3,
                ..selected_source.start.clone()
            },
            end: selected_source.end.clone(),
        };
        let first_rect = reader
            .current_layout_frame()
            .interaction()
            .source_rects(std::slice::from_ref(&first_character))[0];
        let last_rect = reader
            .current_layout_frame()
            .interaction()
            .source_rects(std::slice::from_ref(&last_character))[0];
        let y = logical_coordinate(rect.center().1);
        let anchor = reader
            .hit_test_current_spread(logical_coordinate(first_rect.x1) - 0.1, y, true)
            .unwrap()
            .unwrap();
        let focus = reader
            .hit_test_current_spread(logical_coordinate(rect.x1) - 0.1, y, true)
            .unwrap()
            .unwrap();
        let selection = reader.selection_between(&anchor, &focus).unwrap().unwrap();

        assert_eq!(selection.text, "选择文字");
        assert!(!selection.ranges.is_empty());
        assert!(!selection.rects.is_empty());
        assert!(
            reader
                .source_ranges_contain_point(
                    &selection.ranges,
                    selection.rects[0].x + selection.rects[0].width / 2.0,
                    selection.rects[0].y + selection.rects[0].height / 2.0,
                )
                .unwrap()
        );

        let reverse_anchor = reader
            .hit_test_current_spread(logical_coordinate(last_rect.x0) + 0.1, y, true)
            .unwrap()
            .unwrap();
        let reverse_focus = reader
            .hit_test_current_spread(logical_coordinate(rect.x0) + 0.1, y, true)
            .unwrap()
            .unwrap();
        let reverse_selection = reader
            .selection_between(&reverse_anchor, &reverse_focus)
            .unwrap()
            .unwrap();
        assert_eq!(reverse_selection.text, "选择文字");
    }

    #[test]
    fn scroll_layout_owns_cross_frame_text_interaction_and_anchor_geometry() {
        let source = CountingSource::new(&["Alpha beta gamma.".into()]);
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let beta_range = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 6,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 10,
            },
        };
        let layout = reader.current_scroll_layout(false).unwrap();
        let position = layout.frames()[0].position;
        let frame = &layout.frames()[0];
        let rect = frame
            .frame
            .interaction()
            .source_rects(std::slice::from_ref(&beta_range))[0];
        let hit = layout
            .hit_test_text(
                0,
                logical_coordinate(rect.center().0),
                logical_coordinate(rect.center().1 - frame.origin_y),
                true,
            )
            .unwrap();
        let selected = reader
            .selection_between_with_granularity(&hit, &hit, SelectionGranularity::Word)
            .unwrap()
            .unwrap();
        assert_eq!(selected.text, "beta");

        let (anchor, focus) = layout.cursors_for_source_ranges(&selected.ranges).unwrap();
        assert_eq!(anchor.position(), position);
        assert_eq!(layout.cursor_range().unwrap().0.position(), position);
        assert!(
            layout
                .move_text_cursor(focus, TextCursorDirection::Previous)
                .is_some()
        );

        let expected_top = layout.content_y(0, rect.y0).unwrap();
        let actual_top = layout.source_anchor_top(&beta_range.start).unwrap();
        assert!((actual_top - expected_top).abs() < f32::EPSILON);

        let focus_geometry = layout
            .focus_geometry(
                std::slice::from_ref(&beta_range),
                Some(std::slice::from_ref(&beta_range)),
            )
            .unwrap();
        assert_eq!(focus_geometry.position, position);
        assert!((focus_geometry.bounds.y0 - expected_top).abs() < f32::EPSILON);
        assert!(focus_geometry.activation_bounds.is_some());

        let focus_layout = reader
            .current_focus_layout(
                &layout,
                Some(&beta_range.start),
                ReaderFocusLayoutPolicy::default(),
                |_| false,
            )
            .unwrap();
        assert_eq!(focus_layout.units.len(), 1);
        assert!(focus_layout.units[0].contains_anchor(&beta_range.start));
        assert_eq!(focus_layout.units[0].geometry.position, position);
        assert!(focus_layout.first_unit_after_anchor.is_some());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the linked-footnote fixture keeps the source reference, target anchor, and resolved focus assertion together"
    )]
    fn focus_layout_resolves_linked_footnotes_from_reading_ir() {
        let id = SpineItemId::new("section-0").unwrap();
        let href = PublicationUrl::parse("section-0.xhtml").unwrap();
        let range = |node: &str, end: u64| SourceRange {
            start: SourceAnchor {
                spine: id.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: id.clone(),
                node: node.into(),
                text_offset: end,
            },
        };
        let reference_range = range("paragraph", 8);
        let note_range = range("note", 17);
        let section = Section {
            id: id.clone(),
            href: href.clone(),
            blocks: vec![
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![
                        Inline::Text(TextRun {
                            text: "Body".into(),
                            style: TextStyle::default(),
                            link: None,
                        }),
                        Inline::Text(TextRun {
                            text: "[1]".into(),
                            style: TextStyle {
                                link_role: LinkRole::FootnoteReference,
                                ..TextStyle::default()
                            },
                            link: Some(PublicationUrl::parse("section-0.xhtml#note-1").unwrap()),
                        }),
                    ],
                    style: BlockStyle::default(),
                    source: Some(reference_range.clone()),
                }),
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![
                        Inline::Text(TextRun {
                            text: "[1]".into(),
                            style: TextStyle {
                                link_role: LinkRole::FootnoteBacklink,
                                ..TextStyle::default()
                            },
                            link: Some(PublicationUrl::parse("section-0.xhtml#paragraph").unwrap()),
                        }),
                        Inline::Text(TextRun {
                            text: " Footnote body".into(),
                            style: TextStyle::default(),
                            link: None,
                        }),
                    ],
                    style: BlockStyle::default(),
                    source: Some(note_range.clone()),
                }),
            ],
            anchors: vec![SectionAnchor {
                fragment: "note-1".into(),
                source: note_range.start.clone(),
            }],
        };
        let source = Arc::new(CountingSource {
            book: Book {
                id: PublicationId::new("focus-footnote-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: vec![SpineItem {
                    id,
                    href,
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            sections: vec![section],
            parse_counts: vec![AtomicUsize::new(0)],
            background_delay: Duration::ZERO,
        });
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let scroll = reader.current_scroll_layout(false).unwrap();
        let focus = reader
            .current_focus_layout(
                &scroll,
                Some(&reference_range.start),
                ReaderFocusLayoutPolicy::default(),
                |_| false,
            )
            .unwrap();

        assert_eq!(focus.units.len(), 1);
        assert_eq!(focus.units[0].footnotes.len(), 1);
        assert_eq!(focus.units[0].footnotes[0].marker.as_deref(), Some("[1]"));
        assert_eq!(
            focus.units[0].footnotes[0].text.as_deref(),
            Some("Footnote body")
        );
    }

    #[test]
    fn focus_viewport_policy_centers_short_units_and_aligns_tall_units() {
        let policy = ReaderFocusViewportPolicy::default();
        let short = FrameRect::new(0.0, 100.0, 500.0, 180.0);
        let tall = FrameRect::new(0.0, 400.0, 500.0, 1_200.0);

        assert_eq!(
            policy.unit_viewport_target(short, 600.0),
            ReaderFocusViewportTarget {
                content_y: 140.0,
                viewport_y: 300.0,
            }
        );
        assert_eq!(
            policy.unit_viewport_target(tall, 600.0),
            ReaderFocusViewportTarget {
                content_y: 400.0,
                viewport_y: 0.0,
            }
        );
        assert!((policy.unit_target_offset(short, 600.0) - 220.0).abs() < f32::EPSILON);
        assert!((policy.unit_target_offset(tall, 600.0) - 700.0).abs() < f32::EPSILON);
        assert_eq!(
            policy.nearest_unit_for_offset([short, tall], 680.0, 600.0),
            Some(1)
        );
    }

    #[test]
    fn focus_navigation_uses_durable_half_open_unit_identity() {
        let spine = SpineItemId::new("section-0").unwrap();
        let range = |node: &str, start, end| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: start,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: end,
            },
        };
        let position = ReaderPosition {
            section_index: 0,
            segment_index: 0,
            page_index: 0,
        };
        let units = [range("p1", 0, 5), range("p2", 0, 5)].map(|range| ReaderFocusUnit {
            paint_ranges: vec![range.clone()],
            range,
            geometry: ReaderFocusGeometry {
                position,
                bounds: FrameRect::new(0.0, 0.0, 100.0, 20.0),
                activation_bounds: None,
            },
            content_kind: ReaderFocusContentKind::Text,
            activation: ReaderFocusActivation::Inline,
            footnotes: Vec::new(),
        });
        let inside_second = SourceAnchor {
            spine: spine.clone(),
            node: "p2".into(),
            text_offset: 4,
        };
        let exclusive_end = SourceAnchor {
            spine,
            node: "p2".into(),
            text_offset: 5,
        };

        assert_eq!(
            resolve_focus_unit_index(&units, Some(&inside_second), None, position),
            1
        );
        assert!(!units[1].contains_anchor(&exclusive_end));
        assert_eq!(
            focus_move(0, units.len(), PageDirection::Next),
            ReaderFocusMove::Select(1)
        );
        assert_eq!(
            focus_move(1, units.len(), PageDirection::Next),
            ReaderFocusMove::Boundary
        );

        let mut state = ReaderFocusState::new(Some(inside_second));
        state.resolve(&units, None, position);
        assert_eq!(state.active_index(), 1);
        state.show_actions();
        assert_eq!(
            state.apply(&units, ReaderFocusCommand::Move(PageDirection::Previous)),
            ReaderFocusTransition::Selected(0)
        );
        assert!(state.actions_visible());
        state.show_footnotes();
        state.add_footnote_scroll_delta(32.0);
        assert_eq!(
            state.apply(&units, ReaderFocusCommand::Move(PageDirection::Next)),
            ReaderFocusTransition::Selected(1)
        );
        assert!(!state.footnotes_visible());
        assert!(state.take_footnote_scroll_delta().abs() < f32::EPSILON);
        assert_eq!(
            state.apply(&units, ReaderFocusCommand::Move(PageDirection::Next)),
            ReaderFocusTransition::Boundary(PageDirection::Next)
        );
    }

    struct FocusCandidateFixture {
        section: Section,
        reading_ranges: Vec<SourceRange>,
        heading: SourceRange,
        root: SourceRange,
        child: SourceRange,
        paragraph: SourceRange,
    }

    fn focus_candidate_fixture() -> FocusCandidateFixture {
        let spine = SpineItemId::new("section-0").unwrap();
        let range = |node: &str, end| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: end,
            },
        };
        let text_block = |kind, text: &str, source: SourceRange| {
            Block::Text(TextBlock {
                kind,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(source),
            })
        };
        let heading = range("heading", 7);
        let root = range("root", 4);
        let child = range("child", 5);
        let definition = range("note", 9);
        let paragraph = range("paragraph", 10);
        let backlink = PublicationUrl::parse("chapter.xhtml#reference").unwrap();
        let section = Section {
            id: spine,
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                text_block(TextBlockKind::Heading(1), "Heading", heading.clone()),
                text_block(
                    TextBlockKind::ListItem {
                        ordered: false,
                        ordinal: 1,
                        depth: 0,
                        marker_visible: true,
                    },
                    "Root",
                    root.clone(),
                ),
                text_block(
                    TextBlockKind::ListItem {
                        ordered: false,
                        ordinal: 1,
                        depth: 1,
                        marker_visible: true,
                    },
                    "Child",
                    child.clone(),
                ),
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: "1. Note".into(),
                        style: TextStyle {
                            link_role: LinkRole::FootnoteBacklink,
                            ..TextStyle::default()
                        },
                        link: Some(backlink),
                    })],
                    style: BlockStyle::default(),
                    source: Some(definition.clone()),
                }),
                text_block(TextBlockKind::Paragraph, "Structured", paragraph.clone()),
            ],
            anchors: Vec::new(),
        };
        let reading_ranges = vec![
            heading.clone(),
            root.clone(),
            child.clone(),
            definition,
            paragraph.clone(),
        ];
        FocusCandidateFixture {
            section,
            reading_ranges,
            heading,
            root,
            child,
            paragraph,
        }
    }

    #[test]
    fn focus_candidates_compile_reading_ir_before_geometry() {
        let FocusCandidateFixture {
            section,
            reading_ranges,
            heading,
            root,
            child,
            paragraph,
        } = focus_candidate_fixture();

        let candidates = compile_focus_candidates(&section, &reading_ranges, |range| {
            range.start.node == "paragraph"
        });

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].block_indices, [1, 2]);
        assert_eq!(candidates[0].text, "Root\nChild");
        assert_eq!(
            candidates[0].geometry_ranges,
            [heading, root.clone(), child.clone()]
        );
        assert_eq!(candidates[0].range.start, root.start);
        assert_eq!(candidates[0].range.end, child.end);
        assert_eq!(candidates[0].activation, ReaderFocusActivation::Inline);
        assert_eq!(candidates[1].activation, ReaderFocusActivation::Structured);
        assert_eq!(
            focus_candidate_index_for_anchor(
                &section,
                &candidates,
                Some(&SourceAnchor {
                    spine: section.id.clone(),
                    node: "heading".into(),
                    text_offset: 0,
                })
            ),
            Some(0)
        );
        assert_eq!(
            focus_candidate_index_for_anchor(&section, &candidates, Some(&paragraph.start)),
            Some(1)
        );
    }

    #[test]
    fn semantic_selection_expands_to_word_sentence_and_paragraph_boundaries() {
        let text = "Hello, world! 下一句。";
        let word_start = text.find("world").unwrap();
        let hit = ReaderTextHit {
            position: ReaderPosition {
                section_index: 0,
                segment_index: 0,
                page_index: 0,
            },
            region_index: 0,
            byte_index: word_start + 2,
            cluster_start: word_start + 1,
            cluster_end: word_start + 2,
        };

        assert_eq!(
            semantic_byte_range(text, 0..text.len(), &hit, SelectionGranularity::Word),
            Some(word_start..word_start + "world".len())
        );
        assert_eq!(
            semantic_byte_range(text, 0..text.len(), &hit, SelectionGranularity::Sentence),
            Some(0.."Hello, world!".len())
        );
        assert_eq!(
            semantic_byte_range(text, 0..text.len(), &hit, SelectionGranularity::Paragraph),
            Some(0..text.len())
        );
    }

    #[test]
    fn word_boundary_extension_includes_only_terminal_punctuation() {
        let sentence = "Alpha beta! Gamma.";
        let beta = sentence.find("beta").unwrap();
        assert_eq!(
            extend_word_to_sentence_or_paragraph_end(
                sentence,
                0..sentence.len(),
                beta..beta + "beta".len(),
            ),
            beta..beta + "beta!".len()
        );

        let middle = "Alpha beta, gamma.";
        let beta = middle.find("beta").unwrap();
        assert_eq!(
            extend_word_to_sentence_or_paragraph_end(
                middle,
                0..middle.len(),
                beta..beta + "beta".len(),
            ),
            beta..beta + "beta".len()
        );

        let quoted = "内容。” 下一句。";
        assert_eq!(
            extend_word_to_sentence_or_paragraph_end(quoted, 0..quoted.len(), 0.."内容".len(),),
            0.."内容。”".len()
        );
    }

    #[test]
    fn dragging_words_to_sentence_end_includes_punctuation_but_clicking_does_not() {
        let text = "Alpha beta! Gamma delta.";
        let source = CountingSource::new(&[text.into()]);
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let source_range = |start, end| SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: start,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: end,
            },
        };
        let hit_for = |reader: &mut ReaderSession, range: SourceRange| {
            let rect = reader
                .current_layout_frame()
                .interaction()
                .source_rects(&[range])[0];
            reader
                .hit_test_current_spread(
                    logical_coordinate(rect.center().0),
                    logical_coordinate(rect.center().1),
                    true,
                )
                .unwrap()
                .unwrap()
        };
        let alpha = hit_for(&mut reader, source_range(0, 1));
        let beta = hit_for(&mut reader, source_range(7, 8));

        let dragged = reader
            .selection_between_with_granularity(&alpha, &beta, SelectionGranularity::Word)
            .unwrap()
            .unwrap();
        assert_eq!(dragged.text, "Alpha beta!");

        let clicked = reader
            .selection_between_with_granularity(&beta, &beta, SelectionGranularity::Word)
            .unwrap()
            .unwrap();
        assert_eq!(clicked.text, "beta");
    }

    #[test]
    fn reader_text_cursors_move_and_restore_from_durable_ranges() {
        let text = "Alpha beta gamma.";
        let source = CountingSource::new(&[text.into()]);
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let beta_range = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 6,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 10,
            },
        };
        let rect = reader
            .current_layout_frame()
            .interaction()
            .source_rects(std::slice::from_ref(&beta_range))[0];
        let hit = reader
            .hit_test_current_spread(
                logical_coordinate(rect.center().0),
                logical_coordinate(rect.center().1),
                true,
            )
            .unwrap()
            .unwrap();
        let selected = reader
            .selection_between_with_granularity(&hit, &hit, SelectionGranularity::Word)
            .unwrap()
            .unwrap();
        assert_eq!(selected.text, "beta");

        let (anchor, focus) = reader
            .cursors_for_current_spread_source_ranges(&selected.ranges)
            .unwrap()
            .unwrap();
        assert_eq!(anchor.position(), focus.position());
        assert_eq!(
            reader
                .selection_between_cursors(anchor, focus)
                .unwrap()
                .unwrap()
                .text,
            "beta"
        );
        let previous = reader
            .move_text_cursor_current_spread(focus, TextCursorDirection::Previous)
            .unwrap()
            .unwrap();
        assert_eq!(
            reader
                .selection_between_cursors(anchor, previous)
                .unwrap()
                .unwrap()
                .text,
            "bet"
        );

        reader.resize(viewport(500, 400)).unwrap();
        let (restored_anchor, restored_focus) = reader
            .cursors_for_current_spread_source_ranges(&selected.ranges)
            .unwrap()
            .unwrap();
        assert_eq!(
            reader
                .selection_between_cursors(restored_anchor, restored_focus)
                .unwrap()
                .unwrap()
                .text,
            "beta"
        );
    }

    #[test]
    fn semantic_selection_on_the_right_page_keeps_its_spread_offset() {
        let source = CountingSource::new(&["word ".repeat(FRAGMENT_TEXT_BUDGET)]);
        let mut reader = ReaderSession::open(
            source,
            viewport(1_200, 700),
            ReaderStyle {
                spread: SpreadMode::Double,
                ..ReaderStyle::default()
            },
        )
        .unwrap();
        let spread = reader.current_frame_spread().unwrap();
        let secondary = spread.secondary.unwrap();
        let offset_x = spread.secondary_offset_x;
        let leading = secondary.interaction().leading_source_range().unwrap();
        let target = secondary
            .interaction()
            .source_rects(std::slice::from_ref(&leading))
            .into_iter()
            .next()
            .unwrap();
        let hit = reader
            .hit_test_current_spread(
                logical_coordinate(target.center().0) + offset_x,
                logical_coordinate(target.center().1),
                true,
            )
            .unwrap()
            .unwrap();

        let selection = reader
            .selection_between_with_granularity(&hit, &hit, SelectionGranularity::Word)
            .unwrap()
            .unwrap();

        assert!(
            selection
                .rects
                .iter()
                .all(|rect| rect.position == hit.position)
        );
        assert!(selection.rects.iter().all(|rect| rect.x >= offset_x));

        let drag_anchor = reader
            .hit_test_current_spread(
                logical_coordinate(target.x0) + offset_x + 1.0,
                logical_coordinate(target.center().1),
                false,
            )
            .unwrap()
            .unwrap();
        let drag_focus = reader
            .hit_test_current_spread(
                logical_coordinate(target.x1) + offset_x - 1.0,
                logical_coordinate(target.center().1),
                false,
            )
            .unwrap()
            .unwrap();
        let dragged = reader
            .selection_between(&drag_anchor, &drag_focus)
            .unwrap()
            .unwrap();

        assert_eq!(drag_anchor.position, drag_focus.position);
        assert!(dragged.rects.iter().all(|rect| rect.x >= offset_x));
    }

    #[test]
    fn paragraph_selection_covers_continuations_across_logical_pages() {
        let text = "alpha beta gamma delta. ".repeat(800);
        let source = CountingSource::new(std::slice::from_ref(&text));
        let style = ReaderStyle {
            spread: SpreadMode::Single,
            ..ReaderStyle::default()
        };
        let mut reader = ReaderSession::open(source, viewport(320, 180), style).unwrap();
        let first_character = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 1,
            },
        };
        let rect = reader
            .current_layout_frame()
            .interaction()
            .source_rects(std::slice::from_ref(&first_character))[0];
        let hit = reader
            .hit_test_current_spread(
                logical_coordinate(rect.center().0),
                logical_coordinate(rect.center().1),
                true,
            )
            .unwrap()
            .unwrap();

        let selection = reader
            .selection_between_with_granularity(&hit, &hit, SelectionGranularity::Paragraph)
            .unwrap()
            .unwrap();

        assert_eq!(selection.ranges.first().unwrap().start.text_offset, 0);
        assert_eq!(
            selection.ranges.last().unwrap().end.text_offset,
            u64::try_from(text.chars().count()).unwrap()
        );
        assert_eq!(selection.text, text);
        assert_ne!(
            selection.rects.first().unwrap().position,
            selection.rects.last().unwrap().position
        );
    }

    #[test]
    fn visible_text_fragments_follow_the_current_page() {
        let source = CountingSource::new(&["visible page text ".repeat(1_200)]);
        let style = ReaderStyle {
            spread: SpreadMode::Single,
            ..ReaderStyle::default()
        };
        let mut reader = ReaderSession::open(source, viewport(600, 400), style).unwrap();

        let first = reader.current_visible_text_fragments().unwrap();
        assert!(!first.is_empty());
        assert!(first.iter().all(|fragment| {
            fragment.position
                == ReaderPosition {
                    section_index: reader.location().section_index,
                    segment_index: reader.location().segment_index,
                    page_index: reader.location().page_index,
                }
        }));
        let first_ranges = first
            .iter()
            .map(|fragment| fragment.range.clone())
            .collect::<Vec<_>>();
        let first_position = first[0].position;
        assert_eq!(
            reader
                .visible_text_fragments_for_pages(&[first_position])
                .unwrap(),
            first
        );

        assert_eq!(
            reader.turn_page(PageDirection::Next).unwrap().outcome,
            NavigationOutcome::Moved
        );
        let second = reader.current_visible_text_fragments().unwrap();
        assert!(!second.is_empty());
        assert!(second.iter().all(|fragment| {
            fragment.position
                == ReaderPosition {
                    section_index: reader.location().section_index,
                    segment_index: reader.location().segment_index,
                    page_index: reader.location().page_index,
                }
        }));
        assert_ne!(
            first_ranges,
            second
                .iter()
                .map(|fragment| fragment.range.clone())
                .collect::<Vec<_>>()
        );
        let combined = reader
            .visible_text_fragments_for_pages(&[first_position, second[0].position])
            .unwrap();
        assert!(
            combined
                .iter()
                .any(|fragment| fragment.position == first_position)
        );
        assert!(
            combined
                .iter()
                .any(|fragment| fragment.position == second[0].position)
        );
    }

    #[test]
    fn durable_source_navigation_resolves_the_page_after_pagination() {
        let source = CountingSource::new(&["navigation target ".repeat(900)]);
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let anchor = SourceAnchor {
            spine: SpineItemId::new("section-0").unwrap(),
            node: "paragraph-0".into(),
            text_offset: 8_000,
        };

        reader.go_to_source(&anchor).unwrap();
        assert!(reader.location().page_index > 0);
        assert!(
            reader
                .current_layout_frame()
                .interaction()
                .contains_source_anchor(&anchor)
        );
    }

    #[test]
    fn durable_locator_restores_after_viewport_repagination() {
        let source = CountingSource::new(&["durable locator ".repeat(1_200)]);
        let mut first =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        first
            .go_to_source(&SourceAnchor {
                spine: SpineItemId::new("section-0").unwrap(),
                node: "paragraph-0".into(),
                text_offset: 9_000,
            })
            .unwrap();
        let locator = first.current_locator();
        let source_anchor = locator.source.as_ref().unwrap().start.clone();

        let mut restored =
            ReaderSession::open(source, viewport(820, 620), ReaderStyle::default()).unwrap();
        restored.restore_locator(&locator).unwrap();

        assert!(
            restored
                .current_layout_frame()
                .interaction()
                .contains_source_anchor(&source_anchor)
        );
        assert_eq!(
            restored.current_locator().publication_id,
            locator.publication_id
        );
    }

    #[test]
    fn opening_at_a_locator_skips_the_unneeded_first_section() {
        let source =
            CountingSource::new(&["first section".repeat(200), "resumed section".repeat(200)]);
        let locator = LocatorV1 {
            version: LocatorV1::VERSION,
            publication_id: source.book.id.clone(),
            href: source.book.sections[1].href.clone(),
            progression: Some(0.0),
            total_progression: Some(0.5),
            position: None,
            source: None,
            partial_cfi: None,
            text: None,
        };

        let reader = ReaderSession::open_with_fonts_at_locator(
            source.clone(),
            viewport(600, 400),
            ReaderStyle::default(),
            Arc::default(),
            &locator,
        )
        .unwrap();

        assert_eq!(reader.location().section_index, 1);
        assert_eq!(source.parse_count(0), 0);
        assert_eq!(source.parse_count(1), 1);
    }

    #[test]
    fn application_text_engine_opens_directly_at_a_locator() {
        let source =
            CountingSource::new(&["first section".repeat(200), "resumed section".repeat(200)]);
        let locator = LocatorV1 {
            version: LocatorV1::VERSION,
            publication_id: source.book.id.clone(),
            href: source.book.sections[1].href.clone(),
            progression: Some(0.0),
            total_progression: Some(0.5),
            position: None,
            source: None,
            partial_cfi: None,
            text: None,
        };

        let reader = ReaderSession::open_with_text_engine_at_locator(
            source.clone(),
            viewport(600, 400),
            ReaderStyle::default(),
            LegacyParleyTextEngine::default(),
            &locator,
        )
        .unwrap();

        assert_eq!(reader.location().section_index, 1);
        assert_eq!(source.parse_count(0), 0);
        assert_eq!(source.parse_count(1), 1);
    }

    #[test]
    fn oversized_text_block_is_split_into_stable_source_ranged_fragments() {
        let source = CountingSource::new(&["a".repeat(FRAGMENT_TEXT_BUDGET * 2 + 17)]);
        let mut section = source.sections[0].clone();
        let spine = section.id.clone();
        section.blocks[0] = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "a".repeat(FRAGMENT_TEXT_BUDGET * 2 + 17),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle::default(),
            source: Some(SourceRange {
                start: SourceAnchor {
                    spine: spine.clone(),
                    node: "n0".into(),
                    text_offset: 0,
                },
                end: SourceAnchor {
                    spine,
                    node: "n0".into(),
                    text_offset: u64::try_from(FRAGMENT_TEXT_BUDGET * 2 + 17).unwrap(),
                },
            }),
        });

        let prepared = prepare_section(section, &HashSet::new(), &[]);

        assert_eq!(prepared.fragments.len(), 3);
        assert_eq!(prepared.segments.len(), 1);
        assert_eq!(block_text_len(&prepared.fragments[0].blocks[0]), 4_096);
        assert_eq!(block_text_len(&prepared.fragments[1].blocks[0]), 4_096);
        assert_eq!(block_text_len(&prepared.fragments[2].blocks[0]), 17);
        let ranges = prepared
            .fragments
            .iter()
            .map(|fragment| block_source(&fragment.blocks[0]).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ranges[0].start.text_offset, 0);
        assert_eq!(ranges[0].end.text_offset, 4_096);
        assert_eq!(ranges[1].start.text_offset, 4_096);
        assert_eq!(ranges[1].end.text_offset, 8_192);
        assert_eq!(ranges[2].start.text_offset, 8_192);
        assert_eq!(ranges[2].end.text_offset, 8_209);
    }

    #[test]
    fn content_fragment_boundaries_never_commit_partial_pages() {
        let text_len = FRAGMENT_TEXT_BUDGET * 3 + 100;
        let source = CountingSource::new(&["a".repeat(text_len)]);
        let mut section = source.sections[0].clone();
        let spine = section.id.clone();
        let Block::Text(block) = &mut section.blocks[0] else {
            unreachable!();
        };
        block.source = Some(SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "n0".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "n0".into(),
                text_offset: u64::try_from(text_len).unwrap(),
            },
        });
        let prepared = prepare_section(section, &HashSet::new(), &[]);
        let segment = &prepared.segments[0];
        let fragments = prepared.fragments[segment.fragment_range.clone()]
            .iter()
            .map(|fragment| fragment.blocks.as_slice())
            .collect::<Vec<_>>();

        let layout = LayoutEngine::new()
            .layout_fragments(
                source.as_ref(),
                &fragments,
                viewport(600, 50_000),
                &ReaderStyle::default(),
            )
            .unwrap();

        assert_eq!(prepared.fragments.len(), 4);
        assert_eq!(prepared.segments.len(), 1);
        assert_eq!(layout.pages.len(), 1);
        let ranges = layout.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                PageItem::Text(placement) => placement.source.as_ref(),
                PageItem::Image(placement) => placement.source.as_ref(),
                PageItem::Quote(_) | PageItem::Table(_) | PageItem::Separator(_) => None,
            })
            .collect::<Vec<_>>();
        assert!(
            ranges
                .iter()
                .any(|range| range.end.text_offset == u64::try_from(FRAGMENT_TEXT_BUDGET).unwrap())
        );
        assert!(ranges.iter().any(|range| {
            range.start.text_offset == u64::try_from(FRAGMENT_TEXT_BUDGET).unwrap()
        }));
        assert!(
            ranges
                .iter()
                .any(|range| { range.end.text_offset == u64::try_from(text_len).unwrap() })
        );
    }

    #[test]
    fn continued_list_item_does_not_repeat_its_marker() {
        let parts = split_text_block(TextBlock {
            kind: TextBlockKind::ListItem {
                ordered: true,
                ordinal: 7,
                depth: 0,
                marker_visible: true,
            },
            content: vec![Inline::Text(TextRun {
                text: "item ".repeat(FRAGMENT_TEXT_BUDGET),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle {
                indent: 24.0,
                ..BlockStyle::default()
            },
            source: None,
        });

        assert!(parts.len() > 1);
        assert!(matches!(
            parts[0].kind,
            TextBlockKind::ListItem {
                ordered: true,
                ordinal: 7,
                depth: 0,
                marker_visible: true,
            }
        ));
        assert!(
            parts[1..]
                .iter()
                .all(|part| part.kind == TextBlockKind::Paragraph)
        );
        assert!(parts[1..].iter().all(|part| part.style.indent == 0.0));
    }

    #[test]
    fn page_turns_across_content_fragments_do_not_reparse_the_authored_section() {
        let source = CountingSource::new(&["long text ".repeat(FRAGMENT_TEXT_BUDGET)]);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        assert_eq!(reader.location().segment_count, 1);
        assert!(reader.location().page_count > 10);

        for _ in 0..10 {
            assert_eq!(
                reader.turn_page(PageDirection::Next).unwrap().outcome,
                NavigationOutcome::Moved
            );
        }
        assert_eq!(reader.location().segment_index, 0);
        assert_eq!(reader.location().page_index, 10);
        assert_eq!(source.parse_count(0), 1);

        assert_eq!(
            reader.turn_page(PageDirection::Previous).unwrap().outcome,
            NavigationOutcome::Moved
        );
        assert_eq!(reader.location().segment_index, 0);
        assert_eq!(reader.location().page_index, 9);
        assert_eq!(source.parse_count(0), 1);
    }

    #[test]
    fn double_spread_composes_adjacent_pages_from_one_continuous_section() {
        let source = CountingSource::new(&["long text ".repeat(FRAGMENT_TEXT_BUDGET)]);
        let mut reader = ReaderSession::open(
            source,
            viewport(1_200, 700),
            ReaderStyle {
                spread: rebook_layout::SpreadMode::Double,
                ..ReaderStyle::default()
            },
        )
        .unwrap();
        assert_eq!(reader.location().segment_count, 1);
        assert!(reader.current_page_count() > 2);
        reader.current_page = reader.current_page_count() - 2;

        let next = reader
            .next_position(reader.current_position())
            .unwrap()
            .expect("another logical page should follow");
        assert_eq!(next.section_index, 0);
        assert_eq!(next.segment_index, 0);
        let spread = reader.current_frame_spread().unwrap();
        assert!(spread.secondary.is_some());
    }

    #[test]
    fn segment_window_prefetch_makes_short_section_switches_cache_only() {
        let source = CountingSource::new(&["第一章".into(), "第二章".into(), "第三章".into()]);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();

        reader.prefetch_adjacent().unwrap();
        reader.wait_for_prefetch().unwrap();
        assert_eq!(reader.cached_segment_count(), 3);
        assert_eq!(source.parse_count(1), 1);
        assert_eq!(source.parse_count(2), 1);
        reader.turn_page(PageDirection::Next).unwrap();
        assert_eq!(reader.location().section_index, 1);
        assert_eq!(source.parse_count(1), 1);

        reader.prefetch_adjacent().unwrap();
        reader.wait_for_prefetch().unwrap();
        assert_eq!(reader.cached_segment_count(), 3);
        assert_eq!(source.parse_count(2), 1);
        reader.turn_page(PageDirection::Next).unwrap();
        assert_eq!(reader.location().section_index, 2);
        assert_eq!(source.parse_count(2), 1);
    }

    #[test]
    fn double_spread_composes_across_section_boundaries_without_repeating_pages() {
        let source = CountingSource::new(&[
            "left page".into(),
            "right page".into(),
            "next spread".into(),
        ]);
        let mut reader = ReaderSession::open(
            source.clone(),
            viewport(1_200, 700),
            ReaderStyle {
                spread: rebook_layout::SpreadMode::Double,
                ..ReaderStyle::default()
            },
        )
        .unwrap();

        let spread = reader.current_frame_spread().unwrap();
        let secondary = spread.secondary.as_ref().unwrap();
        let secondary_offset_x = spread.secondary_offset_x;
        assert!(!spread.primary.items.is_empty());
        assert!(
            spread
                .secondary
                .as_ref()
                .is_some_and(|page| !page.items.is_empty())
        );
        assert_eq!(source.parse_count(1), 1);
        assert_eq!(reader.current_spread_section_indices().unwrap(), [0, 1]);

        let leading = secondary.interaction().leading_source_range().unwrap();
        let target = secondary
            .interaction()
            .source_rects(std::slice::from_ref(&leading))
            .into_iter()
            .next()
            .unwrap();
        let hit = reader
            .hit_test_current_spread(
                logical_coordinate(target.x0) + secondary_offset_x + 1.0,
                logical_coordinate(target.center().1),
                true,
            )
            .unwrap()
            .unwrap();
        let selection = reader
            .selection_between_with_granularity(&hit, &hit, SelectionGranularity::Word)
            .unwrap()
            .unwrap();
        assert_eq!(selection.text, "right");
        assert!(
            selection
                .rects
                .iter()
                .all(|rect| rect.position.section_index == 1 && rect.x >= secondary_offset_x)
        );

        assert_eq!(
            reader.turn_page(PageDirection::Next).unwrap().outcome,
            NavigationOutcome::Moved
        );
        assert_eq!(reader.location().section_index, 2);
        assert_eq!(
            reader.turn_page(PageDirection::Previous).unwrap().outcome,
            NavigationOutcome::Moved
        );
        assert_eq!(reader.location().section_index, 0);
    }

    #[test]
    fn double_spread_prefetches_every_page_needed_by_the_next_spread() {
        let source = CountingSource::new(&[
            "page one".into(),
            "page two".into(),
            "page three".into(),
            "page four".into(),
            "page five".into(),
        ]);
        let mut reader = ReaderSession::open(
            source.clone(),
            viewport(1_200, 700),
            ReaderStyle {
                spread: rebook_layout::SpreadMode::Double,
                ..ReaderStyle::default()
            },
        )
        .unwrap();

        reader.prefetch_adjacent().unwrap();
        reader.wait_for_prefetch().unwrap();

        assert_eq!(source.parse_count(1), 1);
        assert_eq!(source.parse_count(2), 1);
        assert_eq!(source.parse_count(3), 1);
    }

    #[test]
    fn toc_href_navigation_resolves_segments_and_reuses_parsed_sections() {
        let mut source = CountingSource::new(&[
            "第一章".repeat(100),
            "第二章".repeat(100),
            "第三章".repeat(100),
        ]);
        let target_section = &mut Arc::get_mut(&mut source).unwrap().sections[1];
        let spine = target_section.id.clone();
        let source_range = |node: &str, length: u64| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.to_owned(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.to_owned(),
                text_offset: length,
            },
        };
        target_section.blocks = vec![
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: "目标之前的长正文。".repeat(2_000),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(source_range("n0", 18_000)),
            }),
            Block::Text(TextBlock {
                kind: TextBlockKind::Heading(2),
                content: vec![Inline::Text(TextRun {
                    text: "目录目标".to_owned(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(source_range("n1", 4)),
            }),
        ];
        target_section.anchors = vec![SectionAnchor {
            fragment: "part-2".to_owned(),
            source: source_range("n1", 4).start,
        }];
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        reader.prefetch_adjacent().unwrap();
        reader.wait_for_prefetch().unwrap();

        let target = PublicationUrl::parse("section-1.xhtml#part-2").unwrap();
        assert_eq!(reader.section_index_for_href(&target), Some(1));
        let target_location = reader.position_for_href(&target).unwrap();
        assert_eq!(target_location.section_index, 1);
        assert_eq!(target_location.segment_index, 0);
        reader.go_to_href(&target).unwrap();
        let resolved_location = reader.position_for_href(&target).unwrap();

        assert_eq!(reader.location().section_index, 1);
        assert_eq!(
            reader.location().segment_index,
            target_location.segment_index
        );
        assert_eq!(reader.location().page_index, resolved_location.page_index);
        assert!(reader.location().page_index > 0);
        assert_eq!(source.parse_count(1), 1);
    }

    #[test]
    fn explicit_toc_navigation_keeps_the_clicked_item_active_on_shared_pages() {
        let mut source = CountingSource::new(&["Shared page content".into()]);
        let source_mut = Arc::get_mut(&mut source).unwrap();
        let spine = source_mut.sections[0].id.clone();
        let source_range = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "n0".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "n0".into(),
                text_offset: 19,
            },
        };
        source_mut.sections[0].anchors = vec![
            SectionAnchor {
                fragment: "first".into(),
                source: source_range.start.clone(),
            },
            SectionAnchor {
                fragment: "second".into(),
                source: SourceAnchor {
                    text_offset: 1,
                    ..source_range.start
                },
            },
        ];
        source_mut.book.table_of_contents = vec![
            TocEntry {
                label: "First".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml#first").unwrap()),
                children: Vec::new(),
            },
            TocEntry {
                label: "Second".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml#second").unwrap()),
                children: Vec::new(),
            },
        ];
        let second_anchor = source_mut.sections[0].anchors[1].source.clone();
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();

        assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("1"));
        let result = reader
            .go_to_href(&PublicationUrl::parse("section-0.xhtml#first").unwrap())
            .unwrap();

        assert_eq!(result.snapshot.active_toc_id.as_deref(), Some("0"));
        assert_eq!(
            reader
                .source_anchor_for_href(&PublicationUrl::parse("section-0.xhtml#second").unwrap())
                .as_ref(),
            Some(&second_anchor)
        );
    }

    #[test]
    fn distant_anchor_navigation_resolves_within_continuous_section_layout() {
        let mut source = CountingSource::new(&["placeholder".into()]);
        let source_mut = Arc::get_mut(&mut source).unwrap();
        let spine = source_mut.sections[0].id.clone();
        let preceding_text_len = FRAGMENT_TEXT_BUDGET * 6 + 100;
        let source_range = |node: &str, length: usize| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: u64::try_from(length).unwrap(),
            },
        };
        source_mut.sections[0].blocks = vec![
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: "a ".repeat(preceding_text_len / 2),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(source_range("n0", preceding_text_len)),
            }),
            Block::Text(TextBlock {
                kind: TextBlockKind::Heading(2),
                content: vec![Inline::Text(TextRun {
                    text: "Target".into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(source_range("n1", 6)),
            }),
        ];
        source_mut.sections[0].anchors = vec![SectionAnchor {
            fragment: "target".into(),
            source: source_range("n1", 6).start,
        }];
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        let target = PublicationUrl::parse("section-0.xhtml#target").unwrap();
        let target_position = reader.position_for_href(&target).unwrap();
        assert_eq!(target_position.segment_index, 0);
        assert!(target_position.page_index > 0);

        reader.go_to_href(&target).unwrap();

        assert_eq!(reader.location().segment_index, 0);
        assert_eq!(reader.location().page_index, target_position.page_index);
        assert!(reader.cache.contains_key(&SegmentKey {
            section_index: 0,
            segment_index: 0,
        }));
        assert_eq!(reader.cache.len(), 1);
        assert_eq!(source.parse_count(0), 1);
    }

    #[test]
    fn large_single_file_books_segment_at_top_level_toc_boundaries() {
        let mut source = CountingSource::new(&["placeholder".into()]);
        let source_mut = Arc::get_mut(&mut source).unwrap();
        let spine = source_mut.sections[0].id.clone();
        let chapter_text_len = LARGE_SECTION_TEXT_BUDGET / 2;
        let source_range = |node: &str| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: u64::try_from(chapter_text_len).unwrap(),
            },
        };
        source_mut.sections[0].blocks = ["chapter-1", "chapter-2", "chapter-3"]
            .into_iter()
            .map(|node| {
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: "a".repeat(chapter_text_len),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(source_range(node)),
                })
            })
            .collect();
        source_mut.sections[0].anchors = ["chapter-2", "chapter-3"]
            .into_iter()
            .map(|fragment| SectionAnchor {
                fragment: fragment.into(),
                source: source_range(fragment).start,
            })
            .collect();
        source_mut.book.table_of_contents = vec![
            TocEntry {
                label: "Chapter 1".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml").unwrap()),
                children: Vec::new(),
            },
            TocEntry {
                label: "Chapter 2".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml#chapter-2").unwrap()),
                children: Vec::new(),
            },
            TocEntry {
                label: "Chapter 3".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml#chapter-3").unwrap()),
                children: Vec::new(),
            },
        ];

        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        assert_eq!(reader.location().segment_count, 3);

        let target = PublicationUrl::parse("section-0.xhtml#chapter-3").unwrap();
        reader.go_to_href(&target).unwrap();
        assert_eq!(reader.location().segment_index, 2);
        assert_eq!(reader.location().page_index, 0);
    }

    #[test]
    fn toc_and_total_progression_advance_across_page_boundaries() {
        let mut source = CountingSource::new(&["placeholder".into()]);
        let source_mut = Arc::get_mut(&mut source).unwrap();
        let spine = source_mut.sections[0].id.clone();
        let preceding_text_len = FRAGMENT_TEXT_BUDGET * 4 + 100;
        let source_range = |node: &str, length: u64| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: length,
            },
        };
        source_mut.sections[0].blocks = vec![
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: "a ".repeat(preceding_text_len / 2),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(source_range(
                    "n0",
                    u64::try_from(preceding_text_len).unwrap(),
                )),
            }),
            Block::Text(TextBlock {
                kind: TextBlockKind::Heading(2),
                content: vec![Inline::Text(TextRun {
                    text: "Later".into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(source_range("n1", 5)),
            }),
        ];
        source_mut.sections[0].anchors = vec![SectionAnchor {
            fragment: "later".into(),
            source: source_range("n1", 5).start,
        }];
        source_mut.book.table_of_contents = vec![
            TocEntry {
                label: "Start".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml").unwrap()),
                children: Vec::new(),
            },
            TocEntry {
                label: "Later".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml#later").unwrap()),
                children: Vec::new(),
            },
        ];
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("0"));
        let mut previous_progress = reader.snapshot().total_progression;

        for _ in 0..100 {
            let result = reader.turn_page(PageDirection::Next).unwrap();
            assert!(result.snapshot.total_progression > previous_progress);
            previous_progress = result.snapshot.total_progression;
            if result.snapshot.active_toc_id.as_deref() == Some("1") {
                assert_eq!(result.snapshot.location.segment_index, 1);
                assert_eq!(result.snapshot.location.page_index, 0);
                assert_eq!(source.parse_count(0), 1);
                return;
            }
        }
        panic!("reader did not reach the later fragment TOC anchor");
    }

    #[test]
    fn adjacent_prefetch_never_blocks_the_caller_thread() {
        let source = CountingSource::with_background_delay(
            &["第一章".into(), "第二章".into()],
            Duration::from_millis(300),
        );
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();

        let started = Instant::now();
        reader.prefetch_adjacent().unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));

        reader.wait_for_prefetch().unwrap();
        assert_eq!(source.parse_count(1), 1);
    }

    #[test]
    fn interactive_page_turn_waits_in_background_instead_of_blocking() {
        let blocking_source = CountingSource::with_background_delay(
            &["first".into(), "second".into()],
            Duration::from_millis(300),
        );
        let mut blocking_reader =
            ReaderSession::open(blocking_source, viewport(600, 400), ReaderStyle::default())
                .unwrap();
        let blocking_started = Instant::now();
        let blocking_result = blocking_reader.turn_page(PageDirection::Next).unwrap();
        let blocking_elapsed = blocking_started.elapsed();
        assert_eq!(blocking_result.outcome, NavigationOutcome::Moved);
        assert!(blocking_elapsed >= Duration::from_millis(250));

        let source = CountingSource::with_background_delay(
            &["first".into(), "second".into()],
            Duration::from_millis(300),
        );
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let nonblocking_started = Instant::now();
        let attempt = reader.try_turn_page(PageDirection::Next).unwrap();
        let nonblocking_elapsed = nonblocking_started.elapsed();

        assert_eq!(attempt, NavigationAttempt::Pending);
        assert!(nonblocking_elapsed < Duration::from_millis(100));
        assert!(blocking_elapsed > nonblocking_elapsed * 2);
        assert_eq!(reader.location().section_index, 0);

        reader.wait_for_prefetch().unwrap();
        let attempt = reader.try_turn_page(PageDirection::Next).unwrap();
        let NavigationAttempt::Ready(result) = attempt else {
            panic!("prefetched destination should be ready");
        };
        assert_eq!(result.outcome, NavigationOutcome::Moved);
        assert_eq!(result.snapshot.location.section_index, 1);
    }

    #[test]
    fn background_section_parse_does_not_block_snapshot_updates() {
        let mut source = CountingSource::with_background_delay(
            &["first".into(), "second".into()],
            Duration::from_millis(300),
        );
        Arc::get_mut(&mut source).unwrap().book.table_of_contents = vec![TocEntry {
            label: "Second".into(),
            href: Some(PublicationUrl::parse("section-1.xhtml").unwrap()),
            children: Vec::new(),
        }];
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        reader.prefetch_adjacent().unwrap();
        thread::sleep(Duration::from_millis(30));

        let started = Instant::now();
        let _snapshot = reader.snapshot();

        assert!(started.elapsed() < Duration::from_millis(100));
        reader.wait_for_prefetch().unwrap();
    }

    #[test]
    fn resize_rebuilds_layout_and_preserves_approximate_progress() {
        let source = CountingSource::new(&["调整窗口后保持阅读进度。".repeat(600)]);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        let old_count = reader.location().page_count;
        assert!(old_count > 4);
        for _ in 0..old_count / 2 {
            reader.turn_page(PageDirection::Next).unwrap();
        }
        let old_fraction = page_fraction(reader.location().page_index, old_count);

        reader.resize(viewport(500, 300)).unwrap();

        let location = reader.location();
        let new_fraction = page_fraction(location.page_index, location.page_count);
        let one_page = page_fraction(1, location.page_count);
        assert!((new_fraction - old_fraction).abs() <= one_page);
        assert_eq!(source.parse_count(0), 1);
        assert_eq!(reader.cached_segment_count(), 1);
    }

    #[test]
    fn font_family_change_rebuilds_layout_and_preserves_approximate_progress() {
        let source = CountingSource::new(&["字体切换后保持阅读进度。".repeat(600)]);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        let old_count = reader.location().page_count;
        assert!(old_count > 4);
        for _ in 0..old_count / 2 {
            reader.turn_page(PageDirection::Next).unwrap();
        }
        let old_fraction = page_fraction(reader.location().page_index, old_count);

        let mut style = reader.style();
        style.typography.default_font = ReaderDefaultFont::SansSerif;
        reader.set_style(style).unwrap();

        let location = reader.location();
        let new_fraction = page_fraction(location.page_index, location.page_count);
        let one_page = page_fraction(1, location.page_count);
        assert!((new_fraction - old_fraction).abs() <= one_page);
        assert_eq!(
            reader.style().typography.default_font,
            ReaderDefaultFont::SansSerif
        );
        assert_eq!(source.parse_count(0), 1);
        assert_eq!(reader.cached_segment_count(), 1);
    }

    #[test]
    fn source_refresh_reparses_and_preserves_approximate_progress() {
        let source = CountingSource::new(&["派生正文刷新后保持阅读进度。".repeat(600)]);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        let old_count = reader.location().page_count;
        for _ in 0..old_count / 2 {
            reader.turn_page(PageDirection::Next).unwrap();
        }
        let old_fraction = page_fraction(reader.location().page_index, old_count);

        reader.refresh_source().unwrap();

        let location = reader.location();
        let new_fraction = page_fraction(location.page_index, location.page_count);
        let one_page = page_fraction(1, location.page_count);
        assert!((new_fraction - old_fraction).abs() <= one_page);
        assert_eq!(source.parse_count(0), 2);
        assert_eq!(reader.cached_segment_count(), 1);
    }

    #[test]
    fn source_refresh_preserves_the_first_visible_source_anchor_after_repagination() {
        let original = CountingSource::new(&["stable anchor ".repeat(1_600)]);
        let derived = CountingSource::new(&["stable anchor ".repeat(2_400)]);
        let source = SwitchingSource::new(&original, &derived);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        for _ in 0..reader.location().page_count / 2 {
            reader.turn_page(PageDirection::Next).unwrap();
        }
        let anchor = reader
            .current_layout_frame()
            .interaction()
            .leading_source_range()
            .unwrap()
            .start;

        source.set_derived(true);
        reader.refresh_source().unwrap();

        assert!(
            reader
                .current_layout_frame()
                .interaction()
                .contains_source_anchor(&anchor)
        );
    }

    #[test]
    fn source_refresh_rebuilds_navigation_when_reading_order_changes() {
        let original = CountingSource::new(&[
            "Original page one".into(),
            "Original page two".into(),
            "Original page three".into(),
        ]);
        let derived = CountingSource::new(&["Continuous OCR section".into()]);
        let source = SwitchingSource::new(&original, &derived);
        let mut reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();

        source.set_derived(true);
        let derived_target = source.derived_book.sections[0].href.clone();
        let snapshot = reader
            .refresh_source_with_style_at_href(ReaderStyle::default(), Some(&derived_target))
            .unwrap();
        assert_eq!(snapshot.location.section_index, 0);
        assert_eq!(reader.section_index_for_href(&derived_target), Some(0));
        assert_eq!(reader.section_count(), 1);

        source.set_derived(false);
        let original_target = source.original_book.sections[2].href.clone();
        let snapshot = reader
            .refresh_source_with_style_at_href(ReaderStyle::default(), Some(&original_target))
            .unwrap();
        assert_eq!(snapshot.location.section_index, 2);
        assert_eq!(reader.section_index_for_href(&original_target), Some(2));
        assert_eq!(reader.section_count(), 3);
    }

    #[test]
    fn toc_items_preserve_reading_order_depth_and_ancestry() {
        let first_target = PublicationUrl::parse("text/chapter-1.xhtml").unwrap();
        let child_target = PublicationUrl::parse("text/chapter-1.xhtml#part-1").unwrap();
        let items = flatten_toc(&[
            TocEntry {
                label: "第一章".into(),
                href: Some(first_target.clone()),
                children: vec![TocEntry {
                    label: "第一节".into(),
                    href: Some(child_target.clone()),
                    children: Vec::new(),
                }],
            },
            TocEntry {
                label: "第二章".into(),
                href: None,
                children: Vec::new(),
            },
        ]);

        assert_eq!(items.len(), 3);
        assert_eq!((items[0].label.as_str(), items[0].depth), ("第一章", 0));
        assert_eq!(items[0].target.as_ref(), Some(&first_target));
        assert!(items[0].has_children);
        assert!(items[0].ancestors.is_empty());
        assert_eq!((items[1].label.as_str(), items[1].depth), ("第一节", 1));
        assert_eq!(items[1].target.as_ref(), Some(&child_target));
        assert_eq!(items[1].ancestors, ["0"]);
        assert_eq!((items[2].label.as_str(), items[2].depth), ("第二章", 0));
        assert!(items[2].target.is_none());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture mirrors one complete TOC hierarchy"
    )]
    fn chapter_prelude_and_leaf_targets_are_independent_reading_units() {
        let mut source = CountingSource::new(&["placeholder".into()]);
        let source_mut = Arc::get_mut(&mut source).unwrap();
        let spine = source_mut.sections[0].id.clone();
        let block = |node: &str, text: &str| {
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(SourceRange {
                    start: SourceAnchor {
                        spine: spine.clone(),
                        node: node.into(),
                        text_offset: 0,
                    },
                    end: SourceAnchor {
                        spine: spine.clone(),
                        node: node.into(),
                        text_offset: u64::try_from(text.chars().count()).unwrap(),
                    },
                }),
            })
        };
        source_mut.sections[0].blocks = vec![
            block("intro", "Introduction"),
            block("leaf-a", "First leaf"),
            block("leaf-b", "Second leaf"),
        ];
        source_mut.sections[0].anchors = vec![
            SectionAnchor {
                fragment: "chapter".into(),
                source: SourceAnchor {
                    spine: spine.clone(),
                    node: "intro".into(),
                    text_offset: 0,
                },
            },
            SectionAnchor {
                fragment: "leaf-a".into(),
                source: SourceAnchor {
                    spine: spine.clone(),
                    node: "leaf-a".into(),
                    text_offset: 0,
                },
            },
            SectionAnchor {
                fragment: "leaf-b".into(),
                source: SourceAnchor {
                    spine: spine.clone(),
                    node: "leaf-b".into(),
                    text_offset: 0,
                },
            },
        ];
        source_mut.book.table_of_contents = vec![TocEntry {
            label: "Chapter".into(),
            href: Some(PublicationUrl::parse("section-0.xhtml#chapter").unwrap()),
            children: vec![
                TocEntry {
                    label: "Leaf A".into(),
                    href: Some(PublicationUrl::parse("section-0.xhtml#leaf-a").unwrap()),
                    children: Vec::new(),
                },
                TocEntry {
                    label: "Leaf B".into(),
                    href: Some(PublicationUrl::parse("section-0.xhtml#leaf-b").unwrap()),
                    children: Vec::new(),
                },
            ],
        }];

        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        assert_eq!(
            reader.reading_unit_location(),
            ReadingUnitLocation { index: 0, count: 3 }
        );
        assert_eq!(reader.location().segment_count, 3);
        let prelude_ranges = reader.current_reading_unit_source_ranges().unwrap();
        assert_eq!(prelude_ranges.len(), 1);
        assert_eq!(prelude_ranges[0].start.node, "intro");

        let result = reader
            .go_to_adjacent_reading_unit(PageDirection::Next)
            .unwrap();
        assert_eq!(result.outcome, NavigationOutcome::Moved);
        assert_eq!(
            reader.reading_unit_location(),
            ReadingUnitLocation { index: 1, count: 3 }
        );
        let first_leaf_ranges = reader.current_reading_unit_source_ranges().unwrap();
        assert_eq!(first_leaf_ranges.len(), 1);
        assert_eq!(first_leaf_ranges[0].start.node, "leaf-a");
        assert_eq!(reader.location().segment_index, 1);

        reader
            .go_to_adjacent_reading_unit(PageDirection::Next)
            .unwrap();
        let second_leaf_ranges = reader.current_reading_unit_source_ranges().unwrap();
        assert_eq!(second_leaf_ranges.len(), 1);
        assert_eq!(second_leaf_ranges[0].start.node, "leaf-b");
        assert_eq!(reader.location().segment_index, 2);
    }

    #[test]
    fn heading_only_chapter_prelude_joins_the_first_subsection() {
        let spine = SpineItemId::new("chapter").unwrap();
        let block = |node: &str, kind: TextBlockKind, text: &str| {
            Block::Text(TextBlock {
                kind,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(SourceRange {
                    start: SourceAnchor {
                        spine: spine.clone(),
                        node: node.into(),
                        text_offset: 0,
                    },
                    end: SourceAnchor {
                        spine: spine.clone(),
                        node: node.into(),
                        text_offset: u64::try_from(text.chars().count()).unwrap(),
                    },
                }),
            })
        };
        let anchor = |fragment: &str, node: &str| SectionAnchor {
            fragment: fragment.into(),
            source: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
        };
        let fragments = vec![
            ContentFragment {
                blocks: vec![block("heading", TextBlockKind::Heading(1), "Prelude")],
                anchors: vec![anchor("chapter", "heading")],
            },
            ContentFragment {
                blocks: vec![block("body", TextBlockKind::Paragraph, "First subsection")],
                anchors: vec![anchor("leaf", "body")],
            },
        ];
        let units = build_reading_units(
            &[("chapter".into(), 0), ("leaf".into(), 1)],
            &[Some("chapter".into()), Some("leaf".into())],
            &fragments,
        );

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].fragment_range, 0..2);
        assert_eq!(units[0].start.as_ref().unwrap().node, "body");
    }

    #[test]
    fn fixed_layout_toc_units_span_complete_physical_pages() {
        let mut source = CountingSource::new(&[
            "Chapter title".into(),
            "Section one page one".into(),
            "Section one page two".into(),
            "Next chapter".into(),
            "Section two".into(),
        ]);
        let source_mut = Arc::get_mut(&mut source).unwrap();
        source_mut.book.metadata.layout = RenditionLayout::PrePaginated;
        let target = |index: usize| source_mut.book.sections[index].href.clone();
        source_mut.book.table_of_contents = vec![
            TocEntry {
                label: "Chapter 1".into(),
                href: Some(target(0)),
                children: vec![TocEntry {
                    label: "Section 1.1".into(),
                    href: Some(target(1)),
                    children: Vec::new(),
                }],
            },
            TocEntry {
                label: "Chapter 2".into(),
                href: Some(target(3)),
                children: vec![TocEntry {
                    label: "Section 2.1".into(),
                    href: Some(target(4)),
                    children: Vec::new(),
                }],
            },
        ];

        let mut reader = ReaderSession::open(
            source,
            viewport(600, 400),
            ReaderStyle {
                spread: SpreadMode::Scroll,
                ..ReaderStyle::default()
            },
        )
        .unwrap();
        assert_eq!(
            reader.reading_unit_location(),
            ReadingUnitLocation { index: 0, count: 4 }
        );
        assert_eq!(reader.current_reading_unit_frames().unwrap().len(), 1);

        reader
            .go_to_adjacent_reading_unit(PageDirection::Next)
            .unwrap();
        let pages = reader.current_reading_unit_frames().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].position.section_index, 1);
        assert_eq!(pages[1].position.section_index, 2);
        let snapshot = reader.set_visible_position(pages[1].position).unwrap();
        assert_eq!(snapshot.location.section_index, 2);
        assert_eq!(
            reader.reading_unit_location(),
            ReadingUnitLocation { index: 1, count: 4 }
        );
    }

    #[test]
    fn fixed_layout_scroll_units_materialize_only_nearby_pages() {
        let source = LazyFixedSource::new(3);
        let mut reader = ReaderSession::open(
            Arc::clone(&source) as Arc<dyn BookSource>,
            viewport(600, 400),
            ReaderStyle {
                spread: SpreadMode::Scroll,
                ..ReaderStyle::default()
            },
        )
        .unwrap();

        let pages = reader.current_reading_unit_frames().unwrap();
        assert_eq!(pages.len(), 3);
        assert!(!pages[0].placeholder);
        assert!(pages[1].placeholder);
        assert!(pages[2].placeholder);
        assert_eq!(source.parse_counts[0].load(Ordering::Relaxed), 1);
        assert_eq!(source.parse_counts[1].load(Ordering::Relaxed), 0);
        assert_eq!(source.parse_counts[2].load(Ordering::Relaxed), 0);
        assert_eq!(source.raster_counts[0].load(Ordering::Relaxed), 1);
        assert_eq!(source.raster_counts[1].load(Ordering::Relaxed), 0);

        let target = pages[1].position;
        assert!(!reader.try_materialize_position(target).unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !reader.try_materialize_position(target).unwrap() {
            assert!(Instant::now() < deadline, "background page did not finish");
            std::thread::yield_now();
        }

        let pages = reader.current_reading_unit_frames().unwrap();
        assert!(!pages[1].placeholder);
        assert!(pages[2].placeholder);
        assert_eq!(source.parse_counts[1].load(Ordering::Relaxed), 1);
        assert_eq!(source.raster_counts[1].load(Ordering::Relaxed), 1);
        assert_eq!(source.parse_counts[2].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn parent_only_toc_keeps_the_spine_as_one_reading_unit() {
        let mut source = CountingSource::new(&["content".into()]);
        Arc::get_mut(&mut source).unwrap().book.table_of_contents = vec![TocEntry {
            label: "Chapter".into(),
            href: Some(PublicationUrl::parse("section-0.xhtml#chapter").unwrap()),
            children: vec![TocEntry {
                label: "Non-navigable leaf".into(),
                href: None,
                children: Vec::new(),
            }],
        }];
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();

        assert_eq!(
            reader.reading_unit_location(),
            ReadingUnitLocation { index: 0, count: 1 }
        );
    }

    #[test]
    fn active_toc_follows_the_nearest_preceding_segment_page() {
        let items = vec![
            TocViewItem {
                id: "previous".into(),
                label: "Previous".into(),
                target: Some(PublicationUrl::parse("section-0.xhtml#previous").unwrap()),
                depth: 0,
                ancestors: Vec::new(),
                has_children: false,
            },
            TocViewItem {
                id: "chapter".into(),
                label: "Chapter".into(),
                target: Some(PublicationUrl::parse("section-1.xhtml#chapter").unwrap()),
                depth: 0,
                ancestors: Vec::new(),
                has_children: true,
            },
            TocViewItem {
                id: "subsection".into(),
                label: "Subsection".into(),
                target: Some(PublicationUrl::parse("section-1.xhtml#subsection").unwrap()),
                depth: 1,
                ancestors: vec!["chapter".into()],
                has_children: false,
            },
            TocViewItem {
                id: "future".into(),
                label: "Future".into(),
                target: Some(PublicationUrl::parse("section-2.xhtml#future").unwrap()),
                depth: 0,
                ancestors: Vec::new(),
                has_children: false,
            },
        ];
        let position = |section_index, segment_index, page_index| ReaderPosition {
            section_index,
            segment_index,
            page_index,
        };
        let resolve = |target: &PublicationUrl| match (target.path(), target.fragment()) {
            ("section-0.xhtml", _) => Some(position(0, 0, 4)),
            ("section-1.xhtml", Some("chapter")) => Some(position(1, 1, 2)),
            ("section-1.xhtml", Some("subsection")) => Some(position(1, 2, 0)),
            ("section-2.xhtml", _) => Some(position(2, 0, 0)),
            _ => None,
        };

        assert_eq!(
            active_toc_item_for_location(&items, &[1, 2], &[0], 1, 0, 1, resolve)
                .unwrap()
                .id,
            "previous"
        );
        assert_eq!(
            active_toc_item_for_location(&items, &[1, 2], &[0], 1, 1, 2, resolve)
                .unwrap()
                .id,
            "chapter"
        );
        assert_eq!(
            active_toc_item_for_location(&items, &[1, 2], &[0], 1, 2, 0, resolve)
                .unwrap()
                .id,
            "subsection"
        );
    }

    #[test]
    fn large_toc_snapshot_resolves_only_neighboring_sections() {
        let item_count = 2_034;
        let items = (0..item_count)
            .map(|index| TocViewItem {
                id: index.to_string(),
                label: format!("Chapter {index}"),
                target: Some(PublicationUrl::parse(&format!("section-{index}.xhtml")).unwrap()),
                depth: 0,
                ancestors: Vec::new(),
                has_children: false,
            })
            .collect::<Vec<_>>();
        let section_indices = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.target.as_ref().unwrap().path().to_owned(), index))
            .collect::<HashMap<_, _>>();
        let index = TocIndex::new(&items, &section_indices, item_count);
        let current_section = 1_500;
        let preceding_section = index.preceding_section_by_section[current_section].unwrap();
        let resolve_count = AtomicUsize::new(0);

        let active = active_toc_item_for_location(
            &items,
            &index.items_by_section[current_section],
            &index.items_by_section[preceding_section],
            current_section,
            0,
            0,
            |target| {
                resolve_count.fetch_add(1, Ordering::Relaxed);
                let section_index = section_indices.get(target.path()).copied()?;
                Some(ReaderPosition {
                    section_index,
                    segment_index: 0,
                    page_index: 0,
                })
            },
        )
        .unwrap();

        assert_eq!(active.id, current_section.to_string());
        assert_eq!(resolve_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn snapshot_owns_active_toc_state_and_progression() {
        let mut source = CountingSource::new(&["正文".repeat(600)]);
        Arc::get_mut(&mut source).unwrap().book.table_of_contents = vec![TocEntry {
            label: "Chapter".into(),
            href: Some(PublicationUrl::parse("section-0.xhtml").unwrap()),
            children: vec![TocEntry {
                label: "Child".into(),
                href: Some(PublicationUrl::parse("section-0.xhtml").unwrap()),
                children: Vec::new(),
            }],
        }];
        let reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();

        let snapshot = reader.snapshot();
        assert_eq!(snapshot.active_toc_id.as_deref(), Some("0/0"));
        assert_eq!(snapshot.active_toc_path, ["0"]);
        assert!(snapshot.total_progression > 0.0);
        assert!(snapshot.total_progression <= 1.0);
    }

    #[test]
    fn stale_prefetch_result_cannot_clear_current_generation_request() {
        let source = CountingSource::new(&["第一章".into(), "第二章".into()]);
        let mut reader =
            ReaderSession::open(source, viewport(600, 400), ReaderStyle::default()).unwrap();
        let worker = reader
            .prefetch_worker
            .as_ref()
            .expect("default reader session enables background prefetch");
        let stale_generation = worker.generation();
        let current_generation = worker.invalidate();
        let segment = SegmentKey {
            section_index: 1,
            segment_index: 0,
        };
        let current_key = PrefetchKey {
            generation: current_generation,
            segment,
        };
        reader.prefetch_inflight.insert(current_key);
        let section = Arc::clone(reader.current_section_data());

        reader.install_prefetch(PrefetchResult {
            key: segment,
            generation: stale_generation,
            segment: Ok(Arc::new(CachedSegment {
                section,
                frames: Vec::new(),
                #[cfg(feature = "legacy-renderer")]
                pages: Vec::new(),
                anchor_pages: HashMap::new(),
                visible_pages: 1,
                continuation_offset_x: 0.0,
            })),
        });

        assert!(reader.prefetch_inflight.contains(&current_key));
    }

    #[test]
    fn dropping_reader_joins_worker_and_releases_source() {
        let source = CountingSource::new(&["正文".into()]);
        let weak = Arc::downgrade(&source);
        let reader =
            ReaderSession::open(source.clone(), viewport(600, 400), ReaderStyle::default())
                .unwrap();
        drop(source);

        assert!(weak.upgrade().is_some());
        drop(reader);
        assert!(weak.upgrade().is_none());
    }
}
