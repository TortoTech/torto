use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, RichText, TextureHandle, Vec2};
use peniko::Blob;

use crate::async_task::{TaskResult, TaskSlot};
use crate::library::{LibraryBook, LocalLibrary};
use crate::preferences::{self, AppLanguage};
use crate::reader::{BookDisplayMetadata, DesktopReader, open_reader};
use crate::settings::AppliedSettings;
use crate::sync::{
    LocalSyncBook, SyncProgress, SyncReport, SyncSettings, SyncStage, SyncStore, append_sync_log,
    format_error_chain, run_sync,
};
use crate::ui::{
    Icon, ToastKind, decode_color_image, dialog_action_button, dialog_danger_button, icon,
    icon_button, paint_icon, palette, show_toast,
};

const NOTICE_AUTO_DISMISS_DELAY: Duration = Duration::from_secs(3);
// Keep shelf notifications below the 28 px top inset and 44 px header so they
// never cover the import or settings actions. Success, error, and sync progress
// all flow through the same shelf notification slot.
const SHELF_TOAST_TOP_OFFSET: f32 = 84.0;
const SHELF_SCROLLBAR_GUTTER: f32 = 16.0;
const CARD_WIDTH: f32 = 180.0;
const CARD_HEIGHT: f32 = 300.0;
const COVER_WIDTH: f32 = 160.0;
const COVER_HEIGHT: f32 = 228.0;
const SHELF_TITLE_BOLD_OFFSET: f32 = 0.45;

fn sync_progress_text(
    language: AppLanguage,
    stage: SyncStage,
    completed: u64,
    total: u64,
) -> String {
    let label = match stage {
        SyncStage::Checking => language.text("检查同步状态", "Checking sync status"),
        SyncStage::Uploading => language.text("上传中", "Uploading"),
        SyncStage::Downloading => language.text("下载中", "Downloading"),
        SyncStage::ReadingData => language.text("同步阅读数据", "Syncing reading data"),
        SyncStage::DerivedData => language.text("同步 OCR 数据", "Syncing OCR data"),
    };
    let percent = if total == 0 {
        100
    } else {
        u64::try_from(
            (u128::from(completed).saturating_mul(100) + u128::from(total / 2)) / u128::from(total),
        )
        .unwrap_or(100)
        .min(100)
    };
    format!("{label} {percent}%")
}

fn sync_progress_log(progress: &SyncProgress) -> Option<String> {
    match progress {
        SyncProgress::Stage {
            stage,
            completed,
            total,
        } if *completed == 0 || *completed == *total => {
            Some(format!("sync stage={stage:?} progress={completed}/{total}"))
        }
        SyncProgress::Downloaded {
            completed, total, ..
        } => Some(format!("sync downloaded_book progress={completed}/{total}")),
        SyncProgress::Stage { .. } => None,
    }
}

pub(crate) struct ShelfFeature {
    statistics: crate::statistics::Page,
    shelf: ShelfState,
    import_task: TaskSlot<()>,
    pending_reader: Option<DesktopReader>,
    reader_fonts: Arc<[Blob<u8>]>,
    local_store: Option<SyncStore>,
    sync: SyncUiState,
    language: AppLanguage,
    search_shortcut: egui::KeyboardShortcut,
    import_books_shortcut: egui::KeyboardShortcut,
    settings_requested: bool,
    cover_textures: HashMap<String, TextureHandle>,
    read_activity: HashMap<String, u64>,
}

struct ShelfState {
    library: LocalLibrary,
    query: String,
    notice: Option<String>,
    notice_dismiss_at: Option<Instant>,
    error: Option<String>,
    error_dismiss_at: Option<Instant>,
    remove_confirmation: Option<ShelfRemoveConfirmation>,
    selected_book_id: Option<String>,
    focus_selected_book: bool,
}

struct SyncUiState {
    settings: SyncSettings,
    password: String,
    task: TaskSlot<SyncTask>,
    status: String,
    imported_books: usize,
    import_error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SyncTask {
    pub(crate) settings: SyncSettings,
    pub(crate) password: String,
    pub(crate) books: Vec<LocalSyncBook>,
}

pub(crate) type SyncTaskMessage = TaskResult<SyncReport>;
pub(crate) struct SyncProgressMessage {
    pub(crate) id: u64,
    pub(crate) progress: SyncProgress,
}
pub(crate) type ShelfImportTaskMessage = TaskResult<Option<Vec<PathBuf>>>;

#[derive(Clone, Debug)]
struct ShelfRemoveConfirmation {
    id: String,
    title: String,
}

impl ShelfFeature {
    pub(crate) fn update_book_metadata(
        &mut self,
        book_id: &str,
        title: &str,
        authors: &[String],
    ) -> crate::library::LibraryResult<bool> {
        self.shelf.library.update_metadata(book_id, title, authors)
    }

    pub(crate) fn new(library: LocalLibrary, reader_fonts: Arc<[Blob<u8>]>) -> Self {
        let (language, language_error) = preferences::load_app_language().map_or_else(
            |error| {
                (
                    AppLanguage::default(),
                    Some(format!("加载通用设置失败：{error}")),
                )
            },
            |language| (language, None),
        );
        let (settings, settings_error) = SyncSettings::load_default().map_or_else(
            |error| {
                (
                    SyncSettings::new_device(),
                    Some(format!("加载 WebDAV 同步设置失败：{error}")),
                )
            },
            |settings| (settings, None),
        );
        let (password, password_error) = settings.load_password().map_or_else(
            |error| {
                (
                    String::new(),
                    Some(format!("读取 Windows 凭据失败：{error}")),
                )
            },
            |password| (password, None),
        );
        let (local_store, store_error) = SyncStore::open_default(settings.device_id.clone())
            .map_or_else(
                |error| (None, Some(format!("打开本地阅读数据库失败：{error}"))),
                |store| (Some(store), None),
            );
        let initial_error = language_error
            .or(settings_error)
            .or(password_error)
            .or(store_error);
        let can_start_sync = initial_error.is_none();
        let initial_error_dismiss_at = initial_error
            .as_ref()
            .map(|_| Instant::now() + NOTICE_AUTO_DISMISS_DELAY);
        let mut feature = Self {
            statistics: crate::statistics::Page::default(),
            shelf: ShelfState {
                library,
                query: String::new(),
                notice: None,
                notice_dismiss_at: None,
                error: initial_error,
                error_dismiss_at: initial_error_dismiss_at,
                remove_confirmation: None,
                selected_book_id: None,
                focus_selected_book: true,
            },
            import_task: TaskSlot::default(),
            pending_reader: None,
            reader_fonts,
            local_store,
            sync: SyncUiState {
                settings,
                password,
                task: TaskSlot::default(),
                status: String::new(),
                imported_books: 0,
                import_error: None,
            },
            language,
            search_shortcut: egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::F),
            import_books_shortcut: egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::O),
            settings_requested: false,
            cover_textures: HashMap::new(),
            read_activity: HashMap::new(),
        };
        feature.refresh_read_activity();
        if can_start_sync {
            feature.start_sync();
        }
        feature
    }

    pub(crate) fn open_book(&mut self, path: &Path) {
        let Some(local_store) = self.local_store.clone() else {
            self.show_error(
                self.language
                    .text(
                        "本地阅读数据库不可用，无法打开书籍",
                        "The local reading database is unavailable, so the book cannot be opened",
                    )
                    .into(),
            );
            return;
        };
        let (book, imported) = match self.shelf.library.import_for_open(path) {
            Ok(result) => result,
            Err(error) => {
                self.show_error(format!(
                    "{}: {error}",
                    self.language.text("无法打开书籍", "Unable to open book")
                ));
                return;
            }
        };
        if imported {
            self.cover_textures.clear();
        }
        let metadata = Some(BookDisplayMetadata::from(&book));
        crate::statistics::register_book(&book);
        match open_reader(
            &book.path,
            Arc::clone(&self.reader_fonts),
            metadata,
            book.cover_bytes.clone(),
            local_store,
        ) {
            Ok(reader) => {
                self.pending_reader = Some(reader);
                self.shelf.error = None;
                self.shelf.error_dismiss_at = None;
            }
            Err(error) => {
                self.show_error(format!(
                    "{}: {error}",
                    self.language.text("无法打开书籍", "Unable to open book")
                ));
            }
        }
    }

    fn import_books(&mut self, paths: &[PathBuf]) {
        self.shelf.error = None;
        match self.shelf.library.import_files(paths) {
            Ok(summary) => {
                self.cover_textures.clear();
                let message = match (
                    self.language.resolved(),
                    summary.imported,
                    summary.duplicates,
                ) {
                    (AppLanguage::SimplifiedChinese, 0, duplicate) => {
                        format!("所选的 {duplicate} 本书已在书架中")
                    }
                    (AppLanguage::SimplifiedChinese, imported, 0) => {
                        format!("已导入 {imported} 本书")
                    }
                    (AppLanguage::SimplifiedChinese, imported, duplicate) => {
                        format!("已导入 {imported} 本书，跳过 {duplicate} 本重复书籍")
                    }
                    (AppLanguage::English, 0, duplicate) => {
                        format!("All {duplicate} selected books are already on the shelf")
                    }
                    (AppLanguage::English, imported, 0) => format!("Imported {imported} books"),
                    (AppLanguage::English, imported, duplicate) => {
                        format!("Imported {imported} books and skipped {duplicate} duplicates")
                    }
                    (AppLanguage::System, _, _) => unreachable!(),
                };
                self.show_notice(message);
            }
            Err(error) => {
                self.show_error(format!(
                    "{}: {error}",
                    self.language.text("导入失败", "Import failed")
                ));
            }
        }
    }

    fn remove_book(&mut self, id: &str) {
        match self.shelf.library.remove(id) {
            Ok(true) => {
                if let Some(store) = &self.local_store
                    && let Err(error) = store.set_book_present(id, false)
                {
                    tracing::warn!(%error, "failed to persist local book removal tombstone");
                }
                self.cover_textures.remove(id);
                self.show_notice(
                    self.language
                        .text("已从本地书架移除", "Removed from the local shelf")
                        .into(),
                );
                self.shelf.error = None;
            }
            Ok(false) => {
                self.show_error(
                    self.language
                        .text(
                            "书籍已不在本地书架中",
                            "The book is no longer on the local shelf",
                        )
                        .into(),
                );
            }
            Err(error) => {
                self.show_error(format!(
                    "{}: {error}",
                    self.language.text("移除失败", "Remove failed")
                ));
            }
        }
    }

    pub(crate) fn take_opened_reader(&mut self) -> Option<DesktopReader> {
        self.pending_reader.take()
    }

    pub(crate) fn take_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    pub(crate) fn apply_global_settings(&mut self, settings: &AppliedSettings) {
        self.language = settings.language;
        self.search_shortcut = settings.shortcuts.search;
        self.import_books_shortcut = settings.shortcuts.import_books;
        self.sync.settings.clone_from(&settings.sync_settings);
        self.sync.password.clone_from(&settings.sync_password);
        self.start_sync();
    }

    pub(crate) fn resume(&mut self) {
        if let Ok(language) = preferences::load_app_language() {
            self.language = language;
        }
        self.refresh_read_activity();
        self.shelf.selected_book_id = None;
        self.shelf.focus_selected_book = true;
        self.start_sync();
    }

    fn refresh_read_activity(&mut self) {
        let open = self.statistics.open;
        self.statistics
            .open(self.shelf.library.books(), self.local_store.as_ref());
        self.statistics.open = open;
        let Some(store) = &self.local_store else {
            self.read_activity.clear();
            return;
        };
        match store.progress_activity_times() {
            Ok(activity_times) => self.read_activity = activity_times,
            Err(error) => {
                tracing::warn!(%error, "failed to load shelf reading activity");
                self.read_activity.clear();
            }
        }
    }

    fn start_sync(&mut self) {
        if self.sync.task.is_pending() || !self.sync.settings.enabled {
            return;
        }
        if let Err(error) = self.sync.settings.validate() {
            self.show_error(format!(
                "{}: {error}",
                self.language.text("无法开始同步", "Unable to start sync")
            ));
            return;
        }
        if self.sync.password.is_empty() {
            self.show_error(
                self.language
                    .text(
                        "无法开始同步：请先填写 WebDAV 密码",
                        "Unable to start sync: enter the WebDAV password first",
                    )
                    .into(),
            );
            return;
        }
        self.sync.status = self
            .language
            .text("正在同步书籍与阅读数据…", "Syncing books and reading data…")
            .into();
        self.sync.imported_books = 0;
        self.sync.import_error = None;
        self.shelf.notice = Some(self.sync.status.clone());
        self.shelf.notice_dismiss_at = None;
        self.sync.task.begin(SyncTask {
            settings: self.sync.settings.clone(),
            password: self.sync.password.clone(),
            books: self
                .shelf
                .library
                .books()
                .iter()
                .map(|book| LocalSyncBook {
                    id: book.id.clone(),
                    title: book.title.clone(),
                    authors: book.authors.clone(),
                    file_name: book.file_name.clone(),
                    path: book.path.clone(),
                    cover_bytes: book.cover_bytes.clone(),
                    added_at: book.added_at,
                })
                .collect(),
        });
    }

    pub(crate) fn spawn_pending_tasks(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<crate::platform::UserEvent>,
    ) {
        if let Some(request) = self.import_task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let paths = rfd::AsyncFileDialog::new()
                    .add_filter(
                        "E-books",
                        &[
                            "epub", "mobi", "azw", "azw3", "fb2", "fbz", "cbz", "chm", "pdf",
                        ],
                    )
                    .pick_files()
                    .await
                    .map(|files| {
                        files
                            .into_iter()
                            .map(|file| file.path().to_path_buf())
                            .collect()
                    });
                let _ = proxy.send_event(crate::platform::UserEvent::ShelfImport(
                    ShelfImportTaskMessage {
                        id: request.id,
                        result: Ok(paths),
                    },
                ));
            });
        }

        if let Some(request) = self.sync.task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let id = request.id;
                let payload = request.payload;
                if let Err(log_error) = append_sync_log(
                    "INFO",
                    &format!("sync started books={}", payload.books.len()),
                ) {
                    tracing::warn!(%log_error, "failed to append WebDAV sync log");
                }
                let progress_proxy = proxy.clone();
                let result = match run_sync(
                    payload.settings,
                    payload.password,
                    payload.books,
                    move |progress| {
                        if let Some(message) = sync_progress_log(&progress)
                            && let Err(error) = append_sync_log("INFO", &message)
                        {
                            tracing::warn!(%error, "failed to append WebDAV sync log");
                        }
                        let _ = progress_proxy.send_event(
                            crate::platform::UserEvent::ShelfSyncProgress(SyncProgressMessage {
                                id,
                                progress,
                            }),
                        );
                    },
                )
                .await
                {
                    Ok(report) => {
                        if let Err(log_error) = append_sync_log(
                            "INFO",
                            &format!(
                                "sync completed uploaded_books={} downloads={} updated_progress={}",
                                report.uploaded_books,
                                report.downloaded_books,
                                report.updated_progress,
                            ),
                        ) {
                            tracing::warn!(%log_error, "failed to append WebDAV sync log");
                        }
                        Ok(report)
                    }
                    Err(error) => {
                        let detail = format_error_chain(error.as_ref());
                        if let Err(log_error) = append_sync_log("ERROR", &detail) {
                            tracing::warn!(%log_error, "failed to append WebDAV sync log");
                        }
                        Err(detail)
                    }
                };
                let _ = proxy.send_event(crate::platform::UserEvent::ShelfSync(SyncTaskMessage {
                    id,
                    result,
                }));
            });
        }
    }

    pub(crate) fn complete_import(&mut self, message: ShelfImportTaskMessage) {
        if self.import_task.complete(message.id).is_none() {
            return;
        }
        match message.result {
            Ok(Some(paths)) => self.import_books(&paths),
            Ok(None) => {}
            Err(error) => self.show_error(format!(
                "{}: {error}",
                self.language
                    .text("打开文件选择器失败", "Unable to open file picker")
            )),
        }
    }

    pub(crate) fn complete_sync(&mut self, message: SyncTaskMessage) {
        if self.sync.task.complete(message.id).is_none() {
            return;
        }
        self.refresh_read_activity();
        match message.result {
            Ok(report) => {
                if let Some(error) = self.sync.import_error.take() {
                    self.show_error(error);
                    return;
                }
                if let Err(error) = self.apply_synced_generated_metadata() {
                    self.show_error(format!(
                        "{}: {error}",
                        self.language.text(
                            "应用同步的书籍元数据失败",
                            "Failed to apply synced book metadata"
                        )
                    ));
                    return;
                }
                let imported = self.sync.imported_books;
                self.sync.status = format!(
                    "{} · ↑{} ↓{} · {}",
                    self.language.text("同步完成", "Sync complete"),
                    report.uploaded_books,
                    imported,
                    report.updated_progress,
                );
                self.show_notice(self.sync.status.clone());
            }
            Err(error) => {
                self.show_error(format!(
                    "{}: {error}",
                    self.language.text("WebDAV 同步失败", "WebDAV sync failed")
                ));
            }
        }
    }

    pub(crate) fn update_sync_progress(&mut self, message: SyncProgressMessage) {
        if self.sync.task.in_flight(message.id).is_none() {
            return;
        }
        match message.progress {
            SyncProgress::Stage {
                stage,
                completed,
                total,
            } => {
                self.sync.status = sync_progress_text(self.language, stage, completed, total);
            }
            SyncProgress::Downloaded {
                book,
                cache_path,
                completed,
                total,
            } => {
                match self.shelf.library.import_remote(*book) {
                    Ok(true) => {
                        self.sync.imported_books += 1;
                        self.cover_textures.clear();
                        fs::remove_file(&cache_path).ok();
                    }
                    Ok(false) => {
                        fs::remove_file(&cache_path).ok();
                    }
                    Err(error) => {
                        self.sync.import_error = Some(format!(
                            "{}: {error}",
                            self.language
                                .text("导入云端书籍失败", "Failed to import a cloud book")
                        ));
                    }
                }
                self.sync.status =
                    sync_progress_text(self.language, SyncStage::Downloading, completed, total);
            }
        }
        if self.sync.import_error.is_none() {
            self.shelf.error = None;
            self.shelf.error_dismiss_at = None;
            self.shelf.notice = Some(self.sync.status.clone());
            self.shelf.notice_dismiss_at = None;
        }
    }

    fn apply_synced_generated_metadata(&mut self) -> crate::library::LibraryResult<()> {
        let book_ids = self
            .shelf
            .library
            .books()
            .iter()
            .map(|book| book.id.clone())
            .collect::<Vec<_>>();
        for book_id in book_ids {
            if let Some(metadata) = crate::generated_metadata::load(&book_id)? {
                self.shelf
                    .library
                    .update_metadata(&book_id, &metadata.title, &metadata.authors)?;
            }
        }
        Ok(())
    }

    pub(crate) fn ui(&mut self, root_ui: &mut egui::Ui, interaction_blocked: bool) {
        if self.statistics.open {
            self.statistics
                .ui(root_ui, self.language, interaction_blocked);
            return;
        }
        let ctx = root_ui.ctx().clone();
        self.dismiss_transient_messages_if_due(&ctx);
        let focus_search = !interaction_blocked
            && ctx.input_mut(|input| input.consume_shortcut(&self.search_shortcut));
        let import_books = !interaction_blocked
            && ctx.input_mut(|input| input.consume_shortcut(&self.import_books_shortcut));
        if import_books && !self.import_task.is_pending() {
            self.import_task.begin(());
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette().background)
                    .inner_margin(egui::Margin {
                        left: 36,
                        right: 16,
                        top: 28,
                        bottom: 28,
                    }),
            )
            .show(root_ui, |ui| {
                let search_response = self.shelf_header(ui, interaction_blocked);
                if interaction_blocked && search_response.has_focus() {
                    search_response.surrender_focus();
                }
                if focus_search {
                    search_response.request_focus();
                    self.shelf.focus_selected_book = false;
                }
                ui.add_space(26.0);

                let query = self.shelf.query.trim().to_lowercase();
                let mut books: Vec<LibraryBook> = self
                    .shelf
                    .library
                    .books()
                    .iter()
                    .filter(|book| book_matches_query(book, &query))
                    .cloned()
                    .collect();
                sort_shelf_books(&mut books, &self.read_activity);
                if search_response.changed() {
                    self.shelf.selected_book_id = books.first().map(|book| book.id.clone());
                    // Keep the editor active while the user continues typing. The first result
                    // remains the logical keyboard selection and Enter opens it.
                    self.shelf.focus_selected_book = false;
                } else if self
                    .shelf
                    .selected_book_id
                    .as_ref()
                    .is_none_or(|selected| !books.iter().any(|book| &book.id == selected))
                {
                    self.shelf.selected_book_id = books.first().map(|book| book.id.clone());
                    self.shelf.focus_selected_book = !search_response.has_focus();
                }
                if books.is_empty() {
                    self.empty_shelf(ui, query.is_empty());
                } else {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.set_width(
                            (ui.available_width() - SHELF_SCROLLBAR_GUTTER).max(CARD_WIDTH),
                        );
                        self.book_grid(
                            ui,
                            &books,
                            search_response.has_focus(),
                            interaction_blocked,
                        );
                    });
                }
            });
        if !interaction_blocked {
            self.dialogs(&ctx);
        }
    }

    fn shelf_header(&mut self, ui: &mut egui::Ui, interaction_blocked: bool) -> egui::Response {
        let book_count = self.shelf.library.books().len();
        let search_hint = shelf_search_hint(self.language, book_count);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 44.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let search_response = shelf_search_field(ui, &mut self.shelf.query, &search_hint);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let settings = icon_button(ui, Icon::Settings);
                    if ui
                        .add_enabled_ui(!interaction_blocked, |ui| icon_button(ui, Icon::Chart))
                        .inner
                        .on_hover_text(self.language.text("阅读统计", "Reading statistics"))
                        .clicked()
                    {
                        self.statistics
                            .open(self.shelf.library.books(), self.local_store.as_ref());
                    }
                    if !interaction_blocked {
                        if settings
                            .on_hover_text(self.language.text("设置", "Settings"))
                            .clicked()
                        {
                            self.settings_requested = true;
                        }
                    }
                    let import = shelf_import_button(ui, self.language.text("导入", "Import"));
                    if !interaction_blocked && import.clicked() && !self.import_task.is_pending() {
                        self.import_task.begin(());
                    }
                });
                search_response
            },
        )
        .inner
    }

    fn empty_shelf(&mut self, ui: &mut egui::Ui, no_books: bool) {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 280.0),
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(icon(Icon::BookOpen).size(30.0).color(palette().muted));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(if no_books {
                            self.language.text("书架还是空的", "Your library is empty")
                        } else {
                            self.language.text("没有匹配的书籍", "No matching books")
                        })
                        .size(crate::ui::scaled_font_size(16.0))
                        .strong()
                        .color(palette().text),
                    );
                    if no_books {
                        ui.label(
                            RichText::new(self.language.text(
                                "导入 EPUB、MOBI、PDF 等文件开始阅读",
                                "Import EPUB, MOBI, PDF, and other books to begin",
                            ))
                            .size(crate::ui::scaled_font_size(12.0))
                            .color(palette().muted),
                        );
                    }
                });
            },
        );
    }

    fn book_grid(
        &mut self,
        ui: &mut egui::Ui,
        books: &[LibraryBook],
        search_has_focus: bool,
        interaction_blocked: bool,
    ) {
        let columns = shelf_grid_columns(ui.available_width());
        let selected_index = self
            .shelf
            .selected_book_id
            .as_ref()
            .and_then(|selected| books.iter().position(|book| &book.id == selected))
            .unwrap_or(0);
        let keyboard_action = if !interaction_blocked && self.shelf.remove_confirmation.is_none() {
            shelf_keyboard_action(ui, search_has_focus)
        } else {
            None
        };
        let mut open_path = None;
        match keyboard_action {
            Some(ShelfKeyboardAction::Move(direction)) => {
                let next_index =
                    move_shelf_selection(selected_index, books.len(), columns, direction);
                self.shelf.selected_book_id = Some(books[next_index].id.clone());
                self.shelf.focus_selected_book = true;
            }
            Some(ShelfKeyboardAction::FocusSelection) => {
                self.shelf.focus_selected_book = true;
            }
            Some(ShelfKeyboardAction::Open) => {
                open_path = Some(books[selected_index].path.clone());
            }
            None => {}
        }
        let selected_id = self.shelf.selected_book_id.clone();
        let request_selected_focus = std::mem::take(&mut self.shelf.focus_selected_book);
        egui::Grid::new("shelf-grid")
            .num_columns(columns)
            .spacing(Vec2::new(20.0, 24.0))
            .show(ui, |ui| {
                for (index, book) in books.iter().enumerate() {
                    let selected = selected_id.as_deref() == Some(book.id.as_str());
                    if self.book_card(
                        ui,
                        book,
                        selected,
                        selected && request_selected_focus,
                        interaction_blocked,
                    ) {
                        self.shelf.selected_book_id = Some(book.id.clone());
                        open_path = Some(book.path.clone());
                    }
                    if (index + 1) % columns == 0 {
                        ui.end_row();
                    }
                }
            });
        if let Some(path) = open_path {
            self.open_book(&path);
        }
    }

    fn book_card(
        &mut self,
        ui: &mut egui::Ui,
        book: &LibraryBook,
        selected: bool,
        request_focus: bool,
        interaction_blocked: bool,
    ) -> bool {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(CARD_WIDTH, CARD_HEIGHT), egui::Sense::click());
        let response = if interaction_blocked {
            response
        } else {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        };
        if request_focus && !interaction_blocked {
            response.request_focus();
            response.scroll_to_me(None);
        }
        if response.has_focus() && !interaction_blocked {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(response.id, shelf_book_focus_filter());
            });
        }
        let texture = self.cover_texture(ui.ctx(), book);
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 10.0, palette().accent_soft.gamma_multiply(0.65));
        } else if !interaction_blocked && response.hovered() {
            painter.rect_filled(rect, 10.0, palette().accent_soft.gamma_multiply(0.42));
        }
        let cover_rect = egui::Rect::from_min_size(
            rect.min + Vec2::splat(10.0),
            Vec2::new(COVER_WIDTH, COVER_HEIGHT),
        );
        if let Some(texture) = texture {
            egui::Image::new(&texture)
                .uv(cover_uv_rect(cover_rect.size(), texture.size_vec2()))
                .fit_to_exact_size(cover_rect.size())
                .corner_radius(6)
                .show_loading_spinner(false)
                .paint_at(ui, cover_rect);
        } else {
            painter.rect_filled(cover_rect, 6.0, palette().surface_muted);
            paint_icon(
                ui,
                egui::Rect::from_center_size(cover_rect.center(), Vec2::splat(24.0)),
                Icon::BookOpen,
                palette().muted,
            );
        }

        let title_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, cover_rect.bottom() + 12.0),
            Vec2::new(CARD_WIDTH - 24.0, 20.0),
        );
        let title = painter.layout_job(single_line_card_text_job(
            book.title.clone(),
            egui::FontId::proportional(crate::ui::scaled_font_size(12.0)),
            palette().text,
            title_rect.width() - SHELF_TITLE_BOLD_OFFSET,
        ));
        let title_painter = painter.with_clip_rect(title_rect);
        title_painter.galley(title_rect.min, title.clone(), palette().text);
        title_painter.galley(
            title_rect.min + egui::vec2(SHELF_TITLE_BOLD_OFFSET, 0.0),
            title,
            palette().text,
        );

        let clicked = !interaction_blocked && response.clicked();
        painter.text(
            egui::pos2(rect.left() + 12.0, rect.bottom() - 18.0),
            egui::Align2::LEFT_CENTER,
            self.statistics.badge(&book.id, self.language),
            egui::FontId::proportional(11.0),
            palette().muted,
        );
        if !interaction_blocked {
            response.context_menu(|ui| {
                if ui
                    .button(self.language.text("阅读详情", "Reading details"))
                    .clicked()
                {
                    self.statistics.show_book(
                        book,
                        self.shelf.library.books(),
                        self.local_store.as_ref(),
                    );
                    ui.close();
                }
                if ui
                    .button(self.language.text("从书架移除", "Remove from library"))
                    .clicked()
                {
                    self.shelf.remove_confirmation = Some(ShelfRemoveConfirmation {
                        id: book.id.clone(),
                        title: book.title.clone(),
                    });
                    ui.close();
                }
            });
            response.on_hover_text(&book.title);
        }
        clicked
    }

    fn cover_texture(&mut self, ctx: &egui::Context, book: &LibraryBook) -> Option<TextureHandle> {
        if let Some(texture) = self.cover_textures.get(&book.id) {
            return Some(texture.clone());
        }
        let image = decode_color_image(book.cover_bytes.as_deref()?).ok()?;
        let texture = ctx.load_texture(
            format!("cover:{}", book.id),
            image,
            egui::TextureOptions::LINEAR,
        );
        self.cover_textures.insert(book.id.clone(), texture.clone());
        Some(texture)
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(error) = &self.shelf.error {
            show_toast(
                ctx,
                "shelf-error",
                error,
                ToastKind::Error,
                Vec2::new(-24.0, SHELF_TOAST_TOP_OFFSET),
                false,
            );
        } else if let Some(notice) = &self.shelf.notice {
            show_toast(
                ctx,
                "shelf-notice",
                notice,
                if self.sync.task.is_pending() {
                    ToastKind::Loading
                } else {
                    ToastKind::Success
                },
                Vec2::new(-24.0, SHELF_TOAST_TOP_OFFSET),
                false,
            );
        }
        let confirmation = self.shelf.remove_confirmation.clone();
        if let Some(confirmation) = confirmation {
            let mut cancel = false;
            let mut remove = false;
            let screen_width = ctx.content_rect().width();
            let modal_width = (screen_width - 48.0).clamp(280.0, 380.0).min(screen_width);
            let modal = egui::Modal::new(egui::Id::new("shelf-remove-book-modal"))
                .backdrop_color(Color32::BLACK.gamma_multiply(0.42))
                .frame(
                    egui::Frame::new()
                        .fill(palette().surface)
                        .stroke(egui::Stroke::new(1.0, palette().border))
                        .corner_radius(12)
                        .inner_margin(egui::Margin::symmetric(22, 18)),
                )
                .show(ctx, |ui| {
                    ui.set_width(modal_width);
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    ui.label(
                        RichText::new(self.language.text("移除书籍", "Remove book"))
                            .size(crate::ui::scaled_font_size(17.0))
                            .strong()
                            .color(palette().text),
                    );
                    ui.label(
                        RichText::new(self.language.text(
                            "只会移除这台设备上的本地副本，不会删除云端文件。",
                            "This only removes the local copy from this device. Cloud files are not deleted.",
                        ))
                        .size(crate::ui::scaled_font_size(12.0))
                        .color(palette().muted),
                    );
                    ui.add_space(4.0);
                    egui::Frame::new()
                        .fill(palette().surface_muted)
                        .stroke(egui::Stroke::new(1.0, palette().border))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width((modal_width - 24.0).max(1.0));
                            ui.label(
                                RichText::new(&confirmation.title)
                                    .size(crate::ui::scaled_font_size(13.0))
                                    .strong()
                                    .color(palette().text),
                            );
                        });
                    ui.add_space(8.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if dialog_danger_button(ui, self.language.text("移除", "Remove")).clicked()
                        {
                            remove = true;
                        }
                        if dialog_action_button(ui, self.language.text("取消", "Cancel"), false)
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
            cancel |= modal.should_close();
            if remove {
                self.shelf.remove_confirmation = None;
                self.remove_book(&confirmation.id);
            } else if cancel {
                self.shelf.remove_confirmation = None;
            }
        }
    }

    fn show_notice(&mut self, message: String) {
        self.shelf.error = None;
        self.shelf.error_dismiss_at = None;
        self.shelf.notice = Some(message);
        self.shelf.notice_dismiss_at = Some(Instant::now() + NOTICE_AUTO_DISMISS_DELAY);
    }

    fn show_error(&mut self, message: String) {
        self.shelf.notice = None;
        self.shelf.notice_dismiss_at = None;
        self.shelf.error = Some(message);
        self.shelf.error_dismiss_at = Some(Instant::now() + NOTICE_AUTO_DISMISS_DELAY);
    }

    fn dismiss_transient_messages_if_due(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self
            .shelf
            .notice_dismiss_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.shelf.notice = None;
            self.shelf.notice_dismiss_at = None;
        }
        if self
            .shelf
            .error_dismiss_at
            .is_some_and(|deadline| now >= deadline)
        {
            self.shelf.error = None;
            self.shelf.error_dismiss_at = None;
        }
        if let Some(deadline) = [self.shelf.notice_dismiss_at, self.shelf.error_dismiss_at]
            .into_iter()
            .flatten()
            .min()
        {
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
        }
    }
}

fn single_line_card_text_job(
    text: String,
    font_id: egui::FontId,
    color: Color32,
    max_width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::simple(text, font_id, color, max_width);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShelfSelectionDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShelfKeyboardAction {
    Move(ShelfSelectionDirection),
    FocusSelection,
    Open,
}

const fn shelf_book_focus_filter() -> egui::EventFilter {
    egui::EventFilter {
        tab: false,
        horizontal_arrows: true,
        vertical_arrows: true,
        escape: false,
    }
}

fn shelf_grid_columns(available_width: f32) -> usize {
    let mut columns = 1_usize;
    let mut occupied = CARD_WIDTH;
    while occupied + 20.0 + CARD_WIDTH <= available_width {
        columns += 1;
        occupied += 20.0 + CARD_WIDTH;
    }
    columns
}

fn shelf_keyboard_action(ui: &mut egui::Ui, search_has_focus: bool) -> Option<ShelfKeyboardAction> {
    ui.input_mut(|input| {
        if search_has_focus {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                return Some(ShelfKeyboardAction::FocusSelection);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                return Some(ShelfKeyboardAction::Open);
            }
            return None;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
            Some(ShelfKeyboardAction::Move(ShelfSelectionDirection::Left))
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
            Some(ShelfKeyboardAction::Move(ShelfSelectionDirection::Right))
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            Some(ShelfKeyboardAction::Move(ShelfSelectionDirection::Up))
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            Some(ShelfKeyboardAction::Move(ShelfSelectionDirection::Down))
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            Some(ShelfKeyboardAction::Open)
        } else {
            None
        }
    })
}

fn move_shelf_selection(
    current: usize,
    book_count: usize,
    columns: usize,
    direction: ShelfSelectionDirection,
) -> usize {
    if book_count == 0 {
        return 0;
    }
    let current = current.min(book_count - 1);
    let columns = columns.max(1);
    match direction {
        ShelfSelectionDirection::Left => current.saturating_sub(1),
        ShelfSelectionDirection::Right => (current + 1).min(book_count - 1),
        ShelfSelectionDirection::Up => current.saturating_sub(columns),
        ShelfSelectionDirection::Down => (current + columns).min(book_count - 1),
    }
}

fn shelf_search_field(ui: &mut egui::Ui, query: &mut String, hint: &str) -> egui::Response {
    let width = ui.available_width().clamp(180.0, 320.0);
    egui::Frame::new()
        .fill(palette().surface)
        .stroke(egui::Stroke::new(1.0, palette().border))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.set_width(width - 22.0);
            ui.horizontal_centered(|ui| {
                ui.add(icon(Icon::Search).size(15.0).color(palette().muted));
                ui.add(
                    egui::TextEdit::singleline(query)
                        .hint_text(hint)
                        .desired_width(ui.available_width())
                        .frame(egui::Frame::NONE)
                        .vertical_align(egui::Align::Center),
                )
            })
            .inner
        })
        .inner
}

fn shelf_import_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(80.0, 36.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if response.is_pointer_button_down_on() {
        palette().accent.gamma_multiply(0.84)
    } else if response.hovered() {
        palette().accent.gamma_multiply(0.92)
    } else {
        palette().accent
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, fill);
    let icon_size = 15.0;
    let text_font = egui::FontId::proportional(crate::ui::scaled_font_size(13.0));
    let text_galley = painter.layout_no_wrap(label.into(), text_font, Color32::WHITE);
    let gap = 6.0;
    let content_width = icon_size + gap + text_galley.size().x;
    let start_x = rect.center().x - content_width / 2.0;
    paint_icon(
        ui,
        egui::Rect::from_min_size(
            egui::pos2(start_x, rect.center().y - icon_size / 2.0),
            Vec2::splat(icon_size),
        ),
        Icon::Plus,
        Color32::WHITE,
    );
    painter.galley(
        egui::pos2(
            start_x + content_width - text_galley.size().x,
            rect.center().y - text_galley.size().y / 2.0,
        ),
        text_galley,
        Color32::WHITE,
    );
    response
}

fn cover_uv_rect(bounds_size: Vec2, image_size: Vec2) -> egui::Rect {
    let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    if bounds_size.x <= 0.0
        || bounds_size.y <= 0.0
        || !bounds_size.x.is_finite()
        || !bounds_size.y.is_finite()
        || image_size.x <= 0.0
        || image_size.y <= 0.0
        || !image_size.x.is_finite()
        || !image_size.y.is_finite()
    {
        return full;
    }
    let bounds_aspect = bounds_size.x / bounds_size.y;
    let image_aspect = image_size.x / image_size.y;
    if image_aspect > bounds_aspect {
        let visible_width = bounds_aspect / image_aspect;
        let inset = (1.0 - visible_width) / 2.0;
        egui::Rect::from_min_max(egui::pos2(inset, 0.0), egui::pos2(1.0 - inset, 1.0))
    } else if image_aspect < bounds_aspect {
        let visible_height = image_aspect / bounds_aspect;
        let inset = (1.0 - visible_height) / 2.0;
        egui::Rect::from_min_max(egui::pos2(0.0, inset), egui::pos2(1.0, 1.0 - inset))
    } else {
        full
    }
}

fn book_matches_query(book: &LibraryBook, query: &str) -> bool {
    query.is_empty()
        || book.title.to_lowercase().contains(query)
        || book
            .authors
            .iter()
            .any(|author| author.to_lowercase().contains(query))
}

fn shelf_search_hint(language: AppLanguage, book_count: usize) -> String {
    match language.resolved() {
        AppLanguage::SimplifiedChinese => {
            format!("从{book_count}本书籍中搜索书名或作者")
        }
        AppLanguage::English if book_count == 1 => "Search title or author in 1 book".into(),
        AppLanguage::English => format!("Search titles or authors in {book_count} books"),
        AppLanguage::System => unreachable!(),
    }
}

fn sort_shelf_books(books: &mut [LibraryBook], read_activity: &HashMap<String, u64>) {
    books.sort_by(|left, right| {
        let left_activity = read_activity
            .get(&left.id)
            .copied()
            .unwrap_or_default()
            .max(left.added_at);
        let right_activity = read_activity
            .get(&right.id)
            .copied()
            .unwrap_or_default()
            .max(right.added_at);
        right_activity
            .cmp(&left_activity)
            .then_with(|| right.added_at.cmp(&left.added_at))
            .then_with(|| left.id.cmp(&right.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(id: &str, added_at: u64) -> LibraryBook {
        LibraryBook {
            id: id.into(),
            title: id.into(),
            authors: Vec::new(),
            file_name: format!("{id}.epub"),
            path: PathBuf::from(format!("{id}.epub")),
            cover_bytes: None,
            added_at,
        }
    }

    #[test]
    fn cover_uv_uses_a_centered_object_fit_cover_crop() {
        let wide = cover_uv_rect(Vec2::new(2.0, 3.0), Vec2::new(2.0, 1.0));
        assert!((wide.center().x - 0.5).abs() < f32::EPSILON);
        assert!((wide.width() - 1.0 / 3.0).abs() < 0.001);
        assert!((wide.height() - 1.0).abs() < f32::EPSILON);

        let tall = cover_uv_rect(Vec2::new(2.0, 3.0), Vec2::new(1.0, 3.0));
        assert!((tall.center().y - 0.5).abs() < f32::EPSILON);
        assert!((tall.width() - 1.0).abs() < f32::EPSILON);
        assert!((tall.height() - 0.5).abs() < 0.001);
    }

    #[test]
    fn shelf_titles_use_the_same_single_line_truncation_contract_as_egui_labels() {
        let job = single_line_card_text_job(
            "A title that is much wider than the shelf card".into(),
            egui::FontId::proportional(12.0),
            Color32::WHITE,
            120.0,
        );
        assert_eq!(job.wrap.max_rows, 1);
        assert!(job.wrap.break_anywhere);
        assert_eq!(job.wrap.max_width, 120.0);
    }

    #[test]
    fn books_are_sorted_by_the_latest_import_or_reading_activity() {
        let mut books = vec![
            book("newly-imported", 400),
            book("recently-read", 100),
            book("older-activity", 200),
        ];
        let activity = HashMap::from([
            ("recently-read".into(), 500),
            ("older-activity".into(), 250),
        ]);

        sort_shelf_books(&mut books, &activity);

        assert_eq!(
            books
                .iter()
                .map(|book| book.id.as_str())
                .collect::<Vec<_>>(),
            vec!["recently-read", "newly-imported", "older-activity"]
        );
    }

    #[test]
    fn a_newer_unread_import_precedes_an_older_reading_activity() {
        let mut books = vec![book("read", 100), book("unread", 400)];
        let activity = HashMap::from([("read".into(), 300)]);

        sort_shelf_books(&mut books, &activity);

        assert_eq!(
            books
                .iter()
                .map(|book| book.id.as_str())
                .collect::<Vec<_>>(),
            vec!["unread", "read"]
        );
    }

    #[test]
    fn unread_books_are_sorted_by_latest_import_time() {
        let mut books = vec![book("old", 10), book("new", 30), book("middle", 20)];

        sort_shelf_books(&mut books, &HashMap::new());

        assert_eq!(
            books
                .iter()
                .map(|book| book.id.as_str())
                .collect::<Vec<_>>(),
            vec!["new", "middle", "old"]
        );
    }

    #[test]
    fn shelf_search_hint_includes_the_library_book_count() {
        assert_eq!(
            shelf_search_hint(AppLanguage::SimplifiedChinese, 27),
            "从27本书籍中搜索书名或作者"
        );
        assert_eq!(
            shelf_search_hint(AppLanguage::English, 1),
            "Search title or author in 1 book"
        );
        assert_eq!(
            shelf_search_hint(AppLanguage::English, 27),
            "Search titles or authors in 27 books"
        );
    }

    #[test]
    fn shelf_selection_moves_within_the_current_grid() {
        assert_eq!(
            move_shelf_selection(0, 8, 3, ShelfSelectionDirection::Right),
            1
        );
        assert_eq!(
            move_shelf_selection(1, 8, 3, ShelfSelectionDirection::Down),
            4
        );
        assert_eq!(
            move_shelf_selection(4, 8, 3, ShelfSelectionDirection::Up),
            1
        );
        assert_eq!(
            move_shelf_selection(1, 8, 3, ShelfSelectionDirection::Left),
            0
        );
    }

    #[test]
    fn shelf_selection_stays_inside_partial_last_rows() {
        assert_eq!(
            move_shelf_selection(5, 8, 3, ShelfSelectionDirection::Down),
            7
        );
        assert_eq!(
            move_shelf_selection(7, 8, 3, ShelfSelectionDirection::Right),
            7
        );
        assert_eq!(
            move_shelf_selection(0, 8, 3, ShelfSelectionDirection::Up),
            0
        );
    }

    #[test]
    fn shelf_book_focus_keeps_arrow_keys_inside_the_grid() {
        let filter = shelf_book_focus_filter();

        assert!(filter.horizontal_arrows);
        assert!(filter.vertical_arrows);
        assert!(!filter.tab);
        assert!(!filter.escape);
    }
}
