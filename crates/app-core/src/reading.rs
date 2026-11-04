//! Shared composition order. Translation keeps original source identities;
//! semantic layout and paragraph structure operate on the translated view.
use crate::plugins::{
    ParagraphStructureSource, PluginSettings, RewriteBookSource, TranslationBookSource,
    semantic_layout::SemanticLayoutSource,
};
use rebook_publication::{BookSource, RenditionLayout};
use std::sync::Arc;

/// Focus presentation never changes the saved classic layout preference.
pub fn effective_typesetting(
    focus: bool,
    configured: &rebook_layout::ReaderTypesetting,
) -> rebook_layout::ReaderTypesetting {
    if focus {
        rebook_layout::ReaderTypesetting::unified()
    } else {
        configured.clone()
    }
}

/// Change presentation without losing the reader's spacing preferences.
pub fn typesetting(
    profile: &rebook_layout::ReaderTypesetting,
    mode: rebook_layout::TypesettingMode,
) -> rebook_layout::ReaderTypesetting {
    let mut profile = profile.clone();
    profile.mode = mode;
    profile.line_break_strategy = if mode == rebook_layout::TypesettingMode::Unified {
        rebook_layout::LineBreakStrategy::Optimized
    } else {
        rebook_layout::LineBreakStrategy::Greedy
    };
    profile
}

pub fn note_style(reader: &rebook_layout::ReaderStyle, size: f32) -> rebook_layout::ReaderStyle {
    let mut style = reader.clone();
    style.typography.font_size = size;
    style.typography.minimum_font_size = size;
    style.horizontal_margin = 2.0;
    style.minimum_horizontal_margin = 0.0;
    style.minimum_content_width = 0.0;
    style.top_margin = 0.0;
    style.bottom_margin = 0.0;
    style.spread = rebook_layout::SpreadMode::Single;
    style.focus_footnote_icons = false;
    style.website_icons = reader.typesetting.mode == rebook_layout::TypesettingMode::Unified;
    style.typesetting.mode = rebook_layout::TypesettingMode::Book;
    style.typesetting.line_break_strategy = rebook_layout::LineBreakStrategy::Optimized;
    style
}

/// Parse native note markup through the shared HTML IR and reading renderer.
/// Math is kept semantic; authored markup never executes HTML or fetches URLs.
pub fn note_section(
    markup: &str,
    descriptor: &rebook_publication::SpineItem,
) -> Result<rebook_publication::Section, String> {
    use pulldown_cmark::{Event, Options, Parser, html};
    let strong = crate::chat_markdown::normalize_loose_strong_delimiters(markup);
    let normalized = crate::chat_markdown::normalize_math_delimiters(&strong);
    let events = Parser::new_ext(
        &normalized,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_MATH,
    )
    .map(|event| match event {
        math @ (Event::InlineMath(_) | Event::DisplayMath(_)) => {
            let display = matches!(math, Event::DisplayMath(_));
            let (Event::InlineMath(latex) | Event::DisplayMath(latex)) = math else {
                unreachable!()
            };
            let escaped = latex
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            Event::InlineHtml(
                format!(
                    "<span class=\"math{}\">{escaped}</span>",
                    if display { " math-display" } else { "" }
                )
                .into(),
            )
        }
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        other => other,
    });
    let mut body = String::new();
    html::push_html(&mut body, events);
    rebook_html::parse_section(
        &format!("<html xmlns=\"http://www.w3.org/1999/xhtml\"><head/><body>{body}</body></html>"),
        descriptor,
        |_| None,
    )
    .map_err(|e| e.to_string())
}

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

/// A popup retains only its selected IR and delegates resources/semantic notes.
/// It does not create or persist a second reading position for the book.
pub struct FragmentSource {
    parent: Arc<dyn BookSource>,
    book: rebook_publication::Book,
    section: rebook_publication::Section,
}
impl FragmentSource {
    pub fn new(parent: Arc<dyn BookSource>, section: rebook_publication::Section) -> Arc<Self> {
        Self::with_origin(parent, section, None)
    }
    pub fn with_origin(
        parent: Arc<dyn BookSource>,
        mut section: rebook_publication::Section,
        origin: Option<&rebook_publication::SourceRange>,
    ) -> Arc<Self> {
        let mut book = parent.book().clone();
        book.metadata.layout = rebook_publication::RenditionLayout::Reflowable;
        book.sections = vec![rebook_publication::SpineItem {
            id: section.id.clone(),
            href: section.href.clone(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        }];
        book.table_of_contents.clear();
        for text in section
            .blocks
            .iter_mut()
            .flat_map(rebook_publication::Block::text_blocks_mut)
        {
            parent.prepare_media_references(&mut text.content, origin.or(text.source.as_ref()));
        }
        Arc::new(Self {
            parent,
            book,
            section,
        })
    }
}
impl BookSource for FragmentSource {
    fn book(&self) -> &rebook_publication::Book {
        &self.book
    }
    fn inner_source(&self) -> Option<&dyn BookSource> {
        Some(self.parent.as_ref())
    }
    fn parse_section(
        &self,
        index: usize,
    ) -> Result<rebook_publication::Section, rebook_publication::PublicationError> {
        if index != 0 {
            return Err(rebook_publication::PublicationError::ResourceNotFound(
                index.to_string(),
            ));
        }
        Ok(self.section.clone())
    }
    fn resource(
        &self,
        href: &rebook_publication::PublicationUrl,
    ) -> Result<rebook_publication::Resource, rebook_publication::PublicationError> {
        self.parent.resource(href)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::*;
    #[test]
    fn unified_composition_and_notes_inherit_the_reading_profile() {
        let mut reader = ReaderStyle::default();
        reader.typography.monospace_font = "Reader mono".into();
        reader.typography.font_weight = 575;
        reader.typesetting.body_line_height = 1.7;
        let unified = typesetting(&reader.typesetting, TypesettingMode::Unified);
        assert_eq!(unified.line_break_strategy, LineBreakStrategy::Optimized);
        assert_eq!(
            unified.body_line_height,
            reader.typesetting.body_line_height
        );
        assert_eq!(
            typesetting(&unified, TypesettingMode::Book).line_break_strategy,
            LineBreakStrategy::Greedy
        );
        reader.typesetting = unified;
        let note = note_style(&reader, 23.0);
        assert_eq!(note.typography.monospace_font, "Reader mono");
        assert_eq!(note.typography.font_weight, 575);
        assert_eq!(note.typography.font_size, 23.0);
        assert_eq!(note.minimum_content_width, 0.0);
        assert_eq!(
            note.typesetting.line_break_strategy,
            LineBreakStrategy::Optimized
        );
        assert_eq!(note.spread, SpreadMode::Single);
        assert!(note.website_icons);
    }
}
