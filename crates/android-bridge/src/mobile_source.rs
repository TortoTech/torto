//! Reader-owned page spacing; the publication and desktop parsing stay unchanged.
use rebook_publication::{Book, BookSource, PublicationError, PublicationUrl, RenditionLayout, Resource, Section};
use std::sync::Arc;

pub(crate) fn for_reading(source: Arc<dyn BookSource>) -> Arc<dyn BookSource> {
    if source.book().metadata.layout == RenditionLayout::PrePaginated {
        source
    } else {
        Arc::new(MobileSource(source))
    }
}

struct MobileSource(Arc<dyn BookSource>);

impl BookSource for MobileSource {
    fn inner_source(&self) -> Option<&dyn BookSource> {
        Some(self.0.as_ref())
    }
    fn book(&self) -> &Book {
        self.0.book()
    }
    fn content_revision(&self) -> Option<u64> {
        self.0.content_revision()
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        // Unified layout already resets authored margins. Filtering at the
        // mobile source also keeps book layout and all source-based tools aligned.
        self.0.parse_section_without_body_margins(index)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.0.resource(href)
    }
}
