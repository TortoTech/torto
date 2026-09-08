mod book;
mod config;
mod error;
mod reader;

use std::path::Path;
use std::sync::Arc;

use rebook_formats::open_bytes;
use rebook_layout::ReaderFontBlob;

pub use book::EngineBook;
pub use config::{EngineConfig, ReaderConfig};
pub use error::EngineError;
pub use reader::EngineReader;
pub use rebook_reader::{
    NavigationAttempt, NavigationPreparation, NavigationResult, NavigationToken, PageDirection,
    PreparedNavigation, ReaderError, ReaderPosition, ReaderSession, ReaderSnapshot, ReaderSpread,
};

pub struct Engine {
    fonts: Arc<[ReaderFontBlob]>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            fonts: config.fonts,
        }
    }

    pub fn fonts(&self) -> Arc<[ReaderFontBlob]> {
        Arc::clone(&self.fonts)
    }

    pub fn open_bytes(
        &self,
        bytes: impl Into<Arc<[u8]>>,
        file_name: &str,
    ) -> Result<EngineBook, EngineError> {
        let opened = open_bytes(bytes, file_name)?;
        Ok(EngineBook::new(opened))
    }

    pub fn open_file(&self, path: impl AsRef<Path>) -> Result<EngineBook, EngineError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("book");
        self.open_bytes(bytes, file_name)
    }

    pub fn create_reader(
        &self,
        book: &EngineBook,
        config: ReaderConfig,
    ) -> Result<EngineReader, EngineError> {
        let session = if let Some(locator) = &config.locator {
            ReaderSession::open_with_fonts_at_locator(
                book.source(),
                config.viewport,
                config.style,
                Arc::clone(&self.fonts),
                locator,
            )?
        } else {
            ReaderSession::open_with_fonts(
                book.source(),
                config.viewport,
                config.style,
                Arc::clone(&self.fonts),
            )?
        };
        Ok(EngineReader::new(session))
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(EngineConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::{LayoutViewport, ReaderStyle};
    use rebook_publication::{
        Block, BlockStyle, Book, BookSource, Inline, Metadata, PublicationError, PublicationId,
        PublicationUrl, Resource, Section, SpineItem, SpineItemId, TextBlock, TextBlockKind,
        TextRun, TextStyle,
    };
    use std::sync::Arc;

    struct InMemorySource {
        book: Book,
        section: Section,
    }

    impl InMemorySource {
        fn new() -> Self {
            let id = SpineItemId::new("chapter1").unwrap();
            let href = PublicationUrl::parse("chapter1.xhtml").unwrap();
            let section = Section {
                id: id.clone(),
                href: href.clone(),
                blocks: vec![Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: "Hello world from engine facade!".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: None,
                })],
                anchors: Vec::new(),
            };
            let book = Book {
                id: PublicationId::new("test-book").unwrap(),
                metadata: Metadata {
                    title: "Test Book".into(),
                    authors: vec!["Author".into()],
                    ..Default::default()
                },
                table_of_contents: Vec::new(),
                sections: vec![SpineItem {
                    id,
                    href,
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                cover: None,
            };
            Self { book, section }
        }
    }

    impl BookSource for InMemorySource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, _index: usize) -> Result<Section, PublicationError> {
            Ok(self.section.clone())
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    #[test]
    fn unsupported_file_name_produces_typed_error() {
        let engine = Engine::default();
        let result = engine.open_bytes(vec![0, 1, 2, 3], "unknown.xyz");
        assert!(result.is_err());
        match result {
            Err(EngineError::Format(rebook_formats::FormatError::UnsupportedFormat(name))) => {
                assert_eq!(name, "unknown.xyz");
            }
            _ => panic!("expected UnsupportedFormat"),
        }
    }

    #[test]
    fn reader_creation_and_spread_works_with_custom_source() {
        let source: Arc<dyn BookSource> = Arc::new(InMemorySource::new());
        let session = ReaderSession::open_with_fonts(
            source,
            LayoutViewport {
                width: 800,
                height: 1000,
            },
            ReaderStyle::default(),
            Arc::default(),
        )
        .unwrap();
        let mut reader = EngineReader::new(session);

        assert_eq!(reader.book().metadata.title, "Test Book");
        let spread = reader.current_spread().unwrap();
        assert!(spread.primary.width() > 0);
        let locator = reader.current_locator();
        assert_eq!(locator.publication_id.as_str(), "test-book");

        // Test navigation preparation on facade
        let boundary = reader.prepare_navigation(PageDirection::Previous).unwrap();
        assert!(matches!(boundary, NavigationPreparation::Boundary));
    }
}
