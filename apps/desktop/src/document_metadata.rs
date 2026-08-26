use std::io;

use rebook_session::{
    GeneratedPdfMetadata, GeneratedTocDraft, PdfDocumentMetadataCommand,
    PdfDocumentMetadataMutationTarget, PdfOcrPageRoleAssignment,
    apply_pdf_document_metadata_command,
};

/// Legacy storage/sync adapter for the shared PDF-derived metadata protocol.
///
/// UI and AI code submit toolkit-neutral commands through this module. The
/// individual version-1 files remain byte-compatible while the storage layout
/// is migrated independently from the document-session contract.
struct DesktopPdfDocumentMetadataTarget;

impl PdfDocumentMetadataMutationTarget for DesktopPdfDocumentMetadataTarget {
    type Error = io::Error;

    fn replace_bibliographic(
        &mut self,
        book_id: &str,
        metadata: &GeneratedPdfMetadata,
    ) -> Result<(), Self::Error> {
        crate::generated_metadata::persist_normalized(book_id, metadata)
    }

    fn replace_generated_toc(
        &mut self,
        book_id: &str,
        draft: &GeneratedTocDraft,
    ) -> Result<(), Self::Error> {
        crate::generated_toc::persist_normalized(book_id, draft)
    }

    fn replace_ocr_page_roles(
        &mut self,
        book_id: &str,
        roles: &[PdfOcrPageRoleAssignment],
    ) -> Result<(), Self::Error> {
        crate::plugins::persist_pdf_ocr_page_roles(book_id, roles)
    }
}

pub(crate) fn apply(
    book_id: &str,
    command: PdfDocumentMetadataCommand,
) -> io::Result<PdfDocumentMetadataCommand> {
    apply_pdf_document_metadata_command(&mut DesktopPdfDocumentMetadataTarget, book_id, command)
}
