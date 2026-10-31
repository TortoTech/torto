mod application;
mod clipboard;
#[cfg(test)]
mod egui_alpha_tests;
#[cfg(test)]
mod native_text_input_tests;
mod egui_surface;
mod gpu;
mod reader_layers;
#[cfg(test)]
pub(crate) use reader_layers::tests::LayerAudit;
mod repaint;

pub(crate) use application::run;
pub(crate) use clipboard::images_without_text;
pub(crate) use clipboard::feedback_images;

pub(crate) enum UserEvent {
    Fonts(crate::fonts::Message),
    Feedback(crate::feedback::Message),
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
    SettingsCredentials(crate::settings::CredentialsMessage),
    SettingsProviderModels(crate::settings::ProviderModelsMessage),
    ReaderSearch(crate::reader::SearchTaskMessage),
    ReaderChatStream(crate::reader::ChatStreamMessage),
    ReaderChat(crate::reader::ChatTaskMessage),
    ReaderChatPaste(crate::reader::ChatPasteMessage),
    FeedbackImages(crate::feedback::screenshots::Message),
    FeedbackPreview(crate::feedback::screenshots::PreviewMessage),
    ReaderTranslation(crate::reader::TranslationTaskMessage),
    ReaderTocTranslation(crate::reader::TocTranslationTaskMessage),
    ReaderPdfToc(crate::reader::PdfTocTaskMessage),
    ReaderPdfOcr(crate::reader::PdfOcrTaskMessage),
    ReaderPdfOriginal(crate::reader::PdfOriginalTaskMessage),
    ReaderPdfNative(crate::reader::PdfNativeTaskMessage),
}
