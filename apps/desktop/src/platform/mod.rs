mod application;
mod gpu;

pub(crate) use application::run;

pub(crate) enum UserEvent {
    RepaintAfter(std::time::Duration),
    #[cfg(target_os = "macos")]
    OpenBook(std::path::PathBuf),
    #[cfg(target_os = "windows")]
    Update(crate::updater::UpdateTaskMessage),
    ShelfImport(crate::shelf::ShelfImportTaskMessage),
    ShelfSync(crate::shelf::SyncTaskMessage),
    ReaderSearch(crate::reader::SearchTaskMessage),
    ReaderSemanticIndex(crate::reader::SemanticIndexTaskMessage),
    ReaderChatStream(crate::reader::ChatStreamMessage),
    ReaderChat(crate::reader::ChatTaskMessage),
    ReaderTranslation(crate::reader::TranslationTaskMessage),
    ReaderTocTranslation(crate::reader::TocTranslationTaskMessage),
    ReaderPdfToc(crate::reader::PdfTocTaskMessage),
    ReaderPdfOcr(crate::reader::PdfOcrTaskMessage),
}
