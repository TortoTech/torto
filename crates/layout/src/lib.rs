//! Renderer-independent pagination for normalized reading IR.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use image::ImageError;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle, FontWeight, IndentOptions,
    InlineBox as ParleyInlineBox, InlineBoxKind, Layout, LayoutContext, LineHeight, StyleProperty,
};
use rebook_publication::{
    Block, BookSource, CaptionPosition, FixedPageDimensions, FixedPageTextLayer, FixedPageTextRect,
    ImageBlock, ImageStyle, Inline, MathRun, PublicationError, PublicationUrl, RenditionLayout,
    Rgba, Section, SourceRange, TableBlock, TableCell, TextAlignment, TextBaseline, TextBlock,
    TextBlockKind, TextRun, TextStyle, WritingSystem,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_COLUMN_GAP: f32 = 36.0;
const IMAGE_BLOCK_GAP: f32 = 14.0;
const TABLE_BLOCK_GAP: f32 = 14.0;
const MIN_COLUMN_WIDTH: f32 = 360.0;
const MAX_COLUMN_WIDTH: f32 = 800.0;
const DEFAULT_TOP_MARGIN: f32 = 0.0;
const DEFAULT_BOTTOM_MARGIN: f32 = 24.0;

/// Logical viewport in device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutViewport {
    pub width: u32,
    pub height: u32,
}

impl LayoutViewport {
    pub fn new(width: u32, height: u32) -> Result<Self, LayoutError> {
        if width == 0 || height == 0 {
            return Err(LayoutError::InvalidViewport);
        }
        Ok(Self { width, height })
    }
}

/// User-controlled values that invalidate pagination.
#[derive(Debug, Clone, PartialEq)]
pub struct ReaderStyle {
    pub typography: ReaderTypography,
    pub typesetting: ReaderTypesetting,
    /// Publication-wide writing system used by automatic typography defaults.
    pub writing_system: WritingSystem,
    pub horizontal_margin: f32,
    pub top_margin: f32,
    pub bottom_margin: f32,
    pub column_gap: f32,
    /// Minimum visual gap between consecutive prose paragraphs. A zero value
    /// preserves the publication-authored margins exactly.
    pub minimum_paragraph_gap: f32,
    pub spread: SpreadMode,
    pub foreground: Rgba,
    pub background: Rgba,
}

/// Chooses whether reflowable content follows publication-authored metrics or
/// the reader's semantic, cross-book typesetting profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypesettingMode {
    #[default]
    Book,
    Unified,
}

/// Chooses whether paragraph indentation follows the book language or an
/// explicit reader value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParagraphIndentMode {
    #[default]
    Auto,
    Custom,
}

/// Reader-controlled metrics applied consistently to semantic reading blocks.
/// Relative values scale with the base reading font so one font-size change
/// keeps headings, prose, and tables in proportion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderTypesetting {
    pub mode: TypesettingMode,
    pub heading_scale: f32,
    pub body_line_height: f32,
    pub paragraph_indent_mode: ParagraphIndentMode,
    pub paragraph_indent_em: f32,
    pub paragraph_gap_em: f32,
    pub heading_body_gap_em: f32,
    pub media_gap_em: f32,
    pub caption_font_scale: f32,
    pub caption_gap_em: f32,
    pub list_indent_em: f32,
    pub table_font_scale: f32,
    pub table_line_height: f32,
    pub table_cell_padding_em: f32,
}

impl ReaderTypesetting {
    pub fn unified() -> Self {
        Self {
            mode: TypesettingMode::Unified,
            ..Self::default()
        }
    }

    /// Repairs persisted values before they participate in layout cache keys.
    pub fn normalize(&mut self) {
        self.heading_scale = finite_clamp(self.heading_scale, 1.1, 2.2, 1.6);
        self.body_line_height = finite_clamp(self.body_line_height, 1.2, 2.4, 1.72);
        self.paragraph_indent_em = finite_clamp(self.paragraph_indent_em, 0.0, 4.0, 2.0);
        self.paragraph_gap_em = finite_clamp(self.paragraph_gap_em, 0.0, 2.0, 0.75);
        self.heading_body_gap_em = finite_clamp(self.heading_body_gap_em, 0.2, 2.0, 0.7);
        self.media_gap_em = finite_clamp(self.media_gap_em, 0.5, 2.0, 1.0);
        self.caption_font_scale = finite_clamp(self.caption_font_scale, 0.7, 1.0, 0.88);
        self.caption_gap_em = finite_clamp(self.caption_gap_em, 0.2, 1.0, 0.35);
        self.list_indent_em = finite_clamp(self.list_indent_em, 0.5, 3.0, 1.5);
        self.table_font_scale = finite_clamp(self.table_font_scale, 0.7, 1.2, 0.9);
        self.table_line_height = finite_clamp(self.table_line_height, 1.1, 2.0, 1.45);
        self.table_cell_padding_em = finite_clamp(self.table_cell_padding_em, 0.2, 1.0, 0.35);
    }
}

impl Default for ReaderTypesetting {
    fn default() -> Self {
        Self {
            mode: TypesettingMode::Book,
            heading_scale: 1.6,
            body_line_height: 1.72,
            paragraph_indent_mode: ParagraphIndentMode::Auto,
            paragraph_indent_em: 2.0,
            paragraph_gap_em: 0.75,
            heading_body_gap_em: 0.7,
            media_gap_em: 1.0,
            caption_font_scale: 0.88,
            caption_gap_em: 0.35,
            list_indent_em: 1.5,
            table_font_scale: 0.9,
            table_line_height: 1.45,
            table_cell_padding_em: 0.35,
        }
    }
}

/// Generic family used for ordinary reading text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReaderDefaultFont {
    #[default]
    Serif,
    SansSerif,
}

/// Readest-compatible native typography preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderTypography {
    pub default_font: ReaderDefaultFont,
    pub default_cjk_font: String,
    pub serif_font: String,
    pub sans_serif_font: String,
    pub monospace_font: String,
    pub font_size: f32,
    pub minimum_font_size: f32,
    pub font_weight: u16,
}

impl ReaderTypography {
    /// Repairs persisted or externally supplied settings before layout uses them.
    pub fn normalize(&mut self) {
        let defaults = Self::default();
        normalize_family(&mut self.default_cjk_font, &defaults.default_cjk_font);
        normalize_family(&mut self.serif_font, &defaults.serif_font);
        normalize_family(&mut self.sans_serif_font, &defaults.sans_serif_font);
        normalize_family(&mut self.monospace_font, &defaults.monospace_font);
        self.minimum_font_size = finite_clamp(self.minimum_font_size, 1.0, 120.0, 12.0);
        self.font_size = finite_clamp(self.font_size, self.minimum_font_size, 120.0, 20.0);
        self.font_weight = self.font_weight.clamp(100, 900).div_ceil(100) * 100;
    }

    #[must_use]
    pub fn default_stack(&self) -> String {
        match self.default_font {
            ReaderDefaultFont::Serif => self.serif_stack(),
            ReaderDefaultFont::SansSerif => self.sans_serif_stack(),
        }
    }

    #[must_use]
    pub fn serif_stack(&self) -> String {
        font_stack(
            [
                self.serif_font.as_str(),
                self.default_cjk_font.as_str(),
                "LXGW WenKai GB Screen",
                "LXGW WenKai",
                "Noto Serif SC",
                "Source Han Serif SC",
                "Songti SC",
                "SimSun",
                "Georgia",
                "Times New Roman",
            ],
            "serif",
        )
    }

    #[must_use]
    pub fn sans_serif_stack(&self) -> String {
        font_stack(
            [
                self.sans_serif_font.as_str(),
                self.default_cjk_font.as_str(),
                "LXGW WenKai GB Screen",
                "LXGW WenKai",
                "Noto Sans SC",
                "Source Han Sans SC",
                "PingFang SC",
                "Microsoft YaHei",
                "Roboto",
                "Arial",
            ],
            "sans-serif",
        )
    }

    #[must_use]
    pub fn monospace_stack(&self) -> String {
        font_stack(
            [
                self.monospace_font.as_str(),
                "Fira Code",
                "Consolas",
                self.default_cjk_font.as_str(),
                "LXGW WenKai GB Screen",
                "LXGW WenKai",
                "SFMono-Regular",
                "Menlo",
                "Courier New",
            ],
            "monospace",
        )
    }
}

impl Default for ReaderTypography {
    fn default() -> Self {
        Self {
            default_font: ReaderDefaultFont::Serif,
            default_cjk_font: "LXGW WenKai GB Screen".into(),
            serif_font: "Bitter".into(),
            sans_serif_font: "Roboto".into(),
            monospace_font: "Consolas".into(),
            font_size: 20.0,
            minimum_font_size: 12.0,
            font_weight: 400,
        }
    }
}

/// Maximum number of book pages shown in one viewport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SpreadMode {
    /// Always paginate as one page per viewport.
    #[default]
    Single,
    /// Use a two-page spread when both columns can remain comfortably readable.
    Double,
    /// Paginate as single pages and present the active section as a vertical flow.
    Scroll,
}

impl SpreadMode {
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            Self::Single => Self::Double,
            Self::Double => Self::Scroll,
            Self::Scroll => Self::Single,
        }
    }
}

impl Default for ReaderStyle {
    fn default() -> Self {
        Self {
            typography: ReaderTypography::default(),
            typesetting: ReaderTypesetting::default(),
            writing_system: WritingSystem::Unknown,
            horizontal_margin: 44.0,
            top_margin: DEFAULT_TOP_MARGIN,
            bottom_margin: DEFAULT_BOTTOM_MARGIN,
            column_gap: DEFAULT_COLUMN_GAP,
            minimum_paragraph_gap: 0.0,
            spread: SpreadMode::Double,
            foreground: Rgba::BLACK,
            background: Rgba {
                red: 250,
                green: 248,
                blue: 243,
                alpha: 255,
            },
        }
    }
}

fn normalize_family(value: &mut String, fallback: &str) {
    *value = value.trim().to_owned();
    if value.is_empty() {
        value.push_str(fallback);
    }
}

fn finite_clamp(value: f32, minimum: f32, maximum: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        fallback.clamp(minimum, maximum)
    }
}

fn font_stack<'a>(families: impl IntoIterator<Item = &'a str>, generic: &str) -> String {
    let mut seen = HashSet::new();
    let mut stack = families
        .into_iter()
        .map(str::trim)
        .filter(|family| !family.is_empty())
        .filter(|family| seen.insert(family.to_ascii_lowercase()))
        .map(quote_font_family)
        .collect::<Vec<_>>();
    stack.push(generic.to_owned());
    stack.join(", ")
}

fn quote_font_family(family: &str) -> String {
    format!("\"{}\"", family.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Brush carried through Parley without coupling layout to a paint backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextBrush {
    pub color: Rgba,
    pub underline: bool,
    pub baseline: TextBaseline,
}

impl TextBrush {
    fn new(color: Rgba, underline: bool, baseline: TextBaseline) -> Self {
        Self {
            color,
            underline,
            baseline,
        }
    }
}

/// Shared font bytes registered in both the native reader and the Xilem UI.
pub type ReaderFontBlob = parley::fontique::Blob<u8>;

/// One immutable paginated section.
pub struct SectionLayout {
    pub pages: Vec<PageLayout>,
    pub visible_pages: usize,
    pub continuation_offset_x: f32,
}

/// Renderer-independent display data for one page.
pub struct PageLayout {
    pub viewport: LayoutViewport,
    pub background: Rgba,
    /// Semantic spacing that preceded the first block before pagination moved
    /// it onto this page. Paginated views discard it at the physical page edge,
    /// while continuous views can restore it when stitching pages together.
    pub leading_gap: f32,
    pub items: Vec<PageItem>,
}

/// Positioned page content.
pub enum PageItem {
    Text(TextPlacement),
    Table(TablePlacement),
    Image(ImagePlacement),
    Separator(SeparatorPlacement),
}

/// A line slice from a shaped paragraph.
#[derive(Clone)]
pub struct TextPlacement {
    pub layout: Arc<Layout<TextBrush>>,
    /// UTF-8 text shaped by Parley. Kept alongside the layout so retained
    /// renderers can map pointer hit tests back to durable source offsets.
    pub text: Arc<str>,
    /// Byte length of synthetic display text (for example a list marker) that
    /// precedes the authored source text.
    pub source_text_start: usize,
    pub lines: Range<usize>,
    pub origin_x: f32,
    pub origin_y: f32,
    pub source: Option<SourceRange>,
    /// Formula rasters positioned by Parley inline boxes in this text layout.
    pub inline_images: Arc<[InlineImage]>,
}

/// One positioned table chunk. Large tables can produce one chunk per page.
pub struct TablePlacement {
    pub cells: Vec<TableCellPlacement>,
    pub y: f32,
    pub height: f32,
    pub border: Rgba,
    pub header_fill: Rgba,
}

/// One positioned table cell with selectable text content.
pub struct TableCellPlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub header: bool,
    pub text: Option<TextPlacement>,
}

/// One raster painted at a matching Parley inline-box position.
#[derive(Clone)]
pub struct InlineImage {
    pub id: u64,
    pub image: RasterImage,
    pub width: f32,
    pub height: f32,
}

/// Decoded RGBA image ready for upload by the renderer.
#[derive(Clone)]
pub struct RasterImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

/// Positioned raster image.
pub struct ImagePlacement {
    pub image: RasterImage,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub source: Option<SourceRange>,
    pub text_layer: Option<FixedPageTextLayer>,
    pub replacement: Option<FixedPageTextReplacementPlacement>,
}

/// Translated text repainted inside the original fixed-layout page image.
pub struct FixedPageTextReplacementPlacement {
    pub segments: Vec<FixedPageTextReplacementSegmentPlacement>,
}

/// One shaped translated fragment inside a fixed-page replacement overlay.
pub struct FixedPageTextReplacementSegmentPlacement {
    pub rect: FixedPageTextRect,
    pub text: TextPlacement,
}

/// Positioned thematic break.
pub struct SeparatorPlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

/// Stateful layout engine. Font discovery and shaping caches live for the reader session.
pub struct LayoutEngine {
    font_context: FontContext,
    layout_context: LayoutContext<TextBrush>,
    svg_options: resvg::usvg::Options<'static>,
}

impl Default for LayoutEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutEngine {
    pub fn new() -> Self {
        let mut svg_options = resvg::usvg::Options::default();
        svg_options.fontdb_mut().load_system_fonts();
        Self {
            font_context: FontContext::new(),
            layout_context: LayoutContext::new(),
            svg_options,
        }
    }

    pub fn with_fonts(fonts: impl IntoIterator<Item = ReaderFontBlob>) -> Self {
        let mut engine = Self::new();
        for font in fonts {
            engine.font_context.collection.register_fonts(font, None);
        }
        engine
    }

    pub fn available_font_families(&mut self) -> Vec<String> {
        let mut families = self
            .font_context
            .collection
            .family_names()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        families.sort_by_key(|family| family.to_lowercase());
        families.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        families
    }

    pub fn layout_section(
        &mut self,
        source: &dyn BookSource,
        section: &Section,
        viewport: LayoutViewport,
        reader_style: &ReaderStyle,
    ) -> Result<SectionLayout, LayoutError> {
        self.layout_blocks(source, &section.blocks, viewport, reader_style)
    }

    /// Lays out one viewport-independent slice of a reflowable section. The
    /// reader uses this entry point for bounded fragment compilation without
    /// manufacturing synthetic authored sections.
    pub fn layout_blocks(
        &mut self,
        source: &dyn BookSource,
        blocks: &[Block],
        viewport: LayoutViewport,
        reader_style: &ReaderStyle,
    ) -> Result<SectionLayout, LayoutError> {
        self.layout_fragments(source, &[blocks], viewport, reader_style)
    }

    /// Builds a fixed-page layout with the exact geometry of the eventual
    /// raster while retaining only a single white pixel. This lets continuous
    /// PDF views reserve every physical page without decoding all page images.
    #[allow(
        clippy::cast_precision_loss,
        reason = "fixed page dimensions are bounded by the PDF raster budget"
    )]
    pub fn layout_fixed_page_placeholder(
        &mut self,
        dimensions: FixedPageDimensions,
        viewport: LayoutViewport,
        reader_style: &ReaderStyle,
    ) -> SectionLayout {
        let page_width = viewport.width as f32;
        let page_height = viewport.height as f32;
        let geometry = resolve_page_geometry(page_width, page_height, reader_style);
        let visible_pages = geometry.visible_pages;
        let continuation_offset_x = geometry.continuation_offset_x;
        let mut paginator = Paginator::new(
            viewport,
            reader_style.background,
            geometry,
            true,
            reader_style.minimum_paragraph_gap,
        );
        paginator.push_image(
            RasterImage {
                width: dimensions.width.max(1),
                height: dimensions.height.max(1),
                pixels: Arc::from([255_u8, 255, 255, 255]),
            },
            ImageStyle::default(),
            None,
            None,
        );
        let mut pages = paginator.finish();
        for page in &mut pages {
            for item in &mut page.items {
                if let PageItem::Image(image) = item {
                    image.image = RasterImage {
                        width: 1,
                        height: 1,
                        pixels: Arc::from([255_u8, 255, 255, 255]),
                    };
                }
            }
        }
        SectionLayout {
            pages,
            visible_pages,
            continuation_offset_x,
        }
    }

    /// Continuously paginates several stable content fragments as one bounded
    /// layout segment. Fragment boundaries do not commit the partial page; the
    /// caller controls random-access cost by choosing the segment size.
    #[allow(
        clippy::cast_precision_loss,
        reason = "reader viewport dimensions are bounded far below f32's exact integer range"
    )]
    pub fn layout_fragments(
        &mut self,
        source: &dyn BookSource,
        fragments: &[&[Block]],
        viewport: LayoutViewport,
        reader_style: &ReaderStyle,
    ) -> Result<SectionLayout, LayoutError> {
        let page_width = viewport.width as f32;
        let page_height = viewport.height as f32;
        let geometry = resolve_page_geometry(page_width, page_height, reader_style);
        let content_width = geometry.width;
        let visible_pages = geometry.visible_pages;
        let continuation_offset_x = geometry.continuation_offset_x;

        let center_standalone_image = source.book().metadata.layout
            == RenditionLayout::PrePaginated
            || fragments_are_standalone_cover(fragments, source.book().cover.as_ref());
        let unified_reflow = reader_style.typesetting.mode == TypesettingMode::Unified
            && source.book().metadata.layout != RenditionLayout::PrePaginated;
        let media_start_offset = if unified_reflow {
            0.0
        } else {
            dominant_paragraph_start_offset(fragments, content_width)
        };
        let mut paginator = Paginator::new(
            viewport,
            reader_style.background,
            geometry,
            center_standalone_image,
            reader_style.minimum_paragraph_gap,
        );
        paginator.media_start_offset = media_start_offset;

        for blocks in fragments {
            for block in *blocks {
                match block {
                    Block::Text(block) => {
                        let resolved = resolve_text_block(block, reader_style, TextContext::Flow);
                        let prepared = self.shape_text(&resolved, reader_style, content_width);
                        paginator.push_text(&prepared, &resolved)?;
                    }
                    Block::Table(table) => {
                        let prepared = self.shape_table(
                            table,
                            reader_style,
                            (content_width - media_start_offset).max(1.0),
                        );
                        paginator.push_table(&prepared);
                    }
                    Block::Image(image) => {
                        let raster = load_raster_image(source, image)?;
                        let mut image_style = image.style;
                        if unified_reflow {
                            let gap = reader_style.typography.font_size
                                * reader_style.typesetting.media_gap_em;
                            image_style.margin_before = gap;
                            image_style.margin_after = gap;
                        }
                        let replacements = paginator.push_image(
                            raster,
                            image_style,
                            image.source.clone(),
                            image.text_layer.clone(),
                        );
                        for replacement in replacements {
                            let prepared =
                                self.shape_fixed_page_replacement(&replacement, reader_style);
                            paginator.push_fixed_page_replacement(&prepared, replacement)?;
                        }
                    }
                    Block::Figure(figure) => {
                        let authored_outer_gap = figure
                            .images
                            .iter()
                            .map(|image| image.style.margin_before.max(image.style.margin_after))
                            .fold(0.0, f32::max)
                            .max(figure.style.margin_before)
                            .max(figure.style.margin_after);
                        let mut images = Vec::with_capacity(figure.images.len());
                        for image in &figure.images {
                            let mut style = image.style;
                            // The figure owns its outer spacing. Keeping authored
                            // margins on each child would double the gap between
                            // an image and its caption.
                            style.margin_before = 0.0;
                            style.margin_after = 0.0;
                            images.push((load_raster_image(source, image)?, style, image));
                        }
                        let captions = figure
                            .captions
                            .iter()
                            .map(|caption| {
                                let resolved =
                                    resolve_text_block(caption, reader_style, TextContext::Flow);
                                let prepared = self.shape_text(
                                    &resolved,
                                    reader_style,
                                    (content_width - media_start_offset).max(1.0),
                                );
                                (prepared, resolved)
                            })
                            .collect::<Vec<_>>();
                        let outer_gap = if unified_reflow {
                            reader_style.typography.font_size
                                * reader_style.typesetting.media_gap_em
                        } else {
                            authored_outer_gap.max(IMAGE_BLOCK_GAP)
                        };
                        let caption_gap = if unified_reflow {
                            reader_style.typography.font_size
                                * reader_style.typesetting.caption_gap_em
                        } else {
                            6.0
                        };
                        let image_height = images
                            .iter()
                            .map(|(raster, style, _)| {
                                paginator.image_display_size(raster, *style).1
                            })
                            .sum::<f32>();
                        let caption_height = captions
                            .iter()
                            .map(|(prepared, resolved)| {
                                prepared_text_height(prepared)
                                    + resolved.style.margin_before.max(0.0)
                                    + resolved.style.margin_after.max(0.0)
                            })
                            .sum::<f32>();
                        let internal_image_gaps =
                            caption_gap * images.len().saturating_sub(1) as f32;
                        let image_caption_gap = if images.is_empty() || captions.is_empty() {
                            0.0
                        } else {
                            caption_gap
                        };
                        paginator.prepare_group(
                            image_height + caption_height + internal_image_gaps + image_caption_gap,
                            outer_gap,
                        );

                        let push_images = |paginator: &mut Paginator,
                                           engine: &mut LayoutEngine|
                         -> Result<(), LayoutError> {
                            for (index, (raster, style, image)) in images.iter().enumerate() {
                                if index > 0 {
                                    paginator.add_semantic_spacing(caption_gap);
                                }
                                let replacements = paginator.push_image_with_gaps(
                                    raster.clone(),
                                    *style,
                                    image.source.clone(),
                                    image.text_layer.clone(),
                                    0.0,
                                    0.0,
                                );
                                for replacement in replacements {
                                    let prepared = engine
                                        .shape_fixed_page_replacement(&replacement, reader_style);
                                    paginator
                                        .push_fixed_page_replacement(&prepared, replacement)?;
                                }
                            }
                            Ok(())
                        };
                        let push_captions = |paginator: &mut Paginator| -> Result<(), LayoutError> {
                            for (prepared, resolved) in &captions {
                                paginator.push_text(prepared, resolved.as_ref())?;
                            }
                            Ok(())
                        };
                        match figure.caption_position {
                            CaptionPosition::Before => {
                                push_captions(&mut paginator)?;
                                if !captions.is_empty() && !images.is_empty() {
                                    paginator.add_semantic_spacing(caption_gap);
                                }
                                push_images(&mut paginator, self)?;
                            }
                            CaptionPosition::After => {
                                push_images(&mut paginator, self)?;
                                if !captions.is_empty() && !images.is_empty() {
                                    paginator.add_semantic_spacing(caption_gap);
                                }
                                push_captions(&mut paginator)?;
                            }
                        }
                        paginator.ensure_minimum_spacing(outer_gap);
                    }
                    Block::Separator => paginator.push_separator(),
                    Block::PageBreak => paginator.force_page(),
                }
            }
        }

        Ok(SectionLayout {
            pages: paginator.finish(),
            visible_pages,
            continuation_offset_x,
        })
    }

    fn shape_text(
        &mut self,
        block: &TextBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
    ) -> PreparedText {
        self.shape_text_with_min_width(block, reader_style, content_width, 40.0)
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "table spans are clamped to 64 and publication rows are bounded by input limits"
    )]
    fn shape_table(
        &mut self,
        table: &TableBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
    ) -> PreparedTable {
        let row_count = table.rows.len();
        let mut occupied = vec![Vec::<bool>::new(); row_count];
        let mut grid_cells = Vec::new();
        let mut column_count = 0;
        for (row_index, row) in table.rows.iter().enumerate() {
            let mut column = 0;
            for cell in &row.cells {
                while occupied[row_index].get(column).copied().unwrap_or(false) {
                    column += 1;
                }
                let column_span = usize::from(cell.column_span.max(1));
                let row_span = usize::from(cell.row_span.max(1)).min(row_count - row_index);
                let end_column = column.saturating_add(column_span);
                for row in occupied.iter_mut().skip(row_index).take(row_span) {
                    row.resize(row.len().max(end_column), false);
                    row[column..end_column].fill(true);
                }
                column_count = column_count.max(end_column);
                grid_cells.push((row_index, row_span, column, column_span, cell));
                column = end_column;
            }
        }
        if column_count == 0 || row_count == 0 {
            return PreparedTable::default();
        }
        let table_metrics = resolve_table_metrics(reader_style);
        let unified = reader_style.typesetting.mode == TypesettingMode::Unified;
        let equal_column_width = content_width / column_count as f32;
        let column_widths = if unified {
            self.adaptive_table_column_widths(
                &grid_cells,
                column_count,
                content_width,
                reader_style,
                table_metrics,
            )
        } else {
            vec![equal_column_width; column_count]
        };
        let minimum_row_height = (reader_style.typography.font_size * table_metrics.font_scale)
            .mul_add(table_metrics.line_height, table_metrics.cell_padding * 2.0);
        let mut row_heights = vec![minimum_row_height; row_count];
        let mut cells = Vec::with_capacity(grid_cells.len());
        for (row, row_span, column, column_span, cell) in grid_cells {
            let block = table_cell_text_block(cell);
            let block = resolve_text_block(&block, reader_style, TextContext::Table).into_owned();
            let cell_width = column_widths[column..column + column_span]
                .iter()
                .sum::<f32>();
            let text_width = (cell_width - table_metrics.cell_padding * 2.0).max(20.0);
            let text = self.shape_text_with_min_width(&block, reader_style, text_width, 8.0);
            let required_height = prepared_text_height(&text) + table_metrics.cell_padding * 2.0;
            if row_span == 1 {
                row_heights[row] = row_heights[row].max(required_height);
            }
            cells.push(PreparedTableCell {
                row,
                row_span,
                column,
                column_span,
                header: cell.header,
                source: block.source.clone(),
                text,
                required_height,
            });
        }
        for cell in cells.iter().filter(|cell| cell.row_span > 1) {
            let current = row_heights[cell.row..cell.row + cell.row_span]
                .iter()
                .sum::<f32>();
            if cell.required_height > current {
                let addition = (cell.required_height - current) / cell.row_span as f32;
                for height in &mut row_heights[cell.row..cell.row + cell.row_span] {
                    *height += addition;
                }
            }
        }
        PreparedTable {
            horizontal_offset: centered_table_offset(unified, content_width, &column_widths),
            column_widths,
            row_heights,
            cells,
            cell_padding: table_metrics.cell_padding,
            center_content: unified,
            block_gap: table_metrics.block_gap,
            border: Rgba {
                alpha: 96,
                ..reader_style.foreground
            },
            header_fill: Rgba {
                alpha: 22,
                ..reader_style.foreground
            },
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "table spans are clamped to 64 and publication rows are bounded by input limits"
    )]
    fn adaptive_table_column_widths(
        &mut self,
        grid_cells: &[(usize, usize, usize, usize, &TableCell)],
        column_count: usize,
        content_width: f32,
        reader_style: &ReaderStyle,
        table_metrics: ResolvedTableMetrics,
    ) -> Vec<f32> {
        let equal_column_width = content_width / column_count as f32;
        let minimum_column_width = (reader_style.typography.font_size * 3.0)
            .min(equal_column_width)
            .max(1.0);
        let mut preferred_widths = vec![minimum_column_width; column_count];
        for (_, _, column, column_span, cell) in grid_cells {
            let block = table_cell_text_block(cell);
            let block = resolve_text_block(&block, reader_style, TextContext::Table).into_owned();
            let unwrapped = self.shape_text_with_min_width(&block, reader_style, 16_384.0, 8.0);
            let inline_slack = reader_style.typography.font_size * table_metrics.font_scale * 0.5;
            let preferred = (unwrapped.layout.full_width().ceil()
                + table_metrics.cell_padding * 2.0
                + inline_slack)
                .clamp(minimum_column_width, content_width);
            let range = *column..(*column + *column_span);
            let current = preferred_widths[range.clone()].iter().sum::<f32>();
            if preferred > current {
                let addition = (preferred - current) / *column_span as f32;
                for width in &mut preferred_widths[range] {
                    *width += addition;
                }
            }
        }
        fit_adaptive_column_widths(&preferred_widths, minimum_column_width, content_width)
    }

    fn shape_text_with_min_width(
        &mut self,
        block: &TextBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
        minimum_width: f32,
    ) -> PreparedText {
        let (start_offset, available_width, first_line_indent) =
            resolve_text_measure(block, content_width, minimum_width);
        let typography = &reader_style.typography;
        let (text, spans, inline_images, source_text_start) = prepare_inline_content(
            block,
            reader_style.foreground,
            typography,
            available_width,
            &self.svg_options,
        );
        let font_stack = if block.kind == TextBlockKind::Preformatted {
            typography.monospace_stack()
        } else {
            typography.default_stack()
        };
        let mut builder =
            self.layout_context
                .ranged_builder(&mut self.font_context, &text, 1.0, false);
        builder.push_default(StyleProperty::FontFamily(FontFamily::from(
            font_stack.as_str(),
        )));
        builder.push_default(StyleProperty::FontSize(typography.font_size));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(f32::from(
            typography.font_weight,
        ))));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            block.style.line_height,
        )));
        let default_brush = TextBrush::new(reader_style.foreground, false, TextBaseline::Normal);
        builder.push_default(StyleProperty::Brush(default_brush));

        for span in spans {
            let size = (typography.font_size * span.style.size_scale.clamp(0.5, 3.0))
                .max(typography.minimum_font_size);
            builder.push(StyleProperty::FontSize(size), span.range.clone());
            builder.push(
                StyleProperty::Brush(TextBrush::new(
                    span.style.color,
                    span.style.underline,
                    span.style.baseline,
                )),
                span.range.clone(),
            );
            if span.style.bold {
                builder.push(
                    StyleProperty::FontWeight(FontWeight::new(
                        f32::from(typography.font_weight).max(FontWeight::BOLD.value()),
                    )),
                    span.range.clone(),
                );
            }
            if span.style.italic {
                builder.push(
                    StyleProperty::FontStyle(FontStyle::Italic),
                    span.range.clone(),
                );
            }
            if span.style.underline {
                builder.push(StyleProperty::Underline(true), span.range);
            }
        }

        for image in &inline_images {
            builder.push_inline_box(ParleyInlineBox {
                id: image.id,
                kind: InlineBoxKind::InFlow,
                index: image.index,
                width: image.width,
                height: image.height,
            });
        }

        let mut layout = builder.build(&text);
        if first_line_indent.abs() > f32::EPSILON {
            layout.set_text_indent(first_line_indent, IndentOptions::default());
        }
        self.apply_list_indent(
            &mut layout,
            block.kind,
            &text[..source_text_start],
            typography,
        );
        layout.break_all_lines(Some(available_width));
        let alignment = text_alignment(block.style.align);
        layout.align(alignment, AlignmentOptions::default());
        PreparedText {
            layout: Arc::new(layout),
            text: text.into(),
            source_text_start,
            start_offset,
            inline_images: inline_images
                .into_iter()
                .map(|image| InlineImage {
                    id: image.id,
                    image: image.image,
                    width: image.width,
                    height: image.height,
                })
                .collect::<Vec<_>>()
                .into(),
        }
    }

    fn measure_list_marker_width(
        &mut self,
        marker: &str,
        font_stack: &str,
        typography: &ReaderTypography,
    ) -> f32 {
        let mut builder =
            self.layout_context
                .ranged_builder(&mut self.font_context, marker, 1.0, false);
        builder.push_default(StyleProperty::FontFamily(FontFamily::from(font_stack)));
        builder.push_default(StyleProperty::FontSize(typography.font_size));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(f32::from(
            typography.font_weight,
        ))));
        let mut layout = builder.build(marker);
        layout.break_all_lines(None);
        layout.full_width()
    }

    fn apply_list_indent(
        &mut self,
        layout: &mut Layout<TextBrush>,
        kind: TextBlockKind,
        marker: &str,
        typography: &ReaderTypography,
    ) {
        if marker.is_empty() {
            return;
        }
        let font_stack = typography.default_stack();
        let marker_width = self.measure_list_marker_width(marker, &font_stack, typography);
        apply_list_hanging_indent(layout, kind, marker_width);
    }

    fn shape_fixed_page_replacement(
        &mut self,
        request: &FixedPageReplacementRequest,
        reader_style: &ReaderStyle,
    ) -> PreparedText {
        let block = fixed_page_replacement_block(&request.text, request.source.clone());
        let mut style = reader_style.clone();
        style.typography.font_size = style.typography.font_size.min(14.0);
        style.typography.minimum_font_size = 5.0;
        let available_width = (request.rect.width - 3.0).max(2.0);
        let available_height = (request.rect.height - 3.0).max(2.0);

        loop {
            let prepared = self.shape_text_with_min_width(&block, &style, available_width, 2.0);
            let height = prepared_text_height(&prepared);
            if height <= available_height || style.typography.font_size <= 5.0 {
                return prepared;
            }
            let next_size = (style.typography.font_size * available_height / height)
                .clamp(5.0, style.typography.font_size - 0.5);
            style.typography.font_size = next_size;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextContext {
    Flow,
    Table,
}

#[derive(Clone, Copy)]
struct ResolvedTableMetrics {
    font_scale: f32,
    line_height: f32,
    cell_padding: f32,
    block_gap: f32,
}

fn resolve_table_metrics(reader_style: &ReaderStyle) -> ResolvedTableMetrics {
    if reader_style.typesetting.mode == TypesettingMode::Unified {
        ResolvedTableMetrics {
            font_scale: reader_style.typesetting.table_font_scale,
            line_height: reader_style.typesetting.table_line_height,
            cell_padding: reader_style.typography.font_size
                * reader_style.typesetting.table_cell_padding_em,
            block_gap: reader_style.typography.font_size * 0.7,
        }
    } else {
        ResolvedTableMetrics {
            font_scale: 1.0,
            line_height: 1.3,
            cell_padding: 6.0,
            block_gap: TABLE_BLOCK_GAP,
        }
    }
}

fn table_cell_text_block(cell: &TableCell) -> TextBlock {
    let mut block = cell.text.clone();
    block.style.align = cell.authored_alignment.unwrap_or(TextAlignment::Center);
    if cell.header {
        for inline in &mut block.content {
            if let Inline::Text(run) = inline {
                run.style.bold = true;
            }
        }
    }
    block
}

fn centered_table_offset(unified: bool, available_width: f32, column_widths: &[f32]) -> f32 {
    if unified {
        ((available_width - column_widths.iter().sum::<f32>()) / 2.0).max(0.0)
    } else {
        0.0
    }
}

fn resolve_text_block<'a>(
    block: &'a TextBlock,
    reader_style: &ReaderStyle,
    context: TextContext,
) -> Cow<'a, TextBlock> {
    if reader_style.typesetting.mode != TypesettingMode::Unified {
        return Cow::Borrowed(block);
    }

    let mut resolved = block.clone();
    let typography = &reader_style.typography;
    let profile = &reader_style.typesetting;
    let base_size = typography.font_size;
    let (scale, line_height, margin_after) = match context {
        TextContext::Table => (profile.table_font_scale, profile.table_line_height, 0.0),
        TextContext::Flow => match block.kind {
            TextBlockKind::Heading(level) => (
                unified_heading_scale(profile.heading_scale, level),
                1.3,
                base_size * profile.heading_body_gap_em,
            ),
            TextBlockKind::Caption => (profile.caption_font_scale, 1.4, 0.0),
            TextBlockKind::Preformatted => (0.9, 1.45, base_size * profile.paragraph_gap_em),
            TextBlockKind::Blockquote => (
                0.95,
                profile.body_line_height,
                base_size * profile.paragraph_gap_em,
            ),
            TextBlockKind::Paragraph
            | TextBlockKind::ListItem { .. }
            | TextBlockKind::DefinitionDescription { .. } => (
                1.0,
                profile.body_line_height,
                base_size * profile.paragraph_gap_em,
            ),
            TextBlockKind::DefinitionTerm { .. } => (
                1.0,
                profile.body_line_height,
                base_size * profile.paragraph_gap_em.min(0.25),
            ),
        },
    };

    if context == TextContext::Flow {
        resolved.style.align = TextAlignment::Start;
    }
    resolved.style.margin_before = 0.0;
    resolved.style.margin_after = margin_after;
    resolved.style.indent =
        if context == TextContext::Flow && block.kind == TextBlockKind::Paragraph {
            base_size * paragraph_indent_em(profile, reader_style.writing_system)
        } else {
            0.0
        };
    resolved.style.line_height = line_height;
    match context {
        TextContext::Table => {
            resolved.style.margin_start = 0.0;
            resolved.style.margin_start_fraction = 0.0;
        }
        TextContext::Flow => match block.kind {
            TextBlockKind::Caption => {
                resolved.style.align = TextAlignment::Center;
                resolved.style.margin_start = 0.0;
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::ListItem { depth, .. } => {
                resolved.style.margin_start =
                    base_size * profile.list_indent_em * (f32::from(depth) + 1.0);
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::DefinitionTerm { depth } => {
                resolved.style.margin_start = base_size * profile.list_indent_em * f32::from(depth);
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::DefinitionDescription { depth } => {
                resolved.style.margin_start =
                    base_size * profile.list_indent_em * (f32::from(depth) + 1.0);
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::Blockquote => {
                resolved.style.margin_start = base_size;
                resolved.style.margin_start_fraction = 0.0;
            }
            _ => {
                resolved.style.margin_start = 0.0;
                resolved.style.margin_start_fraction = 0.0;
            }
        },
    }

    for inline in &mut resolved.content {
        match inline {
            Inline::Text(run) => {
                run.style.color = Rgba::BLACK;
                // Unified typesetting starts from a neutral decoration layer.
                // Semantic/application decorations can opt back in explicitly.
                run.style.underline = false;
                run.style.size_scale = if run.style.baseline == TextBaseline::Normal {
                    scale
                } else {
                    scale * 0.75
                };
                if matches!(
                    block.kind,
                    TextBlockKind::Heading(_) | TextBlockKind::DefinitionTerm { .. }
                ) {
                    run.style.bold = true;
                }
            }
            Inline::Math(run) => run.size_scale = scale,
            Inline::Break => {}
        }
    }
    Cow::Owned(resolved)
}

fn paragraph_indent_em(profile: &ReaderTypesetting, writing_system: WritingSystem) -> f32 {
    if profile.paragraph_indent_mode == ParagraphIndentMode::Custom {
        return profile.paragraph_indent_em;
    }

    match writing_system {
        WritingSystem::Cjk => 2.0,
        WritingSystem::Latin => 1.5,
        WritingSystem::Other | WritingSystem::Unknown => profile.paragraph_indent_em,
    }
}

fn unified_heading_scale(h1_scale: f32, level: u8) -> f32 {
    let emphasis = (h1_scale - 1.0).max(0.0);
    1.0 + emphasis
        * match level {
            1 => 1.0,
            2 => 0.72,
            3 => 0.45,
            4 => 0.25,
            5 => 0.12,
            _ => 0.05,
        }
}

fn load_raster_image(
    source: &dyn BookSource,
    image: &ImageBlock,
) -> Result<RasterImage, LayoutError> {
    if let Some(raster) = source.raster_resource(&image.href)? {
        return Ok(RasterImage {
            width: raster.width,
            height: raster.height,
            pixels: raster.pixels,
        });
    }
    let resource = source.resource(&image.href)?;
    let decoded = image::load_from_memory(&resource.bytes)?.to_rgba8();
    Ok(RasterImage {
        width: decoded.width(),
        height: decoded.height(),
        pixels: decoded.into_raw().into(),
    })
}

fn dominant_paragraph_start_offset(fragments: &[&[Block]], content_width: f32) -> f32 {
    let mut offsets = fragments
        .iter()
        .flat_map(|blocks| blocks.iter())
        .filter_map(|block| match block {
            Block::Text(block) if block.kind == TextBlockKind::Paragraph => Some(
                (block.style.margin_start + content_width * block.style.margin_start_fraction)
                    .clamp(0.0, (content_width - 40.0).max(0.0)),
            ),
            Block::Text(_)
            | Block::Table(_)
            | Block::Image(_)
            | Block::Figure(_)
            | Block::Separator
            | Block::PageBreak => None,
        })
        .collect::<Vec<_>>();
    if offsets.is_empty() {
        return 0.0;
    }
    offsets.sort_by(f32::total_cmp);
    offsets[(offsets.len() - 1) / 2]
}

fn apply_list_hanging_indent(
    layout: &mut Layout<TextBrush>,
    kind: TextBlockKind,
    marker_width: f32,
) {
    let TextBlockKind::ListItem { .. } = kind else {
        return;
    };
    // Keep wrapped list-item lines aligned with the text after the marker. The
    // marker remains in the leading area while continuation lines are indented.
    layout.set_text_indent(
        marker_width,
        IndentOptions {
            hanging: true,
            ..IndentOptions::default()
        },
    );
}

fn text_alignment(alignment: TextAlignment) -> Alignment {
    match alignment {
        TextAlignment::Start => Alignment::Start,
        TextAlignment::Center => Alignment::Center,
        TextAlignment::End => Alignment::End,
        TextAlignment::Justify => Alignment::Justify,
    }
}

fn fragments_are_standalone_cover(fragments: &[&[Block]], cover: Option<&PublicationUrl>) -> bool {
    let Some(cover) = cover else {
        return false;
    };
    let mut visible_blocks = fragments
        .iter()
        .flat_map(|blocks| blocks.iter())
        .filter(|block| !matches!(block, Block::PageBreak));
    matches!(visible_blocks.next(), Some(Block::Image(image)) if &image.href == cover)
        && visible_blocks.next().is_none()
}

fn resolve_page_geometry(
    page_width: f32,
    page_height: f32,
    reader_style: &ReaderStyle,
) -> PageGeometry {
    let (content_left, content_width, column_count, continuation_offset_x) =
        resolve_horizontal_page_geometry(page_width, reader_style);
    let max_vertical_margin = page_height.mul_add(0.2, -8.0).max(20.0);
    let top_margin = reader_style.top_margin.min(max_vertical_margin);
    let bottom_margin = reader_style.bottom_margin.min(max_vertical_margin);
    let content_bottom = (page_height - bottom_margin).max(top_margin + 40.0);

    PageGeometry {
        left: content_left,
        top: top_margin,
        width: content_width,
        bottom: content_bottom,
        visible_pages: column_count,
        continuation_offset_x,
    }
}

fn resolve_horizontal_page_geometry(
    page_width: f32,
    reader_style: &ReaderStyle,
) -> (f32, f32, usize, f32) {
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

/// Returns the horizontal start of the reading content for a viewport.
///
/// Reader chrome uses this to align its title with the exact same centered
/// single- or double-column geometry used by pagination.
pub fn reading_content_left(page_width: f32, reader_style: &ReaderStyle) -> f32 {
    resolve_horizontal_page_geometry(page_width, reader_style).0
}

/// Returns the width of one reading column for a viewport.
pub fn reading_content_width(page_width: f32, reader_style: &ReaderStyle) -> f32 {
    resolve_horizontal_page_geometry(page_width, reader_style).1
}

struct StyledRange {
    range: Range<usize>,
    style: TextStyle,
}

struct PreparedText {
    layout: Arc<Layout<TextBrush>>,
    text: Arc<str>,
    source_text_start: usize,
    start_offset: f32,
    inline_images: Arc<[InlineImage]>,
}

struct PreparedTable {
    horizontal_offset: f32,
    column_widths: Vec<f32>,
    row_heights: Vec<f32>,
    cells: Vec<PreparedTableCell>,
    cell_padding: f32,
    center_content: bool,
    block_gap: f32,
    border: Rgba,
    header_fill: Rgba,
}

impl Default for PreparedTable {
    fn default() -> Self {
        Self {
            horizontal_offset: 0.0,
            column_widths: Vec::new(),
            row_heights: Vec::new(),
            cells: Vec::new(),
            cell_padding: 6.0,
            center_content: false,
            block_gap: TABLE_BLOCK_GAP,
            border: Rgba::BLACK,
            header_fill: Rgba {
                alpha: 0,
                ..Rgba::BLACK
            },
        }
    }
}

fn table_break_is_safe(table: &PreparedTable, row: usize) -> bool {
    row == table.row_heights.len()
        || !table
            .cells
            .iter()
            .any(|cell| cell.row < row && cell.row + cell.row_span > row)
}

fn next_safe_table_break(table: &PreparedTable, row_start: usize) -> usize {
    (row_start + 1..=table.row_heights.len())
        .find(|row| table_break_is_safe(table, *row))
        .unwrap_or(table.row_heights.len())
}

struct PreparedTableCell {
    row: usize,
    row_span: usize,
    column: usize,
    column_span: usize,
    header: bool,
    source: Option<SourceRange>,
    text: PreparedText,
    required_height: f32,
}

#[allow(
    clippy::cast_precision_loss,
    reason = "table column counts are bounded by the parsed table grid"
)]
fn fit_adaptive_column_widths(
    preferred_widths: &[f32],
    minimum_width: f32,
    available_width: f32,
) -> Vec<f32> {
    if preferred_widths.is_empty() || available_width <= 0.0 {
        return Vec::new();
    }
    let column_count = preferred_widths.len();
    let equal_width = available_width / column_count as f32;
    let minimum_width = minimum_width.min(equal_width).max(1.0);
    let minimum_total = minimum_width * column_count as f32;
    if minimum_total >= available_width {
        return vec![equal_width; column_count];
    }

    let preferred = preferred_widths
        .iter()
        .map(|width| width.max(minimum_width))
        .collect::<Vec<_>>();
    let preferred_total = preferred.iter().sum::<f32>();
    if preferred_total <= available_width {
        return preferred;
    }
    let mut fitted = {
        let available_flex = available_width - minimum_total;
        let preferred_flex = preferred
            .iter()
            .map(|width| width - minimum_width)
            .sum::<f32>();
        preferred
            .iter()
            .map(|width| {
                minimum_width
                    + available_flex * ((*width - minimum_width) / preferred_flex.max(1.0))
            })
            .collect::<Vec<_>>()
    };
    let fitted_total = fitted.iter().sum::<f32>();
    if let Some(last) = fitted.last_mut() {
        *last += available_width - fitted_total;
    }
    fitted
}

struct PreparedInlineImage {
    id: u64,
    index: usize,
    image: RasterImage,
    width: f32,
    height: f32,
}

struct FixedPageReplacementRequest {
    text: String,
    rect: FixedPageTextRect,
    source: Option<SourceRange>,
}

fn fixed_page_replacement_block(text: &str, source: Option<SourceRange>) -> TextBlock {
    let mut content = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            content.push(Inline::Break);
        }
        if !line.is_empty() {
            content.push(Inline::Text(TextRun {
                text: line.to_owned(),
                style: TextStyle::default(),
                link: None,
            }));
        }
    }
    TextBlock {
        kind: TextBlockKind::Paragraph,
        content,
        style: rebook_publication::BlockStyle {
            line_height: 1.2,
            ..rebook_publication::BlockStyle::default()
        },
        source,
    }
}

fn prepared_text_height(prepared: &PreparedText) -> f32 {
    let Some(first) = prepared.layout.get(0) else {
        return 0.0;
    };
    let Some(last) = prepared.layout.get(prepared.layout.len().saturating_sub(1)) else {
        return 0.0;
    };
    (last.metrics().block_max_coord - first.metrics().block_min_coord).max(0.0)
}

fn resolve_text_measure(
    block: &TextBlock,
    content_width: f32,
    minimum_width: f32,
) -> (f32, f32, f32) {
    let paragraph = block.kind == TextBlockKind::Paragraph;
    let first_line_indent = if paragraph { block.style.indent } else { 0.0 };
    let block_indent = if paragraph { 0.0 } else { block.style.indent };
    let start_offset = (block_indent
        + block.style.margin_start
        + content_width * block.style.margin_start_fraction)
        .clamp(0.0, (content_width - minimum_width).max(0.0));
    let available_width = (content_width - start_offset).max(minimum_width);
    (start_offset, available_width, first_line_indent)
}

fn prepare_inline_content(
    block: &TextBlock,
    fallback_color: Rgba,
    typography: &ReaderTypography,
    available_width: f32,
    svg_options: &resvg::usvg::Options<'_>,
) -> (String, Vec<StyledRange>, Vec<PreparedInlineImage>, usize) {
    let mut text = String::new();
    let mut spans = Vec::new();
    let mut inline_images = Vec::new();
    let prefix = match block.kind {
        TextBlockKind::ListItem {
            ordered: true,
            ordinal,
            ..
        } => format!("{ordinal}.\u{00a0}"),
        TextBlockKind::ListItem { ordered: false, .. } => "•\u{00a0}".to_owned(),
        _ => String::new(),
    };
    if !prefix.is_empty() {
        let start = text.len();
        text.push_str(&prefix);
        spans.push(StyledRange {
            range: start..text.len(),
            style: TextStyle {
                color: fallback_color,
                ..TextStyle::default()
            },
        });
    }
    let source_text_start = text.len();

    for inline in &block.content {
        match inline {
            Inline::Text(run) => {
                let start = text.len();
                text.push_str(&run.text);
                let mut style = run.style;
                if style.color == Rgba::BLACK {
                    style.color = fallback_color;
                }
                spans.push(StyledRange {
                    range: start..text.len(),
                    style,
                });
            }
            Inline::Math(run) => {
                let id = u64::try_from(inline_images.len()).unwrap_or(u64::MAX);
                if let Ok(image) = rasterize_formula(
                    run,
                    typography,
                    fallback_color,
                    available_width,
                    svg_options,
                ) {
                    inline_images.push(PreparedInlineImage {
                        id,
                        index: text.len(),
                        width: image.1,
                        height: image.2,
                        image: image.0,
                    });
                } else {
                    let start = text.len();
                    text.push('$');
                    text.push_str(&run.latex);
                    text.push('$');
                    spans.push(StyledRange {
                        range: start..text.len(),
                        style: TextStyle {
                            size_scale: run.size_scale,
                            color: fallback_color,
                            ..TextStyle::default()
                        },
                    });
                }
            }
            Inline::Break => text.push('\n'),
        }
    }
    (text, spans, inline_images, source_text_start)
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "formula dimensions are clamped to bounded reader viewport pixels"
)]
fn rasterize_formula(
    run: &MathRun,
    typography: &ReaderTypography,
    color: Rgba,
    available_width: f32,
    svg_options: &resvg::usvg::Options<'_>,
) -> Result<(RasterImage, f32, f32), String> {
    use resvg::tiny_skia::Pixmap;
    use resvg::usvg::{Transform, Tree};

    const RASTER_SCALE: f32 = 2.0;
    const PADDING: f32 = 1.5;
    let semantic_scale = if run.display { 1.12 } else { 1.0 };
    let font_size = (typography.font_size * run.size_scale.clamp(0.5, 3.0) * semantic_scale)
        .max(typography.minimum_font_size);
    let text_color = format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue);
    let rendered = rebook_math::math::render_math(&run.latex, font_size, &text_color, run.display)?;
    let source_width = (rendered.width + PADDING * 2.0).max(1.0);
    let source_height = (rendered.ascent + rendered.descent + PADDING * 2.0).max(1.0);
    let width_scale = (available_width / source_width).min(1.0);
    let display_width = (source_width * width_scale).max(1.0);
    let display_height = (source_height * width_scale).max(1.0);
    let view_y = -rendered.ascent - PADDING;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" width="{}" height="{}"><g transform="translate({}, 0)">{}</g></svg>"#,
        -PADDING,
        view_y,
        source_width,
        source_height,
        source_width,
        source_height,
        PADDING,
        rendered.svg_fragment
    );
    let tree = Tree::from_data(svg.as_bytes(), svg_options).map_err(|error| error.to_string())?;
    let pixel_width = (display_width * RASTER_SCALE).ceil().max(1.0) as u32;
    let pixel_height = (display_height * RASTER_SCALE).ceil().max(1.0) as u32;
    let mut pixmap = Pixmap::new(pixel_width, pixel_height)
        .ok_or_else(|| format!("failed to allocate formula raster {pixel_width}x{pixel_height}"))?;
    resvg::render(
        &tree,
        Transform::from_scale(
            pixel_width as f32 / source_width,
            pixel_height as f32 / source_height,
        ),
        &mut pixmap.as_mut(),
    );
    Ok((
        RasterImage {
            width: pixel_width,
            height: pixel_height,
            pixels: pixmap.data().to_vec().into(),
        },
        display_width,
        display_height,
    ))
}

struct Paginator {
    viewport: LayoutViewport,
    background: Rgba,
    left: f32,
    top: f32,
    width: f32,
    bottom: f32,
    column_has_content: bool,
    cursor_y: f32,
    pages: Vec<PageLayout>,
    items: Vec<PageItem>,
    center_standalone_image: bool,
    minimum_paragraph_gap: f32,
    previous_block_was_paragraph: bool,
    media_start_offset: f32,
    leading_gap: f32,
    pending_leading_gap: f32,
    forced_page_break: bool,
}

#[derive(Clone, Copy)]
struct PageGeometry {
    left: f32,
    top: f32,
    width: f32,
    bottom: f32,
    visible_pages: usize,
    continuation_offset_x: f32,
}

impl Paginator {
    fn new(
        viewport: LayoutViewport,
        background: Rgba,
        geometry: PageGeometry,
        center_standalone_image: bool,
        minimum_paragraph_gap: f32,
    ) -> Self {
        Self {
            viewport,
            background,
            left: geometry.left,
            top: geometry.top,
            width: geometry.width,
            bottom: geometry.bottom,
            column_has_content: false,
            cursor_y: geometry.top,
            pages: Vec::new(),
            items: Vec::new(),
            center_standalone_image,
            minimum_paragraph_gap: minimum_paragraph_gap.max(0.0),
            previous_block_was_paragraph: false,
            media_start_offset: 0.0,
            leading_gap: 0.0,
            pending_leading_gap: 0.0,
            forced_page_break: false,
        }
    }

    fn push_text(&mut self, prepared: &PreparedText, block: &TextBlock) -> Result<(), LayoutError> {
        self.forced_page_break = false;
        let is_paragraph = matches!(block.kind, TextBlockKind::Paragraph);
        if is_paragraph && self.previous_block_was_paragraph {
            self.ensure_minimum_spacing(self.minimum_paragraph_gap);
        }
        self.add_spacing(block.style.margin_before);
        let mut line_start = 0;
        while line_start < prepared.layout.len() {
            let first = prepared
                .layout
                .get(line_start)
                .ok_or(LayoutError::InvalidLayout)?;
            let first_top = first.metrics().block_min_coord;
            let mut line_end = line_start;
            let mut slice_height = 0.0;
            while line_end < prepared.layout.len() {
                let line = prepared
                    .layout
                    .get(line_end)
                    .ok_or(LayoutError::InvalidLayout)?;
                let candidate_height = line.metrics().block_max_coord - first_top;
                let remaining = self.bottom - self.cursor_y;
                if candidate_height > remaining && line_end > line_start {
                    break;
                }
                if candidate_height > remaining && self.column_has_content {
                    self.advance_column();
                    break;
                }
                slice_height = candidate_height.max(line.metrics().line_height);
                line_end += 1;
            }
            if line_end == line_start {
                continue;
            }
            self.items.push(PageItem::Text(TextPlacement {
                layout: Arc::clone(&prepared.layout),
                text: Arc::clone(&prepared.text),
                source_text_start: prepared.source_text_start,
                lines: line_start..line_end,
                origin_x: self.column_left() + prepared.start_offset,
                origin_y: self.cursor_y - first_top,
                source: block.source.clone(),
                inline_images: Arc::clone(&prepared.inline_images),
            }));
            self.pending_leading_gap = 0.0;
            self.column_has_content = true;
            self.cursor_y += slice_height;
            line_start = line_end;
            if line_start < prepared.layout.len() {
                self.advance_column();
            }
        }
        self.add_spacing(block.style.margin_after);
        self.previous_block_was_paragraph = is_paragraph;
        Ok(())
    }

    fn push_table(&mut self, table: &PreparedTable) {
        self.forced_page_break = false;
        self.previous_block_was_paragraph = false;
        if table.row_heights.is_empty() || table.column_widths.is_empty() {
            return;
        }
        self.ensure_minimum_spacing(table.block_gap);
        let mut row_start = 0;
        while row_start < table.row_heights.len() {
            let remaining = self.bottom - self.cursor_y;
            let mut height = 0.0;
            let mut last_safe_break = None;
            for row_end in row_start + 1..=table.row_heights.len() {
                let candidate = height + table.row_heights[row_end - 1];
                if candidate > remaining && row_end > row_start + 1 {
                    break;
                }
                height = candidate;
                if table_break_is_safe(table, row_end) {
                    last_safe_break = Some((row_end, height));
                }
                if candidate > remaining {
                    break;
                }
            }
            let Some((row_end, chunk_height)) = last_safe_break else {
                if self.column_has_content {
                    self.advance_column();
                    continue;
                }
                let row_end = next_safe_table_break(table, row_start);
                let chunk_height = table.row_heights[row_start..row_end].iter().sum();
                self.push_table_chunk(table, row_start, row_end, chunk_height);
                row_start = row_end;
                if row_start < table.row_heights.len() {
                    self.advance_column();
                }
                continue;
            };
            if chunk_height > remaining && self.column_has_content {
                self.advance_column();
                continue;
            }
            self.push_table_chunk(table, row_start, row_end, chunk_height);
            row_start = row_end;
            if row_start < table.row_heights.len() {
                self.advance_column();
            }
        }
        self.add_spacing(table.block_gap);
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "resolved grid coordinates come from bounded table spans and row content"
    )]
    fn push_table_chunk(
        &mut self,
        table: &PreparedTable,
        row_start: usize,
        row_end: usize,
        height: f32,
    ) {
        let table_y = self.cursor_y;
        let mut row_offsets = Vec::with_capacity(row_end - row_start + 1);
        row_offsets.push(0.0);
        for row_height in &table.row_heights[row_start..row_end] {
            row_offsets.push(row_offsets.last().copied().unwrap_or(0.0) + row_height);
        }
        let cells = table
            .cells
            .iter()
            .filter(|cell| cell.row >= row_start && cell.row + cell.row_span <= row_end)
            .map(|cell| {
                let local_row = cell.row - row_start;
                let cell_x = self.column_left()
                    + self.media_start_offset
                    + table.horizontal_offset
                    + table.column_widths[..cell.column].iter().sum::<f32>();
                let cell_y = table_y + row_offsets[local_row];
                let cell_width = table.column_widths[cell.column..cell.column + cell.column_span]
                    .iter()
                    .sum::<f32>();
                let cell_height = row_offsets[local_row + cell.row_span] - row_offsets[local_row];
                let text = cell.text.layout.get(0).map(|first| {
                    let top_padding = if table.center_content {
                        ((cell_height - prepared_text_height(&cell.text)) / 2.0).max(0.0)
                    } else {
                        table.cell_padding
                    };
                    TextPlacement {
                        layout: Arc::clone(&cell.text.layout),
                        text: Arc::clone(&cell.text.text),
                        source_text_start: cell.text.source_text_start,
                        lines: 0..cell.text.layout.len(),
                        origin_x: cell_x + table.cell_padding + cell.text.start_offset,
                        origin_y: cell_y + top_padding - first.metrics().block_min_coord,
                        source: cell.source.clone(),
                        inline_images: Arc::clone(&cell.text.inline_images),
                    }
                });
                TableCellPlacement {
                    x: cell_x,
                    y: cell_y,
                    width: cell_width,
                    height: cell_height,
                    header: cell.header,
                    text,
                }
            })
            .collect();
        self.items.push(PageItem::Table(TablePlacement {
            cells,
            y: table_y,
            height,
            border: table.border,
            header_fill: table.header_fill,
        }));
        self.column_has_content = true;
        self.cursor_y += height;
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "decoded image dimensions are bounded by publication resource limits"
    )]
    fn image_display_size(&self, image: &RasterImage, style: ImageStyle) -> (f32, f32) {
        let intrinsic_width = image.width.max(1) as f32;
        let intrinsic_height = image.height.max(1) as f32;
        let aspect_ratio = intrinsic_width / intrinsic_height;
        let content_height = self.bottom - self.top;
        let media_width = (self.width - self.media_start_offset).max(1.0);
        let requested_height = style.height.map(|height| height.resolve(content_height));
        let requested_width = style
            .width
            .map(|width| width.resolve(media_width))
            .or_else(|| requested_height.map(|height| height * aspect_ratio))
            .unwrap_or(intrinsic_width)
            .max(1.0);
        let requested_height = requested_height
            .unwrap_or(requested_width / aspect_ratio)
            .max(1.0);
        let max_width = style
            .max_width
            .map_or(media_width, |width| width.resolve(media_width))
            .clamp(1.0, media_width);
        let max_height = style
            .max_height
            .map_or(content_height, |height| height.resolve(content_height))
            .clamp(1.0, content_height);
        let scale = (max_width / requested_width)
            .min(max_height / requested_height)
            .min(1.0);
        (requested_width * scale, requested_height * scale)
    }

    fn prepare_group(&mut self, content_height: f32, outer_gap: f32) {
        self.pending_leading_gap = outer_gap.max(0.0);
        self.ensure_minimum_spacing(outer_gap);
        let full_height = self.bottom - self.top;
        if content_height <= full_height
            && self.cursor_y + content_height > self.bottom
            && self.column_has_content
        {
            self.advance_column();
            self.leading_gap = self.leading_gap.max(outer_gap.max(0.0));
        }
    }

    fn push_image(
        &mut self,
        image: RasterImage,
        style: ImageStyle,
        source: Option<SourceRange>,
        text_layer: Option<FixedPageTextLayer>,
    ) -> Vec<FixedPageReplacementRequest> {
        self.push_image_with_gaps(
            image,
            style,
            source,
            text_layer,
            IMAGE_BLOCK_GAP,
            IMAGE_BLOCK_GAP,
        )
    }

    fn push_image_with_gaps(
        &mut self,
        image: RasterImage,
        style: ImageStyle,
        source: Option<SourceRange>,
        text_layer: Option<FixedPageTextLayer>,
        minimum_before: f32,
        minimum_after: f32,
    ) -> Vec<FixedPageReplacementRequest> {
        let restore_gap_on_empty_page = !self.column_has_content
            && self.items.is_empty()
            && !self.pages.is_empty()
            && !self.forced_page_break;
        self.forced_page_break = false;
        self.previous_block_was_paragraph = false;
        let (width, height) = self.image_display_size(&image, style);
        let media_width = (self.width - self.media_start_offset).max(1.0);
        let block_gap = style.margin_before.max(minimum_before);
        let page_count_before_spacing = self.pages.len();
        self.ensure_minimum_spacing(block_gap);
        if self.cursor_y + height > self.bottom && self.column_has_content {
            self.advance_column();
        }
        if restore_gap_on_empty_page || self.pages.len() > page_count_before_spacing {
            self.leading_gap = self.leading_gap.max(block_gap);
        }
        let x = self.column_left() + self.media_start_offset + (media_width - width) / 2.0;
        let replacements = text_layer.as_ref().map_or_else(Vec::new, |layer| {
            let Some(replacement) = layer.replacement.as_ref() else {
                return Vec::new();
            };
            if layer.width <= 0.0 || layer.height <= 0.0 {
                return Vec::new();
            }
            let scale_x = width / layer.width;
            let scale_y = height / layer.height;
            replacement
                .segments
                .iter()
                .map(|segment| FixedPageReplacementRequest {
                    text: segment.text.clone(),
                    rect: FixedPageTextRect {
                        x: x + segment.rect.x * scale_x,
                        y: self.cursor_y + segment.rect.y * scale_y,
                        width: segment.rect.width * scale_x,
                        height: segment.rect.height * scale_y,
                    },
                    source: fixed_page_replacement_source(source.as_ref(), segment),
                })
                .collect()
        });
        self.items.push(PageItem::Image(ImagePlacement {
            image,
            x,
            y: self.cursor_y,
            width,
            height,
            source,
            text_layer,
            replacement: None,
        }));
        self.pending_leading_gap = 0.0;
        self.column_has_content = true;
        self.cursor_y += height + style.margin_after.max(minimum_after);
        replacements
    }

    fn push_fixed_page_replacement(
        &mut self,
        prepared: &PreparedText,
        request: FixedPageReplacementRequest,
    ) -> Result<(), LayoutError> {
        let Some(first) = prepared.layout.get(0) else {
            return Ok(());
        };
        let Some(PageItem::Image(image)) = self.items.last_mut() else {
            return Err(LayoutError::InvalidLayout);
        };
        let padding = 1.5;
        let segment = FixedPageTextReplacementSegmentPlacement {
            rect: request.rect,
            text: TextPlacement {
                layout: Arc::clone(&prepared.layout),
                text: Arc::clone(&prepared.text),
                source_text_start: prepared.source_text_start,
                lines: 0..prepared.layout.len(),
                origin_x: request.rect.x + padding,
                origin_y: request.rect.y + padding - first.metrics().block_min_coord,
                source: request.source,
                inline_images: Arc::clone(&prepared.inline_images),
            },
        };
        image
            .replacement
            .get_or_insert_with(|| FixedPageTextReplacementPlacement {
                segments: Vec::new(),
            })
            .segments
            .push(segment);
        Ok(())
    }

    fn ensure_minimum_spacing(&mut self, amount: f32) {
        let Some(content_bottom) = self.items.last().and_then(|item| match item {
            PageItem::Text(text) => text
                .lines
                .end
                .checked_sub(1)
                .and_then(|line| text.layout.get(line))
                .map(|line| {
                    let metrics = line.metrics();
                    let line_box_bottom = metrics.block_min_coord + metrics.line_height;
                    text.origin_y + metrics.block_max_coord.max(line_box_bottom)
                }),
            PageItem::Image(image) => Some(image.y + image.height),
            PageItem::Table(table) => Some(table.y + table.height),
            PageItem::Separator(separator) => Some(separator.y + 1.0),
        }) else {
            if !self.pages.is_empty() && !self.forced_page_break {
                self.leading_gap = self.leading_gap.max(amount.max(0.0));
            }
            return;
        };
        let target = content_bottom + amount.max(0.0);
        if target > self.bottom {
            self.advance_column();
            self.leading_gap = self.leading_gap.max(amount.max(0.0));
        } else {
            self.cursor_y = self.cursor_y.max(target);
        }
    }

    fn push_separator(&mut self) {
        self.forced_page_break = false;
        self.previous_block_was_paragraph = false;
        self.add_spacing(12.0);
        if self.cursor_y + 1.0 > self.bottom && self.column_has_content {
            self.advance_column();
        }
        self.items.push(PageItem::Separator(SeparatorPlacement {
            x: self.column_left() + self.width * 0.25,
            y: self.cursor_y,
            width: self.width * 0.5,
        }));
        self.pending_leading_gap = 0.0;
        self.column_has_content = true;
        self.cursor_y += 13.0;
    }

    fn add_spacing(&mut self, amount: f32) {
        let amount = amount.max(0.0);
        if self.cursor_y + amount > self.bottom && self.column_has_content {
            self.advance_column();
        } else {
            self.cursor_y += amount;
        }
    }

    fn add_semantic_spacing(&mut self, amount: f32) {
        let page_count = self.pages.len();
        self.add_spacing(amount);
        if self.pages.len() > page_count {
            self.leading_gap = self.leading_gap.max(amount.max(0.0));
        }
    }

    fn force_page(&mut self) {
        self.pending_leading_gap = 0.0;
        if self.column_has_content || !self.items.is_empty() {
            self.advance_column();
        }
        self.forced_page_break = true;
    }

    fn column_left(&self) -> f32 {
        self.left
    }

    fn advance_column(&mut self) {
        let pending_leading_gap = self.pending_leading_gap;
        self.commit_page();
        self.leading_gap = self.leading_gap.max(pending_leading_gap);
    }

    fn commit_page(&mut self) {
        if self.items.is_empty() {
            self.cursor_y = self.top;
            return;
        }
        if self.center_standalone_image
            && let [PageItem::Image(image)] = self.items.as_mut_slice()
        {
            let available_height = self.bottom - self.top;
            let centered_y = self.top + ((available_height - image.height) / 2.0).max(0.0);
            let offset_y = centered_y - image.y;
            image.y = centered_y;
            if let Some(replacement) = image.replacement.as_mut() {
                for segment in &mut replacement.segments {
                    segment.rect.y += offset_y;
                    segment.text.origin_y += offset_y;
                }
            }
        }
        self.pages.push(PageLayout {
            viewport: self.viewport,
            background: self.background,
            leading_gap: std::mem::take(&mut self.leading_gap),
            items: std::mem::take(&mut self.items),
        });
        self.column_has_content = false;
        self.cursor_y = self.top;
    }

    fn finish(mut self) -> Vec<PageLayout> {
        self.commit_page();
        if self.pages.is_empty() {
            self.pages.push(PageLayout {
                viewport: self.viewport,
                background: self.background,
                leading_gap: 0.0,
                items: Vec::new(),
            });
        }
        self.pages
    }
}

fn fixed_page_replacement_source(
    source: Option<&SourceRange>,
    segment: &rebook_publication::FixedPageTextReplacementSegment,
) -> Option<SourceRange> {
    let mut source = source?.clone();
    let start = source
        .start
        .text_offset
        .saturating_add(segment.source_offset);
    source.start.text_offset = start;
    source.end.spine = source.start.spine.clone();
    source.end.node.clone_from(&source.start.node);
    source.end.text_offset =
        start.saturating_add(u64::try_from(segment.text.chars().count()).unwrap_or(u64::MAX));
    Some(source)
}

/// Native layout errors.
#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("viewport dimensions must be positive")]
    InvalidViewport,
    #[error("text layout produced inconsistent line metrics")]
    InvalidLayout,
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error("image decode failed: {0}")]
    Image(#[from] ImageError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_typesetting_replaces_authored_heading_metrics() {
        let block = TextBlock {
            kind: TextBlockKind::Heading(2),
            content: vec![Inline::Text(TextRun {
                text: "Heading".into(),
                style: TextStyle {
                    size_scale: 2.8,
                    ..TextStyle::default()
                },
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                align: TextAlignment::Center,
                margin_before: 40.0,
                margin_after: 50.0,
                indent: 20.0,
                line_height: 0.8,
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        assert_eq!(resolved.style.align, TextAlignment::Start);
        assert!(resolved.style.margin_before.abs() < 0.001);
        assert!((resolved.style.margin_after - 14.0).abs() < 0.001);
        assert!(resolved.style.indent.abs() < 0.001);
        assert!((resolved.style.line_height - 1.3).abs() < 0.001);
        let Inline::Text(run) = &resolved.content[0] else {
            panic!("expected text run");
        };
        assert!((run.style.size_scale - 1.432).abs() < 0.001);
        assert!(run.style.bold);
    }

    #[test]
    fn book_typesetting_preserves_authored_metrics() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "Body".into(),
                style: TextStyle {
                    size_scale: 1.35,
                    ..TextStyle::default()
                },
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                margin_after: 23.0,
                line_height: 1.25,
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };

        let resolved = resolve_text_block(&block, &ReaderStyle::default(), TextContext::Flow);
        assert_eq!(resolved.as_ref(), &block);
    }

    #[test]
    fn reader_typesetting_normalizes_persisted_values() {
        let mut typesetting = ReaderTypesetting {
            mode: TypesettingMode::Unified,
            heading_scale: f32::NAN,
            body_line_height: 9.0,
            paragraph_indent_mode: ParagraphIndentMode::Custom,
            paragraph_indent_em: 8.0,
            paragraph_gap_em: -1.0,
            heading_body_gap_em: 4.0,
            media_gap_em: 0.1,
            caption_font_scale: 0.1,
            caption_gap_em: 4.0,
            list_indent_em: 8.0,
            table_font_scale: 0.1,
            table_line_height: f32::INFINITY,
            table_cell_padding_em: 2.0,
        };
        typesetting.normalize();
        assert!((typesetting.heading_scale - 1.6).abs() < 0.001);
        assert!((typesetting.body_line_height - 2.4).abs() < 0.001);
        assert!((typesetting.paragraph_indent_em - 4.0).abs() < 0.001);
        assert!(typesetting.paragraph_gap_em.abs() < 0.001);
        assert!((typesetting.heading_body_gap_em - 2.0).abs() < 0.001);
        assert!((typesetting.media_gap_em - 0.5).abs() < 0.001);
        assert!((typesetting.caption_font_scale - 0.7).abs() < 0.001);
        assert!((typesetting.caption_gap_em - 1.0).abs() < 0.001);
        assert!((typesetting.list_indent_em - 3.0).abs() < 0.001);
        assert!((typesetting.table_font_scale - 0.7).abs() < 0.001);
        assert!((typesetting.table_line_height - 1.45).abs() < 0.001);
        assert!((typesetting.table_cell_padding_em - 1.0).abs() < 0.001);
    }

    #[test]
    fn adaptive_table_widths_preserve_the_available_measure() {
        let widths = fit_adaptive_column_widths(&[50.0, 200.0], 40.0, 300.0);
        assert_eq!(widths.len(), 2);
        assert!(widths[0] < widths[1]);
        assert!((widths.iter().sum::<f32>() - 250.0).abs() < 0.001);
        assert!(widths.iter().all(|width| *width >= 40.0));
    }

    #[test]
    fn unified_table_preserves_authored_alignment_and_centers_unspecified_cells() {
        let cell = |authored_alignment| TableCell {
            text: TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: "Same content".into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            },
            authored_alignment,
            column_span: 1,
            row_span: 1,
            header: false,
        };
        let table = TableBlock {
            rows: vec![TableRow {
                cells: vec![cell(None), cell(Some(TextAlignment::Start))],
            }],
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let table = LayoutEngine::new().shape_table(&table, &style, 500.0);
        let centered_offset = table.cells[0]
            .text
            .layout
            .get(0)
            .expect("default cell should contain text")
            .metrics()
            .offset;
        let authored_offset = table.cells[1]
            .text
            .layout
            .get(0)
            .expect("authored cell should contain text")
            .metrics()
            .offset;

        assert!(
            centered_offset > 0.0,
            "unspecified cells should be centered"
        );
        assert!(
            authored_offset.abs() < 0.001,
            "authored left alignment should be preserved"
        );
    }

    #[test]
    fn unified_table_keeps_short_cells_on_one_line_when_space_allows() {
        let cell = |text: &str| TableCell {
            text: TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            },
            authored_alignment: None,
            column_span: 1,
            row_span: 1,
            header: false,
        };
        let table = TableBlock {
            rows: vec![
                TableRow {
                    cells: vec![cell(""), cell("U.S."), cell("Norway")],
                },
                TableRow {
                    cells: vec![
                        cell("Introductory course"),
                        cell("1.7 books"),
                        cell("2.8 books"),
                    ],
                },
                TableRow {
                    cells: vec![
                        cell("Advanced course"),
                        cell("2.3 books"),
                        cell("2.8 books"),
                    ],
                },
            ],
            source: None,
        };
        let mut style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        style.typography.font_size = 16.0;
        style.typesetting.table_font_scale = 0.8;

        let table = LayoutEngine::new().shape_table(&table, &style, 744.0);
        assert!(table.column_widths.iter().sum::<f32>() < 744.0);
        assert!(
            table.cells.iter().all(|cell| cell.text.layout.len() == 1),
            "short table cells should remain unwrapped"
        );
    }

    #[test]
    fn unified_typesetting_clears_authored_and_link_underlines() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "Linked and underlined".into(),
                style: TextStyle {
                    underline: true,
                    ..TextStyle::default()
                },
                link: Some(PublicationUrl::parse("chapter.xhtml#target").unwrap()),
            })],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        let Inline::Text(run) = &resolved.content[0] else {
            panic!("expected text run");
        };
        assert!(!run.style.underline);
        assert!(run.link.is_some());
    }

    #[test]
    fn unified_typesetting_applies_a_consistent_list_indent() {
        let block = TextBlock {
            kind: TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
            },
            content: vec![Inline::Text(TextRun {
                text: "A list item".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                margin_start: 90.0,
                margin_start_fraction: 0.2,
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        assert!((resolved.style.margin_start - 30.0).abs() < 0.001);
        assert!(resolved.style.margin_start_fraction.abs() < 0.001);
    }

    #[test]
    fn unified_typesetting_increases_indent_for_nested_list_items() {
        let block = TextBlock {
            kind: TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 2,
            },
            content: vec![Inline::Text(TextRun {
                text: "A nested list item".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        assert!((resolved.style.margin_start - 90.0).abs() < 0.001);
        assert!(resolved.style.margin_start_fraction.abs() < 0.001);
    }

    #[test]
    fn unified_typesetting_distinguishes_definition_terms_and_descriptions() {
        let text = || {
            vec![Inline::Text(TextRun {
                text: "Definition".into(),
                style: TextStyle::default(),
                link: None,
            })]
        };
        let term = TextBlock {
            kind: TextBlockKind::DefinitionTerm { depth: 0 },
            content: text(),
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let description = TextBlock {
            kind: TextBlockKind::DefinitionDescription { depth: 0 },
            content: text(),
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let resolved_term = resolve_text_block(&term, &style, TextContext::Flow);
        let resolved_description = resolve_text_block(&description, &style, TextContext::Flow);
        assert!(resolved_term.style.margin_start.abs() < 0.001);
        assert!((resolved_description.style.margin_start - 30.0).abs() < 0.001);
        let Inline::Text(term_text) = &resolved_term.content[0] else {
            panic!("expected definition term text");
        };
        assert!(term_text.style.bold);
    }

    #[test]
    fn unified_typesetting_applies_first_line_paragraph_indent() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "A sufficiently long paragraph that wraps onto another line so its first-line indentation can be distinguished from continuation lines.".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        assert!((resolved.style.indent - 40.0).abs() < 0.001);

        let mut engine = LayoutEngine::new();
        let prepared = engine.shape_text_with_min_width(&resolved, &style, 320.0, 40.0);
        assert!(prepared.layout.len() > 1);
        let first_offset = prepared.layout.get(0).unwrap().metrics().offset;
        let continuation_offset = prepared.layout.get(1).unwrap().metrics().offset;
        assert!((first_offset - 40.0).abs() < 0.01);
        assert!(continuation_offset.abs() < 0.01);
    }

    #[test]
    fn automatic_paragraph_indent_uses_publication_writing_system() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "Paragraph".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let mut style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            writing_system: WritingSystem::Cjk,
            ..ReaderStyle::default()
        };

        let cjk = resolve_text_block(&block, &style, TextContext::Flow);
        assert!((cjk.style.indent - 40.0).abs() < 0.001);

        style.writing_system = WritingSystem::Latin;
        let latin = resolve_text_block(&block, &style, TextContext::Flow);
        assert!((latin.style.indent - 30.0).abs() < 0.001);

        style.typesetting.paragraph_indent_mode = ParagraphIndentMode::Custom;
        style.typesetting.paragraph_indent_em = 0.8;
        let custom = resolve_text_block(&block, &style, TextContext::Flow);
        assert!((custom.style.indent - 16.0).abs() < 0.001);
    }

    #[test]
    fn reader_typography_matches_readest_defaults_and_builds_cjk_stacks() {
        let typography = ReaderTypography::default();
        assert_eq!(typography.default_font, ReaderDefaultFont::Serif);
        assert_eq!(typography.default_cjk_font, "LXGW WenKai GB Screen");
        assert_eq!(typography.serif_font, "Bitter");
        assert_eq!(typography.sans_serif_font, "Roboto");
        assert_eq!(typography.monospace_font, "Consolas");
        assert!((typography.font_size - 20.0).abs() < f32::EPSILON);
        assert!((typography.minimum_font_size - 12.0).abs() < f32::EPSILON);
        assert_eq!(typography.font_weight, 400);
        assert!(typography.serif_stack().contains("\"Bitter\""));
        assert!(typography.serif_stack().contains("\"SimSun\""));
        assert!(typography.serif_stack().ends_with("serif"));
        assert!(
            typography
                .sans_serif_stack()
                .contains("\"Microsoft YaHei\"")
        );
        assert!(typography.sans_serif_stack().ends_with("sans-serif"));
        assert!(typography.monospace_stack().ends_with("monospace"));
    }

    #[test]
    fn reader_typography_normalizes_persisted_values() {
        let mut typography = ReaderTypography {
            default_cjk_font: "  ".into(),
            serif_font: "  Georgia  ".into(),
            sans_serif_font: String::new(),
            monospace_font: String::new(),
            font_size: f32::NAN,
            minimum_font_size: -4.0,
            font_weight: 455,
            ..ReaderTypography::default()
        };
        typography.normalize();
        assert_eq!(typography.default_cjk_font, "LXGW WenKai GB Screen");
        assert_eq!(typography.serif_font, "Georgia");
        assert_eq!(typography.sans_serif_font, "Roboto");
        assert_eq!(typography.monospace_font, "Consolas");
        assert!((typography.font_size - 20.0).abs() < f32::EPSILON);
        assert!((typography.minimum_font_size - 1.0).abs() < f32::EPSILON);
        assert_eq!(typography.font_weight, 500);
    }

    #[test]
    fn default_page_geometry_compacts_only_the_top_inset() {
        let style = ReaderStyle::default();
        let geometry = resolve_page_geometry(800.0, 600.0, &style);

        assert!((style.top_margin - DEFAULT_TOP_MARGIN).abs() < f32::EPSILON);
        assert!((style.bottom_margin - DEFAULT_BOTTOM_MARGIN).abs() < f32::EPSILON);
        assert!((geometry.top - DEFAULT_TOP_MARGIN).abs() < f32::EPSILON);
        assert!((geometry.bottom - (600.0 - DEFAULT_BOTTOM_MARGIN)).abs() < f32::EPSILON);
    }

    #[test]
    fn wide_viewports_cap_and_center_each_reading_column() {
        let viewport_width = 3_000.0;
        let page_height = 900.0;
        let single = resolve_page_geometry(
            viewport_width,
            page_height,
            &ReaderStyle {
                spread: SpreadMode::Single,
                ..ReaderStyle::default()
            },
        );
        assert_eq!(single.visible_pages, 1);
        assert!((single.width - MAX_COLUMN_WIDTH).abs() < f32::EPSILON);
        assert!((single.left - (viewport_width - MAX_COLUMN_WIDTH) / 2.0).abs() < f32::EPSILON);

        let scroll = resolve_page_geometry(
            viewport_width,
            page_height,
            &ReaderStyle {
                spread: SpreadMode::Scroll,
                ..ReaderStyle::default()
            },
        );
        assert_eq!(scroll.visible_pages, 1);
        assert!((scroll.width - single.width).abs() < f32::EPSILON);
        assert!((scroll.left - single.left).abs() < f32::EPSILON);
        assert!((scroll.top - single.top).abs() < f32::EPSILON);
        assert!((scroll.bottom - single.bottom).abs() < f32::EPSILON);

        let double = resolve_page_geometry(
            viewport_width,
            page_height,
            &ReaderStyle {
                spread: SpreadMode::Double,
                ..ReaderStyle::default()
            },
        );
        let spread_width = MAX_COLUMN_WIDTH * 2.0 + DEFAULT_COLUMN_GAP;
        assert_eq!(double.visible_pages, 2);
        assert!((double.width - MAX_COLUMN_WIDTH).abs() < f32::EPSILON);
        assert!((double.left - (viewport_width - spread_width) / 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn zero_column_gap_places_double_pages_next_to_each_other() {
        let geometry = resolve_page_geometry(
            1_200.0,
            700.0,
            &ReaderStyle {
                spread: SpreadMode::Double,
                column_gap: 0.0,
                ..ReaderStyle::default()
            },
        );

        assert_eq!(geometry.visible_pages, 2);
        assert!((geometry.continuation_offset_x - geometry.width).abs() < f32::EPSILON);
    }

    #[test]
    fn wrapped_list_items_use_the_full_marker_advance_as_hanging_indent() {
        let block = TextBlock {
            kind: TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
            },
            content: vec![Inline::Text(TextRun {
                text: "Create hierarchy. Type embodies what you want to say with your design, and it creates and supports your website structure.".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let mut engine = LayoutEngine::new();
        let prepared =
            engine.shape_text_with_min_width(&block, &ReaderStyle::default(), 320.0, 40.0);
        assert!(prepared.layout.len() > 1);
        let marker_width = engine.measure_list_marker_width(
            "•\u{00a0}",
            &ReaderStyle::default().typography.default_stack(),
            &ReaderStyle::default().typography,
        );
        let continuation_x = prepared.layout.get(1).unwrap().metrics().offset;
        assert!((continuation_x - marker_width).abs() < 0.01);
    }
    use rebook_publication::{
        Book, FixedPageTextReplacement, FixedPageTextReplacementSegment, FixedPageTextSpan,
        ImageBlock, ImageLength, Metadata, PublicationId, PublicationUrl, RasterResource,
        RenditionLayout, Resource, SourceAnchor, SpineItemId, TableCell, TableRow, TocEntry,
    };

    struct EmptySource {
        book: Book,
    }

    impl BookSource for EmptySource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            unreachable!()
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }

        fn raster_resource(
            &self,
            _href: &PublicationUrl,
        ) -> Result<Option<RasterResource>, PublicationError> {
            Ok(Some(RasterResource {
                width: 200,
                height: 100,
                pixels: Vec::new().into(),
            }))
        }
    }

    #[test]
    fn long_paragraph_is_split_into_multiple_pages() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::<TocEntry>::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(rebook_publication::TextRun {
                    text: "这是用于验证分页的数据。".repeat(500),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            })],
            anchors: Vec::new(),
        };
        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(600, 400).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        assert!(layout.pages.len() > 1);
    }

    #[test]
    fn minimum_paragraph_gap_only_expands_consecutive_prose_spacing() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("paragraph-gap-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let paragraph = |text: &str| {
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle {
                    margin_before: 0.0,
                    margin_after: 0.0,
                    ..rebook_publication::BlockStyle::default()
                },
                source: None,
            })
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![paragraph("First paragraph"), paragraph("Second paragraph")],
            anchors: Vec::new(),
        };
        let style = ReaderStyle {
            minimum_paragraph_gap: 12.0,
            ..ReaderStyle::default()
        };
        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(600, 400).unwrap(),
                &style,
            )
            .unwrap();
        let placements = layout.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                PageItem::Text(text) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        let first_line = placements[0].layout.get(0).unwrap();
        let first_bottom = placements[0].origin_y + first_line.metrics().block_max_coord;

        assert!((placements[1].origin_y - first_bottom - 12.0).abs() < 0.001);
    }

    #[test]
    fn inline_math_is_laid_out_as_a_non_text_raster_box() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("math-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![
                    Inline::Text(TextRun {
                        text: "Energy ".into(),
                        style: TextStyle::default(),
                        link: None,
                    }),
                    Inline::Math(MathRun {
                        latex: r"E=mc^2".into(),
                        display: false,
                        size_scale: 1.0,
                    }),
                ],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            })],
            anchors: Vec::new(),
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(600, 400).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        let formula_count = layout
            .pages
            .iter()
            .flat_map(|page| page.items.iter())
            .filter_map(|item| match item {
                PageItem::Text(text) => Some(text.inline_images.len()),
                PageItem::Table(_) | PageItem::Image(_) | PageItem::Separator(_) => None,
            })
            .sum::<usize>();
        assert_eq!(formula_count, 1);
    }

    #[test]
    fn unified_tables_adapt_columns_wrap_and_center_cell_content() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("adaptive-table-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let cell = |text: &str| TableCell {
            text: TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            },
            authored_alignment: None,
            column_span: 1,
            row_span: 1,
            header: false,
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Table(TableBlock {
                rows: vec![TableRow {
                    cells: vec![
                        cell("ID"),
                        cell(
                            "A substantially longer description that must wrap inside its adaptive column.",
                        ),
                    ],
                }],
                source: None,
            })],
            anchors: Vec::new(),
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(420, 360).unwrap(),
                &style,
            )
            .unwrap();
        let table = layout
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .find_map(|item| match item {
                PageItem::Table(table) => Some(table),
                _ => None,
            })
            .expect("table should be laid out");
        let [short, long] = table.cells.as_slice() else {
            panic!("expected two cells");
        };
        assert!(short.width < long.width);
        let short_text = short.text.as_ref().expect("short cell should have text");
        let long_text = long.text.as_ref().expect("long cell should have text");
        assert!(long_text.layout.len() > 1, "long content should wrap");
        assert!(
            short_text
                .layout
                .get(0)
                .is_some_and(|line| line.metrics().offset > 0.0),
            "short content should be horizontally centered"
        );
        assert!(
            short_text.origin_y > long_text.origin_y,
            "short content should be vertically centered beside wrapped content"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the table fixture verifies spans, formula layout, and pagination together"
    )]
    fn structured_tables_keep_spans_formulas_and_safe_page_breaks() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("table-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let cell = |text: &str, column_span, row_span, header| TableCell {
            text: TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            },
            authored_alignment: None,
            column_span,
            row_span,
            header,
        };
        let mut rows = vec![TableRow {
            cells: vec![cell("Header", 2, 1, true)],
        }];
        rows.push(TableRow {
            cells: vec![cell("Merged", 1, 2, false), cell("$", 1, 1, false)],
        });
        rows.push(TableRow {
            cells: vec![TableCell {
                text: TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Math(MathRun {
                        latex: "E=mc^2".into(),
                        display: false,
                        size_scale: 1.0,
                    })],
                    style: rebook_publication::BlockStyle::default(),
                    source: None,
                },
                authored_alignment: None,
                column_span: 1,
                row_span: 1,
                header: false,
            }],
        });
        rows.extend((0..12).map(|index| TableRow {
            cells: vec![
                cell(&format!("row {index}"), 1, 1, false),
                cell("value", 1, 1, false),
            ],
        }));
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Table(TableBlock { rows, source: None })],
            anchors: Vec::new(),
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(600, 240).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        let tables = layout
            .pages
            .iter()
            .flat_map(|page| page.items.iter())
            .filter_map(|item| match item {
                PageItem::Table(table) => Some(table),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(tables.len() > 1, "long table should paginate");
        let header = tables[0]
            .cells
            .iter()
            .find(|cell| cell.header)
            .expect("header cell should be retained");
        let regular = tables
            .iter()
            .flat_map(|table| &table.cells)
            .find(|cell| !cell.header && cell.width < header.width)
            .expect("regular-width cell should exist");
        assert!((header.width - regular.width * 2.0).abs() < 0.1);
        assert!(tables.iter().any(|table| {
            table.cells.iter().any(|cell| {
                cell.text
                    .as_ref()
                    .is_some_and(|text| !text.inline_images.is_empty())
            })
        }));
        assert!(tables.iter().all(|table| {
            table
                .cells
                .iter()
                .all(|cell| cell.y + cell.height <= table.y + table.height + 0.1)
        }));
    }

    #[test]
    fn block_media_uses_the_dominant_paragraph_measure() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("media-measure-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let paragraph_style = rebook_publication::BlockStyle {
            margin_start: 32.0,
            ..rebook_publication::BlockStyle::default()
        };
        let text_block = |text: &str| TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: text.into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: paragraph_style,
            source: None,
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                Block::Text(text_block("Body paragraph")),
                Block::Table(TableBlock {
                    rows: vec![TableRow {
                        cells: vec![TableCell {
                            text: text_block("Cell"),
                            authored_alignment: None,
                            column_span: 1,
                            row_span: 1,
                            header: false,
                        }],
                    }],
                    source: None,
                }),
                Block::Image(ImageBlock {
                    href: PublicationUrl::parse("figure.png").unwrap(),
                    alt: "Figure".into(),
                    style: ImageStyle {
                        width: Some(ImageLength::Fraction(1.0)),
                        ..ImageStyle::default()
                    },
                    source: None,
                    text_layer: None,
                }),
            ],
            anchors: Vec::new(),
        };
        let viewport = LayoutViewport::new(600, 500).unwrap();
        let style = ReaderStyle::default();
        let geometry = resolve_page_geometry(600.0, 500.0, &style);
        let layout = LayoutEngine::new()
            .layout_section(&source, &section, viewport, &style)
            .unwrap();
        let items = layout.pages.iter().flat_map(|page| &page.items);
        let mut text_x = None;
        let mut table_bounds = None;
        let mut image_bounds = None;
        for item in items {
            match item {
                PageItem::Text(text) => {
                    text_x.get_or_insert(text.origin_x);
                }
                PageItem::Table(table) => {
                    let first = table.cells.first().unwrap();
                    table_bounds = Some((first.x, first.x + first.width));
                }
                PageItem::Image(image) => {
                    image_bounds = Some((image.x, image.x + image.width));
                }
                PageItem::Separator(_) => {}
            }
        }
        let expected_left = geometry.left + 32.0;
        let expected_right = geometry.left + geometry.width;
        assert!((text_x.unwrap() - expected_left).abs() < 0.001);
        let (table_left, table_right) = table_bounds.unwrap();
        assert!((table_left - expected_left).abs() < 0.001);
        assert!((table_right - expected_right).abs() < 0.001);
        let (image_left, image_right) = image_bounds.unwrap();
        assert!((image_left - expected_left).abs() < 0.001);
        assert!((image_right - expected_right).abs() < 0.001);
    }

    #[test]
    fn block_start_fraction_tracks_the_available_content_width() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let text_block = |text: &str, style| {
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(rebook_publication::TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style,
                source: None,
            })
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                text_block("Top-level entry", rebook_publication::BlockStyle::default()),
                text_block(
                    "Nested entry",
                    rebook_publication::BlockStyle {
                        margin_start: 12.0,
                        margin_start_fraction: 0.1,
                        ..rebook_publication::BlockStyle::default()
                    },
                ),
            ],
            anchors: Vec::new(),
        };
        let viewport = LayoutViewport::new(600, 400).unwrap();
        let reader_style = ReaderStyle::default();
        let content_width = resolve_page_geometry(600.0, 400.0, &reader_style).width;
        let layout = LayoutEngine::new()
            .layout_section(&source, &section, viewport, &reader_style)
            .unwrap();
        let origins = layout.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                PageItem::Text(text) => Some(text.origin_x),
                PageItem::Table(_) | PageItem::Image(_) | PageItem::Separator(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(origins.len(), 2);
        let expected_offset = 12.0 + content_width * 0.1;
        assert!(((origins[1] - origins[0]) - expected_offset).abs() < 0.001);
    }

    #[test]
    fn double_spread_emits_independent_logical_pages_for_composition() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(rebook_publication::TextRun {
                    text: "双栏分页应当把连续内容放进同一屏幕的左右页面。".repeat(500),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            })],
            anchors: Vec::new(),
        };
        let viewport = LayoutViewport::new(900, 700).unwrap();
        let single = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                viewport,
                &ReaderStyle {
                    spread: SpreadMode::Single,
                    ..ReaderStyle::default()
                },
            )
            .unwrap();
        let double = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                viewport,
                &ReaderStyle {
                    spread: SpreadMode::Double,
                    ..ReaderStyle::default()
                },
            )
            .unwrap();

        assert_eq!(single.visible_pages, 1);
        assert_eq!(double.visible_pages, 2);
        assert!(double.pages.len() >= 2);
        let first_origin = double.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                PageItem::Text(text) => Some(text.origin_x),
                _ => None,
            })
            .expect("first logical page should contain text");
        let second_origin = double.pages[1]
            .items
            .iter()
            .find_map(|item| match item {
                PageItem::Text(text) => Some(text.origin_x),
                _ => None,
            })
            .expect("second logical page should contain text");
        assert!((first_origin - second_origin).abs() < f32::EPSILON);
        assert!(double.continuation_offset_x > 0.0);
    }

    #[test]
    fn image_css_dimensions_are_resolved_and_aspect_ratio_is_preserved() {
        let viewport = LayoutViewport::new(400, 500).unwrap();
        let mut paginator = Paginator::new(
            viewport,
            Rgba::BLACK,
            PageGeometry {
                left: 0.0,
                top: 0.0,
                width: 400.0,
                bottom: 500.0,
                visible_pages: 1,
                continuation_offset_x: 0.0,
            },
            false,
            0.0,
        );
        paginator.push_image(
            RasterImage {
                width: 800,
                height: 600,
                pixels: Vec::new().into(),
            },
            ImageStyle {
                width: Some(ImageLength::Fraction(0.8)),
                max_width: Some(ImageLength::Pixels(250.0)),
                ..ImageStyle::default()
            },
            None,
            None,
        );

        let pages = paginator.finish();
        let PageItem::Image(image) = &pages[0].items[0] else {
            panic!("expected an image placement");
        };
        assert!((image.width - 250.0).abs() < 0.001);
        assert!((image.height - 187.5).abs() < 0.001);
        assert!((image.x - 75.0).abs() < 0.001);
    }

    #[test]
    fn image_after_zero_margin_text_keeps_a_minimum_block_gap() {
        let image_href = PublicationUrl::parse("images/figure.png").unwrap();
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("image-gap-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(rebook_publication::TextRun {
                        text: "Text immediately before a figure.".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: rebook_publication::BlockStyle {
                        margin_after: 0.0,
                        ..rebook_publication::BlockStyle::default()
                    },
                    source: None,
                }),
                Block::Image(ImageBlock {
                    href: image_href,
                    alt: "Figure".into(),
                    style: ImageStyle::default(),
                    source: None,
                    text_layer: None,
                }),
            ],
            anchors: Vec::new(),
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(400, 500).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        let [PageItem::Text(text), PageItem::Image(image)] = layout.pages[0].items.as_slice()
        else {
            panic!("expected text followed by an image");
        };
        let last_line = text.layout.get(text.lines.end - 1).unwrap();
        let metrics = last_line.metrics();
        let text_bottom = text.origin_y
            + metrics
                .block_max_coord
                .max(metrics.block_min_coord + metrics.line_height);

        assert!((image.y - text_bottom - IMAGE_BLOCK_GAP).abs() < 0.001);
    }

    #[test]
    fn unified_figure_keeps_image_and_caption_together_with_semantic_spacing() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("figure-caption-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Figure(rebook_publication::FigureBlock {
                images: vec![ImageBlock {
                    href: PublicationUrl::parse("images/figure.png").unwrap(),
                    alt: "Figure".into(),
                    style: ImageStyle {
                        margin_after: 30.0,
                        ..ImageStyle::default()
                    },
                    source: None,
                    text_layer: None,
                }],
                captions: vec![TextBlock {
                    kind: TextBlockKind::Caption,
                    content: vec![Inline::Text(TextRun {
                        text: "A concise figure caption".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: rebook_publication::BlockStyle::default(),
                    source: None,
                }],
                caption_position: CaptionPosition::After,
                style: rebook_publication::BlockStyle::default(),
                source: None,
            })],
            anchors: Vec::new(),
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(400, 500).unwrap(),
                &style,
            )
            .unwrap();
        let [PageItem::Image(image), PageItem::Text(caption)] = layout.pages[0].items.as_slice()
        else {
            panic!("expected one grouped image and caption");
        };
        let first = caption.layout.get(caption.lines.start).unwrap();
        let caption_top = caption.origin_y + first.metrics().block_min_coord;
        let expected_gap = style.typography.font_size * style.typesetting.caption_gap_em;
        assert!((caption_top - (image.y + image.height) - expected_gap).abs() < 0.01);
        assert!(
            first.metrics().offset > 0.0,
            "a short unified caption should be centered"
        );
    }

    #[test]
    fn authored_image_margin_larger_than_the_default_gap_is_preserved() {
        let viewport = LayoutViewport::new(400, 500).unwrap();
        let mut paginator = Paginator::new(
            viewport,
            Rgba::BLACK,
            PageGeometry {
                left: 20.0,
                top: 40.0,
                width: 360.0,
                bottom: 460.0,
                visible_pages: 1,
                continuation_offset_x: 0.0,
            },
            false,
            0.0,
        );
        paginator.push_separator();
        paginator.push_image(
            RasterImage {
                width: 200,
                height: 100,
                pixels: Vec::new().into(),
            },
            ImageStyle {
                margin_before: 25.0,
                ..ImageStyle::default()
            },
            None,
            None,
        );

        let pages = paginator.finish();
        let [PageItem::Separator(separator), PageItem::Image(image)] = pages[0].items.as_slice()
        else {
            panic!("expected a separator followed by an image");
        };

        assert!((image.y - (separator.y + 1.0) - 25.0).abs() < 0.001);
    }

    #[test]
    fn image_moved_to_the_next_page_starts_at_the_page_margin() {
        let image_href = PublicationUrl::parse("images/figure.png").unwrap();
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("image-page-break-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(rebook_publication::TextRun {
                        text: "Text before a figure that must move.".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: rebook_publication::BlockStyle {
                        margin_after: 0.0,
                        ..rebook_publication::BlockStyle::default()
                    },
                    source: None,
                }),
                Block::Image(ImageBlock {
                    href: image_href,
                    alt: "Figure".into(),
                    style: ImageStyle::default(),
                    source: None,
                    text_layer: None,
                }),
            ],
            anchors: Vec::new(),
        };
        let viewport = LayoutViewport::new(400, 140).unwrap();
        let style = ReaderStyle::default();
        let page_top = resolve_page_geometry(400.0, 140.0, &style).top;

        let layout = LayoutEngine::new()
            .layout_section(&source, &section, viewport, &style)
            .unwrap();
        let PageItem::Image(image) = &layout.pages[1].items[0] else {
            panic!("expected the image on the next page");
        };

        assert!((image.y - page_top).abs() < 0.001);
        assert!((layout.pages[1].leading_gap - IMAGE_BLOCK_GAP).abs() < 0.001);
    }

    #[test]
    fn image_keeps_its_gap_when_previous_block_spacing_already_advanced_the_page() {
        let image_href = PublicationUrl::parse("images/figure.png").unwrap();
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("image-pre-advanced-page-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(rebook_publication::TextRun {
                        text: "Text before a figure.".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: rebook_publication::BlockStyle {
                        margin_after: 300.0,
                        ..rebook_publication::BlockStyle::default()
                    },
                    source: None,
                }),
                Block::Image(ImageBlock {
                    href: image_href,
                    alt: "Figure".into(),
                    style: ImageStyle::default(),
                    source: None,
                    text_layer: None,
                }),
            ],
            anchors: Vec::new(),
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(400, 300).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();

        assert!(matches!(layout.pages[1].items[0], PageItem::Image(_)));
        assert!((layout.pages[1].leading_gap - IMAGE_BLOCK_GAP).abs() < 0.001);
    }

    #[test]
    fn oversized_figure_restores_its_outer_gap_after_moving_to_the_next_page() {
        let viewport = LayoutViewport::new(400, 300).unwrap();
        let mut paginator = Paginator::new(
            viewport,
            Rgba::BLACK,
            PageGeometry {
                left: 20.0,
                top: 40.0,
                width: 360.0,
                bottom: 260.0,
                visible_pages: 1,
                continuation_offset_x: 0.0,
            },
            false,
            0.0,
        );
        paginator.push_separator();
        paginator.add_spacing(180.0);

        let outer_gap = 20.0;
        paginator.prepare_group(300.0, outer_gap);
        paginator.push_image_with_gaps(
            RasterImage {
                width: 200,
                height: 100,
                pixels: Vec::new().into(),
            },
            ImageStyle::default(),
            None,
            None,
            0.0,
            0.0,
        );

        let pages = paginator.finish();
        assert_eq!(pages.len(), 2);
        assert!((pages[1].leading_gap - outer_gap).abs() < 0.001);
    }

    #[test]
    fn semantic_spacing_that_crosses_a_page_is_restored_in_continuous_layout() {
        let viewport = LayoutViewport::new(400, 300).unwrap();
        let mut paginator = Paginator::new(
            viewport,
            Rgba::BLACK,
            PageGeometry {
                left: 20.0,
                top: 40.0,
                width: 360.0,
                bottom: 260.0,
                visible_pages: 1,
                continuation_offset_x: 0.0,
            },
            false,
            0.0,
        );
        paginator.push_image_with_gaps(
            RasterImage {
                width: 200,
                height: 210,
                pixels: Vec::new().into(),
            },
            ImageStyle::default(),
            None,
            None,
            0.0,
            0.0,
        );

        let semantic_gap = 20.0;
        paginator.add_semantic_spacing(semantic_gap);
        paginator.push_separator();

        let pages = paginator.finish();
        assert_eq!(pages.len(), 2);
        assert!((pages[1].leading_gap - semantic_gap).abs() < 0.001);
    }

    #[test]
    fn fixed_page_image_is_vertically_centered_in_the_content_area() {
        let viewport = LayoutViewport::new(400, 500).unwrap();
        let mut paginator = Paginator::new(
            viewport,
            Rgba::BLACK,
            PageGeometry {
                left: 20.0,
                top: 40.0,
                width: 360.0,
                bottom: 460.0,
                visible_pages: 1,
                continuation_offset_x: 0.0,
            },
            true,
            0.0,
        );
        paginator.push_image(
            RasterImage {
                width: 200,
                height: 100,
                pixels: Vec::new().into(),
            },
            ImageStyle::default(),
            None,
            None,
        );

        let pages = paginator.finish();
        let PageItem::Image(image) = &pages[0].items[0] else {
            panic!("expected an image placement");
        };
        assert!((image.y - 200.0).abs() < 0.001);
    }

    #[test]
    fn fixed_page_replacement_stays_on_the_original_page_image() {
        let href = PublicationUrl::parse("page-1.png").unwrap();
        let spine = SpineItemId::new("pdf-page-1").unwrap();
        let source_range = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "pdf-page-text".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "pdf-page-text".into(),
                text_offset: 4,
            },
        };
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("fixed-page-translation").unwrap(),
                metadata: Metadata {
                    layout: RenditionLayout::PrePaginated,
                    ..Metadata::default()
                },
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let replacement_rect = FixedPageTextRect {
            x: 20.0,
            y: 10.0,
            width: 100.0,
            height: 40.0,
        };
        let section = Section {
            id: SpineItemId::new("pdf-page-1").unwrap(),
            href: PublicationUrl::parse("page-1.pdf").unwrap(),
            blocks: vec![Block::Image(ImageBlock {
                href,
                alt: "PDF page 1".into(),
                style: ImageStyle::default(),
                source: Some(source_range),
                text_layer: Some(FixedPageTextLayer {
                    width: 200.0,
                    height: 100.0,
                    text: "PDF text".into(),
                    spans: vec![FixedPageTextSpan {
                        char_range: 0..8,
                        rect: replacement_rect,
                    }],
                    replacement: Some(FixedPageTextReplacement {
                        segments: vec![FixedPageTextReplacementSegment {
                            text: "译文".into(),
                            rect: replacement_rect,
                            source_offset: 0,
                        }],
                    }),
                }),
            })],
            anchors: Vec::new(),
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(400, 500).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();

        assert_eq!(layout.pages.len(), 1);
        let [PageItem::Image(image)] = layout.pages[0].items.as_slice() else {
            panic!("translation must remain attached to the fixed page image");
        };
        let replacement = image
            .replacement
            .as_ref()
            .expect("fixed page should retain its replacement overlay");
        let [segment] = replacement.segments.as_slice() else {
            panic!("expected one translated fixed-page segment");
        };
        assert!(segment.rect.x >= image.x);
        assert!(segment.rect.y >= image.y);
        assert!(segment.rect.x + segment.rect.width <= image.x + image.width);
        assert!(segment.rect.y + segment.rect.height <= image.y + image.height);
        assert_eq!(segment.text.text.as_ref(), "译文");
    }

    #[test]
    fn reflowable_standalone_cover_is_vertically_centered() {
        let cover = PublicationUrl::parse("images/cover.jpg").unwrap();
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("cover-test").unwrap(),
                metadata: Metadata::default(),
                cover: Some(cover.clone()),
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("cover").unwrap(),
            href: PublicationUrl::parse("cover.xhtml").unwrap(),
            blocks: vec![Block::Image(ImageBlock {
                href: cover,
                alt: "Cover".into(),
                style: ImageStyle::default(),
                source: None,
                text_layer: None,
            })],
            anchors: Vec::new(),
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(400, 500).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        let PageItem::Image(image) = &layout.pages[0].items[0] else {
            panic!("expected a cover image placement");
        };

        let style = ReaderStyle::default();
        let geometry = resolve_page_geometry(400.0, 500.0, &style);
        let expected_y = geometry.top + (geometry.bottom - geometry.top - image.height) / 2.0;
        assert!((image.y - expected_y).abs() < 0.001);
    }

    #[test]
    fn reflowable_standalone_non_cover_image_stays_in_normal_flow() {
        let image_href = PublicationUrl::parse("images/illustration.jpg").unwrap();
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("illustration-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let section = Section {
            id: SpineItemId::new("illustration").unwrap(),
            href: PublicationUrl::parse("illustration.xhtml").unwrap(),
            blocks: vec![Block::Image(ImageBlock {
                href: image_href,
                alt: "Illustration".into(),
                style: ImageStyle::default(),
                source: None,
                text_layer: None,
            })],
            anchors: Vec::new(),
        };

        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(400, 500).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        let PageItem::Image(image) = &layout.pages[0].items[0] else {
            panic!("expected an illustration image placement");
        };

        assert!((image.y - ReaderStyle::default().top_margin).abs() < 0.001);
    }
}
