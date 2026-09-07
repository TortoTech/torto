//! Renderer-independent pagination for normalized reading IR.

pub mod linebreak;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use image::ImageError;
use parley::setting::Tag;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle, FontVariation, FontVariations,
    FontWeight, IndentOptions, InlineBox as ParleyInlineBox, InlineBoxKind, Layout, LayoutContext,
    LineHeight, PositionedLayoutItem, StyleProperty,
};
use read_fonts::{FontRef, TableProvider as _};
use rebook_publication::{
    Block, BlockStyle, BookSource, CaptionPosition, FixedPageDimensions, FixedPageTextLayer,
    FixedPageTextRect, ImageBlock, ImageLength, ImageStyle, Inline, InlineImageAlignment,
    InlineRole, LinkRole, MathRun, NoteBlockKind, PublicationError, PublicationUrl,
    RenditionLayout, Rgba, Section, SeparatorKind, SourceRange, TableBlock, TableCell,
    TextAlignment, TextBaseline, TextBlock, TextBlockKind, TextRun, TextStyle, WritingSystem,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_script::{Script, UnicodeScript as _};
use unicode_segmentation::UnicodeSegmentation as _;

const QUOTE_VERTICAL_PADDING: f32 = 12.0;
const LITERATA_FAMILY: &str = "Literata";
const LEGACY_YSABEAU_FAMILY: &str = "Ysabeau Office";
const OPTICAL_SIZE_TAG: Tag = Tag::new(b"opsz");
const MIN_OPTICAL_SIZE: f32 = 7.0;
const MAX_OPTICAL_SIZE: f32 = 72.0;
const CSS_PX_TO_POINTS: f32 = 0.75;

/// Shared light-theme accent used by semantic quote decorations and block activation fills.
pub const LIGHT_QUOTE_ACCENT_COLOR: Rgba = Rgba {
    red: 0xDC,
    green: 0xE2,
    blue: 0xE8,
    alpha: 255,
};

/// Dark-theme quote accent keeps light reader text legible on an active quote block.
pub const DARK_QUOTE_ACCENT_COLOR: Rgba = Rgba {
    red: 0x43,
    green: 0x48,
    blue: 0x4E,
    alpha: 255,
};

/// Returns the quote accent that matches the active reader theme.
#[must_use]
pub const fn quote_accent_color(dark: bool) -> Rgba {
    if dark {
        DARK_QUOTE_ACCENT_COLOR
    } else {
        LIGHT_QUOTE_ACCENT_COLOR
    }
}

fn quote_accent_for_foreground(foreground: Rgba) -> Rgba {
    // Reader foregrounds are dark on light pages and light on dark pages.
    // Integer Rec. 709 weights avoid float comparisons in this hot layout path.
    let luminance = u32::from(foreground.red) * 54
        + u32::from(foreground.green) * 183
        + u32::from(foreground.blue) * 19;
    quote_accent_color(luminance > 128 * 256)
}

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
    /// Replaces linked superscript markers with semantic footnote icon slots.
    pub focus_footnote_icons: bool,
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

/// Selects the paragraph breakpoint algorithm while retaining Parley for
/// shaping, line construction, alignment, and justification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LineBreakStrategy {
    #[default]
    Greedy,
    Optimized,
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
    pub line_break_strategy: LineBreakStrategy,
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
            line_break_strategy: LineBreakStrategy::Optimized,
            ..Self::default()
        }
    }

    /// Repairs persisted values before they participate in layout cache keys.
    pub fn normalize(&mut self) {
        self.heading_scale = finite_clamp(self.heading_scale, 1.1, 2.2, 1.6);
        self.body_line_height = finite_clamp(self.body_line_height, 1.2, 2.4, 1.5);
        self.paragraph_indent_em = finite_clamp(self.paragraph_indent_em, 0.0, 4.0, 2.0);
        self.paragraph_gap_em = finite_clamp(self.paragraph_gap_em, 0.0, 2.0, 0.5);
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
            line_break_strategy: LineBreakStrategy::Greedy,
            heading_scale: 1.6,
            body_line_height: 1.5,
            paragraph_indent_mode: ParagraphIndentMode::Auto,
            paragraph_indent_em: 2.0,
            paragraph_gap_em: 0.5,
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
    Other,
}

/// One explicit Western font selection used by a language profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderFontChoice {
    pub category: ReaderDefaultFont,
    pub family: String,
}

/// Readest-compatible native typography preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderTypography {
    pub default_font: ReaderDefaultFont,
    pub default_cjk_font: String,
    pub serif_font: String,
    pub sans_serif_font: String,
    pub other_font: String,
    /// Western letters and digits used in CJK-primary books. `None` inherits
    /// the Latin profile's primary selection.
    pub cjk_default_font: Option<ReaderFontChoice>,
    /// CJK fallback used in Latin-primary books. `None` inherits the CJK
    /// profile's primary family.
    pub latin_cjk_font: Option<String>,
    pub monospace_font: String,
    pub font_size: f32,
    pub minimum_font_size: f32,
    pub font_weight: u16,
}

impl ReaderTypography {
    /// Repairs persisted or externally supplied settings before layout uses them.
    pub fn normalize(&mut self) {
        let defaults = Self::default();
        let configured_other = self.other_font.trim();
        let used_deprecated_primary = self.default_font == ReaderDefaultFont::Other
            && (configured_other.eq_ignore_ascii_case(LEGACY_YSABEAU_FAMILY)
                || configured_other.eq_ignore_ascii_case(LITERATA_FAMILY));
        if used_deprecated_primary {
            self.default_font = ReaderDefaultFont::Serif;
            self.serif_font = LITERATA_FAMILY.into();
            self.other_font.clear();
        }
        if let Some(choice) = &mut self.cjk_default_font
            && choice
                .family
                .trim()
                .eq_ignore_ascii_case(LEGACY_YSABEAU_FAMILY)
        {
            choice.category = ReaderDefaultFont::Serif;
            choice.family = LITERATA_FAMILY.into();
        } else if let Some(choice) = &mut self.cjk_default_font
            && choice.family.trim().eq_ignore_ascii_case(LITERATA_FAMILY)
        {
            choice.category = ReaderDefaultFont::Serif;
            choice.family = LITERATA_FAMILY.into();
        }
        normalize_family(&mut self.default_cjk_font, &defaults.default_cjk_font);
        if self.default_cjk_font.eq_ignore_ascii_case("LXGW WenKai") {
            self.default_cjk_font = "LXGW WenKai GB Screen".into();
        }
        normalize_family(&mut self.serif_font, &defaults.serif_font);
        normalize_family(&mut self.sans_serif_font, &defaults.sans_serif_font);
        self.other_font = self.other_font.trim().to_owned();
        if let Some(choice) = &mut self.cjk_default_font {
            choice.family = choice.family.trim().to_owned();
            if choice.family.is_empty() {
                self.cjk_default_font = None;
            }
        }
        if let Some(family) = &mut self.latin_cjk_font {
            *family = family.trim().to_owned();
            if family.is_empty() {
                self.latin_cjk_font = None;
            }
        }
        normalize_family(&mut self.monospace_font, &defaults.monospace_font);
        self.minimum_font_size = finite_clamp(self.minimum_font_size, 1.0, 120.0, 12.0);
        self.font_size = finite_clamp(self.font_size, self.minimum_font_size, 120.0, 20.0);
        self.font_weight = self.font_weight.clamp(200, 900);
    }

    #[must_use]
    pub fn default_stack(&self) -> String {
        self.default_stack_for(WritingSystem::Unknown)
    }

    #[must_use]
    pub fn default_stack_for(&self, writing_system: WritingSystem) -> String {
        match writing_system {
            WritingSystem::Cjk => {
                let (category, family) = self.cjk_default_font.as_ref().map_or_else(
                    || (self.default_font, self.default_western_family()),
                    |choice| (choice.category, choice.family.as_str()),
                );
                self.reading_stack(category, family, &self.default_cjk_font)
            }
            WritingSystem::Latin | WritingSystem::Other | WritingSystem::Unknown => self
                .reading_stack(
                    self.default_font,
                    self.default_western_family(),
                    self.latin_cjk_font
                        .as_deref()
                        .unwrap_or(&self.default_cjk_font),
                ),
        }
    }

    #[must_use]
    pub fn serif_stack(&self) -> String {
        self.reading_stack(
            ReaderDefaultFont::Serif,
            &self.serif_font,
            &self.default_cjk_font,
        )
    }

    fn default_western_family(&self) -> &str {
        match self.default_font {
            ReaderDefaultFont::Serif => &self.serif_font,
            ReaderDefaultFont::SansSerif => &self.sans_serif_font,
            ReaderDefaultFont::Other => &self.other_font,
        }
    }

    fn reading_stack(
        &self,
        category: ReaderDefaultFont,
        western_family: &str,
        cjk_family: &str,
    ) -> String {
        match category {
            ReaderDefaultFont::Serif => font_stack(
                [
                    western_family,
                    cjk_family,
                    "LXGW WenKai GB Screen",
                    "Noto Serif SC",
                    "Source Han Serif SC",
                    "Songti SC",
                    "SimSun",
                    "Georgia",
                    "Times New Roman",
                ],
                "serif",
            ),
            ReaderDefaultFont::SansSerif => font_stack(
                [
                    western_family,
                    cjk_family,
                    "LXGW WenKai GB Screen",
                    "Noto Sans SC",
                    "Source Han Sans SC",
                    "PingFang SC",
                    "Microsoft YaHei",
                    "Roboto",
                    "Arial",
                ],
                "sans-serif",
            ),
            ReaderDefaultFont::Other => font_stack(
                [
                    western_family,
                    cjk_family,
                    "LXGW WenKai GB Screen",
                    "Noto Sans SC",
                    "Source Han Sans SC",
                    "PingFang SC",
                    "Microsoft YaHei",
                    "Roboto",
                    "Arial",
                ],
                "sans-serif",
            ),
        }
    }

    #[must_use]
    pub fn sans_serif_stack(&self) -> String {
        self.reading_stack(
            ReaderDefaultFont::SansSerif,
            &self.sans_serif_font,
            &self.default_cjk_font,
        )
    }

    #[must_use]
    pub fn monospace_stack(&self) -> String {
        font_stack(
            [
                self.monospace_font.as_str(),
                self.default_cjk_font.as_str(),
                "LXGW WenKai GB Screen",
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
            serif_font: LITERATA_FAMILY.into(),
            sans_serif_font: "Arial".into(),
            other_font: String::new(),
            cjk_default_font: Some(ReaderFontChoice {
                category: ReaderDefaultFont::Serif,
                family: LITERATA_FAMILY.into(),
            }),
            latin_cjk_font: None,
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
            focus_footnote_icons: false,
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

fn optical_size_for_font(font_size: f32) -> f32 {
    (font_size * CSS_PX_TO_POINTS).clamp(MIN_OPTICAL_SIZE, MAX_OPTICAL_SIZE)
}

fn optical_size_variations(font_size: f32) -> [FontVariation; 1] {
    [FontVariation::new(
        OPTICAL_SIZE_TAG,
        optical_size_for_font(font_size),
    )]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReaderFontClassification {
    serif: bool,
    sans_serif: bool,
    monospace: bool,
}

fn classify_reader_font(
    panose: Option<&[u8]>,
    family_class: Option<i16>,
    fixed_pitch: bool,
) -> ReaderFontClassification {
    let panose = panose.filter(|panose| panose.len() >= 4);
    let monospace = fixed_pitch || panose.is_some_and(|panose| panose[0] == 2 && panose[3] == 9);
    if monospace {
        return ReaderFontClassification {
            monospace: true,
            ..ReaderFontClassification::default()
        };
    }
    if let Some(panose) = panose.filter(|panose| panose[0] == 2) {
        let classification = ReaderFontClassification {
            serif: matches!(panose[1], 2..=10),
            sans_serif: matches!(panose[1], 11..=15),
            monospace: false,
        };
        if classification.serif || classification.sans_serif {
            return classification;
        }
    }
    let family_class = family_class.map(|value| value.to_be_bytes()[0]);
    ReaderFontClassification {
        serif: family_class.is_some_and(|class| matches!(class, 1..=5 | 7)),
        sans_serif: family_class == Some(8),
        monospace: false,
    }
}

fn infer_reader_font_classification(family: &str) -> ReaderFontClassification {
    let normalized = family.to_ascii_lowercase();
    let words = normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let has_word = |candidate| words.contains(&candidate);
    let serif = has_word("serif")
        || has_word("roman")
        || has_word("antiqua")
        || has_word("mincho")
        || ["baskerville", "bodoni", "bookman", "literata", "sitka"]
            .iter()
            .any(|prefix| normalized.starts_with(prefix));
    let sans_serif = !serif
        && (has_word("sans")
            || has_word("gothic")
            || has_word("grotesk")
            || has_word("ui")
            || ["arial", "helvetica"]
                .iter()
                .any(|prefix| normalized.starts_with(prefix)));
    ReaderFontClassification {
        serif,
        sans_serif,
        monospace: false,
    }
}

fn is_symbolic_reader_font(family: &str, panose: Option<&[u8]>, family_class: Option<i16>) -> bool {
    let family_class = family_class.map(|value| value.to_be_bytes()[0]);
    let normalized = family.to_ascii_lowercase();
    panose.is_some_and(|panose| panose.first() == Some(&5))
        || family_class == Some(12)
        || normalized.contains("math")
        || normalized.contains("symbol")
        || normalized.contains("webdings")
        || normalized.contains("wingdings")
}

fn supports_common_chinese(charmap: &parley::fontique::Charmap<'_>) -> bool {
    const COMMON_CHINESE_PROBE: &str =
        "中文字体阅读书籍测试国家学习时间这样问题繁體國學時門風龍臺灣";
    COMMON_CHINESE_PROBE
        .chars()
        .all(|character| charmap.map(character).is_some())
}

fn supports_common_latin(charmap: &parley::fontique::Charmap<'_>) -> bool {
    const COMMON_LATIN_PROBE: &str =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    COMMON_LATIN_PROBE
        .chars()
        .all(|character| charmap.map(character).is_some())
}

fn has_embedded_bitmap_glyphs(font: &FontRef<'_>) -> bool {
    const BITMAP_TABLES: [[u8; 4]; 6] =
        [*b"EBDT", *b"EBLC", *b"CBDT", *b"CBLC", *b"bdat", *b"bloc"];
    font.table_directory()
        .table_records()
        .iter()
        .any(|record| BITMAP_TABLES.contains(&record.tag().to_be_bytes()))
}

/// Brush carried through Parley without coupling layout to a paint backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextBrush {
    pub color: Rgba,
    pub underline: bool,
    pub baseline: TextBaseline,
    pub footnote_reference: bool,
    /// Stable identifier for all glyph runs produced by one semantic footnote marker.
    ///
    /// Font fallback can split a marker such as `【3】` into several glyph runs. The
    /// renderer uses this identifier to collapse those runs back into one icon.
    pub footnote_reference_group: u32,
}

impl TextBrush {
    fn new(
        color: Rgba,
        underline: bool,
        baseline: TextBaseline,
        footnote_reference_group: u32,
    ) -> Self {
        Self {
            color,
            underline,
            baseline,
            footnote_reference: footnote_reference_group != 0,
            footnote_reference_group,
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
    Quote(QuotePlacement),
    Table(TablePlacement),
    Image(ImagePlacement),
    Separator(SeparatorPlacement),
}

/// Unified-typesetting decoration for one page slice of a semantic quotation.
pub struct QuotePlacement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub continued_before: bool,
    pub continued_after: bool,
    pub fill: Rgba,
    pub accent: Rgba,
    pub sources: Vec<SourceRange>,
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
    /// Full horizontal measure available to this shaped block.
    pub available_width: f32,
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
    /// Paint offset relative to Parley's baseline-aligned inline box.
    pub offset_y: f32,
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
    publication_languages: Vec<String>,
}

fn should_layout_flow_block(block: &Block, reader_style: &ReaderStyle) -> bool {
    !reader_style.focus_footnote_icons || !block.is_footnote_definition()
}

fn collect_layout_blocks<'a>(
    blocks: &'a [Block],
    reader_style: &ReaderStyle,
    output: &mut Vec<&'a Block>,
) {
    for block in blocks {
        if let Block::Note(note) = block {
            let hidden = match note.kind {
                NoteBlockKind::Definition => reader_style.focus_footnote_icons,
                NoteBlockKind::Section => reader_style.typesetting.mode == TypesettingMode::Unified,
            };
            if !hidden {
                collect_layout_blocks(&note.blocks, reader_style, output);
            }
            continue;
        }
        if should_layout_flow_block(block, reader_style) {
            output.push(block);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReaderFontFamilies {
    pub all: Vec<String>,
    pub serif: Vec<String>,
    pub sans_serif: Vec<String>,
    pub other: Vec<String>,
    pub monospace: Vec<String>,
    pub chinese: Vec<String>,
}

impl ReaderFontFamilies {
    pub fn include_configured(&mut self, typography: &ReaderTypography) {
        include_available_family(&self.all, &mut self.serif, &typography.serif_font);
        include_available_family(&self.all, &mut self.sans_serif, &typography.sans_serif_font);
        include_available_family(&self.all, &mut self.other, &typography.other_font);
        include_available_family(&self.all, &mut self.monospace, &typography.monospace_font);
        if let Some(choice) = &typography.cjk_default_font {
            let category = match choice.category {
                ReaderDefaultFont::Serif => &mut self.serif,
                ReaderDefaultFont::SansSerif => &mut self.sans_serif,
                ReaderDefaultFont::Other => &mut self.other,
            };
            include_available_family(&self.all, category, &choice.family);
        }
        if let Some(family) = &typography.latin_cjk_font {
            include_available_family(&self.all, &mut self.chinese, family);
        }
    }

    /// Replaces persisted reader families that the native renderer cannot
    /// safely paint with the matching bundled default (or a validated fallback).
    pub fn repair_typography(&self, typography: &mut ReaderTypography) -> bool {
        typography.normalize();
        let defaults = ReaderTypography::default();
        let mut repaired = repair_available_family(
            &self.chinese,
            &mut typography.default_cjk_font,
            &defaults.default_cjk_font,
        ) | repair_available_family(
            &self.serif,
            &mut typography.serif_font,
            &defaults.serif_font,
        ) | repair_available_family(
            &self.sans_serif,
            &mut typography.sans_serif_font,
            &defaults.sans_serif_font,
        ) | repair_optional_family(&self.other, &mut typography.other_font)
            | repair_available_family(
                &self.monospace,
                &mut typography.monospace_font,
                &defaults.monospace_font,
            );
        if typography.default_font == ReaderDefaultFont::Other && typography.other_font.is_empty() {
            typography.default_font = ReaderDefaultFont::Serif;
            repaired = true;
        }
        let cjk_default_repaired = typography.cjk_default_font.as_mut().is_some_and(|choice| {
            let available = match choice.category {
                ReaderDefaultFont::Serif => &self.serif,
                ReaderDefaultFont::SansSerif => &self.sans_serif,
                ReaderDefaultFont::Other => &self.other,
            };
            repair_optional_family(available, &mut choice.family)
        });
        if typography
            .cjk_default_font
            .as_ref()
            .is_some_and(|choice| choice.family.is_empty())
        {
            typography.cjk_default_font = None;
        }
        let latin_cjk_repaired =
            repair_optional_family_option(&self.chinese, &mut typography.latin_cjk_font);
        repaired | cjk_default_repaired | latin_cjk_repaired
    }
}

fn repair_optional_family(available: &[String], current: &mut String) -> bool {
    let Some(matching) = available
        .iter()
        .find(|family| family.eq_ignore_ascii_case(current))
    else {
        let repaired = !current.is_empty();
        current.clear();
        return repaired;
    };
    if matching == current {
        false
    } else {
        current.clone_from(matching);
        true
    }
}

fn repair_optional_family_option(available: &[String], current: &mut Option<String>) -> bool {
    let Some(family) = current else {
        return false;
    };
    let repaired = repair_optional_family(available, family);
    if family.is_empty() {
        *current = None;
    }
    repaired
}

fn repair_available_family(available: &[String], current: &mut String, default: &str) -> bool {
    let replacement = available
        .iter()
        .find(|family| family.eq_ignore_ascii_case(current))
        .or_else(|| {
            available
                .iter()
                .find(|family| family.eq_ignore_ascii_case(default))
        })
        .or_else(|| available.first());
    let Some(replacement) = replacement else {
        return false;
    };
    if replacement == current {
        return false;
    }
    current.clone_from(replacement);
    true
}

fn include_available_family(all: &[String], category: &mut Vec<String>, family: &str) {
    let Some(available) = all
        .iter()
        .find(|available| available.eq_ignore_ascii_case(family))
    else {
        return;
    };
    if !category
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(available))
    {
        category.push(available.clone());
        category.sort_by_key(|family| family.to_lowercase());
    }
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
            publication_languages: Vec::new(),
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

    pub fn available_reader_font_families(&mut self) -> ReaderFontFamilies {
        let discovered = self.available_font_families();
        let mut families = ReaderFontFamilies::default();
        for family_name in &discovered {
            let font_info = self
                .font_context
                .collection
                .family_by_name(family_name)
                .and_then(|family| family.default_font().cloned());
            let Some(font_info) = font_info else {
                continue;
            };
            let Some(data) = font_info.load(None) else {
                continue;
            };
            let charmap = font_info.charmap_index().charmap(data.as_ref());
            let supports_chinese = charmap
                .as_ref()
                .is_some_and(|charmap| supports_common_chinese(charmap));
            let supports_latin = charmap
                .as_ref()
                .is_some_and(|charmap| supports_common_latin(charmap));
            let Ok(font) = FontRef::from_index(data.as_ref(), font_info.index()) else {
                continue;
            };
            // Vello 0.10 prefers a matching embedded bitmap strike over the
            // outline. Several Windows fonts (notably SimSun/宋体) expose EBDT
            // masks that Vello cannot paint and does not fall back from,
            // leaving matching glyphs blank. Keep such families out of every
            // reader selector until the renderer supports that bitmap format.
            if has_embedded_bitmap_glyphs(&font) {
                continue;
            }
            families.all.push(family_name.clone());
            if supports_chinese {
                families.chinese.push(family_name.clone());
            }
            let fixed_pitch = font
                .post()
                .ok()
                .is_some_and(|post| post.is_fixed_pitch() != 0);
            let os2 = font.os2().ok();
            let panose = os2.as_ref().map(|os2| os2.panose_10());
            let family_class = os2.as_ref().map(|os2| os2.s_family_class());
            let mut classification = classify_reader_font(panose, family_class, fixed_pitch);
            if !classification.serif && !classification.sans_serif && !classification.monospace {
                classification = infer_reader_font_classification(family_name);
            }
            if classification.monospace {
                families.monospace.push(family_name.clone());
            } else if classification.serif {
                families.serif.push(family_name.clone());
            } else if classification.sans_serif {
                families.sans_serif.push(family_name.clone());
            } else if supports_latin
                && !supports_chinese
                && !is_symbolic_reader_font(family_name, panose, family_class)
            {
                families.other.push(family_name.clone());
            }
        }
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
        self.publication_languages
            .clone_from(&source.book().metadata.languages);
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

        let mut layout_blocks = Vec::new();
        for blocks in fragments {
            collect_layout_blocks(blocks, reader_style, &mut layout_blocks);
        }
        let mut block_index = 0;
        while block_index < layout_blocks.len() {
            if unified_reflow
                && let (Some(Block::Image(image)), Some(Block::Text(caption))) = (
                    layout_blocks.get(block_index).copied(),
                    layout_blocks.get(block_index + 1).copied(),
                )
                && caption.kind == TextBlockKind::Caption
            {
                self.push_figure(
                    &mut paginator,
                    source,
                    std::slice::from_ref(image),
                    std::slice::from_ref(caption),
                    CaptionPosition::After,
                    BlockStyle::default(),
                    reader_style,
                    content_width,
                    media_start_offset,
                    true,
                )?;
                block_index += 2;
                continue;
            }

            if unified_reflow
                && let Some(Block::Text(ordinal)) = layout_blocks.get(block_index).copied()
                && let TextBlockKind::HeadingOrdinal(level) = ordinal.kind
                && let Some(Block::Text(title)) = layout_blocks.get(block_index + 1).copied()
                && title.kind.heading_level() == Some(level)
            {
                let ordinal = resolve_text_block(ordinal, reader_style, TextContext::Flow);
                let title = resolve_text_block(title, reader_style, TextContext::Flow);
                let ordinal_text = self.shape_text(&ordinal, reader_style, content_width);
                let title_text = self.shape_text(&title, reader_style, content_width);
                paginator.keep_together_if_fits(
                    prepared_flow_height(&ordinal_text)
                        + ordinal.style.margin_before.max(0.0)
                        + ordinal.style.margin_after.max(0.0)
                        + prepared_flow_height(&title_text)
                        + title.style.margin_before.max(0.0)
                        + title.style.margin_after.max(0.0),
                );
            }

            let block = layout_blocks[block_index];
            match block {
                Block::Text(block) => {
                    let resolved = resolve_text_block(block, reader_style, TextContext::Flow);
                    let prepared = self.shape_text_from_source(
                        source,
                        &resolved,
                        reader_style,
                        content_width,
                    )?;
                    paginator.push_text(&prepared, &resolved)?;
                }
                Block::Quote(quote) => {
                    let quote_horizontal_padding = if unified_reflow {
                        reader_style.typography.font_size
                            * paragraph_indent_em(
                                &reader_style.typesetting,
                                reader_style.writing_system,
                            )
                    } else {
                        0.0
                    };
                    let quote_width = if unified_reflow {
                        (content_width - quote_horizontal_padding * 2.0).max(40.0)
                    } else {
                        content_width
                    };
                    let mut prepared_body = Vec::with_capacity(quote.body.len());
                    for (index, body) in quote.body.iter().enumerate() {
                        let mut resolved =
                            resolve_text_block(body, reader_style, TextContext::Flow);
                        if unified_reflow
                            && quote.attribution.is_none()
                            && index + 1 == quote.body.len()
                        {
                            // The card already owns its bottom padding. Keeping the unified
                            // paragraph gap after the final body paragraph would make a quote
                            // without an attribution visibly bottom-heavy.
                            resolved.to_mut().style.margin_after = 0.0;
                        }
                        let mut prepared = self.shape_text(&resolved, reader_style, quote_width);
                        if unified_reflow {
                            prepared.start_offset += quote_horizontal_padding;
                        }
                        prepared_body.push((prepared, resolved));
                    }
                    let prepared_attribution = quote.attribution.as_ref().map(|attribution| {
                        let resolved =
                            resolve_text_block(attribution, reader_style, TextContext::Flow);
                        let mut prepared = self.shape_text(&resolved, reader_style, quote_width);
                        if unified_reflow {
                            prepared.start_offset += quote_horizontal_padding;
                        }
                        (prepared, resolved)
                    });
                    if unified_reflow {
                        let content_height = prepared_body
                            .iter()
                            .map(|(prepared, resolved)| {
                                prepared_flow_height(prepared)
                                    + resolved.style.margin_before.max(0.0)
                                    + resolved.style.margin_after.max(0.0)
                            })
                            .chain(prepared_attribution.iter().map(|(prepared, resolved)| {
                                prepared_flow_height(prepared)
                                    + resolved.style.margin_before.max(0.0)
                                    + resolved.style.margin_after.max(0.0)
                            }))
                            .sum::<f32>();
                        // Keep a short quote card intact. The leading outer
                        // gap and both internal paddings must fit before its
                        // trailing attribution; the final outer gap may flow
                        // naturally after the completed card.
                        paginator
                            .keep_together_if_fits(content_height + QUOTE_VERTICAL_PADDING * 3.0);
                        let sources = quote
                            .body
                            .iter()
                            .filter_map(|block| block.source.clone())
                            .chain(
                                quote
                                    .attribution
                                    .iter()
                                    .filter_map(|block| block.source.clone()),
                            )
                            .collect();
                        let outer_gap = (reader_style.typography.font_size
                            * reader_style.typesetting.paragraph_gap_em)
                            .max(QUOTE_VERTICAL_PADDING);
                        paginator.begin_quote(sources, reader_style.foreground, outer_gap);
                    }
                    for (prepared, resolved) in &prepared_body {
                        paginator.push_text(prepared, resolved.as_ref())?;
                    }
                    if let Some((prepared, resolved)) = &prepared_attribution {
                        paginator.push_text(prepared, resolved.as_ref())?;
                    }
                    if unified_reflow {
                        paginator.end_quote();
                    }
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
                Block::Figure(figure) => self.push_figure(
                    &mut paginator,
                    source,
                    &figure.images,
                    &figure.captions,
                    figure.caption_position,
                    figure.style,
                    reader_style,
                    content_width,
                    media_start_offset,
                    unified_reflow,
                )?,
                Block::Separator(separator) => {
                    if !unified_reflow || separator.in_quote {
                        match separator.kind {
                            SeparatorKind::Spacing => paginator.ensure_minimum_spacing(
                                separator.style.margin_before.max(0.0)
                                    + reader_style.typography.font_size
                                        * separator.style.line_height.max(1.0)
                                    + separator.style.margin_after.max(0.0),
                            ),
                            SeparatorKind::Rule => paginator.push_separator(),
                            SeparatorKind::Ornament => {
                                if let Some(image) = &separator.image {
                                    let raster = load_raster_image(source, image)?;
                                    let replacements = paginator.push_image(
                                        raster,
                                        image.style,
                                        image.source.clone(),
                                        image.text_layer.clone(),
                                    );
                                    for replacement in replacements {
                                        let prepared = self.shape_fixed_page_replacement(
                                            &replacement,
                                            reader_style,
                                        );
                                        paginator
                                            .push_fixed_page_replacement(&prepared, replacement)?;
                                    }
                                }
                            }
                        }
                    }
                }
                Block::LineBreak => paginator.ensure_minimum_spacing(
                    reader_style.typography.font_size
                        * if unified_reflow {
                            unified_body_line_height(reader_style.writing_system)
                        } else {
                            reader_style.typesetting.body_line_height
                        },
                ),
                Block::PageBreak => paginator.force_page(),
                Block::Note(_) => unreachable!("note blocks are flattened before layout"),
            }
            block_index += 1;
        }

        Ok(SectionLayout {
            pages: paginator.finish(),
            visible_pages,
            continuation_offset_x,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn push_figure(
        &mut self,
        paginator: &mut Paginator,
        source: &dyn BookSource,
        figure_images: &[ImageBlock],
        figure_captions: &[TextBlock],
        caption_position: CaptionPosition,
        figure_style: BlockStyle,
        reader_style: &ReaderStyle,
        content_width: f32,
        media_start_offset: f32,
        unified_reflow: bool,
    ) -> Result<(), LayoutError> {
        let authored_outer_gap = figure_images
            .iter()
            .map(|image| image.style.margin_before.max(image.style.margin_after))
            .fold(0.0, f32::max)
            .max(figure_style.margin_before)
            .max(figure_style.margin_after);
        let mut images = Vec::with_capacity(figure_images.len());
        for image in figure_images {
            let mut style = image.style;
            // The figure owns its outer spacing. Keeping authored margins on each
            // child would double the gap between an image and its caption.
            style.margin_before = 0.0;
            style.margin_after = 0.0;
            images.push((load_raster_image(source, image)?, style, image));
        }
        let captions = figure_captions
            .iter()
            .map(|caption| {
                self.shape_figure_caption(
                    caption,
                    reader_style,
                    (content_width - media_start_offset).max(1.0),
                    unified_reflow,
                )
            })
            .collect::<Vec<_>>();
        let outer_gap = if unified_reflow {
            reader_style.typography.font_size * reader_style.typesetting.media_gap_em
        } else {
            authored_outer_gap.max(IMAGE_BLOCK_GAP)
        };
        let caption_gap = if unified_reflow {
            reader_style.typography.font_size * reader_style.typesetting.caption_gap_em
        } else {
            6.0
        };
        let image_height = images
            .iter()
            .map(|(raster, style, _)| paginator.image_display_size(raster, *style).1)
            .sum::<f32>();
        let caption_height = captions
            .iter()
            .map(|(prepared, resolved)| {
                prepared_text_height(prepared)
                    + resolved.style.margin_before.max(0.0)
                    + resolved.style.margin_after.max(0.0)
            })
            .sum::<f32>();
        let internal_image_gaps = caption_gap * images.len().saturating_sub(1) as f32;
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
                    let prepared = engine.shape_fixed_page_replacement(&replacement, reader_style);
                    paginator.push_fixed_page_replacement(&prepared, replacement)?;
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
        match caption_position {
            CaptionPosition::Before => {
                push_captions(paginator)?;
                if !captions.is_empty() && !images.is_empty() {
                    paginator.add_semantic_spacing(caption_gap);
                }
                push_images(paginator, self)?;
            }
            CaptionPosition::After => {
                push_images(paginator, self)?;
                if !captions.is_empty() && !images.is_empty() {
                    paginator.add_semantic_spacing(caption_gap);
                }
                push_captions(paginator)?;
            }
        }
        paginator.ensure_minimum_spacing(outer_gap);
        Ok(())
    }

    fn shape_text(
        &mut self,
        block: &TextBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
    ) -> PreparedText {
        self.shape_text_with_min_width(block, reader_style, content_width, 40.0)
    }

    fn shape_text_from_source(
        &mut self,
        source: &dyn BookSource,
        block: &TextBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
    ) -> Result<PreparedText, LayoutError> {
        let mut rasters = Vec::with_capacity(block.content.len());
        for inline in &block.content {
            rasters.push(match inline {
                Inline::Image(run) => Some(load_raster_image(source, &run.image)?),
                Inline::Text(_) | Inline::Math(_) | Inline::Break => None,
            });
        }
        Ok(self.shape_text_with_min_width_and_rasters(
            block,
            reader_style,
            content_width,
            40.0,
            &rasters,
        ))
    }

    fn shape_figure_caption<'a>(
        &mut self,
        caption: &'a TextBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
        unified_reflow: bool,
    ) -> (PreparedText, Cow<'a, TextBlock>) {
        let mut resolved = resolve_text_block(caption, reader_style, TextContext::Flow);
        let mut prepared = self.shape_text(&resolved, reader_style, content_width);
        if unified_reflow && prepared.layout.len() > 1 {
            resolved.to_mut().style.align = TextAlignment::Start;
            prepared = self.shape_text(&resolved, reader_style, content_width);
        }
        (prepared, resolved)
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
        self.shape_text_with_min_width_and_rasters(
            block,
            reader_style,
            content_width,
            minimum_width,
            &[],
        )
    }

    #[allow(clippy::too_many_lines)]
    fn shape_text_with_min_width_and_rasters(
        &mut self,
        block: &TextBlock,
        reader_style: &ReaderStyle,
        content_width: f32,
        minimum_width: f32,
        inline_rasters: &[Option<RasterImage>],
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
            reader_style.focus_footnote_icons,
            inline_rasters,
        );
        let font_stack = if block.kind == TextBlockKind::Preformatted {
            typography.monospace_stack()
        } else {
            typography.default_stack_for(reader_style.writing_system)
        };
        let mut layout = self.build_text_layout(
            &text,
            &spans,
            &inline_images,
            &font_stack,
            typography,
            block.style.line_height,
            reader_style.foreground,
            &[],
        );
        self.apply_text_indents(
            &mut layout,
            block,
            &text[..source_text_start],
            typography,
            &font_stack,
            first_line_indent,
        );
        let should_optimize = reader_style.typesetting.line_break_strategy
            == LineBreakStrategy::Optimized
            && matches!(
                block.style.align,
                TextAlignment::Start | TextAlignment::Justify
            )
            && matches!(
                block.kind,
                TextBlockKind::Paragraph
                    | TextBlockKind::Blockquote
                    | TextBlockKind::Caption
                    | TextBlockKind::ListItem { .. }
                    | TextBlockKind::DefinitionDescription { .. }
            );
        let candidate_hyphens = if should_optimize {
            self.prepare_hyphen_candidates(
                &text,
                &spans,
                &inline_images,
                &font_stack,
                typography,
                block.style.line_height,
                reader_style.foreground,
            )
        } else {
            HashMap::new()
        };
        let hyphen_widths = candidate_hyphens
            .iter()
            .map(|(offset, glyph)| (*offset, glyph.width))
            .collect::<HashMap<_, _>>();
        let mut selected_hyphens = Vec::new();
        let mut optimized = should_optimize
            && linebreak::parley::plan_optimized(
                &mut layout,
                &text,
                available_width,
                first_line_indent,
                typography.font_size,
                &hyphen_widths,
            )
            .and_then(|plan| {
                let mut adjusted = self.build_text_layout(
                    &text,
                    &spans,
                    &inline_images,
                    &font_stack,
                    typography,
                    block.style.line_height,
                    reader_style.foreground,
                    &plan.adjustments,
                );
                self.apply_text_indents(
                    &mut adjusted,
                    block,
                    &text[..source_text_start],
                    typography,
                    &font_stack,
                    first_line_indent,
                );
                linebreak::parley::apply_breaks(&mut adjusted, &plan.lines, available_width)?;
                selected_hyphens = plan
                    .hyphen_offsets
                    .iter()
                    .enumerate()
                    .filter_map(|(line_index, offset)| {
                        let glyph = candidate_hyphens.get(offset.as_ref()?)?.clone();
                        Some(PreparedHyphen { line_index, glyph })
                    })
                    .collect();
                layout = adjusted;
                Some(())
            })
            .is_some();
        if !optimized {
            layout.break_all_lines(Some(available_width));
            if block.style.align == TextAlignment::Justify
                && let Some(plan) = linebreak::parley::plan_wrapped_justification(
                    &mut layout,
                    &text,
                    available_width,
                )
            {
                let mut adjusted = self.build_text_layout(
                    &text,
                    &spans,
                    &inline_images,
                    &font_stack,
                    typography,
                    block.style.line_height,
                    reader_style.foreground,
                    &plan.adjustments,
                );
                self.apply_text_indents(
                    &mut adjusted,
                    block,
                    &text[..source_text_start],
                    typography,
                    &font_stack,
                    first_line_indent,
                );
                if linebreak::parley::apply_breaks(&mut adjusted, &plan.lines, available_width)
                    .is_some()
                {
                    layout = adjusted;
                    optimized = true;
                }
            }
        }
        let alignment = if optimized {
            Alignment::Start
        } else {
            text_alignment(block.style.align)
        };
        layout.align(alignment, AlignmentOptions::default());
        PreparedText {
            layout: Arc::new(layout),
            text: text.into(),
            source_text_start,
            start_offset,
            available_width,
            inline_images: inline_images
                .into_iter()
                .map(|image| InlineImage {
                    id: image.id,
                    image: image.image,
                    width: image.width,
                    height: image.height,
                    offset_y: image.offset_y,
                })
                .collect::<Vec<_>>()
                .into(),
            hyphens: selected_hyphens.into(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_hyphen_candidates(
        &mut self,
        text: &str,
        spans: &[StyledRange],
        inline_images: &[PreparedInlineImage],
        font_stack: &str,
        typography: &ReaderTypography,
        line_height: f32,
        foreground: Rgba,
    ) -> HashMap<usize, PreparedHyphenGlyph> {
        let hyphenation_spans = spans
            .iter()
            .filter(|span| !span.range.is_empty())
            .map(|span| linebreak::hyphenation::HyphenationSpan {
                range: span.range.clone(),
                language: span.style.language,
                mode: span.style.hyphenation,
                suppress: span.hyphenation_suppressed,
            })
            .collect::<Vec<_>>();
        let blockers = inline_images
            .iter()
            .map(|image| image.index)
            .collect::<Vec<_>>();
        let mut opportunities = linebreak::hyphenation::break_opportunities(
            text,
            &hyphenation_spans,
            &self.publication_languages,
            &blockers,
        )
        .into_iter()
        .collect::<Vec<_>>();
        opportunities.sort_unstable();

        let hyphen_text: Arc<str> = Arc::from("\u{2010}");
        let mut style_cache = Vec::<(TextStyle, PreparedHyphenGlyph)>::new();
        let mut prepared = HashMap::new();
        for offset in opportunities {
            let Some(style) = spans
                .iter()
                .find(|span| {
                    span.range.start < offset
                        && offset <= span.range.end
                        && !span.hyphenation_suppressed
                })
                .map(|span| span.style)
            else {
                continue;
            };
            if let Some((_, glyph)) = style_cache.iter().find(|(cached, _)| *cached == style) {
                prepared.insert(offset, glyph.clone());
                continue;
            }
            let hyphen_span = StyledRange {
                range: 0..hyphen_text.len(),
                style,
                footnote_reference_group: 0,
                hyphenation_suppressed: true,
            };
            let mut layout = self.build_text_layout(
                &hyphen_text,
                std::slice::from_ref(&hyphen_span),
                &[],
                font_stack,
                typography,
                line_height,
                foreground,
                &[],
            );
            layout.break_all_lines(None);
            let Some(line) = layout.get(0) else {
                continue;
            };
            let width = positioned_line_content_end(line);
            if !width.is_finite() || width <= 0.0 {
                continue;
            }
            let glyph = PreparedHyphenGlyph {
                layout: Arc::new(layout),
                text: Arc::clone(&hyphen_text),
                width,
            };
            style_cache.push((style, glyph.clone()));
            prepared.insert(offset, glyph);
        }
        prepared
    }

    #[allow(clippy::too_many_arguments)]
    fn build_text_layout(
        &mut self,
        text: &str,
        spans: &[StyledRange],
        inline_images: &[PreparedInlineImage],
        font_stack: &str,
        typography: &ReaderTypography,
        line_height: f32,
        foreground: Rgba,
        spacing: &[linebreak::parley::SpacingAdjustment],
    ) -> Layout<TextBrush> {
        let mut builder =
            self.layout_context
                .ranged_builder(&mut self.font_context, text, 1.0, false);
        builder.push_default(StyleProperty::FontFamily(FontFamily::from(font_stack)));
        builder.push_default(StyleProperty::FontSize(typography.font_size));
        let default_variations = optical_size_variations(typography.font_size);
        builder.push_default(StyleProperty::FontVariations(FontVariations::from(
            &default_variations,
        )));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(f32::from(
            typography.font_weight,
        ))));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            line_height,
        )));
        let default_brush = TextBrush::new(foreground, false, TextBaseline::Normal, 0);
        builder.push_default(StyleProperty::Brush(default_brush));

        for span in spans {
            let size = (typography.font_size * span.style.size_scale.clamp(0.5, 3.0))
                .max(typography.minimum_font_size);
            builder.push(StyleProperty::FontSize(size), span.range.clone());
            let variations = optical_size_variations(size);
            builder.push(
                StyleProperty::FontVariations(FontVariations::from(&variations)),
                span.range.clone(),
            );
            builder.push(
                StyleProperty::Brush(TextBrush::new(
                    span.style.color,
                    span.style.underline,
                    span.style.baseline,
                    span.footnote_reference_group,
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
                builder.push(StyleProperty::Underline(true), span.range.clone());
            }
        }

        for adjustment in spacing {
            builder.push(
                StyleProperty::LetterSpacing(adjustment.amount),
                adjustment.range.clone(),
            );
        }
        for image in inline_images {
            builder.push_inline_box(ParleyInlineBox {
                id: image.id,
                kind: InlineBoxKind::InFlow,
                index: image.index,
                width: image.width,
                height: image.box_height,
            });
        }
        builder.build(text)
    }

    fn apply_text_indents(
        &mut self,
        layout: &mut Layout<TextBrush>,
        block: &TextBlock,
        marker: &str,
        typography: &ReaderTypography,
        font_stack: &str,
        first_line_indent: f32,
    ) {
        if first_line_indent.abs() > f32::EPSILON {
            layout.set_text_indent(
                first_line_indent,
                IndentOptions {
                    each_line: block.style.subparagraph_gap_em.is_some(),
                    ..IndentOptions::default()
                },
            );
        }
        self.apply_list_indent(layout, block.kind, marker, typography, font_stack);
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
        let variations = optical_size_variations(typography.font_size);
        builder.push_default(StyleProperty::FontVariations(FontVariations::from(
            &variations,
        )));
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
        font_stack: &str,
    ) {
        if marker.is_empty() {
            return;
        }
        let marker_width = self.measure_list_marker_width(marker, font_stack, typography);
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

fn unified_body_line_height(system: WritingSystem) -> f32 {
    match system {
        WritingSystem::Cjk => 1.7,
        WritingSystem::Latin => 1.4,
        WritingSystem::Other | WritingSystem::Unknown => 1.5,
    }
}

fn resolve_text_block<'a>(
    block: &'a TextBlock,
    reader_style: &ReaderStyle,
    context: TextContext,
) -> Cow<'a, TextBlock> {
    if reader_style.typesetting.mode != TypesettingMode::Unified {
        if block.style.hard_break_after {
            let mut resolved = block.clone();
            resolved.style.margin_after +=
                reader_style.typography.font_size * resolved.style.line_height.max(1.0);
            return Cow::Owned(resolved);
        }
        return Cow::Borrowed(block);
    }

    let mut resolved = block.clone();
    let typography = &reader_style.typography;
    let profile = &reader_style.typesetting;
    let base_size = typography.font_size;
    let display_system = block
        .content
        .iter()
        .find_map(|inline| match inline {
            Inline::Text(run) => run.style.display_writing_system,
            _ => None,
        })
        .unwrap_or(reader_style.writing_system);
    let body_line_height = unified_body_line_height(display_system);
    let (scale, line_height, margin_after) = match context {
        TextContext::Table => (profile.table_font_scale, profile.table_line_height, 0.0),
        TextContext::Flow => match block.kind {
            TextBlockKind::Heading(level) => (
                unified_heading_scale(profile.heading_scale, level),
                1.3,
                base_size * profile.heading_body_gap_em,
            ),
            TextBlockKind::HeadingOrdinal(level) => (
                unified_heading_scale(profile.heading_scale, level) * 0.72,
                1.15,
                base_size * 0.25,
            ),
            TextBlockKind::Caption => (profile.caption_font_scale, 1.4, 0.0),
            TextBlockKind::Preformatted => (0.9, 1.45, base_size * profile.paragraph_gap_em),
            TextBlockKind::Blockquote => {
                (0.95, body_line_height, base_size * profile.paragraph_gap_em)
            }
            TextBlockKind::QuoteAttribution => (0.88, 1.4, 0.0),
            TextBlockKind::Paragraph
            | TextBlockKind::FootnoteDefinition
            | TextBlockKind::ListItem { .. }
            | TextBlockKind::DefinitionDescription { .. } => {
                (1.0, body_line_height, base_size * profile.paragraph_gap_em)
            }
            TextBlockKind::DefinitionTerm { .. } => (
                1.0,
                body_line_height,
                base_size * profile.paragraph_gap_em.min(0.25),
            ),
        },
    };
    let margin_after = margin_after
        + if block.style.hard_break_after {
            base_size * line_height
        } else {
            0.0
        };

    if context == TextContext::Flow {
        let prose = matches!(
            block.kind,
            TextBlockKind::Paragraph | TextBlockKind::Blockquote
        );
        resolved.style.align = if prose
            && let Some(authored_alignment) = block.style.authored_alignment
            && authored_alignment != TextAlignment::Start
        {
            authored_alignment
        } else if prose
            || matches!(block.kind, TextBlockKind::ListItem { .. })
                && text_block_supports_space_justification(block)
        {
            TextAlignment::Justify
        } else {
            TextAlignment::Start
        };
    }
    resolved.style.margin_before = 0.0;
    resolved.style.margin_after = margin_after;
    resolved.style.indent = if context == TextContext::Flow {
        match block.kind {
            TextBlockKind::Paragraph => {
                base_size * paragraph_indent_em(profile, reader_style.writing_system)
            }
            TextBlockKind::Blockquote if block.style.indent > f32::EPSILON => base_size * 2.0,
            TextBlockKind::Blockquote => 0.0,
            _ => 0.0,
        }
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
                resolved.style.margin_start = 0.0;
                resolved.style.margin_start_fraction = 0.0;
            }
            TextBlockKind::QuoteAttribution => {
                resolved.style.align = TextAlignment::End;
                resolved.style.margin_start = 0.0;
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
                if block.kind.is_heading()
                    || matches!(block.kind, TextBlockKind::DefinitionTerm { .. })
                {
                    run.style.bold = true;
                }
                if block.kind.is_heading() {
                    // Unified/focus typesetting owns heading presentation.
                    // Preserve inline emphasis in prose, but do not carry an
                    // authored block-level italic heading into focus mode.
                    run.style.italic = false;
                }
                if matches!(
                    block.kind,
                    TextBlockKind::Blockquote | TextBlockKind::QuoteAttribution
                ) {
                    run.style.bold = false;
                    run.style.italic = false;
                }
                if block.kind == TextBlockKind::Caption {
                    // Unified captions use one neutral presentation regardless of
                    // publisher CSS or semantic tags that would recreate bold or
                    // italic styling in the later script-aware pass.
                    run.style.bold = false;
                    run.style.italic = false;
                    run.style.emphasis = false;
                    run.style.alternate_voice = false;
                    run.style.citation = false;
                }
            }
            Inline::Math(run) => run.size_scale = scale,
            Inline::Image(run) => run.size_scale = scale,
            Inline::Break => {}
        }
    }
    resolve_semantic_inline_presentation(&mut resolved.content, reader_style.writing_system);
    Cow::Owned(resolved)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SemanticScriptClass {
    Cjk,
    ItalicFriendly,
    Neutral,
}

fn resolve_semantic_inline_presentation(
    content: &mut Vec<Inline>,
    fallback_writing_system: WritingSystem,
) {
    let original = std::mem::take(content);
    for inline in original {
        let Inline::Text(run) = inline else {
            content.push(inline);
            continue;
        };
        if !run.style.emphasis && !run.style.alternate_voice && !run.style.citation {
            content.push(Inline::Text(run));
            continue;
        }
        let spans = semantic_script_spans(&run.text, fallback_writing_system);
        if spans.is_empty() {
            content.push(Inline::Text(run));
            continue;
        }
        for (range, script) in spans {
            let mut style = run.style;
            let emphasized = style.emphasis || style.alternate_voice;
            match script {
                SemanticScriptClass::Cjk => {
                    if emphasized {
                        style.bold = true;
                    }
                    style.italic = false;
                }
                SemanticScriptClass::ItalicFriendly | SemanticScriptClass::Neutral => {
                    if emphasized || style.citation {
                        style.italic = true;
                    }
                }
            }
            content.push(Inline::Text(TextRun {
                text: run.text[range].to_owned(),
                style,
                link: run.link.clone(),
            }));
        }
    }
}

fn semantic_script_spans(
    text: &str,
    fallback_writing_system: WritingSystem,
) -> Vec<(Range<usize>, SemanticScriptClass)> {
    let mut clusters = text
        .grapheme_indices(true)
        .map(|(start, grapheme)| {
            (
                start..start + grapheme.len(),
                semantic_grapheme_script(grapheme),
                grapheme.chars().next(),
            )
        })
        .collect::<Vec<_>>();
    if clusters.is_empty() {
        return Vec::new();
    }
    let fallback = if fallback_writing_system == WritingSystem::Cjk {
        SemanticScriptClass::Cjk
    } else {
        SemanticScriptClass::ItalicFriendly
    };
    let mut next_strong = vec![None; clusters.len()];
    let mut next = None;
    for index in (0..clusters.len()).rev() {
        next_strong[index] = next;
        if clusters[index].1 != SemanticScriptClass::Neutral {
            next = Some(clusters[index].1);
        }
    }
    let mut previous = None;
    for (index, (_, script, first)) in clusters.iter_mut().enumerate() {
        if *script == SemanticScriptClass::Neutral {
            let right = next_strong[index];
            *script = if first.is_some_and(is_semantic_opening_punctuation) {
                right.or(previous).unwrap_or(fallback)
            } else if first.is_some_and(is_semantic_closing_punctuation) {
                previous.or(right).unwrap_or(fallback)
            } else {
                previous.or(right).unwrap_or(fallback)
            };
        }
        previous = Some(*script);
    }

    let mut spans: Vec<(Range<usize>, SemanticScriptClass)> = Vec::new();
    for (range, script, _) in clusters {
        if let Some((previous_range, previous_script)) = spans.last_mut()
            && *previous_script == script
            && previous_range.end == range.start
        {
            previous_range.end = range.end;
        } else {
            spans.push((range, script));
        }
    }
    spans
}

fn semantic_grapheme_script(grapheme: &str) -> SemanticScriptClass {
    for character in grapheme.chars() {
        match character.script() {
            Script::Han | Script::Hiragana | Script::Katakana | Script::Hangul => {
                return SemanticScriptClass::Cjk;
            }
            Script::Latin | Script::Greek | Script::Cyrillic => {
                return SemanticScriptClass::ItalicFriendly;
            }
            _ => {}
        }
    }
    SemanticScriptClass::Neutral
}

fn is_semantic_opening_punctuation(character: char) -> bool {
    matches!(
        character,
        '《' | '〈' | '（' | '(' | '【' | '[' | '「' | '『' | '“' | '‘'
    )
}

fn is_semantic_closing_punctuation(character: char) -> bool {
    matches!(
        character,
        '》' | '〉' | '）' | ')' | '】' | ']' | '」' | '』' | '”' | '’'
    )
}

fn text_block_supports_space_justification(block: &TextBlock) -> bool {
    block.content.iter().all(|inline| match inline {
        Inline::Text(run) => run
            .text
            .chars()
            .all(|character| character != '\u{00a0}' && !linebreak::parley::is_cjk(character)),
        Inline::Math(_) | Inline::Image(_) | Inline::Break => true,
    })
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
            | Block::Quote(_)
            | Block::Table(_)
            | Block::Image(_)
            | Block::Figure(_)
            | Block::Note(_)
            | Block::Separator(_)
            | Block::LineBreak
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
        .filter(|block| !matches!(block, Block::LineBreak | Block::PageBreak));
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
    footnote_reference_group: u32,
    hyphenation_suppressed: bool,
}

struct PreparedText {
    layout: Arc<Layout<TextBrush>>,
    text: Arc<str>,
    source_text_start: usize,
    start_offset: f32,
    available_width: f32,
    inline_images: Arc<[InlineImage]>,
    hyphens: Arc<[PreparedHyphen]>,
}

#[derive(Clone)]
struct PreparedHyphenGlyph {
    layout: Arc<Layout<TextBrush>>,
    text: Arc<str>,
    width: f32,
}

#[derive(Clone)]
struct PreparedHyphen {
    line_index: usize,
    glyph: PreparedHyphenGlyph,
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
    box_height: f32,
    offset_y: f32,
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

fn prepared_flow_height(prepared: &PreparedText) -> f32 {
    let Some(first) = prepared.layout.get(0) else {
        return 0.0;
    };
    let Some(last) = prepared.layout.get(prepared.layout.len().saturating_sub(1)) else {
        return 0.0;
    };
    let metrics = last.metrics();
    let line_box_bottom = metrics.block_min_coord + metrics.line_height;
    (metrics.block_max_coord.max(line_box_bottom) - first.metrics().block_min_coord).max(0.0)
}

fn resolve_text_measure(
    block: &TextBlock,
    content_width: f32,
    minimum_width: f32,
) -> (f32, f32, f32) {
    let first_line_indented = matches!(
        block.kind,
        TextBlockKind::Paragraph | TextBlockKind::Blockquote
    );
    let first_line_indent = if first_line_indented {
        block.style.indent
    } else {
        0.0
    };
    let block_indent = if first_line_indented {
        0.0
    } else {
        block.style.indent
    };
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
    focus_footnote_icons: bool,
    inline_rasters: &[Option<RasterImage>],
) -> (String, Vec<StyledRange>, Vec<PreparedInlineImage>, usize) {
    let mut text = String::new();
    let mut spans = Vec::new();
    let mut inline_images = Vec::new();
    let mut next_footnote_reference_group = 1_u32;
    let prefix = list_marker_prefix(block.kind);
    if !prefix.is_empty() {
        let start = text.len();
        text.push_str(&prefix);
        spans.push(StyledRange {
            range: start..text.len(),
            style: TextStyle {
                color: fallback_color,
                ..TextStyle::default()
            },
            footnote_reference_group: 0,
            hyphenation_suppressed: true,
        });
    }
    let source_text_start = text.len();

    for (inline_index, inline) in block.content.iter().enumerate() {
        match inline {
            Inline::Text(run) => {
                let start = text.len();
                let mut style = run.style;
                if style.color == Rgba::BLACK {
                    style.color = fallback_color;
                }
                let linked_footnote_reference = run
                    .link
                    .as_ref()
                    .is_some_and(|target| target.fragment().is_some())
                    && (run.style.link_role == LinkRole::FootnoteReference
                        || (run.style.link_role == LinkRole::Normal
                            && run.style.baseline == TextBaseline::Superscript));
                let footnote_reference = focus_footnote_icons
                    && (run.style.inline_role == InlineRole::Footnote || linked_footnote_reference);
                if footnote_reference {
                    text.push_str(&footnote_icon_placeholder(&run.text));
                } else {
                    text.push_str(&run.text);
                }
                let footnote_reference_group = if footnote_reference {
                    let group = next_footnote_reference_group;
                    next_footnote_reference_group = next_footnote_reference_group.saturating_add(1);
                    group
                } else {
                    0
                };
                spans.push(StyledRange {
                    range: start..text.len(),
                    style,
                    footnote_reference_group,
                    hyphenation_suppressed: run.link.is_some()
                        || footnote_reference
                        || run.style.baseline != TextBaseline::Normal
                        || run.style.inline_role != InlineRole::Normal,
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
                        box_height: image.2,
                        offset_y: 0.0,
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
                        footnote_reference_group: 0,
                        hyphenation_suppressed: true,
                    });
                }
            }
            Inline::Image(run) => {
                let Some(image) = inline_rasters.get(inline_index).and_then(Clone::clone) else {
                    continue;
                };
                let id = u64::try_from(inline_images.len()).unwrap_or(u64::MAX);
                inline_images.push(prepare_inline_raster(
                    run,
                    image,
                    typography,
                    available_width,
                    id,
                    text.len(),
                ));
            }
            Inline::Break => {
                let compact_gap = text
                    .ends_with('\n')
                    .then_some(block.style.subparagraph_gap_em)
                    .flatten();
                if let Some(gap_em) = compact_gap {
                    let start = text.len();
                    text.push('\u{2060}');
                    spans.push(StyledRange {
                        range: start..text.len(),
                        style: TextStyle {
                            size_scale: (block.style.line_height + gap_em.clamp(0.0, 2.0))
                                / block.style.line_height.max(0.1),
                            color: fallback_color,
                            ..TextStyle::default()
                        },
                        footnote_reference_group: 0,
                        hyphenation_suppressed: true,
                    });
                } else {
                    text.push('\n');
                }
            }
        }
    }
    (text, spans, inline_images, source_text_start)
}

#[allow(clippy::cast_precision_loss)]
fn prepare_inline_raster(
    run: &rebook_publication::InlineImageRun,
    image: RasterImage,
    typography: &ReaderTypography,
    available_width: f32,
    id: u64,
    index: usize,
) -> PreparedInlineImage {
    let intrinsic_width = image.width.max(1) as f32;
    let intrinsic_height = image.height.max(1) as f32;
    let aspect_ratio = intrinsic_width / intrinsic_height;
    let surrounding_scale = run.size_scale.max(0.1);
    let authored_height = run.image.style.height.map(|height| match height {
        ImageLength::Pixels(pixels) => typography.font_size * surrounding_scale * pixels / 16.0,
        ImageLength::Fraction(fraction) => typography.font_size * surrounding_scale * fraction,
    });
    let authored_width = run.image.style.width.map(|width| match width {
        ImageLength::Pixels(pixels) => typography.font_size * surrounding_scale * pixels / 16.0,
        ImageLength::Fraction(fraction) => available_width * fraction,
    });
    let (mut requested_width, mut requested_height) = if run.intrinsic_sizing {
        if let Some(height) = authored_height {
            (height * aspect_ratio, height)
        } else if let Some(width) = authored_width {
            (width, width / aspect_ratio)
        } else {
            let height = typography.font_size * surrounding_scale * intrinsic_height / 16.0;
            (height * aspect_ratio, height)
        }
    } else {
        let height = typography.font_size * run.size_scale;
        (height * aspect_ratio, height)
    };
    let minimum_height = typography.font_size * 0.2;
    let maximum_height = typography.font_size * 4.0;
    let height_scale =
        requested_height.clamp(minimum_height, maximum_height) / requested_height.max(1.0);
    requested_width *= height_scale;
    requested_height *= height_scale;
    let width_scale = (available_width / requested_width).min(1.0);
    let display_width = (requested_width * width_scale).max(1.0);
    let display_height = (requested_height * width_scale).max(1.0);
    let (box_height, offset_y) = inline_image_vertical_metrics(
        run.vertical_align,
        display_height,
        typography.font_size * surrounding_scale,
    );
    PreparedInlineImage {
        id,
        index,
        image,
        width: display_width,
        height: display_height,
        box_height,
        offset_y,
    }
}

fn inline_image_vertical_metrics(
    alignment: InlineImageAlignment,
    image_height: f32,
    surrounding_em: f32,
) -> (f32, f32) {
    let baseline_shift = match alignment {
        InlineImageAlignment::Baseline => 0.0,
        // Formula rasters in legacy EPUBs commonly use `vertical-align: middle`
        // to request optical centering in the text band. Center the image between
        // the same 0.8-em ascent and 0.2-em descent used below instead of applying
        // CSS's x-height offset, which places tightly cropped formula glyphs too low.
        InlineImageAlignment::Middle => image_height * 0.5 - surrounding_em * 0.3,
        InlineImageAlignment::TextTop | InlineImageAlignment::Top => {
            image_height - surrounding_em * 0.8
        }
        InlineImageAlignment::TextBottom
        | InlineImageAlignment::Bottom
        | InlineImageAlignment::Sub => surrounding_em * 0.2,
        InlineImageAlignment::Super => -surrounding_em * 0.35,
    };
    // Parley positions inline boxes with their bottom on the baseline. Reserve
    // the complete ascent/descent envelope, then paint the raster inside that
    // box at the authored baseline shift. This prevents a middle/sub-aligned
    // formula from visually colliding with the following line.
    let ascent = surrounding_em * 0.8;
    let descent = surrounding_em * 0.2;
    let above_baseline = (image_height - baseline_shift).max(0.0).max(ascent);
    let below_baseline = baseline_shift.max(0.0).max(descent);
    let box_height = (above_baseline + below_baseline).max(image_height);
    let paint_offset = box_height - image_height + baseline_shift;
    (box_height, paint_offset)
}

fn positioned_line_content_end(line: parley::layout::Line<'_, TextBrush>) -> f32 {
    let mut glyph_end = 0.0_f32;
    let mut inline_end = 0.0_f32;
    for item in line.items() {
        match item {
            PositionedLayoutItem::GlyphRun(run) => {
                glyph_end = glyph_end.max(run.offset() + run.advance());
            }
            PositionedLayoutItem::InlineBox(inline_box) => {
                inline_end = inline_end.max(inline_box.x + inline_box.width);
            }
        }
    }
    (glyph_end - line.metrics().trailing_whitespace)
        .max(inline_end)
        .max(0.0)
}

/// Reserves one compact glyph slot for a semantic footnote while retaining the
/// original scalar count used by source-offset mapping. Remaining scalars become
/// zero-width word joiners so markers such as `【3】` do not leave three ems of
/// blank space around the replacement icon.
fn footnote_icon_placeholder(marker: &str) -> String {
    marker
        .chars()
        .enumerate()
        .map(|(index, _)| if index == 0 { '0' } else { '\u{2060}' })
        .collect()
}

fn list_marker_prefix(kind: TextBlockKind) -> String {
    match kind {
        TextBlockKind::ListItem {
            marker_visible: false,
            ..
        } => String::new(),
        TextBlockKind::ListItem {
            ordered: true,
            ordinal,
            ..
        } => format!("{ordinal}.\u{00a0}"),
        TextBlockKind::ListItem { ordered: false, .. } => "•\u{00a0}".to_owned(),
        _ => String::new(),
    }
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
    active_quote: Option<ActiveQuote>,
}

struct ActiveQuote {
    sources: Vec<SourceRange>,
    fill: Rgba,
    accent: Rgba,
    outer_gap: f32,
    decoration_index: Option<usize>,
    has_started: bool,
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
            active_quote: None,
        }
    }

    fn begin_quote(&mut self, sources: Vec<SourceRange>, foreground: Rgba, outer_gap: f32) {
        self.previous_block_was_paragraph = false;
        self.ensure_minimum_spacing(outer_gap);
        self.active_quote = Some(ActiveQuote {
            sources,
            fill: Rgba {
                alpha: 0,
                ..foreground
            },
            accent: quote_accent_for_foreground(foreground),
            outer_gap,
            decoration_index: None,
            has_started: false,
        });
    }

    fn ensure_quote_decoration(&mut self) {
        let Some(active) = self.active_quote.as_ref() else {
            return;
        };
        if active.decoration_index.is_some() {
            return;
        }
        let continued_before = active.has_started;
        let sources = active.sources.clone();
        let fill = active.fill;
        let accent = active.accent;
        let index = self.items.len();
        self.items.push(PageItem::Quote(QuotePlacement {
            x: self.column_left(),
            y: self.cursor_y,
            width: self.width,
            height: QUOTE_VERTICAL_PADDING,
            continued_before,
            continued_after: false,
            fill,
            accent,
            sources,
        }));
        self.cursor_y = (self.cursor_y + QUOTE_VERTICAL_PADDING).min(self.bottom);
        if let Some(active) = self.active_quote.as_mut() {
            active.decoration_index = Some(index);
            active.has_started = true;
        }
    }

    fn pending_quote_padding(&self) -> f32 {
        self.active_quote.as_ref().map_or(0.0, |active| {
            if active.decoration_index.is_none() {
                QUOTE_VERTICAL_PADDING
            } else {
                0.0
            }
        })
    }

    fn update_quote_decoration(&mut self) {
        let Some(index) = self
            .active_quote
            .as_ref()
            .and_then(|active| active.decoration_index)
        else {
            return;
        };
        if let Some(PageItem::Quote(quote)) = self.items.get_mut(index) {
            quote.height = (self.cursor_y - quote.y).max(QUOTE_VERTICAL_PADDING);
        }
    }

    fn end_quote(&mut self) {
        if self.active_quote.is_none() {
            return;
        }
        let outer_gap = self
            .active_quote
            .as_ref()
            .map_or(QUOTE_VERTICAL_PADDING, |active| active.outer_gap);
        let decoration_index = self
            .active_quote
            .as_ref()
            .and_then(|active| active.decoration_index);
        if let Some(index) = decoration_index {
            self.cursor_y = (self.cursor_y + QUOTE_VERTICAL_PADDING).min(self.bottom);
            self.update_quote_decoration();
            if let Some(PageItem::Quote(quote)) = self.items.get_mut(index) {
                quote.continued_after = false;
            }
        } else if let Some(PageItem::Quote(quote)) = self.pages.last_mut().and_then(|page| {
            page.items
                .iter_mut()
                .rev()
                .find(|item| matches!(item, PageItem::Quote(_)))
        }) {
            quote.continued_after = false;
        }
        self.active_quote = None;
        self.previous_block_was_paragraph = false;
        self.add_preserved_spacing(outer_gap);
    }

    fn push_text(&mut self, prepared: &PreparedText, block: &TextBlock) -> Result<(), LayoutError> {
        self.forced_page_break = false;
        let is_paragraph = matches!(block.kind, TextBlockKind::Paragraph);
        if is_paragraph && self.previous_block_was_paragraph {
            self.ensure_minimum_spacing(self.minimum_paragraph_gap);
        }
        self.add_preserved_spacing(block.style.margin_before);
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
                let remaining = self.bottom - self.cursor_y - self.pending_quote_padding();
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
            // Do not start a quote decoration until its first text line fits on
            // this column. Otherwise a page boundary can retain an orphaned
            // accent bar above the actual quotation.
            self.ensure_quote_decoration();
            let origin_x = self.column_left() + prepared.start_offset;
            let origin_y = self.cursor_y - first_top;
            self.items.push(PageItem::Text(TextPlacement {
                layout: Arc::clone(&prepared.layout),
                text: Arc::clone(&prepared.text),
                source_text_start: prepared.source_text_start,
                lines: line_start..line_end,
                origin_x,
                origin_y,
                available_width: prepared.available_width,
                source: block.source.clone(),
                inline_images: Arc::clone(&prepared.inline_images),
            }));
            for hyphen in prepared
                .hyphens
                .iter()
                .filter(|hyphen| (line_start..line_end).contains(&hyphen.line_index))
            {
                let line = prepared
                    .layout
                    .get(hyphen.line_index)
                    .ok_or(LayoutError::InvalidLayout)?;
                let glyph_line = hyphen
                    .glyph
                    .layout
                    .get(0)
                    .ok_or(LayoutError::InvalidLayout)?;
                self.items.push(PageItem::Text(TextPlacement {
                    layout: Arc::clone(&hyphen.glyph.layout),
                    text: Arc::clone(&hyphen.glyph.text),
                    source_text_start: 0,
                    lines: 0..1,
                    origin_x: origin_x + positioned_line_content_end(line),
                    origin_y: origin_y + line.metrics().baseline - glyph_line.metrics().baseline,
                    available_width: hyphen.glyph.width,
                    source: None,
                    inline_images: Arc::from([]),
                }));
            }
            self.pending_leading_gap = 0.0;
            self.column_has_content = true;
            self.cursor_y += slice_height;
            self.update_quote_decoration();
            line_start = line_end;
            if line_start < prepared.layout.len() {
                self.advance_column();
            }
        }
        // Parley's glyph block bounds can be shorter than the complete line
        // box, especially after translation switches to a CJK fallback font.
        // Selection/highlight geometry covers the line box, so anchor the
        // authored paragraph margin after that same box before advancing.
        self.ensure_minimum_spacing(0.0);
        self.add_preserved_spacing(block.style.margin_after);
        self.update_quote_decoration();
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
        self.add_preserved_spacing(table.block_gap);
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
                        available_width: cell.text.available_width,
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
        self.pending_leading_gap = 0.0;
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

    fn keep_together_if_fits(&mut self, content_height: f32) {
        let content_height = content_height.max(0.0);
        let full_height = self.bottom - self.top;
        if content_height <= full_height
            && self.cursor_y + content_height > self.bottom
            && self.column_has_content
        {
            self.advance_column();
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
        let trailing_gap = style.margin_after.max(minimum_after).max(0.0);
        self.pending_leading_gap = 0.0;
        if trailing_gap < self.bottom - self.top {
            self.pending_leading_gap = trailing_gap;
        }
        self.column_has_content = true;
        self.cursor_y += height + trailing_gap;
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
                available_width: prepared.available_width,
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
        let amount = amount.max(0.0);
        let Some(content_bottom) = self.current_content_bottom() else {
            if !self.pages.is_empty() && !self.forced_page_break {
                self.leading_gap = self.leading_gap.max(amount);
            }
            return;
        };
        self.pending_leading_gap = self.pending_leading_gap.max(amount);
        let target = content_bottom + amount;
        if target > self.bottom {
            self.advance_column();
            self.leading_gap = self.leading_gap.max(amount);
        } else {
            self.cursor_y = self.cursor_y.max(target);
        }
    }

    fn current_content_bottom(&self) -> Option<f32> {
        self.items.last().and_then(|item| match item {
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
            PageItem::Quote(quote) => Some(quote.y + quote.height),
            PageItem::Image(image) => Some(image.y + image.height),
            PageItem::Table(table) => Some(table.y + table.height),
            PageItem::Separator(separator) => Some(separator.y + 1.0),
        })
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

    fn add_preserved_spacing(&mut self, amount: f32) {
        let amount = amount.max(0.0);
        let page_height = self.bottom - self.top;
        let preserved_amount = if amount < page_height { amount } else { 0.0 };
        if self.cursor_y + amount > self.bottom && self.column_has_content {
            self.pending_leading_gap += preserved_amount;
            self.advance_column();
        } else {
            self.cursor_y += amount;
            if !self.column_has_content
                && self.items.is_empty()
                && !self.pages.is_empty()
                && !self.forced_page_break
            {
                self.leading_gap += preserved_amount;
            } else if self.column_has_content {
                self.pending_leading_gap += preserved_amount;
            }
        }
    }

    fn add_semantic_spacing(&mut self, amount: f32) {
        self.add_preserved_spacing(amount);
    }

    fn force_page(&mut self) {
        self.pending_leading_gap = 0.0;
        self.forced_page_break = true;
        if self.column_has_content || !self.items.is_empty() {
            self.advance_column();
        }
    }

    fn column_left(&self) -> f32 {
        self.left
    }

    fn advance_column(&mut self) {
        self.update_quote_decoration();
        if let Some(index) = self
            .active_quote
            .as_ref()
            .and_then(|active| active.decoration_index)
            && let Some(PageItem::Quote(quote)) = self.items.get_mut(index)
        {
            quote.continued_after = true;
            quote.height = (self.bottom - quote.y).max(quote.height);
        }
        let pending_leading_gap = self.pending_leading_gap;
        self.commit_page();
        if let Some(active) = self.active_quote.as_mut() {
            active.decoration_index = None;
        }
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
    fn unified_semantic_styles_follow_each_mixed_script_span() {
        let emphasis = TextStyle {
            italic: true,
            emphasis: true,
            ..TextStyle::default()
        };
        let citation = TextStyle {
            italic: true,
            citation: true,
            ..TextStyle::default()
        };
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "重点 important 结论".into(),
                    style: emphasis,
                    link: None,
                }),
                Inline::Text(TextRun {
                    text: "《Rolling Stone》杂志".into(),
                    style: citation,
                    link: None,
                }),
            ],
            style: BlockStyle::default(),
            source: None,
        };
        let reader_style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            writing_system: WritingSystem::Cjk,
            ..ReaderStyle::default()
        };

        let resolved = resolve_text_block(&block, &reader_style, TextContext::Flow);
        let runs = resolved
            .content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) => Some(run),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            runs.iter()
                .any(|run| { run.text.contains("重点") && run.style.bold && !run.style.italic })
        );
        assert!(
            runs.iter().any(|run| {
                run.text.contains("important") && !run.style.bold && run.style.italic
            })
        );
        assert!(
            runs.iter()
                .any(|run| { run.text.contains("结论") && run.style.bold && !run.style.italic })
        );
        assert!(runs.iter().any(|run| {
            run.text.contains("Rolling Stone") && run.style.citation && run.style.italic
        }));
        assert!(
            runs.iter().any(|run| {
                run.text.contains("杂志") && run.style.citation && !run.style.italic
            })
        );
    }

    #[test]
    fn book_typesetting_keeps_authored_semantic_tag_presentation() {
        let style = TextStyle {
            italic: true,
            emphasis: true,
            ..TextStyle::default()
        };
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "中文强调".into(),
                style,
                link: None,
            })],
            style: BlockStyle::default(),
            source: None,
        };

        let resolved = resolve_text_block(&block, &ReaderStyle::default(), TextContext::Flow);

        assert!(matches!(resolved, Cow::Borrowed(_)));
        let Inline::Text(run) = &resolved.content[0] else {
            panic!("expected text run");
        };
        assert!(run.style.italic);
        assert!(!run.style.bold);
    }

    #[test]
    fn focus_layout_omits_semantic_footnote_definitions_from_the_main_flow() {
        let block = Block::Text(TextBlock {
            kind: TextBlockKind::FootnoteDefinition,
            content: vec![Inline::Text(TextRun {
                text: "Footnote body".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: Default::default(),
            source: None,
        });

        assert!(should_layout_flow_block(&block, &ReaderStyle::default()));
        assert!(!should_layout_flow_block(
            &block,
            &ReaderStyle {
                focus_footnote_icons: true,
                ..ReaderStyle::default()
            }
        ));
    }

    #[test]
    fn unified_layout_hides_note_sections_while_book_layout_keeps_them() {
        let body = Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "Endnote body".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: Default::default(),
            source: None,
        });
        let section_note = Block::Note(rebook_publication::NoteBlock {
            kind: NoteBlockKind::Section,
            blocks: vec![body.clone()],
            source: None,
        });
        let definition_note = Block::Note(rebook_publication::NoteBlock {
            kind: NoteBlockKind::Definition,
            blocks: vec![body],
            source: None,
        });

        let mut output = Vec::new();
        collect_layout_blocks(
            std::slice::from_ref(&section_note),
            &ReaderStyle::default(),
            &mut output,
        );
        assert_eq!(output.len(), 1);

        output.clear();
        collect_layout_blocks(
            std::slice::from_ref(&section_note),
            &ReaderStyle {
                typesetting: ReaderTypesetting::unified(),
                ..ReaderStyle::default()
            },
            &mut output,
        );
        assert!(output.is_empty());

        output.clear();
        collect_layout_blocks(
            std::slice::from_ref(&definition_note),
            &ReaderStyle {
                focus_footnote_icons: true,
                ..ReaderStyle::default()
            },
            &mut output,
        );
        assert!(output.is_empty());
    }

    #[test]
    fn focus_footnote_icons_mark_semantic_references_and_legacy_linked_superscripts() {
        let linked_superscript = TextRun {
            text: "1".into(),
            style: TextStyle {
                baseline: TextBaseline::Superscript,
                ..TextStyle::default()
            },
            link: Some(rebook_publication::PublicationUrl::parse("notes.xhtml#note-1").unwrap()),
        };
        let unlinked_superscript = TextRun {
            text: "2".into(),
            style: linked_superscript.style,
            link: None,
        };
        let linked_baseline_text = TextRun {
            text: "3".into(),
            style: TextStyle::default(),
            link: linked_superscript.link.clone(),
        };
        let semantic_baseline_reference = TextRun {
            text: "[4]".into(),
            style: TextStyle {
                link_role: LinkRole::FootnoteReference,
                ..TextStyle::default()
            },
            link: linked_superscript.link.clone(),
        };
        let superscript_backlink = TextRun {
            text: "[5]".into(),
            style: TextStyle {
                baseline: TextBaseline::Superscript,
                link_role: LinkRole::FootnoteBacklink,
                ..TextStyle::default()
            },
            link: Some(rebook_publication::PublicationUrl::parse("chapter.xhtml#ref-5").unwrap()),
        };
        let inline_footnote = TextRun {
            text: "inline note".into(),
            style: TextStyle {
                inline_role: InlineRole::Footnote,
                ..TextStyle::default()
            },
            link: None,
        };
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(linked_superscript),
                Inline::Text(unlinked_superscript),
                Inline::Text(linked_baseline_text),
                Inline::Text(semantic_baseline_reference),
                Inline::Text(superscript_backlink),
                Inline::Text(inline_footnote),
            ],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let svg_options = resvg::usvg::Options::default();

        let (focus_text, spans, _, _) = prepare_inline_content(
            &block,
            Rgba::BLACK,
            &ReaderTypography::default(),
            320.0,
            &svg_options,
            true,
            &[],
        );
        assert_eq!(
            spans
                .iter()
                .map(|span| span.footnote_reference_group != 0)
                .collect::<Vec<_>>(),
            [true, false, false, true, false, true]
        );
        assert_eq!(
            focus_text,
            format!(
                "023{}[5]{}",
                footnote_icon_placeholder("[4]"),
                footnote_icon_placeholder("inline note")
            )
        );

        let (classic_text, disabled_spans, _, _) = prepare_inline_content(
            &block,
            Rgba::BLACK,
            &ReaderTypography::default(),
            320.0,
            &svg_options,
            false,
            &[],
        );
        assert!(
            disabled_spans
                .iter()
                .all(|span| span.footnote_reference_group == 0)
        );
        assert_eq!(classic_text, "123[4][5]inline note");
    }

    #[test]
    fn prepares_semantic_inline_images_at_their_text_position() {
        let block = TextBlock {
            kind: TextBlockKind::Heading(1),
            content: vec![
                Inline::Image(Box::new(rebook_publication::InlineImageRun {
                    image: ImageBlock {
                        href: PublicationUrl::parse("images/chapter-icon.jpg").unwrap(),
                        alt: String::new(),
                        style: ImageStyle::default(),
                        source: None,
                        text_layer: None,
                    },
                    size_scale: 1.0,
                    intrinsic_sizing: false,
                    vertical_align: InlineImageAlignment::Middle,
                    presentation: true,
                })),
                Inline::Text(TextRun {
                    text: "Chapter title".into(),
                    style: TextStyle::default(),
                    link: None,
                }),
            ],
            style: BlockStyle::default(),
            source: None,
        };
        let raster = RasterImage {
            width: 200,
            height: 100,
            pixels: vec![0; 200 * 100 * 4].into(),
        };
        let typography = ReaderTypography::default();
        let svg_options = resvg::usvg::Options::default();

        let (text, _, images, _) = prepare_inline_content(
            &block,
            Rgba::BLACK,
            &typography,
            320.0,
            &svg_options,
            false,
            &[Some(raster), None],
        );

        assert_eq!(text, "Chapter title");
        let [image] = images.as_slice() else {
            panic!("expected one prepared inline image");
        };
        assert_eq!(image.index, 0);
        assert!((image.height - typography.font_size).abs() < f32::EPSILON);
        assert!((image.width - typography.font_size * 2.0).abs() < f32::EPSILON);
        assert!(image.offset_y > 0.0);
    }

    #[test]
    fn scales_unstyled_inline_images_from_intrinsic_css_pixels() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Image(Box::new(
                rebook_publication::InlineImageRun {
                    image: ImageBlock {
                        href: PublicationUrl::parse("images/pi.jpg").unwrap(),
                        alt: "Image".into(),
                        style: ImageStyle::default(),
                        source: None,
                        text_layer: None,
                    },
                    size_scale: 1.0,
                    intrinsic_sizing: true,
                    vertical_align: InlineImageAlignment::Middle,
                    presentation: false,
                },
            ))],
            style: BlockStyle::default(),
            source: None,
        };
        let raster = RasterImage {
            width: 12,
            height: 12,
            pixels: vec![0; 12 * 12 * 4].into(),
        };
        let typography = ReaderTypography::default();
        let svg_options = resvg::usvg::Options::default();

        let (_, _, images, _) = prepare_inline_content(
            &block,
            Rgba::BLACK,
            &typography,
            320.0,
            &svg_options,
            false,
            &[Some(raster)],
        );

        let [image] = images.as_slice() else {
            panic!("expected one prepared inline image");
        };
        let expected = typography.font_size * 12.0 / 16.0;
        assert!((image.height - expected).abs() < f32::EPSILON);
        assert!((image.width - expected).abs() < f32::EPSILON);
        assert!(image.offset_y > 0.0);
        assert!(image.offset_y < image.height * 0.5);
        let visual_center_from_baseline = -image.box_height + image.offset_y + image.height * 0.5;
        assert!((visual_center_from_baseline + typography.font_size * 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn quote_accent_tracks_the_reader_foreground_theme() {
        assert_eq!(
            quote_accent_for_foreground(Rgba::BLACK),
            LIGHT_QUOTE_ACCENT_COLOR
        );
        assert_eq!(
            quote_accent_for_foreground(Rgba {
                red: 232,
                green: 230,
                blue: 225,
                alpha: 255,
            }),
            DARK_QUOTE_ACCENT_COLOR
        );
    }

    #[test]
    fn sentence_justification_preserves_subparagraph_indents_and_gaps() {
        let sentence = "威斯康星大学麦迪逊分校的比较心理学家哈里·哈洛（Harry Harlow）进行了一项臭名昭著的实验。";
        let run = Inline::Text(TextRun {
            text: sentence.into(),
            style: TextStyle::default(),
            link: None,
        });
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![run.clone(), Inline::Break, Inline::Break, run],
            style: rebook_publication::BlockStyle {
                indent: 32.0,
                subparagraph_gap_em: Some(0.3),
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };
        let style = ReaderStyle::default();
        let mut engine = LayoutEngine::new();
        let natural = engine.shape_text(&block, &style, 400.0);
        block.style.align = TextAlignment::Justify;
        let justified = engine.shape_text(&block, &style, 400.0);
        assert_eq!(natural.layout.len(), justified.layout.len());
        assert_eq!(natural.text, justified.text);
        for (before, after) in natural.layout.lines().zip(justified.layout.lines()) {
            assert_eq!(before.text_range(), after.text_range());
            assert_eq!(before.break_reason(), after.break_reason());
            assert!((before.metrics().offset - after.metrics().offset).abs() < 0.01);
            assert!((before.metrics().line_height - after.metrics().line_height).abs() < 0.01);
            if matches!(
                after.break_reason(),
                parley::layout::BreakReason::None | parley::layout::BreakReason::Explicit
            ) {
                assert!((before.metrics().advance - after.metrics().advance).abs() < 0.01);
            } else {
                assert!(
                    (linebreak::parley::positioned_line_content_end(after) - 400.0).abs() < 0.1
                );
            }
        }
    }

    #[test]
    fn semantic_subparagraph_break_uses_the_compact_configured_gap() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "First sentence.".into(),
                    style: TextStyle::default(),
                    link: None,
                }),
                Inline::Break,
                Inline::Break,
                Inline::Text(TextRun {
                    text: "Second sentence.".into(),
                    style: TextStyle::default(),
                    link: None,
                }),
            ],
            style: rebook_publication::BlockStyle {
                indent: 24.0,
                line_height: 1.5,
                subparagraph_gap_em: Some(0.3),
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };
        let style = ReaderStyle::default();
        let prepared = LayoutEngine::new().shape_text(&block, &style, 400.0);

        assert_eq!(prepared.layout.len(), 2);
        assert!((prepared.layout.get(0).unwrap().metrics().offset - 24.0).abs() < 0.01);
        assert!((prepared.layout.get(1).unwrap().metrics().offset - 24.0).abs() < 0.01);
        let body_line_height = prepared.layout.get(0).unwrap().metrics().line_height;
        let next_line_height = prepared.layout.get(1).unwrap().metrics().line_height;
        assert!(
            (body_line_height - style.typography.font_size * 1.5).abs() < 0.5,
            "expected the preceding line to keep the 1.5em body line height, got {body_line_height}"
        );
        assert!(
            (next_line_height - style.typography.font_size * 1.8).abs() < 0.5,
            "expected 1.5em line height plus a 0.3em subparagraph gap, got {next_line_height}"
        );
    }

    #[test]
    fn unified_typesetting_replaces_authored_heading_metrics() {
        let block = TextBlock {
            kind: TextBlockKind::Heading(2),
            content: vec![Inline::Text(TextRun {
                text: "Heading".into(),
                style: TextStyle {
                    size_scale: 2.8,
                    italic: true,
                    ..TextStyle::default()
                },
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                align: TextAlignment::Center,
                authored_alignment: Some(TextAlignment::Center),
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
        assert!(!run.style.italic);
    }

    #[test]
    fn translated_prose_uses_display_language_line_height() {
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "译文 translated text".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle::default(),
            source: None,
        };
        let mut style = ReaderStyle {
            writing_system: WritingSystem::Latin,
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        assert!(
            (resolve_text_block(&block, &style, TextContext::Flow)
                .style
                .line_height
                - 1.4)
                .abs()
                < 0.001
        );
        if let Inline::Text(run) = &mut block.content[0] {
            run.style.display_writing_system = Some(WritingSystem::Cjk);
        }
        assert!(
            (resolve_text_block(&block, &style, TextContext::Flow)
                .style
                .line_height
                - 1.7)
                .abs()
                < 0.001
        );
        block.kind = TextBlockKind::Caption;
        assert!(
            (resolve_text_block(&block, &style, TextContext::Flow)
                .style
                .line_height
                - 1.4)
                .abs()
                < 0.001
        );
        style.typesetting.mode = TypesettingMode::Book;
        assert_eq!(
            resolve_text_block(&block, &style, TextContext::Flow).as_ref(),
            &block
        );
    }

    #[test]
    fn unified_split_heading_uses_a_compact_ordinal_before_the_title() {
        let text_block = |kind| TextBlock {
            kind,
            content: vec![Inline::Text(TextRun {
                text: "Heading".into(),
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
        let ordinal = text_block(TextBlockKind::HeadingOrdinal(2));
        let title = text_block(TextBlockKind::Heading(2));

        let ordinal = resolve_text_block(&ordinal, &style, TextContext::Flow);
        let title = resolve_text_block(&title, &style, TextContext::Flow);
        let Inline::Text(ordinal_run) = &ordinal.content[0] else {
            panic!("expected ordinal text run");
        };
        let Inline::Text(title_run) = &title.content[0] else {
            panic!("expected title text run");
        };
        assert!(ordinal_run.style.size_scale < title_run.style.size_scale);
        assert!(ordinal.style.margin_after < title.style.margin_after);
        assert!(ordinal_run.style.bold);
        assert!(!ordinal_run.style.italic);
    }

    #[test]
    fn unified_captions_clear_all_authored_bold_and_italic_sources() {
        let block = TextBlock {
            kind: TextBlockKind::Caption,
            content: vec![Inline::Text(TextRun {
                text: "Figure 1. Authored emphasis and citation.".into(),
                style: TextStyle {
                    bold: true,
                    italic: true,
                    emphasis: true,
                    alternate_voice: true,
                    citation: true,
                    ..TextStyle::default()
                },
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: None,
        };
        let classic = ReaderStyle::default();
        let unified = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let classic = resolve_text_block(&block, &classic, TextContext::Flow);
        let unified = resolve_text_block(&block, &unified, TextContext::Flow);
        let Inline::Text(classic_run) = &classic.content[0] else {
            panic!("expected classic caption text");
        };
        let Inline::Text(unified_run) = &unified.content[0] else {
            panic!("expected unified caption text");
        };
        assert!(classic_run.style.bold);
        assert!(classic_run.style.italic);
        assert!(!unified_run.style.bold);
        assert!(!unified_run.style.italic);
        assert!(!unified_run.style.emphasis);
        assert!(!unified_run.style.alternate_voice);
        assert!(!unified_run.style.citation);
    }

    #[test]
    fn unified_typesetting_justifies_latin_paragraphs_and_list_items() {
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        for kind in [
            TextBlockKind::Paragraph,
            TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
                marker_visible: true,
            },
        ] {
            let block = TextBlock {
                kind,
                content: vec![Inline::Text(TextRun {
                    text: "Unified prose".into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle {
                    align: TextAlignment::End,
                    ..rebook_publication::BlockStyle::default()
                },
                source: None,
            };

            let resolved = resolve_text_block(&block, &style, TextContext::Flow);
            assert_eq!(resolved.style.align, TextAlignment::Justify);
        }
    }

    #[test]
    fn unified_typesetting_ignores_authored_start_alignment_for_paragraphs() {
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        for (alignment, expected) in [
            (TextAlignment::Start, TextAlignment::Justify),
            (TextAlignment::Center, TextAlignment::Center),
            (TextAlignment::End, TextAlignment::End),
            (TextAlignment::Justify, TextAlignment::Justify),
        ] {
            let block = TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: "作者声明的正文对齐".into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle {
                    align: alignment,
                    authored_alignment: Some(alignment),
                    ..rebook_publication::BlockStyle::default()
                },
                source: None,
            };

            let resolved = resolve_text_block(&block, &style, TextContext::Flow);
            assert_eq!(resolved.style.align, expected);
        }

        let list_item = TextBlock {
            kind: TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
                marker_visible: true,
            },
            content: vec![Inline::Text(TextRun {
                text: "Unified list prose".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                align: TextAlignment::End,
                authored_alignment: Some(TextAlignment::End),
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };

        let resolved = resolve_text_block(&list_item, &style, TextContext::Flow);
        assert_eq!(resolved.style.align, TextAlignment::Justify);
    }

    #[test]
    fn unified_typesetting_justifies_cjk_and_nbsp_paragraphs_but_not_lists() {
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        let cases = [
            (TextBlockKind::Paragraph, "2015年，Dark Reading报道"),
            (
                TextBlockKind::ListItem {
                    ordered: true,
                    ordinal: 1,
                    depth: 0,
                    marker_visible: true,
                },
                "攻击者调查了几个目标",
            ),
            (TextBlockKind::Paragraph, "Dark\u{00a0}Reading report"),
        ];

        for (kind, text) in cases {
            let expected = if kind == TextBlockKind::Paragraph {
                TextAlignment::Justify
            } else {
                TextAlignment::Start
            };
            let block = TextBlock {
                kind,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            };

            let resolved = resolve_text_block(&block, &style, TextContext::Flow);
            assert_eq!(resolved.style.align, expected, "{text}");
        }
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
    fn structural_break_after_survives_unified_and_book_typesetting() {
        let normal = TextBlock {
            kind: TextBlockKind::Blockquote,
            content: vec![Inline::Text(TextRun {
                text: "First stanza".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                margin_after: 6.0,
                line_height: 1.3,
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };
        let mut separated = normal.clone();
        separated.style.hard_break_after = true;
        let mut style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let normal_unified = resolve_text_block(&normal, &style, TextContext::Flow);
        let separated_unified = resolve_text_block(&separated, &style, TextContext::Flow);
        assert!(
            (separated_unified.style.margin_after
                - normal_unified.style.margin_after
                - style.typography.font_size * style.typesetting.body_line_height)
                .abs()
                < 0.001
        );

        style.typesetting.mode = TypesettingMode::Book;
        let normal_book = resolve_text_block(&normal, &style, TextContext::Flow);
        let separated_book = resolve_text_block(&separated, &style, TextContext::Flow);
        assert!(
            (separated_book.style.margin_after
                - normal_book.style.margin_after
                - style.typography.font_size * separated.style.line_height)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn reader_typesetting_normalizes_persisted_values() {
        let mut typesetting = ReaderTypesetting {
            mode: TypesettingMode::Unified,
            line_break_strategy: LineBreakStrategy::Greedy,
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
    fn unified_typesetting_enables_optimized_line_breaking() {
        assert_eq!(
            ReaderTypesetting::default().line_break_strategy,
            LineBreakStrategy::Greedy
        );
        assert_eq!(
            ReaderTypesetting::unified().line_break_strategy,
            LineBreakStrategy::Optimized
        );
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
    fn unified_quotes_clear_authored_bold_and_italic_styles() {
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        for kind in [TextBlockKind::Blockquote, TextBlockKind::QuoteAttribution] {
            let block = TextBlock {
                kind,
                content: vec![Inline::Text(TextRun {
                    text: "Authored emphasis".into(),
                    style: TextStyle {
                        bold: true,
                        italic: true,
                        ..TextStyle::default()
                    },
                    link: None,
                })],
                style: rebook_publication::BlockStyle::default(),
                source: None,
            };

            let resolved = resolve_text_block(&block, &style, TextContext::Flow);
            let Inline::Text(run) = &resolved.content[0] else {
                panic!("expected text run");
            };
            assert!(!run.style.bold);
            assert!(!run.style.italic);
        }
    }

    #[test]
    fn unified_quotes_justify_baseline_alignment_and_preserve_special_alignment() {
        let block = TextBlock {
            kind: TextBlockKind::Blockquote,
            content: vec![Inline::Text(TextRun {
                text: "An indented quotation with baseline alignment".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                margin_start: 60.0,
                indent: 32.0,
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };
        let style = ReaderStyle {
            typography: ReaderTypography {
                font_size: 20.0,
                ..ReaderTypography::default()
            },
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        assert_eq!(resolved.style.align, TextAlignment::Justify);
        assert!((resolved.style.indent - 40.0).abs() < 0.001);
        assert!(resolved.style.margin_start.abs() < 0.001);
        let (start_offset, _, first_line_indent) = resolve_text_measure(&resolved, 320.0, 40.0);
        assert!(start_offset.abs() < 0.001);
        assert!((first_line_indent - 40.0).abs() < 0.001);

        let mut authored_start = block.clone();
        authored_start.style.authored_alignment = Some(TextAlignment::Start);
        let resolved = resolve_text_block(&authored_start, &style, TextContext::Flow);
        assert_eq!(resolved.style.align, TextAlignment::Justify);

        for alignment in [
            TextAlignment::Center,
            TextAlignment::End,
            TextAlignment::Justify,
        ] {
            let mut specially_aligned = block.clone();
            specially_aligned.style.align = alignment;
            specially_aligned.style.authored_alignment = Some(alignment);
            let resolved = resolve_text_block(&specially_aligned, &style, TextContext::Flow);
            assert_eq!(resolved.style.align, alignment);
        }

        let mut unindented = block;
        unindented.style.indent = 0.0;
        let resolved = resolve_text_block(&unindented, &style, TextContext::Flow);
        assert_eq!(resolved.style.align, TextAlignment::Justify);
        assert!(resolved.style.indent.abs() < 0.001);
    }

    #[test]
    fn unified_typesetting_applies_a_consistent_list_indent() {
        let block = TextBlock {
            kind: TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
                marker_visible: true,
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
                marker_visible: true,
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
    fn unified_typesetting_normalizes_markerless_nested_list_items() {
        let block = TextBlock {
            kind: TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 1,
                marker_visible: false,
            },
            content: vec![Inline::Text(TextRun {
                text: "A marker-less nested outline item".into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle {
                margin_start: 75.2,
                margin_start_fraction: 0.0,
                indent: -17.6,
                ..rebook_publication::BlockStyle::default()
            },
            source: None,
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let resolved = resolve_text_block(&block, &style, TextContext::Flow);
        assert!((resolved.style.margin_start - 60.0).abs() < 0.001);
        assert!(resolved.style.margin_start_fraction.abs() < 0.001);
        assert!(resolved.style.indent.abs() < 0.001);
        assert!(list_marker_prefix(resolved.kind).is_empty());
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
    fn reader_typography_uses_bundled_language_defaults_and_builds_cjk_stacks() {
        let typography = ReaderTypography::default();
        assert_eq!(typography.default_font, ReaderDefaultFont::Serif);
        assert_eq!(typography.default_cjk_font, "LXGW WenKai GB Screen");
        assert_eq!(typography.serif_font, "Literata");
        assert_eq!(typography.sans_serif_font, "Arial");
        assert!(typography.other_font.is_empty());
        assert_eq!(
            typography.cjk_default_font,
            Some(ReaderFontChoice {
                category: ReaderDefaultFont::Serif,
                family: "Literata".into(),
            })
        );
        assert!(typography.latin_cjk_font.is_none());
        assert_eq!(typography.monospace_font, "Consolas");
        assert!((typography.font_size - 20.0).abs() < f32::EPSILON);
        assert!((typography.minimum_font_size - 12.0).abs() < f32::EPSILON);
        assert_eq!(typography.font_weight, 400);
        assert!(typography.serif_stack().starts_with("\"Literata\""));
        assert!(typography.serif_stack().contains("\"SimSun\""));
        assert!(typography.serif_stack().ends_with("serif"));
        assert!(
            typography
                .sans_serif_stack()
                .contains("\"Microsoft YaHei\"")
        );
        assert!(typography.sans_serif_stack().ends_with("sans-serif"));
        assert_eq!(
            typography.monospace_stack(),
            "\"Consolas\", \"LXGW WenKai GB Screen\", monospace"
        );
        assert!(!typography.monospace_stack().contains("Fira Code"));
    }

    #[test]
    fn typography_selects_independent_cjk_and_latin_book_stacks() {
        let typography = ReaderTypography {
            default_font: ReaderDefaultFont::Serif,
            default_cjk_font: "CJK Primary".into(),
            serif_font: "Latin Primary".into(),
            cjk_default_font: Some(ReaderFontChoice {
                category: ReaderDefaultFont::SansSerif,
                family: "CJK Western".into(),
            }),
            latin_cjk_font: Some("Latin CJK Fallback".into()),
            ..ReaderTypography::default()
        };

        assert!(
            typography
                .default_stack_for(WritingSystem::Cjk)
                .starts_with("\"CJK Western\", \"CJK Primary\"")
        );
        assert!(
            typography
                .default_stack_for(WritingSystem::Latin)
                .starts_with("\"Latin Primary\", \"Latin CJK Fallback\"")
        );
        assert_eq!(
            typography.default_stack_for(WritingSystem::Unknown),
            typography.default_stack_for(WritingSystem::Latin)
        );
    }

    #[test]
    fn other_font_category_uses_the_selected_family_with_a_safe_fallback() {
        let typography = ReaderTypography {
            default_font: ReaderDefaultFont::Other,
            other_font: "Decorative Reader".into(),
            ..ReaderTypography::default()
        };

        let stack = typography.default_stack_for(WritingSystem::Latin);
        assert!(stack.starts_with("\"Decorative Reader\""));
        assert!(stack.ends_with("sans-serif"));
    }

    #[test]
    fn legacy_ysabeau_defaults_migrate_to_literata() {
        let mut typography = ReaderTypography {
            default_font: ReaderDefaultFont::Other,
            serif_font: "Georgia".into(),
            other_font: " Ysabeau Office ".into(),
            cjk_default_font: Some(ReaderFontChoice {
                category: ReaderDefaultFont::Other,
                family: "Ysabeau Office".into(),
            }),
            ..ReaderTypography::default()
        };

        typography.normalize();

        assert_eq!(typography.default_font, ReaderDefaultFont::Serif);
        assert_eq!(typography.serif_font, "Literata");
        assert!(typography.other_font.is_empty());
        assert_eq!(
            typography.cjk_default_font,
            Some(ReaderFontChoice {
                category: ReaderDefaultFont::Serif,
                family: "Literata".into(),
            })
        );
    }

    #[test]
    fn transitional_literata_other_category_is_canonicalized_as_serif() {
        let mut typography = ReaderTypography {
            default_font: ReaderDefaultFont::Other,
            serif_font: "Georgia".into(),
            other_font: "Literata".into(),
            cjk_default_font: Some(ReaderFontChoice {
                category: ReaderDefaultFont::Other,
                family: "Literata".into(),
            }),
            ..ReaderTypography::default()
        };

        typography.normalize();

        assert_eq!(typography.default_font, ReaderDefaultFont::Serif);
        assert_eq!(typography.serif_font, "Literata");
        assert!(typography.other_font.is_empty());
        assert_eq!(
            typography.cjk_default_font,
            Some(ReaderFontChoice {
                category: ReaderDefaultFont::Serif,
                family: "Literata".into(),
            })
        );
    }

    #[test]
    fn optical_size_tracks_each_span_size_in_typographic_points() {
        assert!((optical_size_for_font(4.0) - 7.0).abs() < f32::EPSILON);
        assert!((optical_size_for_font(20.0) - 15.0).abs() < f32::EPSILON);
        assert!((optical_size_for_font(32.0) - 24.0).abs() < f32::EPSILON);
        assert!((optical_size_for_font(120.0) - 72.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cjk_prose_digits_use_the_configured_western_font() {
        const LITERATA: &[u8] = include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf");
        const CJK: &[u8] = include_bytes!("../../../assets/fonts/LXGWWenKaiGBScreen.ttf");
        let literata = ReaderFontBlob::new(Arc::new(LITERATA));
        let cjk = ReaderFontBlob::new(Arc::new(CJK));
        let mut engine = LayoutEngine::with_fonts([literata, cjk]);
        let text = "卷二，93页14行—94页1—4行";
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: text.into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle::default(),
            source: None,
        };
        let style = ReaderStyle {
            typography: ReaderTypography {
                default_font: ReaderDefaultFont::Other,
                other_font: "Literata".into(),
                cjk_default_font: Some(ReaderFontChoice {
                    category: ReaderDefaultFont::Other,
                    family: "Literata".into(),
                }),
                ..ReaderTypography::default()
            },
            writing_system: WritingSystem::Cjk,
            ..ReaderStyle::default()
        };
        let prepared = engine.shape_text_with_min_width(&block, &style, 800.0, 40.0);
        let runs = prepared
            .layout
            .lines()
            .flat_map(|line| line.items())
            .filter_map(|item| match item {
                PositionedLayoutItem::GlyphRun(glyphs) => Some((
                    text[glyphs.run().text_range()].to_owned(),
                    glyphs.run().font().data.as_ref() == LITERATA,
                )),
                PositionedLayoutItem::InlineBox(_) => None,
            })
            .collect::<Vec<_>>();

        assert!(
            runs.iter()
                .filter(|(run, _)| run.chars().any(|character| character.is_ascii_digit()))
                .all(|(_, uses_literata)| *uses_literata),
            "CJK prose digits did not use Literata: {runs:?}"
        );
    }

    #[test]
    fn literata_defaults_to_lining_figures() {
        use parley::FontFeatures;

        const LITERATA: &[u8] = include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf");
        const LITERATA_ITALIC: &[u8] =
            include_bytes!("../../../assets/fonts/Literata-Italic-opsz-wght.ttf");
        let literata = ReaderFontBlob::new(Arc::new(LITERATA));
        let literata_italic = ReaderFontBlob::new(Arc::new(LITERATA_ITALIC));
        let mut engine = LayoutEngine::with_fonts([literata, literata_italic]);
        let mut glyph_ids = |features: Option<&str>, italic: bool| {
            let mut builder = engine.layout_context.ranged_builder(
                &mut engine.font_context,
                "0123456789",
                1.0,
                false,
            );
            builder.push_default(StyleProperty::FontFamily(FontFamily::from("Literata")));
            builder.push_default(StyleProperty::FontSize(20.0));
            if italic {
                builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
            }
            if let Some(features) = features {
                builder.push_default(StyleProperty::FontFeatures(FontFeatures::from(features)));
            }
            let mut layout: Layout<TextBrush> = builder.build("0123456789");
            layout.break_all_lines(None);
            layout
                .get(0)
                .unwrap()
                .items()
                .flat_map(|item| match item {
                    PositionedLayoutItem::GlyphRun(run) => {
                        run.glyphs().map(|glyph| glyph.id).collect::<Vec<_>>()
                    }
                    PositionedLayoutItem::InlineBox(_) => Vec::new(),
                })
                .collect::<Vec<_>>()
        };

        for italic in [false, true] {
            let default = glyph_ids(None, italic);
            let lining = glyph_ids(Some("\"lnum\""), italic);
            let oldstyle = glyph_ids(Some("\"onum\""), italic);
            assert_eq!(default, lining);
            assert_ne!(default, oldstyle);
        }
    }

    #[test]
    fn reader_typography_normalizes_persisted_values() {
        let mut typography = ReaderTypography {
            default_cjk_font: "  ".into(),
            serif_font: "  Georgia  ".into(),
            sans_serif_font: String::new(),
            cjk_default_font: Some(ReaderFontChoice {
                category: ReaderDefaultFont::Serif,
                family: "  ".into(),
            }),
            latin_cjk_font: Some("  ".into()),
            monospace_font: String::new(),
            font_size: f32::NAN,
            minimum_font_size: -4.0,
            font_weight: 455,
            ..ReaderTypography::default()
        };
        typography.normalize();
        assert_eq!(typography.default_cjk_font, "LXGW WenKai GB Screen");
        assert_eq!(typography.serif_font, "Georgia");
        assert_eq!(typography.sans_serif_font, "Arial");
        assert!(typography.cjk_default_font.is_none());
        assert!(typography.latin_cjk_font.is_none());
        assert_eq!(typography.monospace_font, "Consolas");
        assert!((typography.font_size - 20.0).abs() < f32::EPSILON);
        assert!((typography.minimum_font_size - 1.0).abs() < f32::EPSILON);
        assert_eq!(typography.font_weight, 455);

        typography.default_cjk_font = "LXGW WenKai".into();
        typography.normalize();
        assert_eq!(typography.default_cjk_font, "LXGW WenKai GB Screen");
    }

    #[test]
    fn unavailable_cjk_preference_is_repaired_to_a_validated_family() {
        let families = ReaderFontFamilies {
            all: vec![
                "LXGW WenKai GB Screen".into(),
                "Georgia".into(),
                "Arial".into(),
                "Consolas".into(),
            ],
            chinese: vec!["LXGW WenKai GB Screen".into()],
            serif: vec!["Georgia".into()],
            sans_serif: vec!["Arial".into()],
            monospace: vec!["Consolas".into()],
            ..ReaderFontFamilies::default()
        };
        let mut typography = ReaderTypography {
            default_font: ReaderDefaultFont::Other,
            default_cjk_font: "宋体".into(),
            serif_font: "Unavailable Serif".into(),
            sans_serif_font: "Unavailable Sans".into(),
            other_font: "Unavailable Other".into(),
            cjk_default_font: Some(ReaderFontChoice {
                category: ReaderDefaultFont::Serif,
                family: "Unavailable CJK Western".into(),
            }),
            latin_cjk_font: Some("Unavailable CJK".into()),
            monospace_font: "Unavailable Mono".into(),
            ..ReaderTypography::default()
        };

        assert!(families.repair_typography(&mut typography));
        assert_eq!(typography.default_cjk_font, "LXGW WenKai GB Screen");
        assert_eq!(typography.serif_font, "Georgia");
        assert_eq!(typography.sans_serif_font, "Arial");
        assert_eq!(typography.default_font, ReaderDefaultFont::Serif);
        assert!(typography.other_font.is_empty());
        assert!(typography.cjk_default_font.is_none());
        assert!(typography.latin_cjk_font.is_none());
        assert_eq!(typography.monospace_font, "Consolas");
        assert!(!families.repair_typography(&mut typography));
    }

    #[test]
    fn reader_font_classification_uses_panose_and_fixed_pitch_metadata() {
        let serif = classify_reader_font(Some(&[2, 2, 5, 3, 0, 0, 0, 0, 0, 0]), None, false);
        assert!(serif.serif);
        assert!(!serif.sans_serif);
        assert!(!serif.monospace);

        let sans = classify_reader_font(Some(&[2, 11, 5, 3, 0, 0, 0, 0, 0, 0]), None, false);
        assert!(!sans.serif);
        assert!(sans.sans_serif);
        assert!(!sans.monospace);

        let monospace = classify_reader_font(Some(&[2, 11, 5, 9, 0, 0, 0, 0, 0, 0]), None, false);
        assert!(!monospace.serif);
        assert!(!monospace.sans_serif);
        assert!(monospace.monospace);

        assert!(classify_reader_font(None, None, true).monospace);

        let family_class_serif = classify_reader_font(Some(&[0; 10]), Some(1 << 8), false);
        assert!(family_class_serif.serif);
        let family_class_sans = classify_reader_font(Some(&[0; 10]), Some(8 << 8), false);
        assert!(family_class_sans.sans_serif);
        let unclassified = classify_reader_font(Some(&[0; 10]), Some(10 << 8), false);
        assert!(!unclassified.serif && !unclassified.sans_serif && !unclassified.monospace);

        assert!(infer_reader_font_classification("Sitka Text").serif);
        assert!(infer_reader_font_classification("Segoe UI Variable Text").sans_serif);
        assert!(infer_reader_font_classification("Comic Sans MS").sans_serif);
        assert!(infer_reader_font_classification("Literata").serif);
        assert!(is_symbolic_reader_font("Symbol", Some(&[5; 10]), None));
        assert!(is_symbolic_reader_font("DejaVu Math TeX Gyre", None, None));
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
                marker_visible: true,
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

    #[test]
    fn optimized_english_paragraph_prepares_only_selected_line_hyphens() {
        let block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![Inline::Text(TextRun {
                text: "Extraordinary typographical considerations improve international readability and representation.".into(),
                style: TextStyle {
                    language: rebook_publication::TextLanguage::EnglishUs,
                    ..TextStyle::default()
                },
                link: None,
            })],
            style: BlockStyle {
                align: TextAlignment::Justify,
                ..BlockStyle::default()
            },
            source: None,
        };
        let mut engine = LayoutEngine::new();
        engine.publication_languages = vec!["en-US".into()];
        let reader_style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        let prepared = (100_u16..=240)
            .step_by(5)
            .map(|width| {
                engine.shape_text_with_min_width(&block, &reader_style, f32::from(width), 40.0)
            })
            .find(|prepared| !prepared.hyphens.is_empty())
            .expect("at least one narrow measure should select a dictionary break");

        assert!(prepared.layout.len() > 1);
        assert!(
            prepared
                .hyphens
                .iter()
                .all(|hyphen| hyphen.line_index + 1 < prepared.layout.len())
        );
        assert!(!prepared.text.contains('\u{2010}'));
    }
    use rebook_publication::{
        Book, FixedPageTextReplacement, FixedPageTextReplacementSegment, FixedPageTextSpan,
        ImageBlock, ImageLength, Metadata, PublicationId, PublicationUrl, QuoteBlock,
        RasterResource, RenditionLayout, Resource, SeparatorBlock, SourceAnchor, SpineItemId,
        TableCell, TableRow, TocEntry,
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
    fn selected_hyphens_are_emitted_as_source_free_text_items() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("hyphenation-test").unwrap(),
                metadata: Metadata {
                    languages: vec!["en-US".into()],
                    ..Metadata::default()
                },
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
                content: vec![Inline::Text(TextRun {
                    text: "Extraordinary typographical considerations improve international readability and representation.".into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: None,
            })],
            anchors: Vec::new(),
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            horizontal_margin: 0.0,
            ..ReaderStyle::default()
        };
        let mut engine = LayoutEngine::new();
        let found = (100_u16..=240).step_by(5).find_map(|width| {
            let layout = engine
                .layout_section(
                    &source,
                    &section,
                    LayoutViewport::new(u32::from(width), 800).unwrap(),
                    &style,
                )
                .ok()?;
            layout.pages.iter().find_map(|page| {
                page.items.iter().find_map(|item| match item {
                    PageItem::Text(text) if text.text.as_ref() == "\u{2010}" => Some(text),
                    _ => None,
                })
            })?;
            Some(())
        });

        assert!(found.is_some());
    }

    #[test]
    fn standalone_line_break_adds_spacing_without_forcing_a_page() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("line-break-test").unwrap(),
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
                style: rebook_publication::BlockStyle::default(),
                source: None,
            })
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![paragraph("Before"), Block::LineBreak, paragraph("After")],
            anchors: Vec::new(),
        };
        let mut style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        style.spread = SpreadMode::Scroll;
        let layout = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                LayoutViewport::new(600, 400).unwrap(),
                &style,
            )
            .unwrap();

        assert_eq!(layout.pages.len(), 1);
        let text_origins = layout.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                PageItem::Text(text) => Some(text.origin_y),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(text_origins.len(), 2);
        assert!(text_origins[1] - text_origins[0] > style.typography.font_size * 2.0);
    }

    #[test]
    fn unified_typesetting_filters_body_separators_but_book_typesetting_preserves_them() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("separator-filter-test").unwrap(),
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
                style: rebook_publication::BlockStyle::default(),
                source: None,
            })
        };
        let ornament = ImageBlock {
            href: PublicationUrl::parse("rule.png").unwrap(),
            alt: String::new(),
            style: ImageStyle::default(),
            source: None,
            text_layer: None,
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                paragraph("Before"),
                Block::Separator(SeparatorBlock::spacing(
                    rebook_publication::BlockStyle::default(),
                )),
                Block::Separator(SeparatorBlock::rule()),
                Block::Separator(SeparatorBlock::ornament(ornament)),
                paragraph("After"),
            ],
            anchors: Vec::new(),
        };
        let viewport = LayoutViewport::new(600, 600).unwrap();
        let unified = LayoutEngine::new()
            .layout_section(
                &source,
                &section,
                viewport,
                &ReaderStyle {
                    typesetting: ReaderTypesetting::unified(),
                    ..ReaderStyle::default()
                },
            )
            .unwrap();
        assert!(
            unified
                .pages
                .iter()
                .flat_map(|page| &page.items)
                .all(|item| { !matches!(item, PageItem::Separator(_) | PageItem::Image(_)) })
        );

        let book = LayoutEngine::new()
            .layout_section(&source, &section, viewport, &ReaderStyle::default())
            .unwrap();
        assert!(
            book.pages
                .iter()
                .flat_map(|page| &page.items)
                .any(|item| matches!(item, PageItem::Separator(_)))
        );
        assert!(
            book.pages
                .iter()
                .flat_map(|page| &page.items)
                .any(|item| matches!(item, PageItem::Image(_)))
        );
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
    fn paragraph_margin_starts_after_the_complete_last_line_box() {
        use parley::editing::{Cursor, Selection};
        use parley::layout::Affinity;

        let source = EmptySource {
            book: Book {
                id: PublicationId::new("paragraph-line-box-gap-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let paragraph = |text: &str, margin_after: f32| {
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: text.into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: rebook_publication::BlockStyle {
                    line_height: 2.0,
                    margin_before: 0.0,
                    margin_after,
                    ..rebook_publication::BlockStyle::default()
                },
                source: None,
            })
        };
        let section = Section {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                paragraph("翻译后的中文段落使用不同字体指标。", 12.0),
                paragraph("下一个段落", 0.0),
            ],
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
        let placements = layout.pages[0]
            .items
            .iter()
            .filter_map(|item| match item {
                PageItem::Text(text) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        let first_line = placements[0].layout.get(0).unwrap();
        let first_metrics = first_line.metrics();
        let first_line_box_bottom =
            placements[0].origin_y + first_metrics.block_min_coord + first_metrics.line_height;
        let second_line = placements[1].layout.get(0).unwrap();
        let second_top = placements[1].origin_y + second_line.metrics().block_min_coord;

        assert!((second_top - first_line_box_bottom - 12.0).abs() < 0.001);

        let first_selection = Selection::new(
            Cursor::from_byte_index(&placements[0].layout, 0, Affinity::Downstream),
            Cursor::from_byte_index(
                &placements[0].layout,
                placements[0].text.len(),
                Affinity::Upstream,
            ),
        );
        let first_highlight_bottom = first_selection
            .geometry(&placements[0].layout)
            .into_iter()
            .map(|(rect, _)| rect.y1 as f32 + placements[0].origin_y)
            .fold(f32::NEG_INFINITY, f32::max);
        let second_selection = Selection::new(
            Cursor::from_byte_index(&placements[1].layout, 0, Affinity::Downstream),
            Cursor::from_byte_index(
                &placements[1].layout,
                placements[1].text.len(),
                Affinity::Upstream,
            ),
        );
        let second_highlight_top = second_selection
            .geometry(&placements[1].layout)
            .into_iter()
            .map(|(rect, _)| rect.y0 as f32 + placements[1].origin_y)
            .fold(f32::INFINITY, f32::min);

        assert!((second_highlight_top - first_highlight_bottom - 12.0).abs() < 0.001);
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
                PageItem::Quote(_)
                | PageItem::Table(_)
                | PageItem::Image(_)
                | PageItem::Separator(_) => None,
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
                PageItem::Quote(_) => {}
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
                PageItem::Quote(_)
                | PageItem::Table(_)
                | PageItem::Image(_)
                | PageItem::Separator(_) => None,
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
    fn inferred_adjacent_caption_is_grouped_only_by_unified_typesetting() {
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("inferred-caption-test").unwrap(),
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
                Block::Image(ImageBlock {
                    href: PublicationUrl::parse("images/figure.png").unwrap(),
                    alt: "Figure".into(),
                    style: ImageStyle::default(),
                    source: None,
                    text_layer: None,
                }),
                Block::Text(TextBlock {
                    kind: TextBlockKind::Caption,
                    content: vec![Inline::Text(TextRun {
                        text: "Figure 1. A leaf.".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: None,
                }),
            ],
            anchors: Vec::new(),
        };
        let classic_style = ReaderStyle::default();
        let unified_style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };
        let viewport = LayoutViewport::new(400, 500).unwrap();

        let classic = LayoutEngine::new()
            .layout_section(&source, &section, viewport, &classic_style)
            .unwrap();
        let unified = LayoutEngine::new()
            .layout_section(&source, &section, viewport, &unified_style)
            .unwrap();
        let [PageItem::Image(_), PageItem::Text(classic_caption)] =
            classic.pages[0].items.as_slice()
        else {
            panic!("classic typesetting should preserve the two authored blocks");
        };
        let [
            PageItem::Image(unified_image),
            PageItem::Text(unified_caption),
        ] = unified.pages[0].items.as_slice()
        else {
            panic!("unified typesetting should lay out one inferred figure and caption");
        };
        let classic_line = classic_caption
            .layout
            .get(classic_caption.lines.start)
            .unwrap();
        let unified_line = unified_caption
            .layout
            .get(unified_caption.lines.start)
            .unwrap();
        assert!(classic_line.metrics().offset.abs() < 0.001);
        assert!(unified_line.metrics().offset > 0.0);
        let unified_caption_top = unified_caption.origin_y + unified_line.metrics().block_min_coord;
        let expected_gap =
            unified_style.typography.font_size * unified_style.typesetting.caption_gap_em;
        assert!(
            (unified_caption_top - (unified_image.y + unified_image.height) - expected_gap).abs()
                < 0.01
        );
    }

    #[test]
    fn unified_figure_captions_center_one_line_and_left_align_multiple_lines() {
        let caption = |text: &str| TextBlock {
            kind: TextBlockKind::Caption,
            content: vec![Inline::Text(TextRun {
                text: text.into(),
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
        let mut engine = LayoutEngine::new();

        let short = caption("Figure 1. A leaf.");
        let (short, _) = engine.shape_figure_caption(&short, &style, 320.0, true);
        assert_eq!(short.layout.len(), 1);
        assert!(
            short
                .layout
                .get(0)
                .is_some_and(|line| line.metrics().offset > 0.0),
            "single-line caption should be centered"
        );

        let long = caption(
            "Figure 2. A deliberately long caption that wraps across several lines at this width.",
        );
        let (long, _) = engine.shape_figure_caption(&long, &style, 180.0, true);
        assert!(long.layout.len() > 1);
        assert!(
            (0..long.layout.len()).all(|index| long.layout.get(index).is_some_and(|line| line
                .metrics()
                .offset
                .abs()
                < 0.01)),
            "multi-line caption should be left aligned"
        );
        assert!(
            long.layout
                .lines()
                .take(long.layout.len().saturating_sub(1))
                .all(|line| (line.metrics().inline_max_coord - 180.0).abs() < 0.01),
            "multi-line caption should use optimized full-measure breaks"
        );
        assert!(
            long.layout
                .lines()
                .take(long.layout.len().saturating_sub(1))
                .all(|line| linebreak::parley::positioned_line_content_end(line) >= 179.0),
            "optimized caption lines should visually fill the shared measure"
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
    fn authored_spacing_that_crosses_a_page_is_restored_in_continuous_layout() {
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

        paginator.add_preserved_spacing(15.0);
        paginator.add_preserved_spacing(5.0);
        paginator.push_image_with_gaps(
            RasterImage {
                width: 100,
                height: 50,
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
        assert!((pages[1].leading_gap - 20.0).abs() < 0.001);
    }

    #[test]
    fn trailing_block_spacing_survives_a_later_fit_page_break() {
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
            10.0,
        );
        paginator.push_image_with_gaps(
            RasterImage {
                width: 100,
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
        assert!((pages[1].leading_gap - 10.0).abs() < 0.001);
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

    #[test]
    fn unified_quote_is_one_padded_card_with_right_aligned_attribution() {
        let spine = SpineItemId::new("chapter").unwrap();
        let range = |node: &str, length: u64| SourceRange {
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
        let text = |kind, value: &str, source| TextBlock {
            kind,
            content: vec![Inline::Text(TextRun {
                text: value.into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: Some(source),
        };
        let body_range = range("quote-body", 18);
        let attribution_range = range("quote-source", 12);
        let quote_range = SourceRange {
            start: body_range.start.clone(),
            end: attribution_range.end.clone(),
        };
        let section = Section {
            id: spine.clone(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Quote(QuoteBlock {
                body: vec![text(
                    TextBlockKind::Blockquote,
                    "A structural quote.",
                    body_range,
                )],
                attribution: Some(text(
                    TextBlockKind::QuoteAttribution,
                    "The source",
                    attribution_range,
                )),
                source: Some(quote_range),
            })],
            anchors: Vec::new(),
        };
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("quote-layout-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
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
        let page = &layout.pages[0];
        let quote = page
            .items
            .iter()
            .find_map(|item| match item {
                PageItem::Quote(quote) => Some(quote),
                _ => None,
            })
            .expect("quote card should be positioned");
        let texts = page
            .items
            .iter()
            .filter_map(|item| match item {
                PageItem::Text(text) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(texts.len(), 2);
        assert_eq!(quote.sources.len(), 2);
        let expected_quote_padding = style.typography.font_size
            * paragraph_indent_em(&style.typesetting, style.writing_system);
        assert!(texts[0].origin_x >= quote.x + expected_quote_padding);
        assert!(quote.height > QUOTE_VERTICAL_PADDING * 2.0);
        assert!(
            texts[1]
                .layout
                .get(0)
                .is_some_and(|line| line.metrics().offset > 0.0),
            "attribution should align to the inline end"
        );

        let solo_range = range("quote-without-source", 26);
        let solo_section = Section {
            id: spine.clone(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![Block::Quote(QuoteBlock {
                body: vec![text(
                    TextBlockKind::Blockquote,
                    "A quotation without a source.",
                    solo_range.clone(),
                )],
                attribution: None,
                source: Some(solo_range),
            })],
            anchors: Vec::new(),
        };
        let solo_layout = LayoutEngine::new()
            .layout_section(
                &source,
                &solo_section,
                LayoutViewport::new(420, 360).unwrap(),
                &style,
            )
            .unwrap();
        let solo_page = &solo_layout.pages[0];
        let solo_quote = solo_page
            .items
            .iter()
            .find_map(|item| match item {
                PageItem::Quote(quote) => Some(quote),
                _ => None,
            })
            .expect("source-free quote card should be positioned");
        let solo_text = solo_page
            .items
            .iter()
            .find_map(|item| match item {
                PageItem::Text(text) => Some(text),
                _ => None,
            })
            .expect("source-free quote body should be positioned");
        let last_line = solo_text
            .lines
            .end
            .checked_sub(1)
            .and_then(|index| solo_text.layout.get(index))
            .expect("source-free quote should have a visible line");
        let metrics = last_line.metrics();
        let text_bottom = solo_text.origin_y
            + metrics
                .block_max_coord
                .max(metrics.block_min_coord + metrics.line_height);
        let bottom_padding = solo_quote.y + solo_quote.height - text_bottom;
        assert!(
            (bottom_padding - QUOTE_VERTICAL_PADDING).abs() < 0.01,
            "source-free quote should have one bottom padding, got {bottom_padding}"
        );

        let before_range = range("before-quote", 13);
        let balanced_quote_range = range("balanced-quote", 17);
        let after_range = range("after-quote", 12);
        let balanced_section = Section {
            id: solo_section.id.clone(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                Block::Text(text(
                    TextBlockKind::Paragraph,
                    "Before quote.",
                    before_range,
                )),
                Block::Quote(QuoteBlock {
                    body: vec![text(
                        TextBlockKind::Blockquote,
                        "Balanced quotation.",
                        balanced_quote_range.clone(),
                    )],
                    attribution: None,
                    source: Some(balanced_quote_range),
                }),
                Block::Text(text(TextBlockKind::Paragraph, "After quote.", after_range)),
            ],
            anchors: Vec::new(),
        };
        let balanced_layout = LayoutEngine::new()
            .layout_section(
                &source,
                &balanced_section,
                LayoutViewport::new(420, 360).unwrap(),
                &style,
            )
            .unwrap();
        let balanced_page = &balanced_layout.pages[0];
        let balanced_quote = balanced_page
            .items
            .iter()
            .find_map(|item| match item {
                PageItem::Quote(quote) => Some(quote),
                _ => None,
            })
            .expect("balanced quote card should be positioned");
        let balanced_texts = balanced_page
            .items
            .iter()
            .filter_map(|item| match item {
                PageItem::Text(text) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(balanced_texts.len(), 3);
        let text_top = |text: &TextPlacement| {
            let first = text
                .layout
                .get(text.lines.start)
                .expect("text placement should have a first line");
            text.origin_y + first.metrics().block_min_coord
        };
        let text_bottom = |text: &TextPlacement| {
            let last = text
                .lines
                .end
                .checked_sub(1)
                .and_then(|index| text.layout.get(index))
                .expect("text placement should have a last line");
            let metrics = last.metrics();
            text.origin_y
                + metrics
                    .block_max_coord
                    .max(metrics.block_min_coord + metrics.line_height)
        };
        let margin_before = balanced_quote.y - text_bottom(balanced_texts[0]);
        let margin_after = text_top(balanced_texts[2]) - (balanced_quote.y + balanced_quote.height);
        assert!(
            (margin_before - margin_after).abs() < 0.01,
            "quote margins should be symmetric, got {margin_before} before and {margin_after} after"
        );
    }

    #[test]
    fn unified_short_quote_keeps_its_attribution_on_the_same_page() {
        let spine = SpineItemId::new("chapter").unwrap();
        let range = |node: &str, length: u64| SourceRange {
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
        let text = |kind, value: &str, source| TextBlock {
            kind,
            content: vec![Inline::Text(TextRun {
                text: value.into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: Some(source),
        };
        // Nine quote-owned rules leave enough room for the quote body but not its
        // attribution. The complete short quote should move as one unit.
        let mut blocks = vec![Block::Separator(SeparatorBlock::rule_in_quote()); 9];
        blocks.push(Block::Quote(QuoteBlock {
            body: vec![text(
                TextBlockKind::Blockquote,
                "Reading entails intense mental activity; thoughtful readers pause and reflect.",
                range("quote-body", 79),
            )],
            attribution: Some(text(
                TextBlockKind::QuoteAttribution,
                "Mortimer Adler",
                range("quote-source", 14),
            )),
            source: None,
        }));
        let section = Section {
            id: spine,
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks,
            anchors: Vec::new(),
        };
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("quote-keep-together-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
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
        let page_for = |node: &str| {
            layout.pages.iter().position(|page| {
                page.items.iter().any(|item| {
                    let PageItem::Text(text) = item else {
                        return false;
                    };
                    text.source
                        .as_ref()
                        .is_some_and(|source| source.start.node == node)
                })
            })
        };
        let body_page = page_for("quote-body").expect("quote body should be laid out");
        let source_page = page_for("quote-source").expect("quote attribution should be laid out");
        assert_eq!(body_page, source_page);
        assert!(body_page > 0, "awkward remainder should move the quote");
    }

    #[test]
    fn unified_quote_never_leaves_an_orphaned_accent_bar_at_a_page_boundary() {
        let spine = SpineItemId::new("chapter").unwrap();
        let range = |node: &str, length: u64| SourceRange {
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
        let text = |kind, value: String, source| TextBlock {
            kind,
            content: vec![Inline::Text(TextRun {
                text: value,
                style: TextStyle::default(),
                link: None,
            })],
            style: rebook_publication::BlockStyle::default(),
            source: Some(source),
        };
        let section = Section {
            id: spine.clone(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            blocks: vec![
                Block::Text(text(
                    TextBlockKind::Paragraph,
                    "A preceding paragraph fills the page before the quotation. ".repeat(18),
                    range("preceding", 1_026),
                )),
                Block::Quote(QuoteBlock {
                    body: vec![text(
                        TextBlockKind::Blockquote,
                        "The quotation must begin together with its accent bar.".into(),
                        range("quote-body", 55),
                    )],
                    attribution: Some(text(
                        TextBlockKind::QuoteAttribution,
                        "The source".into(),
                        range("quote-source", 10),
                    )),
                    source: None,
                }),
            ],
            anchors: Vec::new(),
        };
        let source = EmptySource {
            book: Book {
                id: PublicationId::new("quote-boundary-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
        };
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        };

        for height in (220..=420).step_by(8) {
            let layout = LayoutEngine::new()
                .layout_section(
                    &source,
                    &section,
                    LayoutViewport::new(420, height).unwrap(),
                    &style,
                )
                .unwrap();
            for page in &layout.pages {
                if !page
                    .items
                    .iter()
                    .any(|item| matches!(item, PageItem::Quote(_)))
                {
                    continue;
                }
                let has_quote_text = page.items.iter().any(|item| {
                    let PageItem::Text(text) = item else {
                        return false;
                    };
                    text.source.as_ref().is_some_and(|source| {
                        matches!(source.start.node.as_str(), "quote-body" | "quote-source")
                    })
                });
                assert!(
                    has_quote_text,
                    "viewport height {height} produced a quote decoration without quote text"
                );
            }
        }
    }
}
