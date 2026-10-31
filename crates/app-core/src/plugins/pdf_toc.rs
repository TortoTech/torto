//! PDF discovery is driven by the model's tool calls, not a fixed scan pipeline.
use super::{PdfOcrPageRoleAssignment, PluginSettings};
use crate::generated_metadata::GeneratedPdfMetadata;
use crate::generated_toc::GeneratedTocDraft;
use rebook_publication::BookSource;
use std::sync::Arc;
mod agent;

pub struct PdfMetadataExtraction {
    pub toc: Option<GeneratedTocDraft>,
    pub toc_error: Option<String>,
    pub metadata: Option<GeneratedPdfMetadata>,
    pub page_roles: Vec<PdfOcrPageRoleAssignment>,
    pub warnings: Vec<String>,
}

pub async fn extract_pdf_metadata<F>(
    source: Arc<dyn BookSource>,
    settings: PluginSettings,
    need_toc: bool,
    need_page_roles: bool,
    need_book_metadata: bool,
    on_progress: F,
) -> Result<PdfMetadataExtraction, String>
where
    F: FnMut(String) + Send,
{
    agent::run(
        source,
        settings,
        agent::Goals {
            toc: need_toc,
            metadata: need_book_metadata,
            page_roles: need_page_roles,
        },
        on_progress,
    )
    .await
}
