use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rebook_publication::{
    Book, BookSource, PublicationError, PublicationUrl, RasterResource, Resource, Section,
    TableOfContentsOrigin,
};
use serde::{Deserialize, Serialize};

/// Stable anchor prefix inserted by the OCR reflow source for each physical PDF
/// page. The source controller uses it to map a reflow location back to the
/// corresponding fixed page without consulting a frontend.
pub const PDF_PAGE_ANCHOR_PREFIX: &str = "pdf-page-";

/// Active representation of a PDF with an available OCR reflow source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PdfOcrViewMode {
    #[default]
    Original,
    Reflow,
}

impl PdfOcrViewMode {
    /// Returns the other available representation.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Original => Self::Reflow,
            Self::Reflow => Self::Original,
        }
    }

    /// Returns whether the reflowable OCR representation is active.
    #[must_use]
    pub const fn is_reflow(self) -> bool {
        matches!(self, Self::Reflow)
    }
}

/// Result of joining a canonical PDF source with an optional OCR reflow source.
pub struct PdfOcrLoadedSource {
    pub source: Arc<dyn BookSource>,
    pub controller: Option<Arc<PdfOcrSourceController>>,
    pub available: bool,
    pub mode: PdfOcrViewMode,
}

/// Durable reader location used to plan a representation switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdfOcrViewLocation {
    /// Zero-based page index in the canonical fixed-page source.
    OriginalPage(usize),
    /// Closest preceding OCR physical-page anchor in the reflow source.
    ReflowAnchor(Option<String>),
}

/// Pure plan for switching between the canonical and OCR representations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfOcrViewTransition {
    pub previous_mode: PdfOcrViewMode,
    pub next_mode: PdfOcrViewMode,
    pub navigation_target: Option<PublicationUrl>,
}

/// Persistence boundary for the per-device PDF/OCR representation choice.
pub trait PdfOcrViewModeMutationTarget {
    type Error;

    fn persist_pdf_ocr_view_mode(
        &mut self,
        book_id: &str,
        mode: PdfOcrViewMode,
    ) -> Result<(), Self::Error>;
}

/// Persists and activates one PDF/OCR representation choice.
///
/// Persistence happens first. A failed write therefore cannot leave the shared
/// source controller in a mode that was never committed. The returned mode is
/// suitable for rollback after a later reader-layout refresh fails.
pub fn apply_pdf_ocr_view_mode<T>(
    target: &mut T,
    controller: &PdfOcrSourceController,
    book_id: &str,
    mode: PdfOcrViewMode,
) -> Result<PdfOcrViewMode, T::Error>
where
    T: PdfOcrViewModeMutationTarget + ?Sized,
{
    let previous_mode = controller.mode();
    target.persist_pdf_ocr_view_mode(book_id, mode)?;
    controller.set_mode(mode);
    Ok(previous_mode)
}

/// Restores an in-memory representation after a later reader refresh failed.
///
/// The controller is restored even if repairing the persisted preference also
/// fails, because the currently displayed reader must keep observing the source
/// that was successfully laid out. The persistence error is still returned to
/// the frontend for reporting or retry.
pub fn rollback_pdf_ocr_view_mode<T>(
    target: &mut T,
    controller: &PdfOcrSourceController,
    book_id: &str,
    mode: PdfOcrViewMode,
) -> Result<(), T::Error>
where
    T: PdfOcrViewModeMutationTarget + ?Sized,
{
    let persisted = target.persist_pdf_ocr_view_mode(book_id, mode);
    controller.set_mode(mode);
    persisted
}

/// Thread-safe source switch and physical-page mapping for a PDF/OCR pair.
///
/// It implements [`BookSource`] so parser, reader, layout, search, and assistant
/// layers observe one stable object while the selected representation changes.
/// Only durable [`PublicationUrl`] values cross a switch; no UI or layout type
/// enters this boundary.
pub struct PdfOcrSourceController {
    original: Arc<dyn BookSource>,
    reflow: Arc<dyn BookSource>,
    original_page_targets: Vec<PublicationUrl>,
    reflow_page_targets: Vec<PublicationUrl>,
    reflow_enabled: AtomicBool,
}

impl PdfOcrSourceController {
    /// Joins the canonical fixed-page source and the derived OCR source.
    #[must_use]
    pub fn new(
        original: Arc<dyn BookSource>,
        reflow: Arc<dyn BookSource>,
        reflow_page_targets: Vec<PublicationUrl>,
        mode: PdfOcrViewMode,
    ) -> Self {
        let original_page_targets = original
            .book()
            .sections
            .iter()
            .map(|section| section.href.clone())
            .collect();
        Self {
            original,
            reflow,
            original_page_targets,
            reflow_page_targets,
            reflow_enabled: AtomicBool::new(mode.is_reflow()),
        }
    }

    /// Switches the representation observed through [`BookSource`].
    pub fn set_mode(&self, mode: PdfOcrViewMode) {
        self.reflow_enabled
            .store(mode.is_reflow(), Ordering::Release);
    }

    /// Returns the currently selected representation.
    #[must_use]
    pub fn mode(&self) -> PdfOcrViewMode {
        if self.reflow_enabled.load(Ordering::Acquire) {
            PdfOcrViewMode::Reflow
        } else {
            PdfOcrViewMode::Original
        }
    }

    /// Returns whether the reflowable OCR representation is active.
    #[must_use]
    pub fn is_reflow_enabled(&self) -> bool {
        self.mode().is_reflow()
    }

    /// Returns the canonical fixed-page source.
    #[must_use]
    pub fn original_source(&self) -> Arc<dyn BookSource> {
        Arc::clone(&self.original)
    }

    /// Maps a zero-based physical page to its reflow target.
    #[must_use]
    pub fn reflow_target_for_page(&self, page_index: usize) -> Option<PublicationUrl> {
        self.reflow_page_targets.get(page_index).cloned()
    }

    /// Maps a zero-based physical page to its canonical fixed-page target.
    #[must_use]
    pub fn original_target_for_page(&self, page_index: usize) -> Option<PublicationUrl> {
        self.original_page_targets.get(page_index).cloned()
    }

    /// Maps an OCR physical-page anchor fragment to the canonical page target.
    #[must_use]
    pub fn original_target_for_reflow_anchor(&self, fragment: &str) -> Option<PublicationUrl> {
        let page_number = fragment
            .strip_prefix(PDF_PAGE_ANCHOR_PREFIX)?
            .parse::<usize>()
            .ok()?;
        self.original_target_for_page(page_number.checked_sub(1)?)
    }

    /// Plans a toggle without mutating the controller or consulting a UI.
    ///
    /// A frontend can persist the plan, activate `next_mode`, refresh its reader
    /// at `navigation_target`, and restore `previous_mode` if that refresh fails.
    #[must_use]
    pub fn plan_toggle(&self, location: PdfOcrViewLocation) -> PdfOcrViewTransition {
        let previous_mode = self.mode();
        let next_mode = previous_mode.toggled();
        let navigation_target = match (previous_mode, location) {
            (PdfOcrViewMode::Original, PdfOcrViewLocation::OriginalPage(page_index)) => {
                self.reflow_target_for_page(page_index)
            }
            (PdfOcrViewMode::Reflow, PdfOcrViewLocation::ReflowAnchor(Some(fragment))) => {
                self.original_target_for_reflow_anchor(&fragment)
            }
            _ => None,
        };
        PdfOcrViewTransition {
            previous_mode,
            next_mode,
            navigation_target,
        }
    }

    fn active(&self) -> &Arc<dyn BookSource> {
        if self.reflow_enabled.load(Ordering::Acquire) {
            &self.reflow
        } else {
            &self.original
        }
    }
}

impl BookSource for PdfOcrSourceController {
    fn book(&self) -> &Book {
        self.active().book()
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.active().table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        self.active().parse_section(index)
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.active().resource(href)
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.active().raster_resource(href)
    }

    fn fixed_page_dimensions(
        &self,
        section_index: usize,
    ) -> Result<Option<rebook_publication::FixedPageDimensions>, PublicationError> {
        self.active().fixed_page_dimensions(section_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{Metadata, PublicationId, RenditionLayout, SpineItem, SpineItemId};

    struct StubBookSource {
        book: Book,
    }

    #[derive(Default)]
    struct RecordingModeTarget {
        writes: Vec<(String, PdfOcrViewMode)>,
        fail: bool,
    }

    impl PdfOcrViewModeMutationTarget for RecordingModeTarget {
        type Error = &'static str;

        fn persist_pdf_ocr_view_mode(
            &mut self,
            book_id: &str,
            mode: PdfOcrViewMode,
        ) -> Result<(), Self::Error> {
            if self.fail {
                return Err("write failed");
            }
            self.writes.push((book_id.to_owned(), mode));
            Ok(())
        }
    }

    impl BookSource for StubBookSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            Err(PublicationError::ResourceNotFound("stub".into()))
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    fn source(name: &str, layout: RenditionLayout, hrefs: &[&str]) -> Arc<dyn BookSource> {
        Arc::new(StubBookSource {
            book: Book {
                id: PublicationId::new(name).unwrap(),
                metadata: Metadata {
                    layout,
                    ..Metadata::default()
                },
                cover: None,
                sections: hrefs
                    .iter()
                    .enumerate()
                    .map(|(index, href)| SpineItem {
                        id: SpineItemId::new(format!("section-{index}")).unwrap(),
                        href: PublicationUrl::parse(href).unwrap(),
                        media_type: "application/xhtml+xml".into(),
                        linear: true,
                        properties: Vec::new(),
                    })
                    .collect(),
                table_of_contents: Vec::new(),
            },
        })
    }

    #[test]
    fn source_switches_layout_without_reopening_the_publication() {
        let original = source(
            "switch-original",
            RenditionLayout::PrePaginated,
            &["page-1.xhtml"],
        );
        let reflow = source("switch-reflow", RenditionLayout::Reflowable, &["ocr.xhtml"]);
        let controller =
            PdfOcrSourceController::new(original, reflow, Vec::new(), PdfOcrViewMode::Original);

        assert_eq!(
            controller.book().metadata.layout,
            RenditionLayout::PrePaginated
        );
        assert_eq!(controller.mode(), PdfOcrViewMode::Original);
        controller.set_mode(PdfOcrViewMode::Reflow);
        assert_eq!(
            controller.book().metadata.layout,
            RenditionLayout::Reflowable
        );
        assert!(controller.is_reflow_enabled());
        assert_eq!(
            controller.original_source().book().metadata.layout,
            RenditionLayout::PrePaginated
        );
    }

    #[test]
    fn physical_page_targets_round_trip_through_reflow_anchors() {
        let original = source(
            "mapping-original",
            RenditionLayout::PrePaginated,
            &["page-1.xhtml", "page-2.xhtml"],
        );
        let reflow = source(
            "mapping-reflow",
            RenditionLayout::Reflowable,
            &["ocr.xhtml"],
        );
        let reflow_targets = [
            PublicationUrl::parse("ocr.xhtml#pdf-page-1").unwrap(),
            PublicationUrl::parse("ocr.xhtml#pdf-page-2").unwrap(),
        ];
        let controller = PdfOcrSourceController::new(
            original,
            reflow,
            reflow_targets.to_vec(),
            PdfOcrViewMode::Reflow,
        );

        assert_eq!(
            controller.reflow_target_for_page(1),
            Some(reflow_targets[1].clone())
        );
        assert_eq!(
            controller.original_target_for_reflow_anchor("pdf-page-2"),
            Some(PublicationUrl::parse("page-2.xhtml").unwrap())
        );
        assert_eq!(
            controller.original_target_for_reflow_anchor("pdf-page-0"),
            None
        );
        assert_eq!(
            controller.original_target_for_reflow_anchor("chapter-2"),
            None
        );

        controller.set_mode(PdfOcrViewMode::Original);
        let to_reflow = controller.plan_toggle(PdfOcrViewLocation::OriginalPage(1));
        assert_eq!(to_reflow.previous_mode, PdfOcrViewMode::Original);
        assert_eq!(to_reflow.next_mode, PdfOcrViewMode::Reflow);
        assert_eq!(to_reflow.navigation_target, Some(reflow_targets[1].clone()));
        assert_eq!(controller.mode(), PdfOcrViewMode::Original);

        controller.set_mode(PdfOcrViewMode::Reflow);
        let to_original =
            controller.plan_toggle(PdfOcrViewLocation::ReflowAnchor(Some("pdf-page-2".into())));
        assert_eq!(to_original.previous_mode, PdfOcrViewMode::Reflow);
        assert_eq!(to_original.next_mode, PdfOcrViewMode::Original);
        assert_eq!(
            to_original.navigation_target,
            Some(PublicationUrl::parse("page-2.xhtml").unwrap())
        );
    }

    #[test]
    fn view_mode_keeps_the_version_one_json_encoding() {
        assert_eq!(
            serde_json::to_string(&PdfOcrViewMode::Original).unwrap(),
            r#""original""#
        );
        assert_eq!(PdfOcrViewMode::Original.toggled(), PdfOcrViewMode::Reflow);
        assert_eq!(PdfOcrViewMode::Reflow.toggled(), PdfOcrViewMode::Original);
    }

    #[test]
    fn view_mode_changes_only_after_persistence_succeeds() {
        let controller = PdfOcrSourceController::new(
            source(
                "mode-original",
                RenditionLayout::PrePaginated,
                &["page.xhtml"],
            ),
            source("mode-reflow", RenditionLayout::Reflowable, &["ocr.xhtml"]),
            Vec::new(),
            PdfOcrViewMode::Original,
        );
        let mut target = RecordingModeTarget {
            fail: true,
            ..RecordingModeTarget::default()
        };
        assert_eq!(
            apply_pdf_ocr_view_mode(&mut target, &controller, "book-a", PdfOcrViewMode::Reflow,),
            Err("write failed")
        );
        assert_eq!(controller.mode(), PdfOcrViewMode::Original);

        target.fail = false;
        assert_eq!(
            apply_pdf_ocr_view_mode(&mut target, &controller, "book-a", PdfOcrViewMode::Reflow,),
            Ok(PdfOcrViewMode::Original)
        );
        assert_eq!(controller.mode(), PdfOcrViewMode::Reflow);
        assert_eq!(target.writes, [("book-a".into(), PdfOcrViewMode::Reflow)]);

        target.fail = true;
        assert_eq!(
            rollback_pdf_ocr_view_mode(
                &mut target,
                &controller,
                "book-a",
                PdfOcrViewMode::Original,
            ),
            Err("write failed")
        );
        assert_eq!(controller.mode(), PdfOcrViewMode::Original);
    }
}
