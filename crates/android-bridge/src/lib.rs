//! Android's typed application boundary. Parsing, layout and navigation remain Rust-owned.
use anyrender::ImageRenderer;
use rebook_app_core::library::LocalLibrary;
use rebook_layout::{LayoutViewport, ReaderStyle, SpreadMode, TypesettingMode};
use rebook_publication::{BookSource, PublicationUrl, SourceRange};
use rebook_reader::{PageDirection, ReaderSession, SelectionGranularity};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

mod activity;
#[cfg(target_os = "android")]
mod android;
mod annotations;
mod chat;
mod chat_markup;
mod chat_media;
mod cloud;
mod documents;
mod feedback;
mod interactions;
mod maintenance;
mod mobile_source;
mod pixels;
mod presentation;
mod reflow;
pub use presentation::ReadingControl;
mod scroll;
mod secrets;
mod services;
mod settings;
mod statistics_detail;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("{detail}")]
    Operation { detail: String },
    #[error("Reading session is closed")]
    Closed,
}
type Result<T> = std::result::Result<T, CoreError>;
fn error(e: impl std::fmt::Display) -> CoreError {
    CoreError::Operation {
        detail: e.to_string(),
    }
}

// UniFFI futures do not implicitly enter a Tokio runtime. All work is explicitly
// submitted here; CPU/file operations never execute on Kotlin's calling thread.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .enable_all()
            .build()
            .expect("core runtime")
    })
}
async fn work<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    runtime().spawn_blocking(f).await.map_err(error)?
}

#[derive(Clone, uniffi::Record)]
pub struct BookSummary {
    pub progress: f64,
    pub last_read: u64,
    pub added_at: u64,
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
}
#[derive(Clone, uniffi::Record)]
pub struct TocItem {
    pub id: String,
    pub ancestors: Vec<String>,
    pub has_children: bool,
    pub title: String,
    pub href: String,
    pub depth: u32,
}
#[derive(Clone, uniffi::Record)]
pub struct SearchHit {
    pub title: String,
    pub excerpt: String,
    pub source: String,
}
#[derive(Clone, uniffi::Record)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
#[derive(Clone, uniffi::Record)]
pub struct TextRegion {
    pub text: String,
    pub bounds: Vec<Rect>,
    pub source: String,
}
#[derive(Clone, uniffi::Record)]
pub struct SelectedText {
    pub text: String,
    pub bounds: Vec<Rect>,
    pub highlighted: bool,
}
#[derive(Clone, uniffi::Record)]
pub struct FocusTarget {
    pub delta: f32,
    pub bounds: Vec<Rect>,
}
#[uniffi::export]
pub fn focus_motion_duration(distance: f32) -> u32 {
    rebook_app_core::focus_navigation::duration(distance).as_millis() as u32
}
#[uniffi::export]
pub fn reference_color(dark:bool)->u32 {
    let [r,g,b,a]=rebook_renderer::reference_color(dark);
    u32::from_be_bytes([a,r,g,b])
}
fn reference_ink(s:&ReaderState)->peniko::Color {
    let [r,g,b,a]=rebook_renderer::reference_color(s.session.style().background.red<128);
    peniko::Color::from_rgba8(r,g,b,a)
}
#[uniffi::export]
pub fn page_turn_commit(distance: f32, width: f32, velocity: f32) -> bool {
    rebook_app_core::page_navigation::commit(distance, width, velocity)
}
#[uniffi::export]
pub fn page_turn_resisted_offset(distance: f32, width: f32) -> f32 {
    rebook_app_core::page_navigation::resisted_offset(distance, width)
}
#[derive(Clone, Default, uniffi::Record)]
pub struct PageInfo {
    pub section: u32,
    pub section_count: u32,
    pub page: u32,
    pub page_count: u32,
    pub progress: f64,
    pub regions: Vec<TextRegion>,
    pub marks: Vec<Rect>,
    pub focus_bounds: Vec<Rect>,
    pub focus_fade_color: u32,
    pub focus_highlight: bool,
    pub is_scroll: bool,
    pub scroll_distance: f64,
    pub scroll_context: u64,
    pub raster_top: f32,
    pub raster_height: f32,
    pub previous_focus: Option<FocusTarget>,
    pub next_focus: Option<FocusTarget>,
    pub controls: Vec<ReadingControl>,
    pub active_toc_id: Option<String>,
    pub active_toc_path: Vec<String>,
    pub focus_index: u32,
    pub focus_count: u32,
    pub can_go_back: bool,
}
#[derive(Clone, uniffi::Record)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}
impl Viewport {
    fn validate(&self) -> Result<LayoutViewport> {
        if self.width < 100
            || self.height < 100
            || self.width > 4096
            || self.height > 4096
            || !self.scale.is_finite()
            || !(1.0..=4.0).contains(&self.scale)
            || self.width as f64 * self.height as f64 * (self.scale as f64).powi(2) > 16_777_216.0
        {
            return Err(error("Unsupported viewport dimensions"));
        }
        LayoutViewport::new(self.width, self.height)
            .map(|v| v.with_raster_scale(self.scale))
            .map_err(error)
    }
}

#[derive(uniffi::Object)]
pub struct MobileCore {
    root: PathBuf,
    sync_wake: Arc<rebook_app_core::sync::SyncWake>,
    library_lock: Arc<Mutex<()>>,
    font_download_lock: tokio::sync::Mutex<()>,
}
#[uniffi::export]
impl MobileCore {
    #[uniffi::constructor]
    pub fn new(data_directory: String) -> Result<Arc<Self>> {
        let root = PathBuf::from(data_directory);
        if !root.is_absolute() {
            return Err(error("An absolute application data directory is required"));
        }
        rebook_app_core::environment::initialize(rebook_app_core::environment::AppPaths {
            data: root.clone(),
            roaming: root.clone(),
            config: root.join("config"),
            cache: root.join("cache"),
        })
        .map_err(error)?;
        static HOOK: std::sync::Once = std::sync::Once::new();
        HOOK.call_once(rebook_app_core::diagnostics::install_panic_hook);
        static LIBRARY_LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
        Ok(Arc::new(Self {
            sync_wake:rebook_app_core::sync::SyncWake::for_path(&root.join("sync-v1.sqlite3")),
            root,
            font_download_lock: tokio::sync::Mutex::new(()),
            library_lock: LIBRARY_LOCK
                .get_or_init(|| Arc::new(Mutex::new(())))
                .clone(),
        }))
    }
    pub async fn books(self: Arc<Self>) -> Result<Vec<BookSummary>> {
        work(move || {
            let library = LocalLibrary::load_from(self.root.join("library")).map_err(error)?;
            let store = services::store()?;
            let activity = store.progress_activity_times().map_err(error)?;
            let mut books = library
                .books()
                .iter()
                .map(|b| {
                    Ok(BookSummary {
                        id: b.id.clone(),
                        title: b.title.clone(),
                        authors: b.authors.clone(),
                        added_at: b.added_at,
                        last_read: activity.get(&b.id).copied().unwrap_or(0),
                        progress: store
                            .load_progress(&b.id)
                            .map_err(error)?
                            .and_then(|p| p.locator.total_progression)
                            .unwrap_or(0.0),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            books.sort_by(|a, b| {
                b.last_read
                    .max(b.added_at)
                    .cmp(&a.last_read.max(a.added_at))
                    .then(b.added_at.cmp(&a.added_at))
                    .then(a.id.cmp(&b.id))
            });
            Ok(books)
        })
        .await
    }
    pub async fn cover(self: Arc<Self>, id: String) -> Result<Option<Vec<u8>>> {
        work(move || {
            if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
                return Err(error("Invalid book id"));
            }
            let cache = self.root.join("cache/covers").join(format!("{id}.png"));
            if let Ok(bytes) = std::fs::read(&cache) {
                return Ok(Some(bytes));
            }
            let root = self.root.join("library");
            let bytes = match LocalLibrary::load_cover_from(&root, &id).map_err(error)? {
                Some(bytes) => bytes,
                None => {
                    let Some(path) = LocalLibrary::cover_source_from(&root, &id).map_err(error)? else {
                        return Ok(None);
                    };
                    // Older imports may have saved an empty cover before a parser fix.
                    // Parse only this visible book, on the blocking worker and outside
                    // the library lock; recheck deletion/concurrent sync at commit time.
                    let publication = rebook_formats::open_file(path).map_err(error)?;
                    let Some(extracted) = publication.cover_bytes() else {
                        return Ok(None);
                    };
                    let _guard = self.library_lock.lock().map_err(error)?;
                    let Some(bytes) = LocalLibrary::store_missing_cover_from(&root, &id, extracted)
                        .map_err(error)? else {
                        return Ok(None);
                    };
                    bytes
                }
            };
            let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .map_err(error)?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(64 * 1024 * 1024);
            decoder.limits(limits);
            let image = decoder.decode().map_err(error)?.thumbnail(240, 360);
            let mut output = std::io::Cursor::new(Vec::new());
            image
                .write_to(&mut output, image::ImageFormat::Png)
                .map_err(error)?;
            rebook_app_core::persistence::write_bytes_atomic(&cache, output.get_ref())
                .map_err(error)?;
            Ok(Some(output.into_inner()))
        })
        .await
    }
    pub async fn import_book(self: Arc<Self>, path: String) -> Result<Vec<BookSummary>> {
        let owner = self.clone();
        work(move || {
            let path = PathBuf::from(path);
            if std::fs::metadata(&path).map_err(error)?.len() > 256 * 1024 * 1024 {
                return Err(error("Book exceeds 256 MiB import limit"));
            }
            let _guard = owner.library_lock.lock().map_err(error)?;
            let mut library = LocalLibrary::load_from(owner.root.join("library")).map_err(error)?;
            library.import_files(&[path]).map_err(error)?;
            let store = services::store()?;
            for book in library.books() {
                store.register_book_if_missing(&book.id).map_err(error)?;
            }
            cloud::library_changed(&owner)?;
            Ok(())
        })
        .await?;
        self.books().await
    }
    pub async fn remove_book(self: Arc<Self>, id: String) -> Result<()> {
        work(move || {
            let _guard = self.library_lock.lock().map_err(error)?;
            LocalLibrary::load_from(self.root.join("library"))
                .map_err(error)?
                .remove(&id)
                .map_err(error)?;
            services::store()?
                .set_book_present(&id, false)
                .map_err(error)?;
            cloud::library_changed(&self)
        })
        .await
    }
    pub async fn open_book(
        self: Arc<Self>,
        id: String,
        viewport: Viewport,
    ) -> Result<Arc<MobileReader>> {
        work(move || {
            let viewport = viewport.validate()?;
            let library = LocalLibrary::load_from(self.root.join("library")).map_err(error)?;
            let book = library
                .books()
                .iter()
                .find(|book| book.id == id)
                .ok_or_else(|| error("Book not found"))?;
            let publication = rebook_formats::open_file_for_reading_cached(
                &book.path, Some(&book.id), self.root.join("cache/source-ir"),
            ).map_err(error)?;
            let canonical = mobile_source::for_reading(publication.source());
            let options = settings::load_reading(&self.root);
            let mut style = ReaderStyle::default();
            style.spread = SpreadMode::Single;
            style.focus_footnote_icons = true;
            style.minimum_content_width = 0.0;
            style.typography.font_size = 20.0;
            style.typography.monospace_font = "monospace".into();
            style.typography.sans_serif_font = "sans-serif".into();
            settings::apply_typography(&mut style, &options);
            apply_mobile_page_style(&mut style);
            let fonts =
                rebook_app_core::fonts::store::load(&self.root.join("fonts"), Default::default());
            rebook_app_core::fonts::install(Arc::new(fonts.clone()));
            let settings =
                rebook_app_core::plugins::PluginSettings::load_default().map_err(error)?;
            let original=canonical.clone();
            let pipeline = rebook_app_core::reading::ReadingPipeline::new(
                canonical,
                &id,
                &settings,
                fonts.reader_fonts(),
            );
            pipeline
                .translation
                .set_enabled(options.translated)
                .map_err(error)?;
            let media=Arc::new(rebook_app_core::media::MediaReferenceSource::new(pipeline.structure.clone(),original,
            ));
            let source: Arc<dyn BookSource> = media.clone();
            let mut session = ReaderSession::open_with_fonts(
                source.clone(),
                viewport,
                style,
                fonts.reader_fonts(),
            )
            .map_err(error)?;
            session.set_cache_policy(rebook_reader::ReaderCachePolicy {
                layout_bytes: 24 * 1024 * 1024,
                raster_bytes: 48 * 1024 * 1024,
                prefetch_distance: 1,
            });
            let store = services::store()?;
            store.register_book_if_missing(&id).map_err(error)?;
            let restored_anchor = if let Some(progress) = store.load_progress(&id).map_err(error)? {
                session.restore_locator(&progress.locator).map_err(error)?;
                progress.locator.source
            } else { None };
            store.wake().protect_progress(&id);
            store.wake().active(Some(id.clone()));
            store
                .record_reading_activity(&id, &session.current_locator())
                .map_err(error)?;
            rebook_app_core::statistics::register_book(book);
            // Conversation belongs to this reader instance; only transient book
            // image pixels use a session-scoped cache, never a transcript file.
            let chat_path = self.root.join("cache/chat").join(uuid::Uuid::new_v4().to_string());
            let messages = Vec::new();
            let session_typesetting = session.style().typesetting;
            Ok(Arc::new(MobileReader {
                scheduler: Default::default(),
                ai: Default::default(),
                enhancement_claim: Default::default(),
                ai_snapshots: Default::default(),
                ai_delivery: Default::default(),
                state: Mutex::new(Some(ReaderState {
                    session,
                    source,
                    store,
                    book_id: id,
                    pipeline,
                    media,
                    document: None,
                    selected: None,
                    selection_mode: SelectionGranularity::Free,
                    tracker: rebook_app_core::statistics::Tracker::new(&book.id),
                    format: publication.format(),
                    chat_path,
                    messages,
                    pending: None,
                    translation_config: None,
                    enhancements: Default::default(),
                    staged_layout: vec![],
                    toc_labels: Default::default(),
                    toc_config: None,
                    options,
                    root: self.root.clone(),
                    scroll_offset: 0.0,
                    scroll_distance: 0.0,
                    scroll_context: scroll::context_id(),
                    scroll_cache: None,
                    scroll_center: true,
                    scroll_slices: vec![],
                    selection_start: None,
                    selection_end: None,
                    selection_drag_anchor: None,
                    classic_typesetting: session_typesetting,
                    focus: false,
                    focus_units: vec![],
                    focus_anchor: restored_anchor,
                    focus_history: Default::default(),
                    history: vec![],
                })),
            }))
        })
        .await
    }
}
struct ReaderState {
    session: ReaderSession,
    source: Arc<dyn BookSource>,
    store: rebook_app_core::sync::SyncStore,
    book_id: String,
    pipeline: rebook_app_core::reading::ReadingPipeline,
    media: Arc<rebook_app_core::media::MediaReferenceSource>,
    document: Option<documents::Document>,
    selected: Option<rebook_reader::ReaderSelection>,
    selection_mode: SelectionGranularity,
    tracker: rebook_app_core::statistics::Tracker,
    format: rebook_formats::BookFormat,
    chat_path: PathBuf,
    messages: Vec<chat::ChatMessage>,
    pending: Option<chat::PendingChanges>,
    translation_config: Option<String>,
    enhancements: services::enhancements::State,
    staged_layout: Vec<services::requests::Staged>,
    toc_labels: std::collections::HashMap<String, String>,
    toc_config: Option<String>,
    options: settings::ReadingOptions,
    root: PathBuf,
    scroll_offset: f32,
    scroll_distance: f64,
    scroll_context: u64,
    scroll_cache: Option<scroll::Cache>,
    scroll_center: bool,
    scroll_slices: Vec<scroll::Slice>,
    selection_start: Option<rebook_reader::ReaderTextHit>,
    selection_end: Option<rebook_reader::ReaderTextHit>,
    selection_drag_anchor: Option<rebook_reader::ReaderTextHit>,
    focus: bool,
    classic_typesetting: rebook_layout::ReaderTypesetting,
    focus_units: Vec<interactions::FocusUnit>,
    focus_anchor: Option<SourceRange>,
    focus_history: rebook_app_core::focus_navigation::UnitHistory<reflow::Anchor>,
    history: Vec<rebook_publication::LocatorV1>,
}
#[derive(uniffi::Object)]
pub struct MobileReader {
    scheduler: rebook_app_core::reading_ai::Scheduler,
    ai: Mutex<services::driver::State>,
    enhancement_claim: tokio::sync::Mutex<()>,
    ai_snapshots: rebook_app_core::reading_ai::batching::Snapshots,
    ai_delivery: Mutex<()>,
    state: Mutex<Option<ReaderState>>,
}

#[derive(uniffi::Record)]
pub struct ReaderTurn {
    pub page: Arc<MobilePage>,
    pub reached_end: bool,
}

fn turn_result(s: &mut ReaderState, forward: bool, whole_unit: bool) -> Result<ReaderTurn> {
    s.store.wake().allow_progress(&s.book_id);
    if whole_unit {
        return scroll::turn_unit_result(s,forward).map(|(page,reached_end)|ReaderTurn {page,reached_end});
    }
    clear_selection_state(s);
    if s.focus {
        // Vertical focus stops are not synthetic book-completion pages.
        return scroll::turn(s,forward).map(|page|ReaderTurn {page,reached_end:false,
        });
    }
    let result=s.session.turn_page(if forward {PageDirection::Next}else{PageDirection::Previous}).map_err(error)?;
    let reached_end=forward&&result.outcome==rebook_reader::NavigationOutcome::Boundary;
    render(s).map(|page|ReaderTurn {page,reached_end})
}
impl MobileReader {
    fn with<T>(&self, f: impl FnOnce(&mut ReaderState) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().map_err(error)?;
        f(state.as_mut().ok_or(CoreError::Closed)?)
    }
}
#[uniffi::export]
impl MobileReader {
    pub async fn shutdown(self: Arc<Self>) -> Result<()> {
        self.stop_ai();
        work(move || {
            let state = self.state.lock().map_err(error)?.take();
            if let Some(s)=&state {let _=std::fs::remove_dir_all(s.chat_path.with_extension("images"));}
            if let Some(s)=&state {let wake=s.store.wake();if wake.snapshot().active.as_deref()==Some(&s.book_id){wake.active(None);wake.changed(rebook_app_core::sync::SyncChange::Flush);}}
            drop(state);
            Ok(())
        })
        .await
    }
    pub async fn toc(self: Arc<Self>) -> Result<Vec<TocItem>> {
        work(move || self.with(|s| Ok(services::toc_items(s)))).await
    }
    pub async fn book_languages(self: Arc<Self>) -> Result<Vec<String>> {
        work(move || self.with(|s| Ok(s.source.book().metadata.languages.clone()))).await
    }
    pub async fn page(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        work(move || self.with(render)).await
    }
    /// Render one neighbour without publishing progress, selection or navigation.
    /// Hosts retain at most the preceding and following viewport bitmaps.
    pub async fn preview_turn(self: Arc<Self>, forward: bool) -> Result<Option<Arc<MobilePage>>> {
        work(move || {
            self.with(|s| {
            if s.focus { return scroll::preview_unit(s,forward); }
            let locator = s.session.current_locator();
            let position = scroll::position(s);
            let result = (|| {
                s.session.turn_page(if forward { PageDirection::Next } else { PageDirection::Previous }).map_err(error)?;
                if scroll::position(s) == position { return Ok(None); }
                render_page_with_progress(s, false).map(Some)
            })();
            // Restoration also runs when parsing/rendering the neighbouring chapter fails.
            scroll::restore_position(s, &locator, position)?;
            result
        })
        }).await
    }
    pub async fn turn(self: Arc<Self>, forward: bool) -> Result<Arc<MobilePage>> {
        work(move ||self.with(|s|turn_result(s,forward,false).map(|result|result.page))).await
    }
    /// Distinguish an actual forward book boundary from an unavailable preview.
    pub async fn turn_with_result(self: Arc<Self>, forward: bool, whole_unit: bool,
    ) -> Result<ReaderTurn> {
        work(move ||self.with(|s|turn_result(s,forward,whole_unit))).await
    }
    pub async fn turn_reading_unit(self: Arc<Self>, forward: bool) -> Result<Arc<MobilePage>> {
        work(move ||self.with(|s|{s.store.wake().allow_progress(&s.book_id);scroll::turn_unit(s,forward)})).await
    }
    pub async fn resize(self: Arc<Self>, viewport: Viewport) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                s.scroll_center = true;
                scroll::invalidate(s);
                s.session.resize(viewport.validate()?).map_err(error)?;
                render(s)
            })
        })
        .await
    }
    pub async fn configure(
        self: Arc<Self>,
        font_size: f32,
        unified: bool,
        dark: bool,
    ) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                if !font_size.is_finite() || !(12.0..=48.0).contains(&font_size) {
                    return Err(error("Invalid font size"));
                }
                let mut style = s.session.style();
                settings::apply_typography(&mut style, &s.options);
                style.typography.font_size = font_size;
                let mode = if unified {
                    TypesettingMode::Unified
                } else {
                    TypesettingMode::Book
                };
                s.classic_typesetting = rebook_app_core::reading::typesetting(&s.classic_typesetting,mode);
                apply_presentation_mode(s, &mut style)?;
                if dark {
                    style.background = rebook_publication::Rgba {
                        red: 24,
                        green: 28,
                        blue: 27,
                        alpha: 255,
                    };
                    style.foreground = rebook_publication::Rgba {
                        red: 230,
                        green: 232,
                        blue: 228,
                        alpha: 255,
                    };
                } else {
                    style.background = ReaderStyle::default().background;
                    style.foreground = ReaderStyle::default().foreground;
                }
                s.scroll_center = true;
                scroll::invalidate(s);
                s.session.refresh_source_with_style(style).map_err(error)?;
                render(s)
            })
        })
        .await
    }
    pub async fn navigate(self: Arc<Self>, href: String) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {s.store.wake().allow_progress(&s.book_id);interactions::follow(s, &PublicationUrl::parse(&href).map_err(error)?)})
        })
        .await
    }
    pub async fn navigate_toc(self: Arc<Self>, id: String) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                s.store.wake().allow_progress(&s.book_id);
                let locator = s.session.current_locator();
                s.session.go_to_toc(&id).map_err(error)?;
                s.history.push(locator);
                if s.history.len() > 32 {
                    s.history.remove(0);
                }
                s.focus_anchor = s
                    .session
                    .source_anchor_for_toc(&id)
                    .map(|start| SourceRange {
                        end: start.clone(),
                        start,
                    });
                s.scroll_center = true;
                scroll::invalidate(s);
                render(s)
            })
        })
        .await
    }
    pub async fn search(self: Arc<Self>, query: String) -> Result<Vec<SearchHit>> {
        work(move || {
            self.with(|s| {
                rebook_app_core::search::search_book(s.source.as_ref(), &query, 200)
                    .map_err(error)?
                    .into_iter()
                    .map(|hit| {
                        Ok(SearchHit {
                            title: hit.section_title,
                            excerpt: hit.excerpt,
                            source: serde_json::to_string(&hit.range).map_err(error)?,
                        })
                    })
                    .collect()
            })
        })
        .await
    }
    pub async fn go_to_source(self: Arc<Self>, source: String) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                s.store.wake().allow_progress(&s.book_id);
                let range: SourceRange = serde_json::from_str(&source).map_err(error)?;
                s.scroll_center = true;
                scroll::invalidate(s);
                let previous = s.session.current_locator();
                s.session.go_to_source(&range.start).map_err(error)?;
                s.history.push(previous);
                if s.history.len() > 32 {
                    s.history.remove(0);
                }
                s.focus_anchor = Some(range);
                render(s)
            })
        })
        .await
    }
    pub async fn clear_selection(self: Arc<Self>) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.selected = None;
                s.selection_start = None;
                s.selection_end = None;
                s.selection_drag_anchor = None;
                Ok(())
            })
        })
        .await
    }
    pub async fn selection_mode(self: Arc<Self>, mode: String) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.selection_mode = match mode.as_str() {
                    "free" => SelectionGranularity::Free,
                    "word" => SelectionGranularity::Word,
                    "sentence" => SelectionGranularity::Sentence,
                    "paragraph" => SelectionGranularity::Paragraph,
                    _ => return Err(error("Invalid selection mode")),
                };
                Ok(())
            })
        })
        .await
    }
    pub async fn select(
        self: Arc<Self>,
        x: f32,
        y: f32,
        end_x: f32,
        end_y: f32,
    ) -> Result<Option<SelectedText>> {
        work(move || {
            self.with(|s| {
                if ![x, y, end_x, end_y].into_iter().all(f32::is_finite) {
                    return Err(error("Invalid selection coordinates"));
                }
                s.selected = None;
                let Some(a) = scroll::hit(s, x, y, false)? else {
                    return Ok(None);
                };
                let Some(b) = scroll::hit(s, end_x, end_y, false)? else {
                    return Ok(None);
                };
                if a.ordering_key() <= b.ordering_key() {
                    s.selection_start = Some(a.clone());
                    s.selection_end = Some(b.clone());
                } else {
                    s.selection_start = Some(b.clone());
                    s.selection_end = Some(a.clone());
                }
                let selected = s
                    .session
                    .selection_between_with_granularity(&a, &b, s.selection_mode)
                    .map_err(error)?;
                s.selected = selected.clone();
                scroll::selected(s)
            })
        })
        .await
    }
}
fn clear_selection_state(s: &mut ReaderState) {
    s.selected = None;
    s.selection_start = None;
    s.selection_end = None;
    s.selection_drag_anchor = None;
}
fn apply_presentation_mode(s: &mut ReaderState, style: &mut ReaderStyle) -> Result<()> {
    style.typesetting = rebook_app_core::reading::effective_typesetting(s.focus, &s.classic_typesetting);
    apply_mobile_page_style(style);
    let settings = rebook_app_core::plugins::PluginSettings::load_default().map_err(error)?;
    s.pipeline.semantic.configure_presentation(
        &rebook_app_core::plugins::pdf_native::cache_identity(&s.book_id, s.pipeline.rewrite.as_ref(),
        ),
        &settings,
        style.typesetting.mode == TypesettingMode::Unified,
    );
    Ok(())
}
// Body reflows and reference previews share the same mobile presentation.
fn apply_mobile_typesetting(typesetting: &mut rebook_layout::ReaderTypesetting) {
    typesetting.quote_style = rebook_layout::QuoteStyle::Filled;
    typesetting.fit_block_images = true;
}
fn apply_mobile_page_style(style: &mut ReaderStyle) {
    apply_mobile_typesetting(&mut style.typesetting);
    // All mobile reading modes share the same app frame; authored block spacing
    // remains part of the book's own layout.
    style.horizontal_margin = 20.0;
    style.top_margin = 16.0;
    style.bottom_margin = 16.0;
}
fn render(s: &mut ReaderState) -> Result<Arc<MobilePage>> {
    if s.enhancements.dirty {
        reflow::refresh(s)?;
    }
    clear_selection_state(s);
    if s.focus {
        scroll::render(s)
    } else {
        render_page(s)
    }
}
fn page_info(s: &mut ReaderState, persist: bool) -> Result<PageInfo> {
    let fragments = s.session.current_visible_text_fragments().map_err(error)?;
    let display = s.session.current_page();
    let regions = fragments
        .into_iter()
        .map(|fragment| TextRegion {
            text: fragment.text,
            bounds: display
                .source_rects(std::slice::from_ref(&fragment.range))
                .into_iter()
                .map(|r| Rect {
                    x: r.x0 as f32,
                    y: r.y0 as f32,
                    width: r.width() as f32,
                    height: r.height() as f32,
                })
                .collect(),
            source: serde_json::to_string(&fragment.range).unwrap_or_default(),
        })
        .collect();
    let marks = s
        .store
        .annotations_for_book(&s.book_id)
        .map_err(error)?
        .iter()
        .flat_map(|a| display.source_rects(&a.ranges))
        .map(|r| Rect {
            x: r.x0 as f32,
            y: r.y0 as f32,
            width: r.width() as f32,
            height: r.height() as f32,
        })
        .collect();
    s.focus_units = interactions::focus_units(s)?;
    let index = s
        .focus_anchor
        .as_ref()
        .and_then(|a| {
            s.focus_units.iter().position(|u| {
                u.ranges
                    .iter()
                    .any(|r| r.start.spine == a.start.spine && r.start.node == a.start.node)
            })
        })
        .unwrap_or(0);
    s.focus_anchor = s
        .focus_units
        .get(index)
        .and_then(|u| u.ranges.first().cloned());
    finish_page_info(s, regions, marks, index, false, persist)
}
fn sync_locator(s:&ReaderState,index:usize,is_scroll:bool)->rebook_publication::LocatorV1 {
    let mut locator = s.session.current_locator();
    if is_scroll && let Some(unit) = s.focus_units.get(index) {
        let continuation = locator.source.as_ref().is_some_and(|leading| {
            unit.ranges
                .iter()
                .any(|r| r.start.spine == leading.start.spine && r.start.node == leading.start.node)
        });
        if !continuation && let Some(anchor) = unit.ranges.first() {
            locator.source = Some(anchor.clone());
            if let Some(section) = s
                .source
                .book()
                .sections
                .iter()
                .find(|p| p.id == anchor.start.spine)
            {
                locator.href = section.href.clone();
            }
        }
    }
    locator
}
fn finish_page_info(
    s: &mut ReaderState,
    regions: Vec<TextRegion>,
    marks: Vec<Rect>,
    index: usize,
    is_scroll: bool,
    persist: bool,
) -> Result<PageInfo> {
    let focus_bounds = s
        .focus_units
        .get(index)
        .map_or_else(Vec::new, |u| u.paint_bounds());
    let state = s.session.snapshot();
    let locator=sync_locator(s,index,is_scroll);
    if persist {
        s.store.save_progress(&s.book_id, &locator).map_err(error)?;
    }
    let controls=presentation::controls(s)?;
    Ok(PageInfo {
        active_toc_id:state.active_toc_id,
        active_toc_path:state.active_toc_path,
        controls,
        section: state.location.section_index as u32,
        section_count: s.source.book().sections.len() as u32,
        page: state.location.page_index as u32,
        page_count: state.location.page_count as u32,
        progress: state.total_progression,
        regions,
        marks,
        focus_bounds,
        focus_highlight: false,
        is_scroll,
        scroll_distance: s.scroll_distance,
        scroll_context: s.scroll_context,
        raster_top: 0.0,
        raster_height: s.session.viewport().height as f32,
        previous_focus: None,
        next_focus: None,
        focus_fade_color: {
            let paper = s.session.style().background;
            let alpha = ((1.0 - rebook_renderer::FOCUS_INACTIVE_OPACITY) * 255.0).round() as u32;
            (alpha << 24)
                | (u32::from(paper.red) << 16)
                | (u32::from(paper.green) << 8)
                | u32::from(paper.blue)
        },
        focus_index: index as u32,
        focus_count: s.focus_units.len() as u32,
        can_go_back: !s.history.is_empty(),
    })
}
fn render_page(s: &mut ReaderState) -> Result<Arc<MobilePage>> {
    render_page_with_progress(s, true)
}
fn render_page_with_progress(s: &mut ReaderState, persist: bool) -> Result<Arc<MobilePage>> {
    let info = page_info(s, persist)?;
    let viewport = s.session.viewport();
    let width = (viewport.width as f32 * viewport.raster_scale).ceil() as u32;
    let height = (viewport.height as f32 * viewport.raster_scale).ceil() as u32;
    let display = s.session.current_page();
    let mut rasterizer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
    let mut pixels = Vec::new();
    rasterizer.render_to_vec(
        |scene| {
            display.paint_scaled(scene, viewport.raster_scale);
            display.paint_footnote_icons_scaled(
                scene,
                reference_ink(s),
                viewport.raster_scale);
        },
        &mut pixels,
    );
    Ok(MobilePage::new(width, height, pixels, info))
}
pub use pixels::MobilePage;

#[cfg(test)]
mod tests;
