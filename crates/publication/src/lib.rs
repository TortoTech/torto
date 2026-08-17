//! Format-neutral publication, resource, and locator contracts.

use std::fmt;
use std::sync::Arc;

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable identity for an imported publication, normally derived from its content hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PublicationId(String);

impl PublicationId {
    /// Creates a non-empty publication identity.
    pub fn new(value: impl Into<String>) -> Result<Self, PublicationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PublicationError::InvalidIdentifier);
        }
        Ok(Self(value))
    }

    /// Returns the serialized identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PublicationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable identity for a spine item inside one publication.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpineItemId(String);

impl SpineItemId {
    /// Creates a non-empty spine item identity.
    pub fn new(value: impl Into<String>) -> Result<Self, PublicationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PublicationError::InvalidIdentifier);
        }
        Ok(Self(value))
    }

    /// Returns the serialized identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Canonical URL for a resource inside a publication.
///
/// This type never represents a file-system or network URL. Paths are decoded once, use `/`
/// separators, and cannot escape the publication root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicationUrl {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fragment: Option<String>,
}

impl PublicationUrl {
    /// Parses and canonicalizes a root-relative publication path.
    pub fn parse(value: &str) -> Result<Self, PublicationError> {
        Self::resolve_from_segments(&[], value)
    }

    /// Resolves a relative reference against this resource URL.
    pub fn resolve(&self, reference: &str) -> Result<Self, PublicationError> {
        if let Some(reference) = reference.strip_prefix('#') {
            let fragment = decode_component(reference)?;
            return Ok(Self {
                path: self.path.clone(),
                fragment: non_empty(fragment),
            });
        }

        let segments = self.path.split('/').collect::<Vec<_>>();
        let base = segments
            .split_last()
            .map_or(&[][..], |(_, directory)| directory);
        Self::resolve_from_segments(base, reference)
    }

    /// Returns the canonical archive path without its fragment.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the decoded fragment, if present.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Returns a copy without a fragment for resource lookup.
    #[must_use]
    pub fn resource_url(&self) -> Self {
        Self {
            path: self.path.clone(),
            fragment: None,
        }
    }

    fn resolve_from_segments(base: &[&str], value: &str) -> Result<Self, PublicationError> {
        let (raw_path, raw_fragment) = value
            .split_once('#')
            .map_or((value, None), |parts| (parts.0, Some(parts.1)));
        let raw_path = raw_path.split_once('?').map_or(raw_path, |parts| parts.0);
        if raw_path.starts_with('/') || raw_path.starts_with('\\') {
            return Err(PublicationError::InvalidPublicationUrl(value.to_owned()));
        }
        if looks_like_external_scheme(raw_path) {
            return Err(PublicationError::ExternalUrl(value.to_owned()));
        }

        let decoded = decode_component(raw_path)?;
        if decoded.contains('\\') || decoded.contains('\0') {
            return Err(PublicationError::InvalidPublicationUrl(value.to_owned()));
        }

        let mut segments = base.iter().map(ToString::to_string).collect::<Vec<_>>();
        for segment in decoded.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    if segments.pop().is_none() {
                        return Err(PublicationError::PathEscapesRoot(value.to_owned()));
                    }
                }
                other => segments.push(other.to_owned()),
            }
        }
        if segments.is_empty() {
            return Err(PublicationError::InvalidPublicationUrl(value.to_owned()));
        }
        let fragment = raw_fragment
            .map(decode_component)
            .transpose()?
            .and_then(non_empty);
        Ok(Self {
            path: segments.join("/"),
            fragment,
        })
    }
}

impl fmt::Display for PublicationUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.path)?;
        if let Some(fragment) = &self.fragment {
            write!(formatter, "#{fragment}")?;
        }
        Ok(())
    }
}

/// Normalized metadata used by the shelf and reader.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    /// Primary title in display form.
    pub title: String,
    /// Ordered contributors that authored the publication.
    pub authors: Vec<String>,
    /// BCP 47 language tags declared by the publication.
    pub languages: Vec<String>,
    /// Package-level rendition layout.
    pub layout: RenditionLayout,
}

impl Metadata {
    /// Resolves one publication-wide writing-system hint for reader typography.
    /// Declared BCP 47 metadata wins; the title is only inspected when no
    /// declared language can be mapped to a supported writing system.
    pub fn writing_system(&self) -> WritingSystem {
        self.languages
            .iter()
            .find_map(|language| writing_system_from_language_tag(language))
            .unwrap_or_else(|| writing_system_from_title(&self.title))
    }
}

/// Coarse publication-wide writing system used for typography defaults.
///
/// This is deliberately a rendering hint rather than normalized metadata: an
/// inferred title script must not be persisted as the publication language.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WritingSystem {
    Cjk,
    Latin,
    Other,
    #[default]
    Unknown,
}

/// Package-level rendition layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RenditionLayout {
    /// Content is laid out according to the reader viewport.
    #[default]
    Reflowable,
    /// Content documents declare fixed page dimensions.
    PrePaginated,
}

/// A resource declared by a publication manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Canonical resource URL.
    pub href: PublicationUrl,
    /// Declared media type.
    pub media_type: String,
    /// EPUB manifest properties.
    pub properties: Vec<String>,
}

/// One ordered content document in the publication spine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpineItem {
    /// Stable identity, normally the OPF manifest ID.
    pub id: SpineItemId,
    /// Canonical content document URL.
    pub href: PublicationUrl,
    /// Declared media type.
    pub media_type: String,
    /// Whether this item is part of the normal linear reading order.
    pub linear: bool,
    /// EPUB manifest and spine properties.
    pub properties: Vec<String>,
}

/// Hierarchical table-of-contents entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocEntry {
    /// Human-readable navigation label.
    pub label: String,
    /// Target URL, when the entry is navigable.
    pub href: Option<PublicationUrl>,
    /// Nested entries.
    pub children: Vec<Self>,
}

/// Describes where the currently exposed table of contents came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TableOfContentsOrigin {
    /// Navigation authored by the publication.
    #[default]
    Embedded,
    /// A mechanical reading-order fallback created by the parser.
    Fallback,
    /// Navigation generated and confirmed by the user.
    Generated,
}

/// Promotes the children of a single top-level TOC entry by one level.
///
/// This is a structural presentation rule: a sole parent adds no distinction
/// at the first navigation level. It deliberately does not inspect labels or
/// recursively flatten deeper groups.
pub fn promote_single_toc_root(mut entries: Vec<TocEntry>) -> Vec<TocEntry> {
    if entries.len() == 1 && !entries[0].children.is_empty() {
        return entries.pop().map_or_else(Vec::new, |root| root.children);
    }
    entries
}

/// Fully loaded resource returned through the publication boundary.
#[derive(Debug, Clone)]
pub struct Resource {
    /// Canonical resource URL.
    pub href: PublicationUrl,
    /// Manifest or detected media type.
    pub media_type: String,
    /// Immutable uncompressed bytes.
    pub bytes: Arc<[u8]>,
}

/// Decoded RGBA image supplied by formats that already rasterize their pages.
///
/// This lets fixed-layout formats avoid encoding a temporary image only for the
/// layout engine to decode it immediately afterwards.
#[derive(Debug, Clone)]
pub struct RasterResource {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Immutable row-major RGBA8 pixels.
    pub pixels: Arc<[u8]>,
}

/// Pixel dimensions of a fixed-layout page that can be queried without
/// decoding or rasterizing the page itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedPageDimensions {
    pub width: u32,
    pub height: u32,
}

/// Format-neutral description returned before individual sections are parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    /// Stable identity derived from the source publication.
    pub id: PublicationId,
    /// Normalized package metadata.
    pub metadata: Metadata,
    /// Canonical cover resource, when declared by the publication.
    pub cover: Option<PublicationUrl>,
    /// Ordered reflowable sections.
    pub sections: Vec<SpineItem>,
    /// Hierarchical navigation entries.
    pub table_of_contents: Vec<TocEntry>,
}

/// Fully parsed, renderer-independent section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// Stable section identity.
    pub id: SpineItemId,
    /// Canonical source document URL.
    pub href: PublicationUrl,
    /// Semantic reading blocks in source order.
    pub blocks: Vec<Block>,
    /// Authored fragment identifiers resolved to stable Reading IR positions.
    #[serde(default)]
    pub anchors: Vec<SectionAnchor>,
}

/// One authored HTML/XML fragment identifier and its normalized source position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionAnchor {
    /// Decoded value of the source element's `id` or legacy `name` attribute.
    pub fragment: String,
    /// Stable position of the first rendered block at or after that element.
    pub source: SourceAnchor,
}

/// Normalized reading block. It intentionally contains no DOM or renderer types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Block {
    /// Reflowable text content.
    Text(TextBlock),
    /// Quoted prose and its optional attribution, kept as one semantic unit.
    Quote(QuoteBlock),
    /// Structured rows and cells from an authored table.
    Table(TableBlock),
    /// Raster or vector image resource.
    Image(ImageBlock),
    /// One or more authored images and their semantic caption.
    Figure(FigureBlock),
    /// Thematic separator.
    Separator,
    /// Explicit page boundary requested by the source.
    PageBreak,
}

/// A semantic quotation whose source remains attached to the quoted prose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteBlock {
    /// One or more paragraphs that form the quotation body.
    pub body: Vec<TextBlock>,
    /// Optional source, credit, or attribution rendered after the body.
    pub attribution: Option<TextBlock>,
    /// Stable range spanning the complete authored quote container.
    pub source: Option<SourceRange>,
}

/// Semantic role of a text block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextBlockKind {
    /// Ordinary paragraph.
    Paragraph,
    /// Heading with a one-based source level.
    Heading(u8),
    /// Quoted prose.
    Blockquote,
    /// Source or credit attached to quoted prose.
    QuoteAttribution,
    /// Preformatted text.
    Preformatted,
    /// Text semantically attached to an authored figure.
    Caption,
    /// Ordered or unordered list item.
    ListItem {
        /// Whether numbering should be displayed.
        ordered: bool,
        /// One-based item number when ordered.
        ordinal: u32,
        /// Zero-based nesting depth within the containing list.
        #[serde(default)]
        depth: u8,
        /// Whether this item owns a visible marker. Some EPUB outlines encode
        /// nested items as marker-less paragraphs while retaining list depth.
        #[serde(default = "default_list_marker_visible")]
        marker_visible: bool,
    },
    /// A term introduced by an HTML definition list.
    DefinitionTerm {
        /// Zero-based nesting depth of the containing definition list.
        #[serde(default)]
        depth: u8,
    },
    /// A description associated with one or more definition terms.
    DefinitionDescription {
        /// Zero-based nesting depth of the containing definition list.
        #[serde(default)]
        depth: u8,
    },
}

const fn default_list_marker_visible() -> bool {
    true
}

/// A block of styled inline content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBlock {
    /// Semantic role.
    pub kind: TextBlockKind,
    /// Inline content after whitespace normalization.
    pub content: Vec<Inline>,
    /// Portable block style subset.
    pub style: BlockStyle,
    /// Stable source range for navigation and selection.
    pub source: Option<SourceRange>,
}

/// A structured table kept independent from HTML and renderer details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableBlock {
    /// Rows in authored order.
    pub rows: Vec<TableRow>,
    /// Stable source anchor for block-level navigation.
    pub source: Option<SourceRange>,
}

/// One authored table row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRow {
    /// Cells in authored order. Grid coordinates are resolved from their spans by layout.
    pub cells: Vec<TableCell>,
}

/// One table cell with styled inline content and safe span metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCell {
    /// Cell content represented by the same selectable text IR used by paragraphs.
    pub text: TextBlock,
    /// Authored horizontal alignment inherited by the cell or declared by its
    /// flattened block content. `None` lets the reader apply its table default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_alignment: Option<TextAlignment>,
    /// Number of grid columns occupied by this cell.
    pub column_span: u16,
    /// Number of grid rows occupied by this cell.
    pub row_span: u16,
    /// Whether this cell originated from a semantic table header.
    pub header: bool,
}

/// Inline reading content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Inline {
    /// Styled Unicode text.
    Text(TextRun),
    /// LaTeX formula kept as semantic inline content for native layout.
    Math(MathRun),
    /// Forced line break.
    Break,
}

/// A LaTeX formula embedded in a text block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MathRun {
    /// LaTeX source without Markdown delimiters.
    pub latex: String,
    /// Whether the author requested display-style math.
    pub display: bool,
    /// Font-size multiplier inherited from the surrounding text.
    pub size_scale: f32,
}

/// Styled text span with an optional link target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextRun {
    /// Normalized text.
    pub text: String,
    /// Portable inline style subset.
    pub style: TextStyle,
    /// Resolved link target.
    pub link: Option<PublicationUrl>,
}

/// Image block referencing a publication resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageBlock {
    /// Canonical image URL.
    pub href: PublicationUrl,
    /// Alternative text.
    pub alt: String,
    /// Author sizing constraints normalized from attributes and CSS.
    pub style: ImageStyle,
    /// Stable source range.
    pub source: Option<SourceRange>,
    /// Optional text geometry for fixed-layout pages such as PDF documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_layer: Option<FixedPageTextLayer>,
}

/// An authored image figure kept together with its caption.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FigureBlock {
    /// Images in authored order. The first image is the primary interaction target.
    pub images: Vec<ImageBlock>,
    /// Caption paragraphs in authored order.
    pub captions: Vec<TextBlock>,
    /// Whether the caption was authored before or after the images.
    pub caption_position: CaptionPosition,
    /// Authored outer spacing retained for book-defined typesetting.
    #[serde(default)]
    pub style: BlockStyle,
    /// Stable source anchor for navigation and citation.
    pub source: Option<SourceRange>,
}

/// Authored placement of a figure caption relative to its images.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptionPosition {
    Before,
    #[default]
    After,
}

/// Searchable and selectable text attached to one fixed-layout image page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixedPageTextLayer {
    /// Coordinate-space width used by all span rectangles.
    pub width: f32,
    /// Coordinate-space height used by all span rectangles.
    pub height: f32,
    /// Extracted page text in logical reading order.
    pub text: String,
    /// Source character ranges and their fixed-page rectangles.
    pub spans: Vec<FixedPageTextSpan>,
    /// Optional replacement text painted back into the original fixed page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<FixedPageTextReplacement>,
}

/// Replacement text and its repaint region in fixed-page coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixedPageTextReplacement {
    pub segments: Vec<FixedPageTextReplacementSegment>,
}

/// One translated fragment repainted over its original fixed-page region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixedPageTextReplacementSegment {
    pub text: String,
    pub rect: FixedPageTextRect,
    /// Character offset of this fragment in the translated page text.
    pub source_offset: u64,
}

/// One source-backed fragment in a fixed page text layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixedPageTextSpan {
    /// Unicode scalar range within [`FixedPageTextLayer::text`].
    pub char_range: std::ops::Range<u64>,
    /// Fragment rectangle in the text layer's coordinate space.
    pub rect: FixedPageTextRect,
}

/// Axis-aligned rectangle in fixed-page coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FixedPageTextRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A portable image length resolved by layout against the containing column.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ImageLength {
    /// Absolute CSS pixels.
    Pixels(f32),
    /// Fraction of the containing width or height, where `1.0` is 100%.
    Fraction(f32),
}

impl ImageLength {
    /// Resolves this value against one containing dimension.
    pub fn resolve(self, containing: f32) -> f32 {
        match self {
            Self::Pixels(value) => value,
            Self::Fraction(value) => containing * value,
        }
    }
}

/// Reflowable image sizing subset modeled after the rebook text-image IR.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ImageStyle {
    pub width: Option<ImageLength>,
    pub height: Option<ImageLength>,
    pub max_width: Option<ImageLength>,
    pub max_height: Option<ImageLength>,
    /// Block spacing before the image, inherited from an image-only container when present.
    #[serde(default)]
    pub margin_before: f32,
    /// Block spacing after the image, inherited from an image-only container when present.
    #[serde(default)]
    pub margin_after: f32,
}

/// RGBA color stored without renderer coupling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Rgba {
    /// Opaque black, used for inherited body text.
    pub const BLACK: Self = Self {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    };
}

impl Default for Rgba {
    fn default() -> Self {
        Self::BLACK
    }
}

/// Renderer-independent inline style subset.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Scale relative to the reader's base font size.
    pub size_scale: f32,
    pub color: Rgba,
    /// Vertical placement relative to the surrounding text baseline.
    #[serde(default)]
    pub baseline: TextBaseline,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            underline: false,
            size_scale: 1.0,
            color: Rgba::BLACK,
            baseline: TextBaseline::Normal,
        }
    }
}

/// Semantic baseline placement for inline text such as footnote references.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextBaseline {
    #[default]
    Normal,
    Superscript,
    Subscript,
}

/// Text alignment supported by the native layout engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlignment {
    #[default]
    Start,
    Center,
    End,
    Justify,
}

/// Portable block style subset, expressed in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BlockStyle {
    pub align: TextAlignment,
    pub margin_before: f32,
    pub margin_after: f32,
    /// Flattened CSS margin/padding on the inline-start edge, in pixels.
    #[serde(default)]
    pub margin_start: f32,
    /// Inline-start margin/padding expressed as a fraction of the content width.
    #[serde(default)]
    pub margin_start_fraction: f32,
    pub indent: f32,
    pub line_height: f32,
}

impl Default for BlockStyle {
    fn default() -> Self {
        Self {
            align: TextAlignment::Start,
            margin_before: 0.0,
            margin_after: 16.0,
            margin_start: 0.0,
            margin_start_fraction: 0.0,
            indent: 0.0,
            line_height: 1.72,
        }
    }
}

/// Lazy source boundary: parsers produce stable reading IR one section at a time.
pub trait BookSource: Send + Sync {
    /// Lightweight descriptor available immediately after opening.
    fn book(&self) -> &Book;
    /// Reports whether navigation is authored, mechanical, or generated.
    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        TableOfContentsOrigin::Embedded
    }
    /// Parses one section into the normalized reading IR.
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError>;
    /// Loads a referenced resource subject to format-specific budgets.
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError>;
    /// Returns an already decoded image when the format can provide one cheaply.
    fn raster_resource(
        &self,
        _href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        Ok(None)
    }
    /// Returns the eventual fixed-page raster dimensions without materializing
    /// its pixels. Sources that cannot provide this cheaply return `None`.
    fn fixed_page_dimensions(
        &self,
        _section_index: usize,
    ) -> Result<Option<FixedPageDimensions>, PublicationError> {
        Ok(None)
    }
}

/// Source position shared by DOM, semantic blocks, and layout fragments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAnchor {
    /// Spine item containing the source node.
    pub spine: SpineItemId,
    /// Deterministic node identifier assigned during document parsing.
    pub node: String,
    /// Unicode scalar offset within the node's normalized text.
    pub text_offset: u64,
}

/// Half-open source range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    /// Inclusive start anchor.
    pub start: SourceAnchor,
    /// Exclusive end anchor.
    pub end: SourceAnchor,
}

/// Text quote used to recover a location when structural anchors no longer match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextQuote {
    /// Text immediately before the selected text.
    pub before: String,
    /// Selected or highlighted text.
    pub highlight: String,
    /// Text immediately after the selected text.
    pub after: String,
}

/// Versioned durable reading location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocatorV1 {
    /// Serialization model version.
    pub version: u8,
    /// Publication to which the locator belongs.
    pub publication_id: PublicationId,
    /// Content document containing the location.
    pub href: PublicationUrl,
    /// Progression within the content document.
    pub progression: Option<f64>,
    /// Progression across the whole publication.
    pub total_progression: Option<f64>,
    /// Optional implementation-independent position index.
    pub position: Option<u64>,
    /// Precise source range when available.
    pub source: Option<SourceRange>,
    /// EPUB CFI relative to the current spine item.
    pub partial_cfi: Option<String>,
    /// Text quote fallback.
    pub text: Option<TextQuote>,
}

impl LocatorV1 {
    /// Current locator model version.
    pub const VERSION: u8 = 1;

    /// Creates a locator at the beginning of a resource.
    pub fn at_start(publication_id: PublicationId, href: PublicationUrl) -> Self {
        Self {
            version: Self::VERSION,
            publication_id,
            href,
            progression: Some(0.0),
            total_progression: None,
            position: None,
            source: None,
            partial_cfi: None,
            text: None,
        }
    }

    /// Validates version, finite progress values, and range bounds.
    pub fn validate(&self) -> Result<(), PublicationError> {
        if self.version != Self::VERSION {
            return Err(PublicationError::UnsupportedLocatorVersion(self.version));
        }
        for progression in [self.progression, self.total_progression]
            .into_iter()
            .flatten()
        {
            if !progression.is_finite() || !(0.0..=1.0).contains(&progression) {
                return Err(PublicationError::InvalidProgression(progression));
            }
        }
        Ok(())
    }
}

/// Errors shared across format-neutral publication boundaries.
#[derive(Debug, Error)]
pub enum PublicationError {
    /// An identifier was empty or otherwise unusable.
    #[error("publication identifier cannot be empty")]
    InvalidIdentifier,
    /// An internal URL was malformed.
    #[error("invalid publication URL: {0}")]
    InvalidPublicationUrl(String),
    /// A relative URL escaped the publication root.
    #[error("publication path escapes its root: {0}")]
    PathEscapesRoot(String),
    /// A network or file-system URL was passed to an internal resource API.
    #[error("external URL is not allowed in the publication resource API: {0}")]
    ExternalUrl(String),
    /// A percent-encoded URL was not valid UTF-8.
    #[error("publication URL is not valid UTF-8 after percent decoding: {0}")]
    InvalidUrlEncoding(String),
    /// A requested resource does not exist.
    #[error("publication resource was not found: {0}")]
    ResourceNotFound(String),
    /// Resource access exceeded a configured safety budget.
    #[error("publication resource limit exceeded: {0}")]
    ResourceLimit(String),
    /// Publication content did not satisfy its format requirements.
    #[error("invalid publication: {0}")]
    InvalidPublication(String),
    /// Locator schema version is not supported.
    #[error("unsupported locator version: {0}")]
    UnsupportedLocatorVersion(u8),
    /// A locator progression was not finite or fell outside 0..=1.
    #[error("invalid locator progression: {0}")]
    InvalidProgression(f64),
    /// A lower-level I/O operation failed.
    #[error("publication I/O failed: {0}")]
    Io(String),
}

fn decode_component(value: &str) -> Result<String, PublicationError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(String::from)
        .map_err(|_| PublicationError::InvalidUrlEncoding(value.to_owned()))
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn looks_like_external_scheme(value: &str) -> bool {
    let candidate = value.split('/').next().unwrap_or_default();
    candidate
        .split_once(':')
        .is_some_and(|(scheme, _)| !scheme.is_empty() && scheme.chars().all(is_scheme_character))
}

fn is_scheme_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
}

fn writing_system_from_language_tag(language: &str) -> Option<WritingSystem> {
    let normalized = language.trim().replace('_', "-").to_ascii_lowercase();
    let mut subtags = normalized.split('-');
    let primary = subtags.next()?;
    if primary.is_empty() || primary == "und" {
        return None;
    }

    let remaining = subtags.collect::<Vec<_>>();
    if remaining
        .iter()
        .any(|subtag| matches!(*subtag, "hans" | "hant" | "jpan" | "kore"))
    {
        return Some(WritingSystem::Cjk);
    }
    if remaining.contains(&"latn") {
        return Some(WritingSystem::Latin);
    }

    match primary {
        "zh" | "ja" | "ko" => Some(WritingSystem::Cjk),
        "af" | "ca" | "cs" | "cy" | "da" | "de" | "en" | "es" | "et" | "eu" | "fi" | "fr"
        | "ga" | "gd" | "gl" | "hr" | "hu" | "id" | "is" | "it" | "lt" | "lv" | "ms" | "mt"
        | "nl" | "no" | "pl" | "pt" | "ro" | "sk" | "sl" | "sq" | "sv" | "sw" | "tr" | "vi" => {
            Some(WritingSystem::Latin)
        }
        "ar" | "be" | "bg" | "el" | "fa" | "he" | "hi" | "mk" | "ru" | "sr" | "th" | "uk"
        | "ur" => Some(WritingSystem::Other),
        _ => None,
    }
}

fn writing_system_from_title(title: &str) -> WritingSystem {
    let (mut cjk, mut latin) = (0_u32, 0_u32);
    for character in title.chars() {
        if is_cjk_character(character) {
            cjk += 1;
        } else if is_latin_character(character) {
            latin += 1;
        }
    }

    let total = cjk + latin;
    if total < 2 {
        return WritingSystem::Unknown;
    }
    if cjk * 100 >= total * 60 {
        WritingSystem::Cjk
    } else if latin * 100 >= total * 60 {
        WritingSystem::Latin
    } else {
        WritingSystem::Unknown
    }
}

fn is_cjk_character(character: char) -> bool {
    matches!(
        character,
        '\u{1100}'..='\u{11ff}'
            | '\u{2e80}'..='\u{2fff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{3130}'..='\u{318f}'
            | '\u{31a0}'..='\u{31ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{ac00}'..='\u{d7af}'
            | '\u{f900}'..='\u{faff}'
            | '\u{20000}'..='\u{2fa1f}'
    )
}

fn is_latin_character(character: char) -> bool {
    character.is_alphabetic()
        && matches!(
            character,
            'A'..='Z'
                | 'a'..='z'
                | '\u{00c0}'..='\u{024f}'
                | '\u{1e00}'..='\u{1eff}'
                | '\u{ff21}'..='\u{ff3a}'
                | '\u{ff41}'..='\u{ff5a}'
        )
}

#[cfg(test)]
mod tests {
    use super::{
        LocatorV1, Metadata, PublicationError, PublicationId, PublicationUrl, WritingSystem,
    };

    #[test]
    fn resolves_and_normalizes_relative_publication_urls() {
        let base = PublicationUrl::parse("OPS/Text/chapter.xhtml").expect("valid base URL");
        let resolved = base
            .resolve("../Images/cover%20image.jpg#hero")
            .expect("valid relative URL");

        assert_eq!(resolved.path(), "OPS/Images/cover image.jpg");
        assert_eq!(resolved.fragment(), Some("hero"));
    }

    #[test]
    fn rejects_paths_that_escape_the_publication_root() {
        let base = PublicationUrl::parse("chapter.xhtml").expect("valid base URL");
        let error = base
            .resolve("../../secret")
            .expect_err("path must be rejected");
        assert!(matches!(error, PublicationError::PathEscapesRoot(_)));
    }

    #[test]
    fn rejects_external_urls_in_resource_api() {
        let error = PublicationUrl::parse("https://example.com/book.css")
            .expect_err("external URL must be rejected");
        assert!(matches!(error, PublicationError::ExternalUrl(_)));
    }

    #[test]
    fn validates_locator_progressions() {
        let publication_id = PublicationId::new("sha256:book").expect("valid ID");
        let href = PublicationUrl::parse("OPS/chapter.xhtml").expect("valid URL");
        let mut locator = LocatorV1::at_start(publication_id, href);
        locator.total_progression = Some(1.1);

        assert!(matches!(
            locator.validate(),
            Err(PublicationError::InvalidProgression(_))
        ));
    }

    #[test]
    fn declared_language_takes_priority_over_title_script() {
        let metadata = Metadata {
            title: "系统之美".into(),
            languages: vec!["en-US".into()],
            ..Metadata::default()
        };

        assert_eq!(metadata.writing_system(), WritingSystem::Latin);
    }

    #[test]
    fn missing_language_falls_back_to_title_only() {
        let chinese = Metadata {
            title: "系统之美".into(),
            authors: vec!["Donella Meadows".into()],
            ..Metadata::default()
        };
        let english = Metadata {
            title: "Thinking in Systems".into(),
            authors: vec!["梅多斯".into()],
            ..Metadata::default()
        };

        assert_eq!(chinese.writing_system(), WritingSystem::Cjk);
        assert_eq!(english.writing_system(), WritingSystem::Latin);
    }

    #[test]
    fn ambiguous_or_insufficient_title_script_stays_unknown() {
        let ambiguous = Metadata {
            title: "AI时代".into(),
            ..Metadata::default()
        };
        let author_only = Metadata {
            authors: vec!["中文作者".into()],
            ..Metadata::default()
        };

        assert_eq!(ambiguous.writing_system(), WritingSystem::Unknown);
        assert_eq!(author_only.writing_system(), WritingSystem::Unknown);
    }
}
