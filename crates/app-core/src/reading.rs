//! Shared composition order. Translation keeps original source identities;
//! semantic layout and paragraph structure operate on the translated view.
use crate::plugins::{
    ParagraphStructureSource, PluginSettings, RewriteBookSource, TranslationBookSource,
    semantic_layout::SemanticLayoutSource,
};
use rebook_publication::{BookSource, RenditionLayout};
use std::sync::Arc;

pub struct ReadingPipeline {
    pub rewrite: Arc<RewriteBookSource>,
    pub translation: Arc<TranslationBookSource>,
    pub semantic: Arc<SemanticLayoutSource>,
    pub structure: Arc<ParagraphStructureSource>,
}
impl ReadingPipeline {
    pub fn new(
        original: Arc<dyn BookSource>,
        book_id: &str,
        settings: &PluginSettings,
        fonts: Arc<[peniko::Blob<u8>]>,
    ) -> Self {
        let fixed = original.book().metadata.layout == RenditionLayout::PrePaginated;
        let rewrite = Arc::new(RewriteBookSource::new(original));
        let translation = Arc::new(if fixed {
            TranslationBookSource::new_fixed_page(rewrite.clone(), settings.translation_mode)
        } else {
            TranslationBookSource::new(rewrite.clone(), settings.translation_mode)
        });
        let semantic = Arc::new(SemanticLayoutSource::with_fonts(
            translation.clone(),
            rewrite.clone(),
            fonts,
        ));
        if !fixed {
            semantic.configure(
                &crate::plugins::pdf_native::cache_identity(book_id, rewrite.as_ref()),
                settings,
            );
        }
        let structure = Arc::new(ParagraphStructureSource::new(semantic.clone()));
        Self {
            rewrite,
            translation,
            semantic,
            structure,
        }
    }
}
