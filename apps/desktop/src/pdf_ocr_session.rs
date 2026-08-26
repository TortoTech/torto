use std::io;

use rebook_session::{
    PdfOcrSourceController, PdfOcrViewMode, PdfOcrViewModeMutationTarget, apply_pdf_ocr_view_mode,
    rollback_pdf_ocr_view_mode,
};

struct DesktopPdfOcrViewModeTarget;

impl PdfOcrViewModeMutationTarget for DesktopPdfOcrViewModeTarget {
    type Error = io::Error;

    fn persist_pdf_ocr_view_mode(
        &mut self,
        book_id: &str,
        mode: PdfOcrViewMode,
    ) -> Result<(), Self::Error> {
        crate::plugins::persist_pdf_ocr_view_mode(book_id, mode)
    }
}

pub(crate) fn apply_view_mode(
    controller: &PdfOcrSourceController,
    book_id: &str,
    mode: PdfOcrViewMode,
) -> io::Result<()> {
    apply_pdf_ocr_view_mode(&mut DesktopPdfOcrViewModeTarget, controller, book_id, mode).map(|_| ())
}

pub(crate) fn rollback_view_mode(
    controller: &PdfOcrSourceController,
    book_id: &str,
    mode: PdfOcrViewMode,
) -> io::Result<()> {
    rollback_pdf_ocr_view_mode(&mut DesktopPdfOcrViewModeTarget, controller, book_id, mode)
}
