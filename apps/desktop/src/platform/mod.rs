mod application;
mod gpu;
mod repaint;

pub(crate) use application::run;

pub(crate) enum UserEvent {
    RepaintAfter(std::time::Duration),
    EguiRepaint {
        when: std::time::Instant,
        cumulative_pass_nr: u64,
        viewport_id: egui::ViewportId,
    },
    #[cfg(target_os = "macos")]
    OpenBook(std::path::PathBuf),
    #[cfg(target_os = "windows")]
    Update(crate::updater::UpdateTaskMessage),
    ShelfImport(crate::shelf::ShelfImportTaskMessage),
    ShelfOpen(crate::shelf::OpenTaskMessage),
    ShelfOpenHeader(crate::shelf::OpenHeaderMessage),
    ReaderRendererReady(gpu::RendererReady),
    ShelfSyncProgress(crate::shelf::SyncProgressMessage),
    ShelfSync(crate::shelf::SyncTaskMessage),
    ShelfSyncCheck(crate::shelf::SyncCheckMessage),
    SettingsProviderModels(crate::settings::ProviderModelsMessage),
    ReaderSearch(crate::reader::SearchTaskMessage),
    ReaderChatStream(crate::reader::ChatStreamMessage),
    ReaderChat(crate::reader::ChatTaskMessage),
    ReaderTranslation(crate::reader::TranslationTaskMessage),
    ReaderTocTranslation(crate::reader::TocTranslationTaskMessage),
    ReaderPdfToc(crate::reader::PdfTocTaskMessage),
    ReaderPdfOcr(crate::reader::PdfOcrTaskMessage),
    ReaderPdfOriginal(crate::reader::PdfOriginalTaskMessage),
    ReaderPdfNative(crate::reader::PdfNativeTaskMessage),
}
