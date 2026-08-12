use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use peniko::{Blob, Color};
use rebook_formats::{BookFormat, open_file_for_reading as open_publication_file_for_reading};
use rebook_layout::{LayoutViewport, ReaderStyle, SpreadMode};
use rebook_publication::{BookSource, RenditionLayout, Rgba, SourceRange, TableOfContentsOrigin};
use rebook_reader::{
    PageDirection, ReaderPosition, ReaderSectionPage, ReaderSelection, ReaderSession,
    ReaderSnapshot, ReaderTextHit, SelectionGranularity,
};

use crate::async_task::{TaskResult, TaskSlot};
use crate::generated_toc::GeneratedTocDraft;
use crate::highlights::{HighlightStore, StoredHighlight};
use crate::library::LibraryBook;
use crate::plugins::{
    BlockTranslation, BookSearchResult, ChatReadingContext, ChatRequestKind, ChatResponse,
    ChatTurn, PdfOcrSourceController, PdfOcrViewMode, PluginSettings, RewriteBookSource,
    TranslationBlockInput, TranslationBookSource, has_pending_pdf_ocr_task, load_pdf_ocr_source,
};
use crate::preferences::{self, AppLanguage, AppTheme, ReaderPreferences};
use crate::semantic::{SemanticIndexSummary, SemanticSearchScope};
use crate::settings::ReaderSettingsChange;
use crate::sync::{SyncSettings, SyncStore};

const INITIAL_WIDTH: u32 = 1200;
const INITIAL_HEIGHT: u32 = 800;
const MOTION_DURATION: Duration = Duration::from_millis(180);
const TOOLBAR_MOTION_DURATION: Duration = Duration::from_millis(200);
const TOOLBAR_HIDE_DELAY: Duration = Duration::from_millis(500);
const NOTICE_AUTO_DISMISS_DELAY: Duration = Duration::from_secs(3);
const MOTION_EPSILON: f32 = 0.001;
const SEARCH_MARK_COLOR: Color = Color::from_rgba8(250, 204, 21, 89);
const ASSISTANT_MARK_COLOR: Color = Color::from_rgba8(245, 158, 11, 56);
const SCROLL_PAGE_GAP: f32 = 24.0;
const SCROLL_PREVIOUS_REGION_HEIGHT: f32 = 56.0;
const SCROLL_NEXT_REGION_HEIGHT: f32 = 88.0;
static NEXT_SCENE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SEMANTIC_TASK_GENERATION: AtomicU64 = AtomicU64::new(1);

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
use render::{PageSceneKey, PageSceneLayers};

// Reader page colors follow the app theme; the light pair matches
// ReaderStyle::default so existing books keep their warm paper look.
fn apply_theme_colors(style: &mut ReaderStyle, theme: AppTheme) {
    match theme {
        AppTheme::Light => {
            style.foreground = Rgba::BLACK;
            style.background = Rgba {
                red: 250,
                green: 248,
                blue: 243,
                alpha: 255,
            };
        }
        AppTheme::Dark => {
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
    let highlight_store = HighlightStore::from_repository(local_store.clone());
    let highlights = highlight_store.for_book(&book_id);
    let viewport = LayoutViewport::new(INITIAL_WIDTH, INITIAL_HEIGHT)?;
    let reader_preferences = preferences::load_reader_preferences().unwrap_or_else(|error| {
        tracing::warn!(%error, "failed to load reader preferences; using defaults");
        ReaderPreferences::default()
    });
    let mut style = ReaderStyle {
        spread: reader_preferences.spread,
        typography: reader_preferences.typography.clone(),
        ..ReaderStyle::default()
    };
    if fixed_page {
        style.column_gap = 0.0;
    }
    apply_theme_colors(&mut style, reader_preferences.theme);
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
            plugin_settings,
            language: reader_preferences.language,
            selection_granularity: reader_preferences.selection_granularity,
            sync_settings,
            sync_password,
            source_path: path.to_path_buf(),
        },
    ))
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
    sync_settings: SyncSettings,
    sync_password: String,
    search: SearchUiState,
    search_navigation_requested: Option<BookSearchResult>,
    semantic_index: SemanticIndexUiState,
    chat: ChatUiState,
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
    pending_page_turn: Option<PageDirection>,
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
    pages: Vec<ReaderSectionPage>,
    page_tops: Vec<f32>,
    page_heights: Vec<f32>,
    content_height: f32,
    next_button_top: Option<f32>,
}

impl ScrollSectionLayout {
    #[allow(
        clippy::cast_precision_loss,
        reason = "logical page dimensions are GPU-bounded and egui geometry uses f32"
    )]
    fn new(section_index: usize, section_count: usize, pages: Vec<ReaderSectionPage>) -> Self {
        let has_previous = section_index > 0;
        let has_next = section_index + 1 < section_count;
        let mut cursor = if has_previous {
            SCROLL_PREVIOUS_REGION_HEIGHT
        } else {
            0.0
        };
        let mut page_tops = Vec::with_capacity(pages.len());
        let mut page_heights = Vec::with_capacity(pages.len());
        for (index, entry) in pages.iter().enumerate() {
            let page_height = entry.page.height() as f32;
            page_tops.push(cursor);
            page_heights.push(page_height);
            cursor += page_height;
            if index + 1 < pages.len() {
                cursor += SCROLL_PAGE_GAP;
            }
        }
        let chapter_content_bottom = pages
            .last()
            .and_then(|entry| {
                let bottom = entry.page.content_bottom()?;
                page_tops
                    .last()
                    .map(|top| top + bottom.clamp(0.0, entry.page.height() as f32))
            })
            .unwrap_or(cursor);
        let next_button_top = has_next.then_some(chapter_content_bottom + SCROLL_PAGE_GAP);
        let content_height = next_button_top.map_or(cursor, |top| {
            top + SCROLL_NEXT_REGION_HEIGHT - SCROLL_PAGE_GAP
        });
        Self {
            section_index,
            pages,
            page_tops,
            page_heights,
            content_height,
            next_button_top,
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
            (local_y >= 0.0 && local_y < self.page_heights[index]).then_some((index, local_y))
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
    fn is_scroll_mode(&self) -> bool {
        self.reader.style().spread == SpreadMode::Scroll
    }

    fn current_scroll_layout(
        &mut self,
    ) -> Result<Arc<ScrollSectionLayout>, rebook_reader::ReaderError> {
        let section_index = self.snapshot.location.section_index;
        if let Some(layout) = &self.scroll_section
            && layout.section_index == section_index
        {
            return Ok(Arc::clone(layout));
        }
        let pages = self.reader.current_section_pages()?;
        let layout = Arc::new(ScrollSectionLayout::new(
            section_index,
            self.reader.section_count(),
            pages,
        ));
        self.scroll_section = Some(Arc::clone(&layout));
        Ok(layout)
    }

    fn scroll_page_coordinates(&self, x: f32, y: f32) -> Option<(ReaderPosition, f32, f32)> {
        let viewport = self.scroll_viewport?;
        let layout = self.scroll_section.as_ref()?;
        let (index, local_y) = layout.page_at_content_y(viewport.offset_y + y)?;
        Some((layout.pages[index].position, x, local_y))
    }

    fn update_scroll_viewport(&mut self, viewport: ScrollViewportState) {
        let changed = self.scroll_viewport.is_none_or(|previous| {
            (previous.offset_y - viewport.offset_y).abs() > 0.1 || previous.size != viewport.size
        });
        self.scroll_viewport = Some(viewport);
        if changed {
            self.bump_scene_revision();
        }

        let visible_position = self
            .scroll_section
            .as_ref()
            .and_then(|layout| layout.first_visible_page(viewport.offset_y));
        let current = ReaderPosition {
            section_index: self.snapshot.location.section_index,
            segment_index: self.snapshot.location.segment_index,
            page_index: self.snapshot.location.page_index,
        };
        if let Some(position) = visible_position
            && position != current
        {
            match self.reader.set_visible_position(position) {
                Ok(snapshot) => {
                    self.install_snapshot(snapshot);
                    self.persist_progress();
                }
                Err(error) => self.error = Some(format!("更新滑动阅读位置失败：{error}")),
            }
        }
        if changed {
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
    plugin_settings: PluginSettings,
    language: AppLanguage,
    selection_granularity: SelectionGranularity,
    sync_settings: SyncSettings,
    sync_password: String,
    source_path: PathBuf,
}

#[derive(Clone)]
struct SearchTask {
    source: Arc<dyn BookSource>,
    query: String,
    mode: SearchMode,
    scope: SemanticSearchScope,
    book_id: String,
    settings: PluginSettings,
}

pub(crate) type SearchTaskMessage = TaskResult<Vec<BookSearchResult>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SearchMode {
    #[default]
    Text,
    Semantic,
}

#[derive(Clone)]
struct SemanticIndexTask {
    source: Arc<dyn BookSource>,
    settings: PluginSettings,
    generation: u64,
}

pub(crate) enum SemanticIndexTaskMessage {
    Progress {
        id: u64,
        generation: u64,
        completed: usize,
        total: usize,
    },
    Complete {
        generation: u64,
        message: TaskResult<SemanticIndexSummary>,
    },
}

#[derive(Default)]
struct SemanticIndexUiState {
    progress: String,
    task: TaskSlot<SemanticIndexTask>,
}

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
    mode: SearchMode,
    scope: SemanticSearchScope,
    task: TaskSlot<SearchTask>,
}

#[derive(Clone)]
struct ChatTask {
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

pub(crate) type ChatTaskMessage = TaskResult<ChatResponse>;

pub(crate) struct ChatStreamMessage {
    pub(crate) id: u64,
    pub(crate) content: String,
}

struct ChatStreamingState {
    task_id: u64,
    content: String,
}

#[derive(Default)]
struct ChatUiState {
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
    missing: PdfMetadataMissing,
    settings: PluginSettings,
}

pub(crate) enum PdfTocTaskMessage {
    Progress { id: u64, message: String },
    Complete(TaskResult<crate::plugins::PdfMetadataExtraction>),
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
}

impl Motion {
    const fn settled(value: f32) -> Self {
        Self::settled_with_duration(value, MOTION_DURATION)
    }

    const fn settled_with_duration(value: f32, duration: Duration) -> Self {
        Self {
            value,
            start: value,
            target: value,
            elapsed: Duration::ZERO,
            duration,
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
        let eased = 1.0 - (1.0 - progress).powi(3);
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
    last_motion_tick: Option<Instant>,
    wheel_accumulator: f32,
    last_wheel_turn: Option<Instant>,
    expanded_toc: HashSet<String>,
    last_auto_scrolled_toc: Option<String>,
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
            plugin_settings,
            language,
            selection_granularity,
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
                missing: pdf_metadata_missing,
                settings: plugin_settings.clone(),
            });
        }
        let mut semantic_index = SemanticIndexUiState::default();
        if plugin_settings.semantic_search_enabled {
            let semantic_source: Arc<dyn BookSource> = rewrite_source.clone();
            semantic_index.task.begin(SemanticIndexTask {
                source: semantic_source,
                settings: plugin_settings.clone(),
                generation: NEXT_SEMANTIC_TASK_GENERATION.fetch_add(1, Ordering::Relaxed),
            });
            semantic_index.progress = language.text("索引中 0%", "Indexing 0%").into();
        }
        let mut search = SearchUiState::default();
        if plugin_settings.semantic_search_enabled {
            search.mode = SearchMode::Semantic;
        }
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
            search_navigation_requested: None,
            semantic_index,
            chat: ChatUiState::default(),
            chat_markdown: chat_markdown::ChatMarkdownState::default(),
            translation: TranslationUiState::default(),
            pdf_toc,
            pdf_ocr,
            ui: ReaderUiState {
                sidebar_open: true,
                sidebar_pinned: true,
                sidebar_width: egui_view::SIDEBAR_WIDTH,
                sidebar_tab: SidebarTab::Toc,
                toolbar_hovered: false,
                toolbar_hide_at: None,
                overlay: ReaderOverlay::None,
                assistant_panel: None,
                assistant_width: egui_view::ASSISTANT_WIDTH,
                toolbar_motion: Motion::settled_with_duration(0.0, TOOLBAR_MOTION_DURATION),
                sidebar_motion: Motion::settled(1.0),
                assistant_motion: Motion::settled(0.0),
                menu_motion: Motion::settled(0.0),
                last_motion_tick: None,
                wheel_accumulator: 0.0,
                last_wheel_turn: None,
                expanded_toc,
                last_auto_scrolled_toc: None,
            },
            plugin_settings,
            language,
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
            pending_page_turn: None,
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
    source.book().table_of_contents.is_empty()
        || source.table_of_contents_origin() == TableOfContentsOrigin::Fallback
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
    use super::{
        BookDisplayMetadata, Duration, FollowUp, HashSet, Instant, MOTION_DURATION, Motion,
        NOTICE_AUTO_DISMISS_DELAY, ReaderOverlay, ReaderUiState, SidebarTab, SnapshotEffects,
        TOOLBAR_HIDE_DELAY, TOOLBAR_MOTION_DURATION, TransientMessageTimer, TranslationUiState,
        logical_dimension, resolve_book_display_metadata,
    };

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
            last_motion_tick: None,
            wheel_accumulator: 0.0,
            last_wheel_turn: None,
            expanded_toc: HashSet::new(),
            last_auto_scrolled_toc: None,
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
            last_motion_tick: None,
            wheel_accumulator: 0.0,
            last_wheel_turn: None,
            expanded_toc: HashSet::new(),
            last_auto_scrolled_toc: None,
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
