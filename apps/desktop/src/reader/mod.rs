use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use peniko::{Blob, Color};
use rebook_formats::{BookFormat, open_file_for_reading as open_publication_file_for_reading};
use rebook_layout::{LayoutViewport, ReaderStyle, SpreadMode};
use rebook_publication::{
    Block, BookSource, Inline, PublicationUrl, RenditionLayout, Rgba, SourceAnchor, SourceRange,
    TableOfContentsOrigin, TextBaseline, TextBlock, TextBlockKind,
};
use rebook_reader::{
    PageDirection, ReaderPosition, ReaderSectionPage, ReaderSelection, ReaderSession,
    ReaderSnapshot, ReaderTextHit, SelectionGranularity,
};
use rebook_renderer::PageQuoteBridge;

use crate::async_task::{TaskResult, TaskSlot};
use crate::generated_toc::GeneratedTocDraft;
use crate::highlights::{HighlightStore, StoredHighlight};
use crate::library::LibraryBook;
use crate::plugins::{
    BlockTranslation, BookSearchResult, ChatReadingContext, ChatRequestKind, ChatResponse,
    ChatTurn, PdfOcrSourceController, PdfOcrViewMode, PluginSettings, RewriteBookSource,
    TranslationBlockInput, TranslationBookSource, has_pending_pdf_ocr_task, load_pdf_ocr_source,
};
use crate::preferences::{self, AppLanguage, ReaderPreferences, ReadingMode, ShortcutPreferences};
use crate::settings::ReaderSettingsChange;
use crate::sync::{SyncSettings, SyncStore};

const INITIAL_WIDTH: u32 = 1200;
const INITIAL_HEIGHT: u32 = 800;
const MOTION_DURATION: Duration = Duration::from_millis(180);
const TOOLBAR_MOTION_DURATION: Duration = Duration::from_millis(200);
const FOCUS_SCROLL_POINTS_PER_SECOND: f32 = 1_000.0;
const FOCUS_SCROLL_MIN_DURATION: Duration = Duration::from_millis(160);
const FOCUS_SCROLL_MAX_DURATION: Duration = Duration::from_millis(360);
const FOCUS_UNIT_MIN_HEIGHT: f32 = 240.0;
const TOOLBAR_HIDE_DELAY: Duration = Duration::from_millis(500);
const NOTICE_AUTO_DISMISS_DELAY: Duration = Duration::from_secs(3);
const MOTION_EPSILON: f32 = 0.001;
const SEARCH_MARK_COLOR: Color = Color::from_rgba8(250, 204, 21, 89);
const ASSISTANT_MARK_COLOR: Color = Color::from_rgba8(245, 158, 11, 56);
const FOCUS_MINIMUM_PARAGRAPH_GAP: f32 = 12.0;
const FOCUS_TABLE_BOTTOM_MARGIN: f32 = 24.0;
static NEXT_SCENE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_CHAT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

mod assistant;
mod chat_autocomplete;
mod chat_markdown;
mod egui_view;
mod interaction;
mod navigation;
pub(super) mod render;
mod settings_controller;
mod ui_controller;

use chat_autocomplete::ChatReference;
pub(crate) use egui_view::{ReaderFramePlan, ReaderPageTexture};
pub(crate) use render::ReaderScene;
use render::{PageSceneKey, PageSceneLayers};

// Reader page colors follow the app theme; the light pair matches
// ReaderStyle::default so existing books keep their warm paper look.
fn apply_theme_colors(style: &mut ReaderStyle, theme: egui::Theme) {
    match theme {
        egui::Theme::Light => {
            style.foreground = Rgba::BLACK;
            style.background = Rgba {
                red: 250,
                green: 248,
                blue: 243,
                alpha: 255,
            };
        }
        egui::Theme::Dark => {
            style.foreground = Rgba {
                red: 210,
                green: 207,
                blue: 200,
                alpha: 255,
            };
            style.background = Rgba {
                red: 24,
                green: 23,
                blue: 21,
                alpha: 255,
            };
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "reader construction keeps source wrappers and persisted state restoration together"
)]
pub(super) fn open_reader(
    path: &Path,
    reader_fonts: Arc<[Blob<u8>]>,
    shelf_metadata: Option<BookDisplayMetadata>,
    shelf_cover: Option<Vec<u8>>,
    local_store: SyncStore,
) -> Result<DesktopReader, Box<dyn std::error::Error + Send + Sync>> {
    let started = Instant::now();
    let publication_started = Instant::now();
    let known_publication_id = shelf_metadata.as_ref().map(|metadata| metadata.id.as_str());
    let publication = open_publication_file_for_reading(path, known_publication_id)?;
    let publication_ms = publication_started.elapsed().as_secs_f32() * 1_000.0;
    let format = publication.format();
    let cover = shelf_cover.or_else(|| publication.cover_bytes().map(<[u8]>::to_vec));
    let canonical_source = publication.source();
    let book_id = canonical_source.book().id.to_string();
    let source_title_missing = canonical_source.book().metadata.title.trim().is_empty();
    let source_authors_missing = canonical_source
        .book()
        .metadata
        .authors
        .iter()
        .all(|author| author.trim().is_empty());
    let cached_pdf_metadata = if format == BookFormat::Pdf {
        crate::generated_metadata::load(&book_id).unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load generated PDF metadata");
            None
        })
    } else {
        None
    };
    let mut display_metadata = resolve_book_display_metadata(
        shelf_metadata,
        &book_id,
        &canonical_source.book().metadata.title,
        &canonical_source.book().metadata.authors,
    );
    if let Some(metadata) = cached_pdf_metadata.as_ref() {
        if source_title_missing && !metadata.title.is_empty() {
            display_metadata.title.clone_from(&metadata.title);
        }
        if source_authors_missing && !metadata.authors.is_empty() {
            display_metadata.authors.clone_from(&metadata.authors);
        }
    }
    let pdf_title_missing = source_title_missing
        && cached_pdf_metadata
            .as_ref()
            .is_none_or(|metadata| metadata.title.is_empty());
    let pdf_authors_missing = source_authors_missing
        && cached_pdf_metadata
            .as_ref()
            .is_none_or(|metadata| metadata.authors.is_empty());
    let canonical_source = if format == BookFormat::Pdf {
        crate::generated_toc::load_source(Arc::clone(&canonical_source)).unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load generated PDF table of contents");
            canonical_source
        })
    } else {
        canonical_source
    };
    let plugin_settings = PluginSettings::load_default().unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to load plugin settings; using defaults");
        PluginSettings::default()
    });
    let source_wrappers_started = Instant::now();
    let (canonical_source, pdf_ocr_controller, pdf_ocr_available, pdf_ocr_mode) =
        if format == BookFormat::Pdf {
            match load_pdf_ocr_source(
                Arc::clone(&canonical_source),
                plugin_settings.pdf_ocr_reflow_enabled,
            ) {
                Ok(loaded) => (
                    loaded.source,
                    loaded.controller,
                    loaded.available,
                    loaded.mode,
                ),
                Err(error) => {
                    tracing::warn!(%error, "failed to load cached PDF OCR result");
                    (canonical_source, None, false, PdfOcrViewMode::Original)
                }
            }
        } else {
            (canonical_source, None, false, PdfOcrViewMode::Original)
        };
    let source_wrappers_ms = source_wrappers_started.elapsed().as_secs_f32() * 1_000.0;
    let fixed_page = canonical_source.book().metadata.layout == RenditionLayout::PrePaginated;
    let rewrite_source = Arc::new(RewriteBookSource::new(canonical_source));
    let translation_source = Arc::new(if fixed_page {
        TranslationBookSource::new_fixed_page(
            rewrite_source.clone(),
            plugin_settings.translation_mode,
        )
    } else {
        TranslationBookSource::new(rewrite_source.clone(), plugin_settings.translation_mode)
    });
    let source: Arc<dyn BookSource> = translation_source.clone();
    let mut highlight_store = HighlightStore::from_repository(local_store.clone());
    let mut highlights = highlight_store.for_book(&book_id);
    let viewport = LayoutViewport::new(INITIAL_WIDTH, INITIAL_HEIGHT)?;
    let reader_preferences = preferences::load_reader_preferences().unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to load reader preferences; using defaults");
        ReaderPreferences::default()
    });
    if reader_preferences.selection_granularity == SelectionGranularity::Paragraph {
        repair_legacy_translated_paragraph_highlights(
            rewrite_source.as_ref(),
            &mut highlights,
            &mut highlight_store,
        );
    }
    let reading_mode = allowed_reading_mode(format, pdf_ocr_mode, reader_preferences.reading_mode);
    let mut style = ReaderStyle {
        spread: if reading_mode == ReadingMode::Focus {
            SpreadMode::Scroll
        } else {
            reader_preferences.spread
        },
        focus_footnote_icons: reading_mode == ReadingMode::Focus,
        typography: reader_preferences.typography.clone(),
        typesetting: reader_preferences.typesetting.clone(),
        ..ReaderStyle::default()
    };
    if reading_mode == ReadingMode::Focus
        && reader_preferences.typesetting.mode == rebook_layout::TypesettingMode::Book
    {
        style.minimum_paragraph_gap = FOCUS_MINIMUM_PARAGRAPH_GAP;
    }
    if fixed_page {
        style.column_gap = 0.0;
    }
    apply_theme_colors(&mut style, crate::ui::theme());
    let sync_settings = SyncSettings::load_default().unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to load WebDAV settings; using defaults");
        SyncSettings::new_device()
    });
    let sync_password = sync_settings.load_password().unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to load WebDAV credential");
        String::new()
    });
    let progress_store = Some(local_store);
    let progress_started = Instant::now();
    let stored_progress = progress_store
        .as_ref()
        .map(|store| store.load_progress(&book_id))
        .transpose()?
        .flatten();
    let mut restored_source_range = stored_progress
        .as_ref()
        .and_then(|progress| progress.locator.source.clone());
    let progress_ms = progress_started.elapsed().as_secs_f32() * 1_000.0;
    let resumed = stored_progress.is_some();
    let initial_layout_started = Instant::now();
    let reader = if let Some(progress) = stored_progress {
        match ReaderSession::open_with_fonts_at_locator(
            Arc::clone(&source),
            viewport,
            style.clone(),
            Arc::clone(&reader_fonts),
            &progress.locator,
        ) {
            Ok(reader) => reader,
            Err(error) => {
                tracing::warn!(%error, "failed to open at durable reading locator");
                restored_source_range = None;
                ReaderSession::open_with_fonts(Arc::clone(&source), viewport, style, reader_fonts)?
            }
        }
    } else {
        ReaderSession::open_with_fonts(Arc::clone(&source), viewport, style, reader_fonts)?
    };
    let initial_layout_ms = initial_layout_started.elapsed().as_secs_f32() * 1_000.0;
    let initial_location = reader.location();
    crate::diagnostics::log(
        "reader.open",
        &[
            crate::diagnostics::Field::Text("format", format.label()),
            crate::diagnostics::Field::F32("publication_ms", publication_ms),
            crate::diagnostics::Field::F32("source_wrappers_ms", source_wrappers_ms),
            crate::diagnostics::Field::F32("progress_ms", progress_ms),
            crate::diagnostics::Field::Bool("resumed", resumed),
            crate::diagnostics::Field::F32("initial_layout_ms", initial_layout_ms),
            crate::diagnostics::Field::Usize("section", initial_location.section_index),
            crate::diagnostics::Field::Usize("segment", initial_location.segment_index),
            crate::diagnostics::Field::Usize("page", initial_location.page_index),
            crate::diagnostics::Field::Usize("page_count", initial_location.page_count),
            crate::diagnostics::Field::F32("total_ms", started.elapsed().as_secs_f32() * 1_000.0),
        ],
    );
    tracing::debug!(
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        "opened book"
    );
    Ok(DesktopReader::new(
        reader,
        DesktopReaderResources {
            source,
            rewrite_source,
            translation_source,
            pdf_ocr_controller,
            pdf_ocr_available,
            pdf_ocr_mode,
            cover,
            format,
            book_id,
            display_metadata,
            pdf_metadata_missing: PdfMetadataMissing {
                title: pdf_title_missing,
                authors: pdf_authors_missing,
            },
            highlight_store,
            highlights,
            progress_store,
            restored_source_range,
            plugin_settings,
            language: reader_preferences.language,
            reading_mode,
            selection_granularity: reader_preferences.selection_granularity,
            shortcuts: reader_preferences.shortcuts,
            sync_settings,
            sync_password,
            source_path: path.to_path_buf(),
        },
    ))
}

fn repair_legacy_translated_paragraph_highlights(
    source: &dyn BookSource,
    highlights: &mut [StoredHighlight],
    store: &mut HighlightStore,
) {
    for highlight in highlights {
        let [range] = highlight.ranges.as_slice() else {
            continue;
        };
        let Some(section_index) = source
            .book()
            .sections
            .iter()
            .position(|item| item.id == range.start.spine)
        else {
            continue;
        };
        let Ok(section) = source.parse_section(section_index) else {
            continue;
        };
        let Some(repaired) =
            legacy_translated_paragraph_range(&section.blocks, range, &highlight.quote)
        else {
            continue;
        };
        highlight.ranges = vec![repaired];
        if let Err(error) = store.update(highlight) {
            tracing::warn!(%error, id = %highlight.id, "failed to repair legacy translated highlight");
        }
    }
}

fn legacy_translated_paragraph_range(
    blocks: &[Block],
    range: &SourceRange,
    quote: &str,
) -> Option<SourceRange> {
    let matches_range = |source: &&SourceRange| {
        source.start == range.start
            && source.start.spine == source.end.spine
            && source.start.node == source.end.node
            && range.start.spine == range.end.spine
            && range.start.node == range.end.node
    };
    let (canonical_range, canonical_text) = blocks.iter().find_map(|block| match block {
        Block::Text(block) => block
            .source
            .as_ref()
            .filter(matches_range)
            .map(|source| (source, crate::plugins::text_block_text(block))),
        Block::Quote(quote) => {
            quote
                .body
                .iter()
                .chain(quote.attribution.iter())
                .find_map(|block| {
                    block
                        .source
                        .as_ref()
                        .filter(matches_range)
                        .map(|source| (source, crate::plugins::text_block_text(block)))
                })
        }
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .find_map(|cell| {
                cell.text
                    .source
                    .as_ref()
                    .filter(matches_range)
                    .map(|source| (source, crate::plugins::text_block_text(&cell.text)))
            }),
        Block::Figure(figure) => figure.captions.iter().find_map(|caption| {
            caption
                .source
                .as_ref()
                .filter(matches_range)
                .map(|source| (source, crate::plugins::text_block_text(caption)))
        }),
        Block::Image(_) | Block::Separator | Block::LineBreak | Block::PageBreak => None,
    })?;
    let stored_length = range.end.text_offset.checked_sub(range.start.text_offset)?;
    let canonical_length = canonical_range
        .end
        .text_offset
        .checked_sub(canonical_range.start.text_offset)?;
    if stored_length == 0
        || stored_length >= canonical_length
        || usize::try_from(stored_length).ok()? != quote.chars().count()
    {
        return None;
    }
    let canonical_prefix = canonical_text
        .chars()
        .take(usize::try_from(stored_length).ok()?)
        .collect::<String>();
    (canonical_prefix != quote).then(|| canonical_range.clone())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BookDisplayMetadata {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) authors: Vec<String>,
}

impl From<&LibraryBook> for BookDisplayMetadata {
    fn from(book: &LibraryBook) -> Self {
        Self {
            id: book.id.clone(),
            title: book.title.clone(),
            authors: book.authors.clone(),
        }
    }
}

fn resolve_book_display_metadata(
    shelf_metadata: Option<BookDisplayMetadata>,
    parsed_id: &str,
    parsed_title: &str,
    parsed_authors: &[String],
) -> BookDisplayMetadata {
    shelf_metadata.unwrap_or_else(|| BookDisplayMetadata {
        id: parsed_id.to_owned(),
        title: parsed_title.to_owned(),
        authors: parsed_authors.to_vec(),
    })
}

pub(super) struct DesktopReader {
    reader: ReaderSession,
    source: Arc<dyn BookSource>,
    rewrite_source: Arc<RewriteBookSource>,
    translation_source: Arc<TranslationBookSource>,
    pdf_ocr_controller: Option<Arc<PdfOcrSourceController>>,
    snapshot: ReaderSnapshot,
    cover: Option<Vec<u8>>,
    cover_texture: Option<egui::TextureHandle>,
    format: BookFormat,
    book_id: String,
    display_metadata: BookDisplayMetadata,
    pdf_metadata_missing: PdfMetadataMissing,
    highlight_store: HighlightStore,
    highlights: Vec<StoredHighlight>,
    progress_store: Option<SyncStore>,
    selection_anchor: Option<ReaderTextHit>,
    selection: Option<ReaderSelection>,
    selection_granularity: SelectionGranularity,
    selection_toolbar_visible: bool,
    selected_image: Option<SelectedImage>,
    image_pointer_state: ImagePointerState,
    image_preview: Option<ImagePreview>,
    annotation_note_draft: Option<AnnotationDraft>,
    selected_highlight_id: Option<String>,
    focused_mark: Option<FocusedMark>,
    plugin_settings: PluginSettings,
    language: AppLanguage,
    reading_mode: ReadingMode,
    shortcuts: ShortcutPreferences,
    sync_settings: SyncSettings,
    sync_password: String,
    search: SearchUiState,
    chat: ChatUiState,
    book_chat: Option<ChatUiState>,
    focus_chat_sessions: HashMap<String, ChatUiState>,
    focus_chat_session_key: Option<String>,
    chat_markdown: chat_markdown::ChatMarkdownState,
    translation: TranslationUiState,
    pdf_toc: PdfTocUiState,
    pdf_ocr: PdfOcrUiState,
    ui: ReaderUiState,
    canvas_size: Option<(u32, u32)>,
    scene_id: u64,
    scene_revision: u64,
    page_scenes: HashMap<PageSceneKey, Arc<PageSceneLayers>>,
    page_scene_lru: VecDeque<PageSceneKey>,
    scroll_section: Option<Arc<ScrollSectionLayout>>,
    scroll_viewport: Option<ScrollViewportState>,
    scroll_target_position: Option<ReaderPosition>,
    scroll_target_source: Option<SourceRange>,
    focus_units: Vec<FocusUnit>,
    focus_unit_index: usize,
    focus_target_offset: Option<f32>,
    focus_anchor: Option<SourceAnchor>,
    focus_toc_override: Option<String>,
    pending_page_turn: Option<PageDirection>,
    pending_reading_unit_entry: Option<PageDirection>,
    pending_focus_wheel_turn: Option<PageDirection>,
    pending_keyboard_scroll_delta: f32,
    settings_requested: bool,
    settings_change_requested: Option<ReaderSettingsChange>,
    notice: Option<String>,
    notice_timer: TransientMessageTimer,
    error: Option<String>,
    error_timer: TransientMessageTimer,
    source_path: PathBuf,
    reopen_requested: Option<PathBuf>,
    reopen_notice: Option<String>,
    reopen_error: Option<String>,
    pub(super) exit_requested: bool,
}

struct ImagePreview {
    texture: egui::TextureHandle,
    image: egui::ColorImage,
    source_size: egui::Vec2,
    zoom: f32,
    pan: egui::Vec2,
}

struct ImagePressCandidate {
    started_at: Instant,
    origin: egui::Pos2,
    image: rebook_reader::ReaderImage,
    scroll_mode: bool,
}

enum ImagePointerState {
    Idle,
    Press(ImagePressCandidate),
    SuppressNextClick,
}

struct SelectedImage {
    image: egui::ColorImage,
    position: ReaderPosition,
    bounds: egui::Rect,
    scroll_mode: bool,
}

struct ScrollSectionLayout {
    section_index: usize,
    reading_unit_index: usize,
    pages: Vec<ReaderSectionPage>,
    page_tops: Vec<f32>,
    page_origins: Vec<f32>,
    page_heights: Vec<f32>,
    quote_bridges: Vec<ScrollQuoteBridge>,
    content_height: f32,
}

#[derive(Clone, Copy)]
struct ScrollQuoteBridge {
    top: f32,
    bottom: f32,
    style: PageQuoteBridge,
}

#[derive(Clone)]
struct FocusUnit {
    range: SourceRange,
    paint_ranges: Vec<SourceRange>,
    text: String,
    position: ReaderPosition,
    rect: egui::Rect,
    is_image: bool,
    is_table: bool,
    rectangular_activation: bool,
    rectangular_activation_rect: Option<egui::Rect>,
    footnotes: Vec<FocusFootnote>,
}

#[derive(Clone)]
struct FocusFootnote {
    text: String,
}

fn merge_focus_list_descendant(root: &mut FocusUnit, descendant: FocusUnit) {
    root.range.end = descendant.range.end;
    root.paint_ranges.extend(descendant.paint_ranges);
    if !descendant.text.trim().is_empty() {
        if !root.text.is_empty() {
            root.text.push('\n');
        }
        root.text.push_str(&descendant.text);
    }
    root.rect = root.rect.union(descendant.rect);
    root.footnotes.extend(descendant.footnotes);
}

fn focus_list_descendant_root(active_root: Option<(usize, u8)>, depth: u8) -> Option<usize> {
    active_root.and_then(|(root_index, root_depth)| (depth > root_depth).then_some(root_index))
}

fn focus_unit_matches_highlight_ranges(unit: &FocusUnit, ranges: &[SourceRange]) -> bool {
    ranges == unit.paint_ranges
        || (ranges.len() == 1
            && unit
                .paint_ranges
                .first()
                .is_some_and(|root| root == &ranges[0]))
}

fn focus_unit_contains_source_range(unit: &FocusUnit, range: &SourceRange) -> bool {
    unit.paint_ranges.iter().any(|paint_range| {
        paint_range.start.spine == range.start.spine
            && paint_range.start.node == range.start.node
            && paint_range.start.text_offset < range.end.text_offset
            && range.start.text_offset < paint_range.end.text_offset
    })
}

fn block_source_range(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(block) => block.source.as_ref(),
        Block::Quote(quote) => quote.source.as_ref(),
        Block::Table(table) => table.source.as_ref(),
        Block::Image(image) => image.source.as_ref(),
        Block::Figure(figure) => figure.source.as_ref(),
        Block::Separator | Block::LineBreak | Block::PageBreak => None,
    }
}

fn text_block_footnote_references(block: &TextBlock) -> Vec<(String, PublicationUrl)> {
    block
        .content
        .iter()
        .filter_map(|inline| {
            let Inline::Text(run) = inline else {
                return None;
            };
            (run.style.baseline == TextBaseline::Superscript)
                .then(|| run.link.clone())
                .flatten()
                .filter(|target| target.fragment().is_some())
                .map(|target| (run.text.trim().to_owned(), target))
        })
        .filter(|(marker, _)| !marker.is_empty())
        .collect()
}

fn block_footnote_references(block: &Block) -> Vec<(String, PublicationUrl)> {
    match block {
        Block::Text(block) => text_block_footnote_references(block),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(quote.attribution.iter())
            .flat_map(text_block_footnote_references)
            .collect(),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .flat_map(|cell| text_block_footnote_references(&cell.text))
            .collect(),
        Block::Figure(figure) => figure
            .captions
            .iter()
            .flat_map(text_block_footnote_references)
            .collect(),
        Block::Image(_) | Block::Separator | Block::LineBreak | Block::PageBreak => Vec::new(),
    }
}

fn block_focus_text(block: &Block) -> String {
    match block {
        Block::Text(block) => crate::plugins::text_block_text(block),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(quote.attribution.iter())
            .map(crate::plugins::text_block_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Block::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .map(|cell| crate::plugins::text_block_text(&cell.text))
            .collect::<Vec<_>>()
            .join(" "),
        Block::Image(image) => image.alt.clone(),
        Block::Figure(figure) => figure
            .captions
            .iter()
            .map(crate::plugins::text_block_text)
            .collect::<Vec<_>>()
            .join(" "),
        Block::Separator | Block::LineBreak | Block::PageBreak => String::new(),
    }
}

fn focus_block_paint_ranges(block: &Block, range: &SourceRange) -> Vec<SourceRange> {
    if let Block::Quote(quote) = block {
        let ranges = quote
            .body
            .iter()
            .filter_map(|block| block.source.clone())
            .chain(
                quote
                    .attribution
                    .iter()
                    .filter_map(|block| block.source.clone()),
            )
            .collect::<Vec<_>>();
        if !ranges.is_empty() {
            return ranges;
        }
    }
    if let Block::Table(table) = block {
        let cell_ranges = table
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .filter_map(|cell| cell.text.source.clone())
            .collect::<Vec<_>>();
        if !cell_ranges.is_empty() {
            return cell_ranges;
        }
    }
    vec![range.clone()]
}

fn focus_unit_geometry_ranges(
    is_first_unit: bool,
    leading_heading_ranges: &[SourceRange],
    paint_ranges: &[SourceRange],
) -> Vec<SourceRange> {
    if !is_first_unit || leading_heading_ranges.is_empty() {
        return paint_ranges.to_vec();
    }
    leading_heading_ranges
        .iter()
        .chain(paint_ranges)
        .cloned()
        .collect()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "renderer page coordinates are GPU-bounded and egui geometry uses f32"
)]
fn focus_unit_geometry(
    layout: &ScrollSectionLayout,
    paint_ranges: &[SourceRange],
) -> Option<(egui::Rect, ReaderPosition)> {
    let mut bounds: Option<egui::Rect> = None;
    let mut position = None;
    for (page_index, page) in layout.pages.iter().enumerate() {
        for rect in page
            .page
            .source_rects(paint_ranges)
            .into_iter()
            .chain(page.page.image_source_rects(paint_ranges))
            .chain(page.page.source_table_bounds(paint_ranges))
            .chain(page.page.source_quote_bounds(paint_ranges))
        {
            let rect = egui::Rect::from_min_max(
                egui::pos2(rect.x0 as f32, layout.content_y(page_index, rect.y0 as f32)),
                egui::pos2(rect.x1 as f32, layout.content_y(page_index, rect.y1 as f32)),
            );
            bounds = Some(bounds.map_or(rect, |current| current.union(rect)));
            position.get_or_insert(page.position);
        }
    }
    bounds.zip(position)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "renderer page coordinates are GPU-bounded and egui geometry uses f32"
)]
fn focus_block_activation_geometry(
    layout: &ScrollSectionLayout,
    paint_ranges: &[SourceRange],
) -> Option<egui::Rect> {
    layout
        .pages
        .iter()
        .enumerate()
        .filter_map(|(page_index, page)| {
            let rect = page.page.source_block_bounds(paint_ranges)?;
            Some(egui::Rect::from_min_max(
                egui::pos2(rect.x0 as f32, layout.content_y(page_index, rect.y0 as f32)),
                egui::pos2(rect.x1 as f32, layout.content_y(page_index, rect.y1 as f32)),
            ))
        })
        .reduce(|bounds, next| bounds.union(next))
}

fn focus_anchor_block_index(blocks: &[Block], anchor: Option<&SourceAnchor>) -> Option<usize> {
    let anchor = anchor?;
    blocks.iter().position(|block| {
        let Some(range) = block_source_range(block) else {
            return false;
        };
        source_range_contains_anchor(range, anchor)
            || focus_block_paint_ranges(block, range)
                .iter()
                .any(|range| source_range_contains_anchor(range, anchor))
    })
}

fn resolved_focus_unit_index(
    units: &[FocusUnit],
    anchor: Option<&SourceAnchor>,
    first_unit_after_anchor: Option<usize>,
    current: ReaderPosition,
) -> usize {
    anchor
        .and_then(|anchor| {
            units.iter().position(|unit| {
                source_range_contains_anchor(&unit.range, anchor)
                    || unit
                        .paint_ranges
                        .iter()
                        .any(|range| source_range_contains_anchor(range, anchor))
            })
        })
        .or(first_unit_after_anchor)
        .or_else(|| units.iter().position(|unit| unit.position == current))
        .unwrap_or(0)
}

fn snapshot_position(snapshot: &ReaderSnapshot) -> ReaderPosition {
    ReaderPosition {
        section_index: snapshot.location.section_index,
        segment_index: snapshot.location.segment_index,
        page_index: snapshot.location.page_index,
    }
}

fn source_range_contains_anchor(range: &SourceRange, anchor: &SourceAnchor) -> bool {
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

fn focus_unit_container_center_y(rect: egui::Rect) -> f32 {
    rect.top() + rect.height().max(FOCUS_UNIT_MIN_HEIGHT) / 2.0
}

fn focus_unit_scroll_bounds(
    rect: egui::Rect,
    viewport_height: f32,
    content_padding: f32,
) -> (f32, f32) {
    let top = (rect.top() + content_padding).max(0.0);
    let bottom = (rect.bottom() + content_padding - viewport_height).max(top);
    (top, bottom)
}

fn focus_unit_target_offset_for_rect(rect: egui::Rect, viewport_height: f32) -> f32 {
    let padding = viewport_height * 0.5;
    let (top, bottom) = focus_unit_scroll_bounds(rect, viewport_height, padding);
    if bottom > top {
        top
    } else {
        (focus_unit_container_center_y(rect) + padding - viewport_height / 2.0).max(0.0)
    }
}

fn focus_unit_screen_center_y(
    rect: egui::Rect,
    scroll_offset_y: f32,
    content_padding: f32,
    page_rect: egui::Rect,
) -> f32 {
    page_rect.top() + rect.center().y + content_padding - scroll_offset_y
}

fn focus_scroll_duration(distance: f32) -> Duration {
    let duration = Duration::from_secs_f32(distance.abs() / FOCUS_SCROLL_POINTS_PER_SECOND);
    duration.clamp(FOCUS_SCROLL_MIN_DURATION, FOCUS_SCROLL_MAX_DURATION)
}

impl ScrollSectionLayout {
    #[allow(
        clippy::cast_precision_loss,
        reason = "logical page dimensions are GPU-bounded and egui geometry uses f32"
    )]
    fn new(
        section_index: usize,
        reading_unit_index: usize,
        pages: Vec<ReaderSectionPage>,
        preserve_physical_pages: bool,
    ) -> Self {
        let mut cursor = 0.0;
        let mut page_tops = Vec::with_capacity(pages.len());
        let mut page_origins = Vec::with_capacity(pages.len());
        let mut page_heights = Vec::with_capacity(pages.len());
        let mut quote_bridges = Vec::new();
        for (index, entry) in pages.iter().enumerate() {
            let logical_height = entry.page.height() as f32;
            let content_top = entry
                .visible_top
                .or_else(|| entry.page.content_top())
                .unwrap_or(0.0)
                .clamp(0.0, logical_height);
            let content_bottom = entry
                .visible_bottom
                .or_else(|| entry.page.content_bottom())
                .unwrap_or(logical_height)
                .clamp(content_top, logical_height);
            let leading_gap =
                if preserve_physical_pages || entry.visible_top.is_some() || index == 0 {
                    0.0
                } else {
                    entry.page.leading_gap()
                };
            let page_origin = if preserve_physical_pages || entry.visible_top.is_some() || index > 0
            {
                content_top
            } else {
                0.0
            };
            let page_height = if preserve_physical_pages {
                content_bottom - content_top
            } else {
                content_bottom - page_origin
            };
            if leading_gap > 0.0 {
                if let Some(previous) = index.checked_sub(1).and_then(|index| pages.get(index))
                    && let Some(style) = previous.page.quote_bridge_to(&entry.page)
                {
                    quote_bridges.push(ScrollQuoteBridge {
                        top: cursor,
                        bottom: cursor + leading_gap,
                        style,
                    });
                }
                if let Some(previous_height) = page_heights.last_mut() {
                    *previous_height += leading_gap;
                }
                cursor += leading_gap;
            }
            page_tops.push(cursor);
            page_origins.push(page_origin);
            page_heights.push(page_height);
            cursor += page_height;
        }
        Self {
            section_index,
            reading_unit_index,
            pages,
            page_tops,
            page_origins,
            page_heights,
            quote_bridges,
            content_height: cursor,
        }
    }

    fn page_top(&self, position: ReaderPosition) -> Option<f32> {
        self.pages
            .iter()
            .position(|entry| entry.position == position)
            .and_then(|index| self.page_tops.get(index).copied())
    }

    fn page_at_content_y(&self, y: f32) -> Option<(usize, f32)> {
        self.pages.iter().enumerate().find_map(|(index, _)| {
            let top = self.page_tops[index];
            let local_y = y - top;
            (local_y >= 0.0 && local_y < self.page_heights[index])
                .then_some((index, local_y + self.page_origins[index]))
        })
    }

    fn content_y(&self, index: usize, page_y: f32) -> f32 {
        self.page_tops[index] + page_y - self.page_origins[index]
    }

    fn content_y_for_position(&self, position: ReaderPosition, page_y: f32) -> Option<f32> {
        let index = self
            .pages
            .iter()
            .position(|entry| entry.position == position)?;
        Some(self.content_y(index, page_y))
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "display-list coordinates are viewport-bounded f32 values stored in kurbo f64"
    )]
    fn source_top(&self, range: &SourceRange) -> Option<f32> {
        self.pages.iter().enumerate().find_map(|(index, entry)| {
            entry
                .page
                .source_content_bounds(std::slice::from_ref(range))
                .map(|bounds| self.content_y(index, bounds.y0 as f32))
        })
    }

    fn first_visible_page(&self, offset_y: f32) -> Option<ReaderPosition> {
        self.pages
            .iter()
            .enumerate()
            .find(|(index, _)| self.page_tops[*index] + self.page_heights[*index] > offset_y)
            .map(|(_, entry)| entry.position)
    }

    fn visible_pages(&self, viewport: ScrollViewportState) -> Vec<ReaderPosition> {
        let bottom = viewport.offset_y + viewport.size.y;
        self.pages
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                let top = self.page_tops[*index];
                let page_bottom = top + self.page_heights[*index];
                page_bottom > viewport.offset_y && top < bottom
            })
            .map(|(_, entry)| entry.position)
            .collect()
    }
}

#[derive(Clone, Copy)]
struct ScrollViewportState {
    offset_y: f32,
    size: egui::Vec2,
}

impl DesktopReader {
    fn focus_mode_allowed(&self) -> bool {
        self.format != BookFormat::Pdf
            || self
                .pdf_ocr_controller
                .as_ref()
                .is_some_and(|controller| controller.is_reflow_enabled())
    }

    fn is_focus_mode(&self) -> bool {
        self.reading_mode == ReadingMode::Focus
    }

    fn is_scroll_mode(&self) -> bool {
        self.reader.style().spread == SpreadMode::Scroll
    }

    fn scroll_content_padding(&self, viewport_height: f32) -> f32 {
        if self.is_focus_mode() {
            viewport_height * 0.5
        } else {
            0.0
        }
    }

    fn current_scroll_layout(
        &mut self,
    ) -> Result<Arc<ScrollSectionLayout>, rebook_reader::ReaderError> {
        let section_index = self.snapshot.location.section_index;
        let reading_unit = self.reader.reading_unit_location();
        let preserve_physical_pages =
            self.format == BookFormat::Pdf && self.pdf_ocr.mode == PdfOcrViewMode::Original;
        if let Some(layout) = &self.scroll_section
            && (preserve_physical_pages || layout.section_index == section_index)
            && layout.reading_unit_index == reading_unit.index
        {
            return Ok(Arc::clone(layout));
        }
        let pages = self.reader.current_reading_unit_pages()?;
        let layout = Arc::new(ScrollSectionLayout::new(
            section_index,
            reading_unit.index,
            pages,
            preserve_physical_pages,
        ));
        self.scroll_section = Some(Arc::clone(&layout));
        Ok(layout)
    }

    fn resolve_focus_footnotes(&self, block: &Block) -> Vec<FocusFootnote> {
        let mut seen = HashSet::new();
        block_footnote_references(block)
            .into_iter()
            .filter(|(_, target)| seen.insert(target.to_string()))
            .map(|(marker, target)| FocusFootnote {
                text: self
                    .focus_footnote_text(&target, &marker)
                    .unwrap_or_else(|| {
                        self.language
                            .text("未能读取脚注内容", "Footnote content is unavailable")
                            .to_owned()
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
            block_source_range(block)
                .is_some_and(|range| source_range_contains_anchor(range, &anchor))
                || block_source_range(block).is_some_and(|range| {
                    focus_block_paint_ranges(block, range)
                        .iter()
                        .any(|range| source_range_contains_anchor(range, &anchor))
                })
        })?;
        let text = block_focus_text(block);
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

    #[allow(
        clippy::too_many_lines,
        reason = "focus-unit construction keeps semantic, paint, and geometry ranges synchronized"
    )]
    fn rebuild_focus_units(&mut self, layout: &ScrollSectionLayout) {
        let Ok(section) = self.source.parse_section(layout.section_index) else {
            self.focus_units.clear();
            self.focus_unit_index = 0;
            return;
        };
        let reading_ranges = self
            .reader
            .current_reading_unit_source_ranges()
            .unwrap_or_default();
        let focus_block_index =
            focus_anchor_block_index(&section.blocks, self.focus_anchor.as_ref());
        let mut first_unit_after_anchor = None;
        let mut leading_heading_ranges = Vec::new();
        let mut units: Vec<FocusUnit> = Vec::new();
        let mut active_list_root: Option<(usize, u8)> = None;
        for (block_index, block) in section.blocks.iter().enumerate() {
            if block_source_range(block).is_some_and(|range| !reading_ranges.contains(range)) {
                active_list_root = None;
                continue;
            }
            let (range, paint_ranges, text, is_image, is_table, rectangular_activation, list_depth) =
                match block {
                    Block::Text(block) => {
                        if matches!(block.kind, TextBlockKind::Heading(_)) {
                            active_list_root = None;
                            if units.is_empty()
                                && let Some(range) = block.source.clone()
                            {
                                leading_heading_ranges.push(range);
                            }
                            continue;
                        }
                        let Some(range) = block.source.clone() else {
                            // Bilingual translation companions have no canonical
                            // source range and must not split an authored list tree.
                            continue;
                        };
                        let list_depth = match block.kind {
                            TextBlockKind::ListItem { depth, .. } => Some(depth),
                            _ => None,
                        };
                        (
                            range.clone(),
                            vec![range],
                            crate::plugins::text_block_text(block),
                            false,
                            false,
                            block.kind == TextBlockKind::Preformatted,
                            list_depth,
                        )
                    }
                    Block::Quote(quote) => {
                        let Some(range) = quote.source.clone() else {
                            continue;
                        };
                        let paint_ranges = focus_block_paint_ranges(block, &range);
                        let text = quote
                            .body
                            .iter()
                            .chain(quote.attribution.iter())
                            .map(crate::plugins::text_block_text)
                            .filter(|text| !text.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        (range, paint_ranges, text, false, false, true, None)
                    }
                    Block::Table(table) => {
                        let Some(range) = table.source.clone() else {
                            continue;
                        };
                        let paint_ranges = focus_block_paint_ranges(block, &range);
                        let text = table
                            .rows
                            .iter()
                            .flat_map(|row| &row.cells)
                            .map(|cell| crate::plugins::text_block_text(&cell.text))
                            .collect::<Vec<_>>()
                            .join(" ");
                        (range, paint_ranges, text, false, true, false, None)
                    }
                    Block::Image(image) => {
                        // Fixed-layout PDF pages are represented as images with a text
                        // layer; their paragraphs already supply the focus units. A
                        // source-backed image without a text layer is an authored block
                        // image and should occupy one step in focus navigation.
                        if image.text_layer.is_some() {
                            active_list_root = None;
                            continue;
                        }
                        let Some(range) = image.source.clone() else {
                            continue;
                        };
                        let text = if image.alt.trim().is_empty() {
                            self.language.text("图片", "Image").to_owned()
                        } else {
                            image.alt.clone()
                        };
                        (range.clone(), vec![range], text, true, false, false, None)
                    }
                    Block::Figure(figure) => {
                        let Some(range) = figure.source.clone() else {
                            continue;
                        };
                        let paint_ranges = focus_block_paint_ranges(block, &range);
                        let caption = figure
                            .captions
                            .iter()
                            .map(crate::plugins::text_block_text)
                            .filter(|text| !text.trim().is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        let text = if caption.is_empty() {
                            figure
                                .images
                                .iter()
                                .map(|image| image.alt.trim())
                                .filter(|alt| !alt.is_empty())
                                .collect::<Vec<_>>()
                                .join("; ")
                        } else {
                            caption
                        };
                        let text = if text.is_empty() {
                            self.language.text("图片", "Image").to_owned()
                        } else {
                            text
                        };
                        (range, paint_ranges, text, true, false, false, None)
                    }
                    Block::Separator | Block::LineBreak | Block::PageBreak => {
                        active_list_root = None;
                        continue;
                    }
                };
            if text.trim().is_empty() {
                if list_depth.is_none_or(|depth| depth == 0) {
                    active_list_root = None;
                }
                continue;
            }
            let footnotes = self.resolve_focus_footnotes(block);
            let geometry_ranges = focus_unit_geometry_ranges(
                units.is_empty(),
                &leading_heading_ranges,
                &paint_ranges,
            );
            let Some((mut rect, position)) = focus_unit_geometry(layout, &geometry_ranges) else {
                continue;
            };
            let rectangular_activation_rect = rectangular_activation
                .then(|| focus_block_activation_geometry(layout, &paint_ranges))
                .flatten();
            if is_table {
                rect.max.y += FOCUS_TABLE_BOTTOM_MARGIN;
            }
            let target_reached = first_unit_after_anchor.is_none()
                && focus_block_index.is_some_and(|target| block_index >= target);
            let unit = FocusUnit {
                range,
                paint_ranges,
                text,
                position,
                rect,
                is_image,
                is_table,
                rectangular_activation,
                rectangular_activation_rect,
                footnotes,
            };
            if let Some(depth) = list_depth {
                if let Some(root_index) = focus_list_descendant_root(active_list_root, depth)
                    && units[root_index].range.start.spine == unit.range.start.spine
                {
                    if target_reached {
                        first_unit_after_anchor = Some(root_index);
                    }
                    merge_focus_list_descendant(&mut units[root_index], unit);
                    continue;
                }
                let root_index = units.len();
                if target_reached {
                    first_unit_after_anchor = Some(root_index);
                }
                units.push(unit);
                active_list_root = Some((root_index, depth));
            } else {
                active_list_root = None;
                if target_reached {
                    first_unit_after_anchor = Some(units.len());
                }
                units.push(unit);
            }
        }
        let current = snapshot_position(&self.snapshot);
        self.focus_unit_index = resolved_focus_unit_index(
            &units,
            self.focus_anchor.as_ref(),
            first_unit_after_anchor,
            current,
        );
        self.focus_anchor = units
            .get(self.focus_unit_index)
            .map(|unit| unit.range.start.clone());
        self.focus_units = units;
        self.sync_focus_chat_session();
        self.sync_focus_selected_image();
        self.bump_scene_revision();
    }

    fn focus_unit_target_offset(&self, viewport_height: f32) -> Option<f32> {
        let unit = self.focus_units.get(self.focus_unit_index)?;
        Some(focus_unit_target_offset_for_rect(
            unit.rect,
            viewport_height,
        ))
    }

    fn animate_focus_scroll_to(&mut self, target: f32) {
        let current = self.ui.focus_scroll_motion.map_or_else(
            || {
                self.scroll_viewport
                    .map_or(target, |viewport| viewport.offset_y)
            },
            |motion| motion.value,
        );
        let mut motion = Motion::settled_with_curve(
            current,
            focus_scroll_duration(target - current),
            MotionCurve::EaseInOut,
        );
        motion.animate_to(target);
        self.ui.focus_scroll_motion = Some(motion);
        self.ui.last_motion_tick = Some(Instant::now());
    }

    fn scroll_within_tall_focus_unit(&mut self, direction: PageDirection) -> bool {
        let Some(viewport) = self.scroll_viewport else {
            return false;
        };
        let Some(unit) = self.focus_units.get(self.focus_unit_index) else {
            return false;
        };
        let padding = self.scroll_content_padding(viewport.size.y);
        let (top, bottom) = focus_unit_scroll_bounds(unit.rect, viewport.size.y, padding);
        if bottom <= top {
            return false;
        }
        if self.ui.focus_scroll_motion.is_some_and(|motion| {
            motion.is_animating()
                && match direction {
                    PageDirection::Previous => motion.target <= top + MOTION_EPSILON,
                    PageDirection::Next => motion.target >= bottom - MOTION_EPSILON,
                }
        }) {
            // Repeated key events must not cross a block boundary before the
            // viewport has actually reached the animated edge.
            return true;
        }
        let current = self
            .ui
            .focus_scroll_motion
            .map_or(viewport.offset_y, |motion| motion.target)
            .clamp(top, bottom);
        let step = (viewport.size.y * 0.8).max(FOCUS_UNIT_MIN_HEIGHT);
        let target = match direction {
            PageDirection::Previous if current > top + MOTION_EPSILON => (current - step).max(top),
            PageDirection::Next if current < bottom - MOTION_EPSILON => {
                (current + step).min(bottom)
            }
            _ => return false,
        };
        self.animate_focus_scroll_to(target);
        true
    }

    fn move_focus_unit(&mut self, direction: PageDirection) {
        if self.focus_units.is_empty() {
            return;
        }
        let at_boundary = match direction {
            PageDirection::Previous => self.focus_unit_index == 0,
            PageDirection::Next => self.focus_unit_index + 1 >= self.focus_units.len(),
        };
        if at_boundary {
            self.go_to_adjacent_section(direction);
            return;
        }
        let next = match direction {
            PageDirection::Previous => self.focus_unit_index - 1,
            PageDirection::Next => self.focus_unit_index + 1,
        };
        self.select_focus_unit(next);
    }

    fn select_focus_unit(&mut self, index: usize) {
        if index == self.focus_unit_index || index >= self.focus_units.len() {
            return;
        }
        self.ui.focus_footnotes_visible = false;
        self.ui.focus_footnote_scroll_delta = 0.0;
        self.focus_toc_override = None;
        self.focus_unit_index = index;
        self.focus_anchor = self
            .focus_units
            .get(index)
            .map(|unit| unit.range.start.clone());
        self.sync_focus_chat_session();
        self.sync_focus_selected_image();
        if let Some(target) = self
            .scroll_viewport
            .and_then(|viewport| self.focus_unit_target_offset(viewport.size.y))
        {
            self.animate_focus_scroll_to(target);
        }
        self.bump_scene_revision();
        self.persist_progress();
    }

    fn sync_focus_selected_image(&mut self) {
        let Some(unit) = self
            .focus_units
            .get(self.focus_unit_index)
            .filter(|unit| unit.is_image)
            .cloned()
        else {
            self.selected_image = None;
            return;
        };
        // The retained Vello image layer can outlive the renderer's transient
        // image upload when focus moves away and later returns to this page.
        // The GPU renderer also refreshes the underlying image atlas, while this
        // eviction makes the selected image underlay match the active focus page.
        self.invalidate_page_scene(unit.position);
        let Some(layout) = self.scroll_section.as_ref() else {
            self.selected_image = None;
            return;
        };
        let Some(page_index) = layout
            .pages
            .iter()
            .position(|page| page.position == unit.position)
        else {
            self.selected_image = None;
            return;
        };
        let page_y =
            unit.rect.center().y - layout.page_tops[page_index] + layout.page_origins[page_index];
        self.selected_image = self
            .reader
            .image_at_page(unit.position, unit.rect.center().x, page_y)
            .ok()
            .flatten()
            .and_then(|image| SelectedImage::from_reader_image(&image, true).ok());
    }

    fn scroll_page_coordinates(&self, x: f32, y: f32) -> Option<(ReaderPosition, f32, f32)> {
        let viewport = self.scroll_viewport?;
        let layout = self.scroll_section.as_ref()?;
        let padding = self.scroll_content_padding(viewport.size.y);
        let (index, local_y) = layout.page_at_content_y(viewport.offset_y + y - padding)?;
        Some((layout.pages[index].position, x, local_y))
    }

    fn update_scroll_viewport(&mut self, ctx: &egui::Context, viewport: ScrollViewportState) {
        let changed = self.scroll_viewport.is_none_or(|previous| {
            (previous.offset_y - viewport.offset_y).abs() > 0.1 || previous.size != viewport.size
        });
        self.scroll_viewport = Some(viewport);
        if changed {
            self.bump_scene_revision();
        }

        let (visible_position, placeholder_positions) = if self.is_focus_mode() {
            (
                self.focus_units
                    .get(self.focus_unit_index)
                    .map(|unit| unit.position),
                Vec::new(),
            )
        } else if let Some(layout) = self.scroll_section.as_ref() {
            let visible_position = layout.first_visible_page(viewport.offset_y.max(0.0));
            let placeholders = layout
                .visible_pages(viewport)
                .into_iter()
                .filter(|position| {
                    layout
                        .pages
                        .iter()
                        .any(|entry| entry.position == *position && entry.placeholder)
                })
                .take(2)
                .collect();
            (visible_position, placeholders)
        } else {
            (None, Vec::new())
        };
        let visible_was_placeholder =
            visible_position.is_some_and(|position| placeholder_positions.contains(&position));
        let mut ready_placeholders = Vec::new();
        let mut materialization_pending = false;
        for position in placeholder_positions {
            match self.reader.try_materialize_position(position) {
                Ok(true) => ready_placeholders.push(position),
                Ok(false) => materialization_pending = true,
                Err(error) => {
                    self.error = Some(format!("加载 PDF 页面失败：{error}"));
                    break;
                }
            }
        }
        if !ready_placeholders.is_empty() {
            self.invalidate_page_scenes();
        }
        if materialization_pending {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        let current = ReaderPosition {
            section_index: self.snapshot.location.section_index,
            segment_index: self.snapshot.location.segment_index,
            page_index: self.snapshot.location.page_index,
        };
        let visible_is_pending = visible_position.is_some_and(|position| {
            visible_was_placeholder && !ready_placeholders.contains(&position)
        });
        if let Some(position) = visible_position
            && position != current
            && !visible_is_pending
        {
            match self.reader.set_visible_position(position) {
                Ok(snapshot) => {
                    self.install_snapshot(snapshot);
                    self.persist_progress();
                }
                Err(error) => self.error = Some(format!("更新滑动阅读位置失败：{error}")),
            }
        }
        if changed
            && !self
                .ui
                .focus_scroll_motion
                .is_some_and(Motion::is_animating)
        {
            self.queue_visible_section_translation();
        }
    }

    pub(crate) fn prepare_for_shutdown(&self) {
        self.persist_progress();
    }

    pub(crate) fn take_reopen_request(&mut self) -> Option<PathBuf> {
        self.reopen_requested.take()
    }

    pub(crate) fn take_reopen_notice(&mut self) -> Option<String> {
        self.reopen_notice.take()
    }

    pub(crate) fn take_reopen_error(&mut self) -> Option<String> {
        self.reopen_error.take()
    }

    pub(crate) fn show_notice(&mut self, message: String) {
        self.notice_timer
            .show(&mut self.notice, message, Instant::now());
    }

    fn show_error(&mut self, message: String) {
        self.error_timer
            .show(&mut self.error, message, Instant::now());
    }

    fn apply_generated_toc(&mut self) -> Result<(), String> {
        let Some(draft) = self.pdf_toc.draft.as_ref() else {
            return Err("没有可应用的 AI 目录".into());
        };
        crate::generated_toc::save(&self.book_id, draft)
            .map_err(|error| format!("保存 AI 目录失败：{error}"))?;
        self.pdf_toc.editing = false;
        self.pdf_toc.draft = None;
        self.persist_progress();
        self.reopen_requested = Some(self.source_path.clone());
        Ok(())
    }

    fn edit_generated_toc(&mut self) {
        match crate::generated_toc::load(&self.book_id) {
            Ok(Some(draft)) => {
                self.pdf_toc.draft = Some(draft);
                self.pdf_toc.editing = true;
            }
            Ok(None) => {
                self.show_error("没有可编辑的 AI 目录".into());
            }
            Err(error) => {
                self.show_error(format!("读取 AI 目录失败：{error}"));
            }
        }
    }
}

fn focus_mode_allowed(format: BookFormat, pdf_ocr_mode: PdfOcrViewMode) -> bool {
    format != BookFormat::Pdf || pdf_ocr_mode == PdfOcrViewMode::Reflow
}

fn allowed_reading_mode(
    format: BookFormat,
    pdf_ocr_mode: PdfOcrViewMode,
    requested: ReadingMode,
) -> ReadingMode {
    if requested == ReadingMode::Focus && !focus_mode_allowed(format, pdf_ocr_mode) {
        ReadingMode::Classic
    } else {
        requested
    }
}

struct DesktopReaderResources {
    source: Arc<dyn BookSource>,
    rewrite_source: Arc<RewriteBookSource>,
    translation_source: Arc<TranslationBookSource>,
    pdf_ocr_controller: Option<Arc<PdfOcrSourceController>>,
    pdf_ocr_available: bool,
    pdf_ocr_mode: PdfOcrViewMode,
    cover: Option<Vec<u8>>,
    format: BookFormat,
    book_id: String,
    display_metadata: BookDisplayMetadata,
    pdf_metadata_missing: PdfMetadataMissing,
    highlight_store: HighlightStore,
    highlights: Vec<StoredHighlight>,
    progress_store: Option<SyncStore>,
    restored_source_range: Option<SourceRange>,
    plugin_settings: PluginSettings,
    language: AppLanguage,
    reading_mode: ReadingMode,
    selection_granularity: SelectionGranularity,
    shortcuts: ShortcutPreferences,
    sync_settings: SyncSettings,
    sync_password: String,
    source_path: PathBuf,
}

#[derive(Clone)]
struct SearchTask {
    source: Arc<dyn BookSource>,
    query: String,
}

pub(crate) type SearchTaskMessage = TaskResult<Vec<BookSearchResult>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusedMarkKind {
    Search,
    Assistant,
}

#[derive(Clone, Debug)]
struct FocusedMark {
    ranges: Vec<SourceRange>,
    kind: FocusedMarkKind,
}

impl FocusedMark {
    fn search(range: SourceRange) -> Self {
        Self {
            ranges: vec![range],
            kind: FocusedMarkKind::Search,
        }
    }

    fn assistant(ranges: Vec<SourceRange>) -> Self {
        Self {
            ranges,
            kind: FocusedMarkKind::Assistant,
        }
    }

    fn color(&self) -> Color {
        match self.kind {
            FocusedMarkKind::Search => SEARCH_MARK_COLOR,
            FocusedMarkKind::Assistant => ASSISTANT_MARK_COLOR,
        }
    }
}

#[derive(Default)]
struct SearchUiState {
    query: String,
    focus_input: bool,
    results: Vec<BookSearchResult>,
    status: String,
    task: TaskSlot<SearchTask>,
}

#[derive(Clone)]
struct ChatTask {
    session_id: u64,
    source: Arc<dyn BookSource>,
    format: BookFormat,
    kind: ChatRequestKind,
    rewrite_source: Arc<RewriteBookSource>,
    book_id: String,
    selection: Option<crate::plugins::ChatSelection>,
    annotations: Vec<StoredHighlight>,
    settings: PluginSettings,
    history: Vec<ChatTurn>,
    question: String,
    current: ChatReadingContext,
    response_language: String,
}

pub(crate) struct ChatTaskMessage {
    pub(crate) id: u64,
    pub(crate) session_id: u64,
    pub(crate) result: Result<ChatResponse, String>,
}

pub(crate) struct ChatStreamMessage {
    pub(crate) id: u64,
    pub(crate) session_id: u64,
    pub(crate) content: String,
}

struct ChatStreamingState {
    task_id: u64,
    content: String,
}

struct ChatUiState {
    session_id: u64,
    input: String,
    cursor_char_index: usize,
    suggestion_index: usize,
    move_cursor_to_end: bool,
    references: Vec<ChatReference>,
    reference_options_location: Option<(usize, usize, usize)>,
    reference_options: Vec<ChatReference>,
    messages: Vec<ChatTurn>,
    pending_annotation_actions: Vec<crate::plugins::ChatAnnotationAction>,
    streaming: Option<ChatStreamingState>,
    error: Option<String>,
    error_timer: TransientMessageTimer,
    task: TaskSlot<ChatTask>,
    pending_keyboard_scroll_delta: f32,
}

impl Default for ChatUiState {
    fn default() -> Self {
        Self {
            session_id: NEXT_CHAT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            input: String::new(),
            cursor_char_index: 0,
            suggestion_index: 0,
            move_cursor_to_end: false,
            references: Vec::new(),
            reference_options_location: None,
            reference_options: Vec::new(),
            messages: Vec::new(),
            pending_annotation_actions: Vec::new(),
            streaming: None,
            error: None,
            error_timer: TransientMessageTimer::default(),
            task: TaskSlot::default(),
            pending_keyboard_scroll_delta: 0.0,
        }
    }
}

impl ChatUiState {
    fn has_data(&self) -> bool {
        !self.messages.is_empty()
            || self.task.is_pending()
            || self.streaming.is_some()
            || !self.pending_annotation_actions.is_empty()
    }
}

#[derive(Clone)]
struct TranslationTask {
    section_index: usize,
    settings: PluginSettings,
    blocks: Vec<TranslationBlockInput>,
}

pub(crate) enum TranslationTaskMessage {
    Batch {
        id: u64,
        translations: Vec<BlockTranslation>,
    },
    Complete(TaskResult<()>),
}

#[derive(Clone)]
struct TocTranslationTask {
    toc_ids: Vec<String>,
    settings: PluginSettings,
    blocks: Vec<TranslationBlockInput>,
}

pub(crate) type TocTranslationTaskMessage = TaskResult<Vec<BlockTranslation>>;

#[derive(Clone)]
struct PdfTocTask {
    source: Arc<dyn BookSource>,
    book_id: String,
    need_toc: bool,
    need_page_roles: bool,
    missing: PdfMetadataMissing,
    settings: PluginSettings,
}

pub(crate) enum PdfTocTaskMessage {
    Progress { id: u64, message: String },
    Complete(Box<TaskResult<crate::plugins::PdfMetadataExtraction>>),
}

#[derive(Default)]
struct PdfTocUiState {
    progress: String,
    draft: Option<GeneratedTocDraft>,
    editing: bool,
    task: TaskSlot<PdfTocTask>,
}

#[derive(Clone, Copy, Default)]
struct PdfMetadataMissing {
    title: bool,
    authors: bool,
}

impl PdfMetadataMissing {
    const fn any(self) -> bool {
        self.title || self.authors
    }
}

pub(crate) struct PdfMetadataUpdate {
    pub(crate) book_id: String,
    pub(crate) title: String,
    pub(crate) authors: Vec<String>,
}

#[derive(Clone)]
struct PdfOcrTask {
    path: PathBuf,
    book_id: String,
    page_count: usize,
    settings: PluginSettings,
}

pub(crate) enum PdfOcrTaskMessage {
    Progress { id: u64, message: String },
    Complete(TaskResult<()>),
}

struct PdfOcrUiState {
    available: bool,
    mode: PdfOcrViewMode,
    progress: String,
    task: TaskSlot<PdfOcrTask>,
}

impl PdfOcrUiState {
    fn new(available: bool, mode: PdfOcrViewMode) -> Self {
        Self {
            available,
            mode,
            progress: String::new(),
            task: TaskSlot::default(),
        }
    }
}

#[derive(Default)]
struct TranslationUiState {
    enabled: bool,
    render_enabled: bool,
    error: Option<String>,
    dismiss_at: Option<Instant>,
    task: TaskSlot<TranslationTask>,
    toc_task: TaskSlot<TocTranslationTask>,
    toc_labels: HashMap<String, String>,
}

impl TranslationUiState {
    fn show_error(&mut self, error: String, now: Instant) {
        self.error = Some(error);
        self.dismiss_at = Some(now + NOTICE_AUTO_DISMISS_DELAY);
    }

    fn clear_error(&mut self) {
        self.error = None;
        self.dismiss_at = None;
    }

    fn dismiss_if_due(&mut self, now: Instant) -> bool {
        if self.dismiss_at.is_none_or(|deadline| now < deadline) {
            return false;
        }
        self.clear_error();
        true
    }
}

#[derive(Default)]
struct TransientMessageTimer {
    message: Option<String>,
    dismiss_at: Option<Instant>,
}

impl TransientMessageTimer {
    fn show(&mut self, current: &mut Option<String>, message: String, now: Instant) {
        *current = Some(message);
        self.message.clone_from(current);
        self.dismiss_at = Some(now + NOTICE_AUTO_DISMISS_DELAY);
    }

    fn advance(&mut self, current: &mut Option<String>, now: Instant) -> bool {
        if self.message.as_deref() != current.as_deref() {
            self.message.clone_from(current);
            self.dismiss_at = current.as_ref().map(|_| now + NOTICE_AUTO_DISMISS_DELAY);
        }
        if self.dismiss_at.is_some_and(|deadline| now >= deadline) {
            current.take();
            self.message = None;
            self.dismiss_at = None;
            return true;
        }
        false
    }
}

#[derive(Clone, Copy)]
enum SceneChange {
    Overlays,
    StaticContent,
}

#[derive(Clone, Copy)]
enum MarkRetention {
    Keep,
    ClearSelectedHighlight,
    ClearAll,
}

#[derive(Clone, Copy)]
enum FollowUp {
    None,
    Run,
}

#[derive(Clone, Copy)]
enum ProgressChange {
    Keep,
    Persist,
}

#[derive(Clone, Copy)]
struct SnapshotEffects {
    scene: SceneChange,
    marks: MarkRetention,
    prefetch: FollowUp,
    translation: FollowUp,
    progress: ProgressChange,
}

impl SnapshotEffects {
    const fn navigation() -> Self {
        Self {
            scene: SceneChange::Overlays,
            marks: MarkRetention::ClearSelectedHighlight,
            prefetch: FollowUp::Run,
            translation: FollowUp::Run,
            progress: ProgressChange::Persist,
        }
    }

    const fn static_content_change() -> Self {
        Self {
            scene: SceneChange::StaticContent,
            marks: MarkRetention::Keep,
            prefetch: FollowUp::Run,
            translation: FollowUp::None,
            progress: ProgressChange::Keep,
        }
    }

    const fn viewport_change() -> Self {
        Self {
            translation: FollowUp::Run,
            ..Self::static_content_change()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReaderOverlay {
    None,
    Menu,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SidebarTab {
    #[default]
    Toc,
    Highlights,
    Search,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssistantPanel {
    Chat,
}

#[derive(Clone, Copy, Debug)]
struct Motion {
    value: f32,
    start: f32,
    target: f32,
    elapsed: Duration,
    duration: Duration,
    curve: MotionCurve,
}

#[derive(Clone, Copy, Debug)]
enum MotionCurve {
    EaseOut,
    EaseInOut,
}

impl Motion {
    const fn settled(value: f32) -> Self {
        Self::settled_with_duration(value, MOTION_DURATION)
    }

    const fn settled_with_duration(value: f32, duration: Duration) -> Self {
        Self::settled_with_curve(value, duration, MotionCurve::EaseOut)
    }

    const fn settled_with_curve(value: f32, duration: Duration, curve: MotionCurve) -> Self {
        Self {
            value,
            start: value,
            target: value,
            elapsed: Duration::ZERO,
            duration,
            curve,
        }
    }

    fn animate_to(&mut self, target: f32) -> bool {
        if (self.target - target).abs() <= MOTION_EPSILON {
            return false;
        }
        self.start = self.value;
        self.target = target;
        self.elapsed = Duration::ZERO;
        true
    }

    fn advance(&mut self, delta: Duration) {
        if !self.is_animating() {
            return;
        }
        self.elapsed = self.elapsed.saturating_add(delta);
        let progress = if self.duration.is_zero() {
            1.0
        } else {
            (self.elapsed.as_secs_f32() / self.duration.as_secs_f32()).min(1.0)
        };
        let eased = match self.curve {
            MotionCurve::EaseOut => 1.0 - (1.0 - progress).powi(3),
            MotionCurve::EaseInOut => progress * progress * (3.0 - 2.0 * progress),
        };
        self.value = self.start + (self.target - self.start) * eased;
        if progress >= 1.0 {
            self.value = self.target;
            self.start = self.target;
            self.elapsed = Duration::ZERO;
        }
    }

    fn is_animating(self) -> bool {
        (self.value - self.target).abs() > MOTION_EPSILON
    }

    fn is_visible(self) -> bool {
        self.value > MOTION_EPSILON
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent reader overlays and interaction states are intentionally orthogonal"
)]
struct ReaderUiState {
    sidebar_open: bool,
    sidebar_pinned: bool,
    sidebar_width: f32,
    sidebar_tab: SidebarTab,
    toolbar_hovered: bool,
    toolbar_hide_at: Option<Instant>,
    overlay: ReaderOverlay,
    assistant_panel: Option<AssistantPanel>,
    assistant_width: f32,
    toolbar_motion: Motion,
    sidebar_motion: Motion,
    assistant_motion: Motion,
    menu_motion: Motion,
    focus_scroll_motion: Option<Motion>,
    last_motion_tick: Option<Instant>,
    wheel_accumulator: f32,
    last_wheel_turn: Option<Instant>,
    expanded_toc: HashSet<String>,
    last_auto_scrolled_toc: Option<String>,
    focus_actions_visible: bool,
    focus_footnotes_visible: bool,
    focus_footnote_scroll_delta: f32,
}

impl ReaderUiState {
    fn set_toolbar_hovered(&mut self, hovered: bool, now: Instant) -> bool {
        if self.toolbar_hovered == hovered {
            return false;
        }
        self.toolbar_hovered = hovered;
        if hovered {
            self.reveal_toolbar(now);
        } else if self.overlay != ReaderOverlay::Menu {
            self.schedule_toolbar_hide(now);
        }
        true
    }

    fn is_animating(&self) -> bool {
        self.toolbar_motion.is_animating()
            || self.sidebar_motion.is_animating()
            || self.assistant_motion.is_animating()
            || self.menu_motion.is_animating()
            || self.focus_scroll_motion.is_some_and(Motion::is_animating)
    }

    fn needs_motion_tick(&self) -> bool {
        self.is_animating() || self.toolbar_hide_at.is_some()
    }

    fn refresh_motion_clock(&mut self, now: Instant) {
        if self.needs_motion_tick() {
            self.last_motion_tick.get_or_insert(now);
        } else {
            self.last_motion_tick = None;
        }
    }

    fn reveal_toolbar(&mut self, now: Instant) {
        self.toolbar_hide_at = None;
        self.toolbar_motion.animate_to(1.0);
        self.refresh_motion_clock(now);
    }

    fn schedule_toolbar_hide(&mut self, now: Instant) {
        if self.toolbar_motion.is_visible() || self.toolbar_motion.is_animating() {
            self.toolbar_hide_at = Some(now + TOOLBAR_HIDE_DELAY);
        }
        self.refresh_motion_clock(now);
    }

    fn overlay_visible(&self) -> bool {
        self.menu_motion.is_visible()
    }
}

impl DesktopReader {
    pub(crate) fn log_diagnostic_snapshot(&self, event: &'static str, focused: Option<bool>) {
        crate::diagnostics::log(
            event,
            &[
                crate::diagnostics::Field::Text("screen", "reader"),
                crate::diagnostics::Field::Text(
                    "focus",
                    match focused {
                        Some(true) => "true",
                        Some(false) => "false",
                        None => "unknown",
                    },
                ),
                crate::diagnostics::Field::Bool(
                    "assistant_panel",
                    self.ui.assistant_panel.is_some(),
                ),
                crate::diagnostics::Field::F32("assistant_value", self.ui.assistant_motion.value),
                crate::diagnostics::Field::F32("assistant_start", self.ui.assistant_motion.start),
                crate::diagnostics::Field::F32("assistant_target", self.ui.assistant_motion.target),
                crate::diagnostics::Field::Bool(
                    "assistant_animating",
                    self.ui.assistant_motion.is_animating(),
                ),
                crate::diagnostics::Field::F32("assistant_width", self.ui.assistant_width),
                crate::diagnostics::Field::Bool("sidebar_open", self.ui.sidebar_open),
                crate::diagnostics::Field::Bool("sidebar_pinned", self.ui.sidebar_pinned),
                crate::diagnostics::Field::F32("sidebar_width", self.ui.sidebar_width),
                crate::diagnostics::Field::F32("sidebar_value", self.ui.sidebar_motion.value),
                crate::diagnostics::Field::F32("sidebar_target", self.ui.sidebar_motion.target),
                crate::diagnostics::Field::Bool("chat_pending", self.chat.task.is_pending()),
            ],
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "reader construction keeps all UI state defaults visible in one place"
    )]
    fn new(mut reader: ReaderSession, resources: DesktopReaderResources) -> Self {
        let DesktopReaderResources {
            source,
            rewrite_source,
            translation_source,
            pdf_ocr_controller,
            pdf_ocr_available,
            pdf_ocr_mode,
            cover,
            format,
            book_id,
            display_metadata,
            pdf_metadata_missing,
            highlight_store,
            highlights,
            progress_store,
            restored_source_range,
            plugin_settings,
            language,
            reading_mode,
            selection_granularity,
            shortcuts,
            sync_settings,
            sync_password,
            source_path,
        } = resources;
        let error = reader
            .prefetch_adjacent()
            .err()
            .map(|error| error.to_string());
        let snapshot = reader.snapshot();
        let expanded_toc = snapshot.active_toc_path.iter().cloned().collect();
        let scroll_target_position =
            (reader.style().spread == SpreadMode::Scroll).then_some(ReaderPosition {
                section_index: snapshot.location.section_index,
                segment_index: snapshot.location.segment_index,
                page_index: snapshot.location.page_index,
            });
        let restored_focus_anchor = if reading_mode == ReadingMode::Focus {
            restored_source_range
                .as_ref()
                .map(|range| range.start.clone())
        } else {
            None
        };
        let scroll_target_source =
            if reading_mode != ReadingMode::Focus && reader.style().spread == SpreadMode::Scroll {
                restored_source_range
            } else {
                None
            };
        let mut pdf_ocr = PdfOcrUiState::new(pdf_ocr_available, pdf_ocr_mode);
        let resume_pdf_ocr = format == BookFormat::Pdf
            && plugin_settings.pdf_ocr_enabled
            && has_pending_pdf_ocr_task(&book_id, &plugin_settings).unwrap_or_else(|error| {
                tracing::warn!(%error, "failed to inspect pending PDF OCR task");
                false
            });
        if resume_pdf_ocr {
            pdf_ocr.progress = language
                .text("正在恢复 PDF OCR 任务…", "Resuming PDF OCR task…")
                .into();
            pdf_ocr.task.begin(PdfOcrTask {
                path: source_path.clone(),
                book_id: book_id.clone(),
                page_count: source.book().sections.len(),
                settings: plugin_settings.clone(),
            });
        }
        let pdf_visual_source = pdf_ocr_controller.as_ref().map_or_else(
            || Arc::clone(&source),
            |controller| controller.original_source(),
        );
        let mut pdf_toc = PdfTocUiState::default();
        let need_toc = needs_generated_toc(source.as_ref());
        if format == BookFormat::Pdf
            && plugin_settings.ocr_enabled
            && plugin_settings.ocr_endpoint().is_ok()
            && (need_toc || pdf_metadata_missing.any())
        {
            pdf_toc.progress = language
                .text("正在准备元数据提取…", "Preparing metadata extraction…")
                .into();
            pdf_toc.task.begin(PdfTocTask {
                source: pdf_visual_source,
                book_id: book_id.clone(),
                need_toc,
                need_page_roles: true,
                missing: pdf_metadata_missing,
                settings: plugin_settings.clone(),
            });
        }
        let search = SearchUiState::default();
        Self {
            reader,
            source,
            rewrite_source,
            translation_source,
            pdf_ocr_controller,
            snapshot,
            cover,
            cover_texture: None,
            format,
            book_id,
            display_metadata,
            pdf_metadata_missing,
            highlight_store,
            highlights,
            progress_store,
            selection_anchor: None,
            selection: None,
            selection_granularity,
            selection_toolbar_visible: false,
            selected_image: None,
            image_pointer_state: ImagePointerState::Idle,
            image_preview: None,
            annotation_note_draft: None,
            selected_highlight_id: None,
            focused_mark: None,
            search,
            chat: ChatUiState::default(),
            book_chat: None,
            focus_chat_sessions: HashMap::new(),
            focus_chat_session_key: None,
            chat_markdown: chat_markdown::ChatMarkdownState::default(),
            translation: TranslationUiState::default(),
            pdf_toc,
            pdf_ocr,
            ui: ReaderUiState {
                sidebar_open: reading_mode == ReadingMode::Classic,
                sidebar_pinned: reading_mode == ReadingMode::Classic,
                sidebar_width: egui_view::SIDEBAR_WIDTH,
                sidebar_tab: SidebarTab::Toc,
                toolbar_hovered: false,
                toolbar_hide_at: None,
                overlay: ReaderOverlay::None,
                assistant_panel: None,
                assistant_width: egui_view::ASSISTANT_WIDTH,
                toolbar_motion: Motion::settled_with_duration(0.0, TOOLBAR_MOTION_DURATION),
                sidebar_motion: Motion::settled(if reading_mode == ReadingMode::Classic {
                    1.0
                } else {
                    0.0
                }),
                assistant_motion: Motion::settled(0.0),
                menu_motion: Motion::settled(0.0),
                focus_scroll_motion: None,
                last_motion_tick: None,
                wheel_accumulator: 0.0,
                last_wheel_turn: None,
                expanded_toc,
                last_auto_scrolled_toc: None,
                focus_actions_visible: false,
                focus_footnotes_visible: false,
                focus_footnote_scroll_delta: 0.0,
            },
            plugin_settings,
            language,
            reading_mode,
            shortcuts,
            sync_settings,
            sync_password,
            canvas_size: None,
            scene_id: NEXT_SCENE_ID.fetch_add(1, Ordering::Relaxed),
            scene_revision: 0,
            page_scenes: HashMap::new(),
            page_scene_lru: VecDeque::new(),
            scroll_section: None,
            scroll_viewport: None,
            scroll_target_position,
            scroll_target_source,
            focus_units: Vec::new(),
            focus_unit_index: 0,
            focus_target_offset: None,
            focus_anchor: restored_focus_anchor,
            focus_toc_override: None,
            pending_page_turn: None,
            pending_reading_unit_entry: None,
            pending_focus_wheel_turn: None,
            pending_keyboard_scroll_delta: 0.0,
            settings_requested: false,
            settings_change_requested: None,
            notice: None,
            notice_timer: TransientMessageTimer::default(),
            error,
            error_timer: TransientMessageTimer::default(),
            source_path,
            reopen_requested: None,
            reopen_notice: None,
            reopen_error: None,
            exit_requested: false,
        }
    }
}

fn needs_generated_toc(source: &dyn BookSource) -> bool {
    let entries = &source.book().table_of_contents;
    entries.is_empty()
        || source.table_of_contents_origin() == TableOfContentsOrigin::Fallback
        || (source.table_of_contents_origin() == TableOfContentsOrigin::Embedded
            && embedded_toc_is_page_index(entries))
}

fn embedded_toc_is_page_index(entries: &[rebook_publication::TocEntry]) -> bool {
    fn visit(entries: &[rebook_publication::TocEntry], total: &mut usize, page_labels: &mut usize) {
        for entry in entries {
            *total += 1;
            if toc_label_is_only_a_page_number(&entry.label) {
                *page_labels += 1;
            }
            visit(&entry.children, total, page_labels);
        }
    }

    let mut total = 0;
    let mut page_labels = 0;
    visit(entries, &mut total, &mut page_labels);
    total >= 4 && page_labels * 3 >= total * 2
}

fn toc_label_is_only_a_page_number(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty()
        && label.chars().all(|character| {
            character.is_ascii_digit()
                || ('０'..='９').contains(&character)
                || matches!(
                    character.to_ascii_lowercase(),
                    'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'
                )
        })
}

#[derive(Default)]
pub(super) struct AnnotationDraft {
    note: String,
    focus_pending: bool,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn logical_dimension(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value.round().clamp(1.0, f64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::navigation::snapshot_reanchors_focus;
    use super::{
        BookDisplayMetadata, Duration, FOCUS_SCROLL_MAX_DURATION, FOCUS_SCROLL_MIN_DURATION,
        FocusFootnote, FocusUnit, FollowUp, HashSet, Instant, MOTION_DURATION, Motion, MotionCurve,
        NOTICE_AUTO_DISMISS_DELAY, ReaderOverlay, ReaderUiState, ScrollSectionLayout, SidebarTab,
        SnapshotEffects, TOOLBAR_HIDE_DELAY, TOOLBAR_MOTION_DURATION, TransientMessageTimer,
        TranslationUiState, allowed_reading_mode, embedded_toc_is_page_index,
        focus_block_paint_ranges, focus_list_descendant_root, focus_scroll_duration,
        focus_unit_container_center_y, focus_unit_contains_source_range,
        focus_unit_geometry_ranges, focus_unit_matches_highlight_ranges,
        focus_unit_screen_center_y, focus_unit_scroll_bounds, focus_unit_target_offset_for_rect,
        legacy_translated_paragraph_range, logical_dimension, merge_focus_list_descendant,
        resolve_book_display_metadata, resolved_focus_unit_index, source_range_contains_anchor,
    };
    use crate::plugins::PdfOcrViewMode;
    use crate::preferences::ReadingMode;
    use rebook_formats::BookFormat;
    use rebook_layout::{
        ImagePlacement, LayoutViewport, PageItem, PageLayout, QuotePlacement, RasterImage,
        SeparatorPlacement,
    };
    use rebook_publication::{
        Block, BlockStyle, Rgba, SourceAnchor, SourceRange, SpineItemId, TableBlock, TableCell,
        TableRow, TextBlock, TextBlockKind, TocEntry,
    };
    use rebook_reader::{ReaderPosition, ReaderSectionPage};
    use rebook_renderer::DisplayListCompiler;
    use std::sync::Arc;

    fn toc_entry(label: &str, children: Vec<TocEntry>) -> TocEntry {
        TocEntry {
            label: label.into(),
            href: None,
            children,
        }
    }

    #[test]
    fn embedded_page_number_bookmarks_require_a_generated_pdf_toc() {
        let mut entries = vec![
            toc_entry("书名", Vec::new()),
            toc_entry("版权", Vec::new()),
            toc_entry("目录", Vec::new()),
        ];
        entries.extend((1..=12).map(|page| toc_entry(&page.to_string(), Vec::new())));

        assert!(embedded_toc_is_page_index(&entries));
        assert!(!embedded_toc_is_page_index(&[
            toc_entry("第一章 书是什么？", Vec::new()),
            toc_entry("第二章 一本书的诞生", Vec::new()),
            toc_entry("第三章 设计方法", Vec::new()),
            toc_entry("附录", Vec::new()),
        ]));
    }

    #[test]
    fn original_pdf_forces_classic_mode_until_ocr_reflow_is_active() {
        assert_eq!(
            allowed_reading_mode(
                BookFormat::Pdf,
                PdfOcrViewMode::Original,
                ReadingMode::Focus
            ),
            ReadingMode::Classic
        );
        assert_eq!(
            allowed_reading_mode(BookFormat::Pdf, PdfOcrViewMode::Reflow, ReadingMode::Focus),
            ReadingMode::Focus
        );
        assert_eq!(
            allowed_reading_mode(
                BookFormat::Epub,
                PdfOcrViewMode::Original,
                ReadingMode::Focus
            ),
            ReadingMode::Focus
        );
    }

    #[test]
    fn logical_dimension_rejects_invalid_sizes_and_rounds_pixels() {
        assert_eq!(logical_dimension(f64::NAN), 0);
        assert_eq!(logical_dimension(f64::INFINITY), 0);
        assert_eq!(logical_dimension(-1.0), 0);
        assert_eq!(logical_dimension(0.0), 0);
        assert_eq!(logical_dimension(10.4), 10);
        assert_eq!(logical_dimension(10.6), 11);
    }

    #[test]
    fn physical_pdf_scroll_pages_trim_only_the_layout_margins() {
        let spine = SpineItemId::new("page-1").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "page-image".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "page-image".into(),
                text_offset: 1,
            },
        };
        let page = PageLayout {
            viewport: LayoutViewport::new(400, 500).unwrap(),
            background: Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            leading_gap: 0.0,
            items: vec![PageItem::Image(ImagePlacement {
                image: RasterImage {
                    width: 2,
                    height: 2,
                    pixels: vec![255; 16].into(),
                },
                x: 20.0,
                y: 40.0,
                width: 360.0,
                height: 420.0,
                source: Some(source.clone()),
                text_layer: None,
                replacement: None,
            })],
        };
        let entry = ReaderSectionPage {
            position: ReaderPosition {
                section_index: 0,
                segment_index: 0,
                page_index: 0,
            },
            page: Arc::new(DisplayListCompiler.compile(&page)),
            placeholder: false,
            visible_top: None,
            visible_bottom: None,
        };

        let layout = ScrollSectionLayout::new(0, 0, vec![entry], true);

        assert!((layout.page_origins[0] - 40.0).abs() < f32::EPSILON);
        assert!((layout.page_heights[0] - 420.0).abs() < f32::EPSILON);
        assert_eq!(layout.page_at_content_y(0.0), Some((0, 40.0)));
        assert_eq!(layout.source_top(&source), Some(0.0));
        assert_eq!(
            layout.pages[0].page.source_range_nearest_y(40.0),
            Some(source)
        );
    }

    #[test]
    fn continuous_layout_restores_an_image_gap_lost_at_a_page_break() {
        let page = |page_index, leading_gap| {
            let layout = PageLayout {
                viewport: LayoutViewport::new(400, 500).unwrap(),
                background: Rgba::BLACK,
                leading_gap,
                items: vec![PageItem::Image(ImagePlacement {
                    image: RasterImage {
                        width: 2,
                        height: 2,
                        pixels: vec![255; 16].into(),
                    },
                    x: 20.0,
                    y: 40.0,
                    width: 360.0,
                    height: 100.0,
                    source: None,
                    text_layer: None,
                    replacement: None,
                })],
            };
            ReaderSectionPage {
                position: ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index,
                },
                page: Arc::new(DisplayListCompiler.compile(&layout)),
                placeholder: false,
                visible_top: None,
                visible_bottom: None,
            }
        };
        let layout = ScrollSectionLayout::new(0, 0, vec![page(0, 0.0), page(1, 20.0)], false);
        let first_bottom = layout.content_y(0, 140.0);
        let second_top = layout.content_y(1, 40.0);

        assert!((second_top - first_bottom - 20.0).abs() < f32::EPSILON);
        assert!((layout.page_origins[1] - 40.0).abs() < f32::EPSILON);
        assert!((layout.page_heights[0] - 160.0).abs() < f32::EPSILON);
    }

    #[test]
    fn continuous_layout_bridges_quote_accents_across_preserved_page_gaps() {
        let spine = SpineItemId::new("chapter").unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "poem".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: "poem".into(),
                text_offset: 100,
            },
        };
        let page = |page_index, leading_gap, continued_before, continued_after| {
            let layout = PageLayout {
                viewport: LayoutViewport::new(400, 200).unwrap(),
                background: Rgba::BLACK,
                leading_gap,
                items: vec![
                    PageItem::Quote(QuotePlacement {
                        x: 20.0,
                        y: 10.0,
                        width: 360.0,
                        height: 180.0,
                        continued_before,
                        continued_after,
                        fill: Rgba {
                            alpha: 0,
                            ..Rgba::BLACK
                        },
                        accent: Rgba::BLACK,
                        sources: vec![source.clone()],
                    }),
                    PageItem::Separator(SeparatorPlacement {
                        x: 40.0,
                        y: 50.0,
                        width: 80.0,
                    }),
                ],
            };
            ReaderSectionPage {
                position: ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index,
                },
                page: Arc::new(DisplayListCompiler.compile(&layout)),
                placeholder: false,
                visible_top: None,
                visible_bottom: None,
            }
        };
        let layout = ScrollSectionLayout::new(
            0,
            0,
            vec![page(0, 0.0, false, true), page(1, 20.0, true, false)],
            false,
        );

        let [bridge] = layout.quote_bridges.as_slice() else {
            panic!("matching quote continuations should bridge their preserved page gap");
        };
        assert!((bridge.top - 51.0).abs() < f32::EPSILON);
        assert!((bridge.bottom - 71.0).abs() < f32::EPSILON);
        assert!((bridge.style.x - 26.0).abs() < f32::EPSILON);
        assert!((bridge.style.width - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn legacy_full_paragraph_translation_range_expands_to_the_canonical_source() {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let canonical = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 224,
            },
        };
        let legacy = SourceRange {
            start: canonical.start.clone(),
            end: SourceAnchor {
                spine,
                node: "paragraph-1".into(),
                text_offset: 69,
            },
        };
        let blocks = vec![Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![rebook_publication::Inline::Text(
                rebook_publication::TextRun {
                    text: "a".repeat(224),
                    style: rebook_publication::TextStyle::default(),
                    link: None,
                },
            )],
            style: BlockStyle::default(),
            source: Some(canonical.clone()),
        })];
        let translated_quote = "译".repeat(69);

        assert_eq!(
            legacy_translated_paragraph_range(&blocks, &legacy, &translated_quote),
            Some(canonical)
        );
        assert_eq!(
            legacy_translated_paragraph_range(&blocks, &legacy, &"a".repeat(69)),
            None
        );
    }

    #[test]
    fn focus_unit_anchor_matching_uses_half_open_source_ranges() {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let range = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 4,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 12,
            },
        };
        let at_start = SourceAnchor {
            spine: spine.clone(),
            node: "paragraph-1".into(),
            text_offset: 4,
        };
        let at_end = SourceAnchor {
            spine,
            node: "paragraph-1".into(),
            text_offset: 12,
        };

        assert!(source_range_contains_anchor(&range, &at_start));
        assert!(!source_range_contains_anchor(&range, &at_end));
    }

    #[test]
    fn table_focus_unit_uses_cell_ranges_for_layout_and_highlighting() {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let range = |node: &str| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 8,
            },
        };
        let table_range = range("table");
        let first_cell = range("cell-1");
        let second_cell = range("cell-2");
        let cell = |source| TableCell {
            text: TextBlock {
                kind: TextBlockKind::Paragraph,
                content: Vec::new(),
                style: BlockStyle::default(),
                source: Some(source),
            },
            authored_alignment: None,
            column_span: 1,
            row_span: 1,
            header: false,
        };
        let block = Block::Table(TableBlock {
            rows: vec![TableRow {
                cells: vec![cell(first_cell.clone()), cell(second_cell.clone())],
            }],
            source: Some(table_range.clone()),
        });

        assert_eq!(
            focus_block_paint_ranges(&block, &table_range),
            vec![first_cell, second_cell]
        );
    }

    #[test]
    fn first_focus_unit_geometry_includes_leading_headings_without_highlighting_them() {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let range = |node: &str| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 8,
            },
        };
        let heading = range("heading");
        let table_cell = range("cell");

        assert_eq!(
            focus_unit_geometry_ranges(
                true,
                std::slice::from_ref(&heading),
                std::slice::from_ref(&table_cell),
            ),
            vec![heading, table_cell.clone()]
        );
        assert_eq!(
            focus_unit_geometry_ranges(
                false,
                std::slice::from_ref(&range("later-heading")),
                std::slice::from_ref(&table_cell),
            ),
            vec![table_cell]
        );
    }

    #[test]
    fn nested_list_descendants_merge_into_the_nearest_visible_root_unit() {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let range = |node: &str| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 8,
            },
        };
        let position = ReaderPosition {
            section_index: 0,
            segment_index: 0,
            page_index: 0,
        };
        let unit = |node: &str, text: &str, y: f32| FocusUnit {
            range: range(node),
            paint_ranges: vec![range(node)],
            text: text.into(),
            position,
            rect: egui::Rect::from_min_size(egui::pos2(20.0, y), egui::vec2(400.0, 40.0)),
            is_image: false,
            is_table: false,
            rectangular_activation: false,
            rectangular_activation_rect: None,
            footnotes: Vec::new(),
        };

        assert_eq!(focus_list_descendant_root(Some((0, 0)), 1), Some(0));
        assert_eq!(focus_list_descendant_root(Some((0, 0)), 2), Some(0));
        assert_eq!(focus_list_descendant_root(Some((0, 0)), 0), None);
        assert_eq!(focus_list_descendant_root(Some((0, 1)), 1), None);

        let mut root = unit("root", "语音与写作：文化转型", 100.0);
        let child_range = range("child");
        let mut child = unit("child", "口头文化与书面文化", 160.0);
        child.footnotes.push(FocusFootnote {
            text: "列表子项脚注".into(),
        });
        merge_focus_list_descendant(&mut root, child);
        merge_focus_list_descendant(&mut root, unit("grandchild", "从学术辩论到书面考试", 220.0));

        assert_eq!(root.paint_ranges.len(), 3);
        assert_eq!(root.range.start.node, "root");
        assert_eq!(root.range.end.node, "grandchild");
        assert_eq!(
            root.text,
            "语音与写作：文化转型\n口头文化与书面文化\n从学术辩论到书面考试"
        );
        assert_eq!(root.rect.top(), 100.0);
        assert_eq!(root.rect.bottom(), 260.0);
        assert_eq!(root.footnotes.len(), 1);
        assert_eq!(root.footnotes[0].text, "列表子项脚注");
        assert!(focus_unit_contains_source_range(&root, &child_range));
        assert!(focus_unit_matches_highlight_ranges(
            &root,
            &root.paint_ranges
        ));
        assert!(focus_unit_matches_highlight_ranges(
            &root,
            std::slice::from_ref(&root.paint_ranges[0])
        ));
    }

    #[test]
    fn heading_toc_anchor_selects_the_first_readable_unit_after_it() {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let range = |node: &str| SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 8,
            },
        };
        let current = ReaderPosition {
            section_index: 0,
            segment_index: 0,
            page_index: 0,
        };
        let units = ["previous", "target"].map(|node| FocusUnit {
            range: range(node),
            paint_ranges: vec![range(node)],
            text: node.into(),
            position: current,
            rect: egui::Rect::ZERO,
            is_image: false,
            is_table: false,
            rectangular_activation: false,
            rectangular_activation_rect: None,
            footnotes: Vec::new(),
        });
        let heading_anchor = SourceAnchor {
            spine,
            node: "heading".into(),
            text_offset: 0,
        };

        assert_eq!(
            resolved_focus_unit_index(&units, Some(&heading_anchor), Some(1), current),
            1
        );
    }

    #[test]
    fn focus_anchor_survives_async_content_and_viewport_refreshes() {
        assert!(!snapshot_reanchors_focus(
            SnapshotEffects::static_content_change()
        ));
        assert!(!snapshot_reanchors_focus(SnapshotEffects::viewport_change()));
        assert!(snapshot_reanchors_focus(SnapshotEffects::navigation()));
    }

    #[test]
    fn short_focus_units_share_a_stable_top_inside_the_centered_container() {
        let short = egui::Rect::from_min_size(egui::pos2(20.0, 500.0), egui::vec2(600.0, 48.0));
        let medium = egui::Rect::from_min_size(egui::pos2(20.0, 900.0), egui::vec2(600.0, 180.0));

        assert!((focus_unit_container_center_y(short) - short.top() - 120.0).abs() < f32::EPSILON);
        assert!(
            (focus_unit_container_center_y(medium) - medium.top() - 120.0).abs() < f32::EPSILON
        );
    }

    #[test]
    fn tall_focus_units_expand_the_centered_container() {
        let tall = egui::Rect::from_min_size(egui::pos2(20.0, 500.0), egui::vec2(600.0, 360.0));

        assert!((focus_unit_container_center_y(tall) - tall.center().y).abs() < f32::EPSILON);
    }

    #[test]
    fn oversized_focus_units_start_at_the_top_and_expose_their_full_scroll_range() {
        let table =
            egui::Rect::from_min_size(egui::pos2(20.0, 500.0), egui::vec2(1_200.0, 2_400.0));
        let viewport_height = 800.0;
        let padding = viewport_height * 0.5;

        let (top, bottom) = focus_unit_scroll_bounds(table, viewport_height, padding);

        assert!(
            (focus_unit_target_offset_for_rect(table, viewport_height) - top).abs() < f32::EPSILON
        );
        assert!((top - 900.0).abs() < f32::EPSILON);
        assert!((bottom - 2_500.0).abs() < f32::EPSILON);
    }

    #[test]
    fn focus_units_that_fit_the_viewport_remain_centered() {
        let paragraph =
            egui::Rect::from_min_size(egui::pos2(20.0, 500.0), egui::vec2(600.0, 180.0));

        assert!((focus_unit_target_offset_for_rect(paragraph, 800.0) - 620.0).abs() < f32::EPSILON);
    }

    #[test]
    fn focus_unit_screen_position_tracks_the_highlight_center_and_scroll_offset() {
        let page_rect = egui::Rect::from_min_size(egui::pos2(40.0, 80.0), egui::vec2(800.0, 600.0));
        let unit = egui::Rect::from_min_size(egui::pos2(20.0, 500.0), egui::vec2(600.0, 48.0));
        let padding = 300.0;
        let centered_offset =
            focus_unit_container_center_y(unit) + padding - page_rect.height() / 2.0;

        let anchored = focus_unit_screen_center_y(unit, centered_offset, padding, page_rect);
        let moving = focus_unit_screen_center_y(unit, centered_offset - 75.0, padding, page_rect);
        let expected = page_rect.center().y + unit.center().y - focus_unit_container_center_y(unit);

        assert!((anchored - expected).abs() < f32::EPSILON);
        assert!((moving - anchored - 75.0).abs() < f32::EPSILON);
    }

    #[test]
    fn focus_scroll_duration_scales_with_distance_and_stays_bounded() {
        assert_eq!(focus_scroll_duration(20.0), FOCUS_SCROLL_MIN_DURATION);
        assert!(
            focus_scroll_duration(220.0).abs_diff(Duration::from_millis(220))
                < Duration::from_micros(1)
        );
        assert_eq!(focus_scroll_duration(2_000.0), FOCUS_SCROLL_MAX_DURATION);
    }

    #[test]
    fn focus_scroll_uses_smooth_acceleration_and_deceleration() {
        let mut motion =
            Motion::settled_with_curve(0.0, Duration::from_millis(200), MotionCurve::EaseInOut);
        motion.animate_to(100.0);

        motion.advance(Duration::from_millis(50));
        assert!(motion.value < 25.0);
        motion.advance(Duration::from_millis(50));
        assert!((motion.value - 50.0).abs() < f32::EPSILON);
        motion.advance(Duration::from_millis(50));
        assert!(motion.value > 75.0);
        motion.advance(Duration::from_millis(50));
        assert!((motion.value - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn viewport_changes_reschedule_visible_translation() {
        assert!(matches!(
            SnapshotEffects::viewport_change().translation,
            FollowUp::Run
        ));
        assert!(matches!(
            SnapshotEffects::static_content_change().translation,
            FollowUp::None
        ));
    }

    #[test]
    fn motion_reaches_its_target_with_ease_out_timing() {
        let mut motion = Motion::settled(0.0);

        assert!(motion.animate_to(1.0));
        motion.advance(MOTION_DURATION / 2);
        assert!(motion.value > 0.5);
        assert!(motion.is_animating());

        motion.advance(MOTION_DURATION / 2);
        assert!((motion.value - 1.0).abs() <= f32::EPSILON);
        assert!(!motion.is_animating());
    }

    #[test]
    fn motion_can_reverse_without_jumping() {
        let mut motion = Motion::settled(0.0);
        motion.animate_to(1.0);
        motion.advance(MOTION_DURATION / 3);
        let value_before_reverse = motion.value;

        assert!(motion.animate_to(0.0));
        assert!((motion.value - value_before_reverse).abs() <= f32::EPSILON);
        motion.advance(MOTION_DURATION);
        assert!(motion.value.abs() <= f32::EPSILON);
        assert!(!motion.is_visible());
    }

    #[test]
    fn toolbar_hide_delay_is_cancelled_when_pointer_returns() {
        let now = Instant::now();
        let mut ui = ReaderUiState {
            sidebar_open: false,
            sidebar_pinned: false,
            sidebar_width: super::egui_view::SIDEBAR_WIDTH,
            sidebar_tab: SidebarTab::Toc,
            toolbar_hovered: false,
            toolbar_hide_at: None,
            overlay: ReaderOverlay::None,
            assistant_panel: None,
            assistant_width: super::egui_view::ASSISTANT_WIDTH,
            toolbar_motion: Motion::settled_with_duration(0.0, TOOLBAR_MOTION_DURATION),
            sidebar_motion: Motion::settled(0.0),
            assistant_motion: Motion::settled(0.0),
            menu_motion: Motion::settled(0.0),
            focus_scroll_motion: None,
            last_motion_tick: None,
            wheel_accumulator: 0.0,
            last_wheel_turn: None,
            expanded_toc: HashSet::new(),
            last_auto_scrolled_toc: None,
            focus_actions_visible: false,
            focus_footnotes_visible: false,
            focus_footnote_scroll_delta: 0.0,
        };

        ui.reveal_toolbar(now);
        ui.toolbar_motion.advance(TOOLBAR_MOTION_DURATION);
        ui.schedule_toolbar_hide(now);
        assert_eq!(ui.toolbar_hide_at, Some(now + TOOLBAR_HIDE_DELAY));

        ui.reveal_toolbar(now + TOOLBAR_HIDE_DELAY / 2);
        assert!(ui.toolbar_hide_at.is_none());
        assert!((ui.toolbar_motion.target - 1.0).abs() <= f32::EPSILON);
    }

    #[test]
    fn stationary_pointer_does_not_keep_postponing_toolbar_hide() {
        let now = Instant::now();
        let mut ui = ReaderUiState {
            sidebar_open: false,
            sidebar_pinned: false,
            sidebar_width: super::egui_view::SIDEBAR_WIDTH,
            sidebar_tab: SidebarTab::Toc,
            toolbar_hovered: false,
            toolbar_hide_at: None,
            overlay: ReaderOverlay::None,
            assistant_panel: None,
            assistant_width: super::egui_view::ASSISTANT_WIDTH,
            toolbar_motion: Motion::settled_with_duration(0.0, TOOLBAR_MOTION_DURATION),
            sidebar_motion: Motion::settled(0.0),
            assistant_motion: Motion::settled(0.0),
            menu_motion: Motion::settled(0.0),
            focus_scroll_motion: None,
            last_motion_tick: None,
            wheel_accumulator: 0.0,
            last_wheel_turn: None,
            expanded_toc: HashSet::new(),
            last_auto_scrolled_toc: None,
            focus_actions_visible: false,
            focus_footnotes_visible: false,
            focus_footnote_scroll_delta: 0.0,
        };

        assert!(ui.set_toolbar_hovered(true, now));
        assert!(ui.set_toolbar_hovered(false, now + Duration::from_millis(20)));
        let hide_at = ui.toolbar_hide_at;

        assert!(!ui.set_toolbar_hovered(false, now + Duration::from_millis(200)));
        assert_eq!(ui.toolbar_hide_at, hide_at);
    }

    #[test]
    fn shelf_metadata_overrides_a_hash_based_parser_title() {
        let shelf_metadata = BookDisplayMetadata {
            id: "shelf-id".into(),
            title: "情景学习".into(),
            authors: Vec::new(),
        };

        let resolved = resolve_book_display_metadata(
            Some(shelf_metadata.clone()),
            "parsed-id",
            "21f76642e79935732871e58d99d4e7eb4e890a8ae1ed93f859097b655a37e434",
            &[],
        );

        assert_eq!(resolved, shelf_metadata);
    }

    #[test]
    fn parsed_metadata_remains_the_fallback_for_external_files() {
        let authors = vec!["作者".to_owned()];
        let resolved = resolve_book_display_metadata(None, "parsed-id", "外部文件", &authors);

        assert_eq!(resolved.id, "parsed-id");
        assert_eq!(resolved.title, "外部文件");
        assert_eq!(resolved.authors, authors);
    }

    #[test]
    fn translation_error_notice_auto_dismisses_after_three_seconds() {
        let now = Instant::now();
        let mut translation = TranslationUiState::default();
        translation.show_error("测试错误".into(), now);

        assert_eq!(
            translation.dismiss_at,
            Some(now + NOTICE_AUTO_DISMISS_DELAY)
        );
        assert!(!translation.dismiss_if_due(now + NOTICE_AUTO_DISMISS_DELAY / 2));
        assert_eq!(translation.error.as_deref(), Some("测试错误"));

        assert!(translation.dismiss_if_due(now + NOTICE_AUTO_DISMISS_DELAY));
        assert!(translation.error.is_none());
        assert!(translation.dismiss_at.is_none());
    }

    #[test]
    fn manually_dismissing_translation_notice_cancels_auto_dismiss() {
        let mut translation = TranslationUiState::default();
        translation.show_error("测试错误".into(), Instant::now());

        translation.clear_error();

        assert!(translation.error.is_none());
        assert!(translation.dismiss_at.is_none());
    }

    #[test]
    fn every_transient_message_gets_a_fresh_auto_dismiss_deadline() {
        let now = Instant::now();
        let mut timer = TransientMessageTimer::default();
        let mut message = Some("first".to_owned());

        assert!(!timer.advance(&mut message, now));
        assert_eq!(timer.dismiss_at, Some(now + NOTICE_AUTO_DISMISS_DELAY));

        let replacement_time = now + NOTICE_AUTO_DISMISS_DELAY / 2;
        message = Some("second".to_owned());
        assert!(!timer.advance(&mut message, replacement_time));
        assert_eq!(
            timer.dismiss_at,
            Some(replacement_time + NOTICE_AUTO_DISMISS_DELAY)
        );
        assert!(!timer.advance(
            &mut message,
            replacement_time + NOTICE_AUTO_DISMISS_DELAY / 2
        ));
        assert!(timer.advance(&mut message, replacement_time + NOTICE_AUTO_DISMISS_DELAY));
        assert!(message.is_none());
        assert!(timer.dismiss_at.is_none());
    }

    #[test]
    fn repeating_a_notice_restarts_its_auto_dismiss_deadline() {
        let now = Instant::now();
        let mut timer = TransientMessageTimer::default();
        let mut message = None;
        timer.show(&mut message, "copied".into(), now);

        let repeated_at = now + NOTICE_AUTO_DISMISS_DELAY / 2;
        timer.show(&mut message, "copied".into(), repeated_at);

        assert_eq!(message.as_deref(), Some("copied"));
        assert_eq!(
            timer.dismiss_at,
            Some(repeated_at + NOTICE_AUTO_DISMISS_DELAY)
        );
    }
}
