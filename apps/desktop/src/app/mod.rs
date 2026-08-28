use std::path::Path;
use std::sync::Arc;

use peniko::Blob;

use crate::library::LocalLibrary;
use crate::platform::UserEvent;
use crate::preferences::{AppTheme, InterfaceTypography};
use crate::reader::{
    ChatStreamMessage, ChatTaskMessage, PdfOcrTaskMessage, PdfTocTaskMessage, SearchTaskMessage,
    TocTranslationTaskMessage, TranslationTaskMessage,
};
use crate::reader::{DesktopReader, ReaderFramePlan, ReaderPageTexture, ReaderScene};
use crate::settings::{SettingsFeature, settings_overlay};
use crate::shelf::{ShelfFeature, ShelfImportTaskMessage, SyncProgressMessage, SyncTaskMessage};

pub(crate) struct DesktopApp {
    shelf: ShelfFeature,
    reader: Option<DesktopReader>,
    settings: SettingsFeature,
    applied_settings_revision: u64,
    pending_reader_notice: Option<String>,
    pending_reader_error: Option<String>,
    fullscreen_toggle_requested: bool,
    #[cfg(target_os = "windows")]
    updater: crate::updater::WindowsUpdater,
}

impl DesktopApp {
    pub(crate) fn new(library: LocalLibrary, reader_fonts: Arc<[Blob<u8>]>) -> Self {
        let settings = SettingsFeature::new(&reader_fonts);
        Self {
            shelf: ShelfFeature::new(library, reader_fonts),
            reader: None,
            settings,
            applied_settings_revision: 0,
            pending_reader_notice: None,
            pending_reader_error: None,
            fullscreen_toggle_requested: false,
            #[cfg(target_os = "windows")]
            updater: crate::updater::WindowsUpdater::new(),
        }
    }

    pub(crate) fn open_book(&mut self, path: &Path) {
        self.pending_reader_notice = None;
        self.pending_reader_error = None;
        self.shelf.open_book(path);
        if let Some(next_reader) = self.shelf.take_opened_reader() {
            if let Some(current_reader) = self.reader.as_ref() {
                current_reader.prepare_for_shutdown();
            }
            self.reader = Some(next_reader);
        }
    }

    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        page_texture: Option<ReaderPageTexture>,
    ) -> Option<ReaderFramePlan> {
        self.reconcile_state(ui.ctx());
        let open_settings_shortcut = self.settings.applied().shortcuts.open_settings;
        if !self.settings.is_open()
            && ui
                .ctx()
                .input_mut(|input| input.consume_shortcut(&open_settings_shortcut))
        {
            self.settings.open();
        }
        let interaction_blocked = self.settings.is_open();
        let plan = if let Some(reader) = self.reader.as_mut() {
            Some(reader.ui(ui, page_texture, interaction_blocked))
        } else {
            self.shelf.ui(ui, interaction_blocked);
            None
        };

        let settings_requested = self.reader.as_mut().map_or_else(
            || self.shelf.take_settings_request(),
            DesktopReader::take_settings_request,
        );
        if settings_requested {
            self.settings.open();
        }
        if let Some(change) = self
            .reader
            .as_mut()
            .and_then(DesktopReader::take_settings_change_request)
            && let Err(error) = self.settings.apply_reader_change(change)
            && let Some(reader) = self.reader.as_mut()
        {
            reader.report_settings_error(error);
        }
        settings_overlay(ui.ctx(), &mut self.settings);
        if ui.ctx().input_mut(|input| {
            input.consume_shortcut(&self.settings.applied().shortcuts.fullscreen)
        }) {
            self.fullscreen_toggle_requested = true;
        }
        #[cfg(target_os = "windows")]
        {
            if self.settings.take_update_check_request() {
                self.updater.request_check();
            }
            if self.settings.take_update_request() {
                self.updater.request_update();
            }
            self.updater
                .overlay(ui.ctx(), self.settings.applied().language);
        }
        self.apply_settings_if_changed(ui.ctx());
        plan
    }

    pub(crate) fn take_fullscreen_toggle_request(&mut self) -> bool {
        std::mem::take(&mut self.fullscreen_toggle_requested)
    }

    pub(crate) fn open_settings_shortcut(&self) -> egui::KeyboardShortcut {
        self.settings.applied().shortcuts.open_settings
    }

    pub(crate) fn open_settings(&mut self) {
        if !self.settings.is_open() {
            self.settings.open();
        }
    }

    pub(crate) fn reader_scene(&mut self) -> Option<ReaderScene> {
        self.reader.as_mut().map(DesktopReader::page_scene)
    }

    pub(crate) fn spawn_pending_tasks(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        self.shelf.spawn_pending_tasks(runtime, proxy);
        self.settings.spawn_pending_tasks(runtime, proxy);
        #[cfg(target_os = "windows")]
        self.updater.spawn_pending_tasks(runtime, proxy);
        if let Some(reader) = self.reader.as_mut() {
            reader.spawn_pending_tasks(runtime, proxy);
        }
    }

    pub(crate) fn complete_shelf_sync(&mut self, message: SyncTaskMessage) {
        self.shelf.complete_sync(message);
    }

    pub(crate) fn update_shelf_sync_progress(&mut self, message: SyncProgressMessage) {
        self.shelf.update_sync_progress(message);
    }

    pub(crate) fn complete_shelf_import(&mut self, message: ShelfImportTaskMessage) {
        self.shelf.complete_import(message);
    }

    pub(crate) fn complete_settings_provider_models(
        &mut self,
        message: crate::settings::ProviderModelsMessage,
    ) {
        self.settings.complete_provider_models(message);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn complete_update(&mut self, message: crate::updater::UpdateTaskMessage) {
        if let Some(result) = self.updater.complete(message) {
            self.settings.complete_update_check(result);
        }
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn take_update_install_request(&mut self) -> Option<crate::updater::InstallRequest> {
        let request = self.updater.take_install_request();
        if request.is_some()
            && let Some(reader) = self.reader.as_ref()
        {
            reader.prepare_for_shutdown();
        }
        request
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn report_update_install_error(
        &mut self,
        request: crate::updater::InstallRequest,
        message: String,
    ) {
        self.updater.report_install_error(request, message);
    }

    pub(crate) fn complete_reader_search(&mut self, message: SearchTaskMessage) {
        if let Some(reader) = self.reader.as_mut() {
            reader.complete_search(message);
        }
    }

    pub(crate) fn complete_reader_chat(&mut self, message: ChatTaskMessage) {
        if let Some(reader) = self.reader.as_mut() {
            reader.complete_chat(message);
        }
    }

    pub(crate) fn update_reader_chat_stream(&mut self, message: ChatStreamMessage) {
        if let Some(reader) = self.reader.as_mut() {
            reader.update_chat_stream(message);
        }
    }

    pub(crate) fn complete_reader_translation(&mut self, message: TranslationTaskMessage) {
        if let Some(reader) = self.reader.as_mut() {
            reader.complete_translation(message);
        }
    }

    pub(crate) fn complete_reader_toc_translation(&mut self, message: TocTranslationTaskMessage) {
        if let Some(reader) = self.reader.as_mut() {
            reader.complete_toc_translation(message);
        }
    }

    pub(crate) fn complete_reader_pdf_toc(&mut self, message: PdfTocTaskMessage) {
        let update = self
            .reader
            .as_mut()
            .and_then(|reader| reader.complete_pdf_toc(message));
        let Some(update) = update else {
            return;
        };
        if let Err(error) =
            self.shelf
                .update_book_metadata(&update.book_id, &update.title, &update.authors)
            && let Some(reader) = self.reader.as_mut()
        {
            reader.report_settings_error(format!("保存书架元数据失败：{error}"));
        }
    }

    pub(crate) fn complete_reader_pdf_ocr(&mut self, message: PdfOcrTaskMessage) {
        if let Some(reader) = self.reader.as_mut() {
            reader.complete_pdf_ocr(message);
        }
    }

    pub(crate) fn log_reader_diagnostics(&self, event: &'static str, focused: Option<bool>) {
        if let Some(reader) = self.reader.as_ref() {
            reader.log_diagnostic_snapshot(event, focused);
        } else {
            crate::diagnostics::log(
                event,
                &[
                    crate::diagnostics::Field::Text("screen", "shelf"),
                    crate::diagnostics::Field::Text(
                        "focus",
                        match focused {
                            Some(true) => "true",
                            Some(false) => "false",
                            None => "unknown",
                        },
                    ),
                ],
            );
        }
    }

    fn reconcile_state(&mut self, ctx: &egui::Context) {
        let reopen = self.reader.as_mut().and_then(|reader| {
            reader.take_reopen_request().map(|path| {
                (
                    path,
                    reader.take_reopen_notice(),
                    reader.take_reopen_error(),
                )
            })
        });
        if let Some((path, notice, error)) = reopen {
            self.pending_reader_notice = notice;
            self.pending_reader_error = error;
            self.reader = None;
            self.shelf.open_book(&path);
        }
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| reader.exit_requested)
        {
            self.reader = None;
            self.shelf.resume();
        }
        self.promote_opened_reader();
        self.apply_settings_if_changed(ctx);
    }

    fn promote_opened_reader(&mut self) {
        if self.reader.is_none() {
            self.reader = self.shelf.take_opened_reader().map(|mut reader| {
                if let Some(error) = self.pending_reader_error.take() {
                    self.pending_reader_notice = None;
                    reader.report_settings_error(error);
                } else if let Some(notice) = self.pending_reader_notice.take() {
                    reader.show_notice(notice);
                }
                reader
            });
        }
    }

    fn apply_settings_if_changed(&mut self, ctx: &egui::Context) {
        let revision = self.settings.revision();
        let revision_changed = revision != self.applied_settings_revision;
        let applied = self.settings.applied().clone();
        if revision_changed {
            crate::ui::apply_interface_typography(ctx, &applied.interface_typography);
            crate::ui::set_theme(ctx, applied.theme);
        }
        let resolved_theme_changed = crate::ui::sync_system_theme(ctx, applied.theme);
        if !revision_changed && !resolved_theme_changed {
            return;
        }
        crate::ui::apply_visuals(ctx, &crate::ui::palette());
        ctx.request_repaint();
        self.shelf.apply_global_settings(&applied);
        if let Some(reader) = self.reader.as_mut() {
            reader.apply_global_settings(&applied);
        }
        self.applied_settings_revision = revision;
    }

    pub(crate) fn theme(&self) -> AppTheme {
        self.settings.applied().theme
    }

    pub(crate) fn interface_typography(&self) -> &InterfaceTypography {
        &self.settings.applied().interface_typography
    }
}
