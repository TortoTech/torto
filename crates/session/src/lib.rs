//! Toolkit-neutral composition of one opened document's derived source layers.
//!
//! Applications open a canonical [`BookSource`], then install assistant and
//! presentation overlays through [`DocumentSourcePipeline`]. Keeping the order
//! here prevents egui and GPUI frontends from constructing subtly different
//! Reading IR views while all durable locations continue to use source anchors.

mod annotations;
mod assistant_tools;
mod pdf_metadata;
mod pdf_ocr;
mod pdf_ocr_source;
mod preferences;

pub use annotations::{StoredHighlightMutationTarget, StoredHighlightUndo};
pub use assistant_tools::DocumentAssistantToolHost;
pub use pdf_metadata::{
    GeneratedPdfMetadata, GeneratedTocDraft, GeneratedTocEntry, PdfDocumentMetadataCommand,
    PdfDocumentMetadataMutationTarget, PdfOcrPageRole, PdfOcrPageRoleAssignment,
    apply_pdf_document_metadata_command, normalize_generated_pdf_metadata, normalize_generated_toc,
    normalize_generated_toc_entries, normalize_pdf_ocr_page_roles,
};
pub use pdf_ocr::{
    PDF_PAGE_ANCHOR_PREFIX, PdfOcrLoadedSource, PdfOcrSourceController, PdfOcrViewLocation,
    PdfOcrViewMode, PdfOcrViewModeMutationTarget, PdfOcrViewTransition, apply_pdf_ocr_view_mode,
    rollback_pdf_ocr_view_mode,
};
pub use pdf_ocr_source::{
    PdfOcrMarkupEngine, PdfOcrReflowBookSource, PdfOcrReflowDocument, PdfOcrReflowPage,
    PdfOcrReflowResource, PdfOcrTocAnchor,
};
pub use preferences::{
    ReaderDocumentPreferenceChange, ReaderDocumentPreferences, ResolvedReaderDocumentPreferences,
};

use std::sync::Arc;

use rebook_assistant::{RewriteBookSource, TranslationBookSource, TranslationMode};
use rebook_publication::{BookSource, RenditionLayout};
use rebook_reader::ParagraphStructureSource;

/// Ordered, source-preserving views of one canonical publication.
///
/// The pipeline is deliberately independent of UI, layout, rendering, storage,
/// and provider configuration. Its fixed order is:
///
/// 1. canonical parser/OCR source;
/// 2. assistant rewrite overlay;
/// 3. translation overlay;
/// 4. sentence-structure presentation overlay.
///
/// Every layer retains the canonical spine and node identities. Mutable overlay
/// state remains available through typed handles so a document session can
/// toggle translation, roll back rewrites, or structure a paragraph without
/// rebuilding or downcasting the chain.
#[derive(Clone)]
pub struct DocumentSourcePipeline {
    canonical: Arc<dyn BookSource>,
    rewrite: Arc<RewriteBookSource>,
    translation: Arc<TranslationBookSource>,
    structure: Arc<ParagraphStructureSource>,
    presented: Arc<dyn BookSource>,
}

impl DocumentSourcePipeline {
    /// Builds the shared derived-source chain around a canonical publication.
    #[must_use]
    pub fn new(canonical: Arc<dyn BookSource>, translation_mode: TranslationMode) -> Self {
        let fixed_page = canonical.book().metadata.layout == RenditionLayout::PrePaginated;
        let rewrite = Arc::new(RewriteBookSource::new(Arc::clone(&canonical)));
        let translation = Arc::new(if fixed_page {
            TranslationBookSource::new_fixed_page(rewrite.clone(), translation_mode)
        } else {
            TranslationBookSource::new(rewrite.clone(), translation_mode)
        });
        let structure = Arc::new(ParagraphStructureSource::new(translation.clone()));
        let presented: Arc<dyn BookSource> = structure.clone();
        Self {
            canonical,
            rewrite,
            translation,
            structure,
            presented,
        }
    }

    /// Canonical parser/OCR source before any derived presentation overlays.
    #[must_use]
    pub fn canonical_source(&self) -> &Arc<dyn BookSource> {
        &self.canonical
    }

    /// Transactional assistant rewrite overlay.
    #[must_use]
    pub fn rewrite_source(&self) -> &Arc<RewriteBookSource> {
        &self.rewrite
    }

    /// Toggleable replace/bilingual translation overlay.
    #[must_use]
    pub fn translation_source(&self) -> &Arc<TranslationBookSource> {
        &self.translation
    }

    /// Sentence-sized paragraph presentation overlay.
    #[must_use]
    pub fn structure_source(&self) -> &Arc<ParagraphStructureSource> {
        &self.structure
    }

    /// Outermost source that reader/layout sessions should consume.
    #[must_use]
    pub fn presented_source(&self) -> &Arc<dyn BookSource> {
        &self.presented
    }
}

#[cfg(test)]
mod tests {
    use rebook_assistant::{BlockRewrite, BlockTranslation, text_block_text};
    use rebook_publication::{
        Block, BlockStyle, Book, Inline, Metadata, PublicationError, PublicationId, PublicationUrl,
        Resource, Section, SourceAnchor, SourceRange, SpineItem, SpineItemId, TextBlock,
        TextBlockKind, TextRun, TextStyle,
    };
    use rebook_reader::ParagraphStructureKey;

    use super::*;

    struct TestSource {
        book: Book,
        section: Section,
    }

    impl BookSource for TestSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            Ok(self.section.clone())
        }

        fn resource(&self, _href: &PublicationUrl) -> Result<Resource, PublicationError> {
            unreachable!()
        }
    }

    fn source(layout: RenditionLayout) -> Arc<dyn BookSource> {
        let spine = SpineItemId::new("chapter").unwrap();
        let href = PublicationUrl::parse("chapter.xhtml").unwrap();
        let text = "Original sentence. Another sentence.";
        let range = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: u64::try_from(text.chars().count()).unwrap(),
            },
        };
        let metadata = Metadata {
            layout,
            ..Metadata::default()
        };
        Arc::new(TestSource {
            book: Book {
                id: PublicationId::new("source-pipeline-test").unwrap(),
                metadata,
                cover: None,
                sections: vec![SpineItem {
                    id: spine.clone(),
                    href: href.clone(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            section: Section {
                id: spine,
                href,
                blocks: vec![Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: text.into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(range),
                })],
                anchors: Vec::new(),
            },
        })
    }

    fn first_text(section: &Section) -> &TextBlock {
        let Some(Block::Text(block)) = section.blocks.first() else {
            panic!("expected a text block");
        };
        block
    }

    #[test]
    fn inactive_pipeline_preserves_canonical_reading_ir_and_source_range() {
        let canonical = source(RenditionLayout::Reflowable);
        let expected = canonical.parse_section(0).unwrap();
        let expected_range = first_text(&expected).source.clone();
        let pipeline =
            DocumentSourcePipeline::new(Arc::clone(&canonical), TranslationMode::Bilingual);

        let actual = pipeline.presented_source().parse_section(0).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(first_text(&actual).source, expected_range);
        assert!(Arc::ptr_eq(pipeline.canonical_source(), &canonical));
    }

    #[test]
    fn composes_rewrite_translation_then_sentence_structure() {
        let pipeline = DocumentSourcePipeline::new(
            source(RenditionLayout::Reflowable),
            TranslationMode::Replace,
        );
        pipeline
            .rewrite_source()
            .apply_rewrites(&[BlockRewrite {
                section_index: 0,
                block_id: "paragraph-1".into(),
                text: "Rewritten sentence. Another rewritten sentence.".into(),
            }])
            .unwrap();
        let key = ParagraphStructureKey {
            section_index: 0,
            node: "paragraph-1".into(),
        };
        pipeline.structure_source().set_active(key, true).unwrap();

        let rewritten = pipeline.presented_source().parse_section(0).unwrap();
        assert_eq!(
            text_block_text(first_text(&rewritten)),
            "Rewritten sentence. \n\nAnother rewritten sentence."
        );

        pipeline
            .translation_source()
            .store_batch(
                0,
                &[BlockTranslation {
                    block_index: 0,
                    segment_index: None,
                    text: "Translated sentence. Another translated sentence.".into(),
                }],
            )
            .unwrap();
        pipeline.translation_source().set_enabled(true).unwrap();

        let translated = pipeline.presented_source().parse_section(0).unwrap();
        let translated = first_text(&translated);
        assert_eq!(
            text_block_text(translated),
            "Translated sentence. \n\nAnother translated sentence."
        );
        let range = translated.source.as_ref().unwrap();
        assert_eq!(range.start.node, "paragraph-1");
        assert_eq!(range.start.spine.as_str(), "chapter");
    }

    #[test]
    fn fixed_page_pipeline_forces_replace_translation_mode() {
        let pipeline = DocumentSourcePipeline::new(
            source(RenditionLayout::PrePaginated),
            TranslationMode::Bilingual,
        );
        pipeline.translation_source().set_enabled(true).unwrap();
        pipeline
            .translation_source()
            .store_batch(
                0,
                &[BlockTranslation {
                    block_index: 0,
                    segment_index: None,
                    text: "Translated fixed page.".into(),
                }],
            )
            .unwrap();

        let section = pipeline.presented_source().parse_section(0).unwrap();

        assert_eq!(section.blocks.len(), 1);
        assert_eq!(
            text_block_text(first_text(&section)),
            "Translated fixed page."
        );
    }
}
