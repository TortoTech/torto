use crate::{
    Book, FixedPageDimensions, FormulaNote, Inline, PublicationError, PublicationUrl,
    RasterResource, Resource, Section, SourceRange, TableOfContentsOrigin,
};

/// Lazy source boundary: parsers produce stable reading IR one section at a time.
pub trait BookSource: Send + Sync {
    /// Optional wrapped source for pass-through capabilities. This must be a
    /// memory-only lookup: never initialize a lazy source here. Section output,
    /// content revisions and fingerprints deliberately remain explicit because
    /// a wrapper may transform parsed content.
    fn inner_source(&self) -> Option<&dyn BookSource> {
        None
    }

    /// Memory-only, already validated notes currently visible for this formula.
    fn visible_formula_notes(&self, source: &SourceRange, latex: &str) -> Vec<FormulaNote> {
        self.inner_source()
            .map_or_else(Vec::new, |inner| inner.visible_formula_notes(source, latex))
    }
    /// Memory-only status: a render encountered an unavailable font resource.
    /// Wrappers forward this without forcing their lazy source to load.
    fn missing_font_resources(&self) -> bool {
        self.inner_source()
            .is_some_and(BookSource::missing_font_resources)
    }
    /// Apply an already prepared, memory-only media catalog to popup content.
    /// Implementations must never parse sections or load resources here.
    fn prepare_media_references(&self, content: &mut Vec<Inline>, origin: Option<&SourceRange>) {
        if let Some(inner) = self.inner_source() {
            inner.prepare_media_references(content, origin);
        }
    }
    fn media_reference_revision(&self) -> u64 {
        self.inner_source()
            .map_or(0, BookSource::media_reference_revision)
    }
    /// Lightweight descriptor available immediately after opening.
    fn book(&self) -> &Book;
    /// Optional generation for the parsed content. A source returning `Some`
    /// must advance it whenever its section output changes; `None` requires
    /// consumers to validate derived caches against the content itself.
    fn content_revision(&self) -> Option<u64> {
        None
    }
    /// Reports whether navigation is authored, mechanical, or generated.
    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.inner_source().map_or(
            TableOfContentsOrigin::Embedded,
            BookSource::table_of_contents_origin,
        )
    }
    /// Parses one section into the normalized reading IR.
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError>;
    /// Optional SHA-256 of `serde_json::to_vec()` for the last parsed section.
    /// This is a cached lookup, never I/O. Derived sources must return `None`
    /// when their output differs; consumers also validate `content_revision`.
    fn cached_section_fingerprint(&self, _index: usize) -> Option<[u8; 32]> {
        None
    }
    /// Loads a referenced resource subject to format-specific budgets.
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError>;
    /// An authored cover page used when no usable standalone cover image exists.
    fn cover_section(&self) -> Result<Option<Section>, PublicationError> {
        self.inner_source()
            .map_or(Ok(None), BookSource::cover_section)
    }
    /// Returns an already decoded image when the format can provide one cheaply.
    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.inner_source()
            .map_or(Ok(None), |inner| inner.raster_resource(href))
    }
    /// Returns the eventual fixed-page raster dimensions without materializing
    /// its pixels. Sources that cannot provide this cheaply return `None`.
    fn fixed_page_dimensions(
        &self,
        section_index: usize,
    ) -> Result<Option<FixedPageDimensions>, PublicationError> {
        self.inner_source()
            .map_or(Ok(None), |inner| inner.fixed_page_dimensions(section_index))
    }

    /// Dimensions of a source-owned raster, without generating its pixels.
    /// Such resources must be immutable for the publication ID and revision;
    /// this lets interactive readers schedule materialization after layout.
    fn raster_resource_dimensions(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<[u32; 2]>, PublicationError> {
        self.inner_source()
            .map_or(Ok(None), |inner| inner.raster_resource_dimensions(href))
    }

    /// Intrinsic pixel dimensions of an encoded image, without decoding pixels.
    /// A source returning `Some` guarantees the encoded bytes are immutable for
    /// this publication ID and content revision. Unlike source-owned rasters,
    /// these images still use `resource()` and the renderer's display resizer.
    fn image_resource_dimensions(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<[u32; 2]>, PublicationError> {
        self.inner_source()
            .map_or(Ok(None), |inner| inner.image_resource_dimensions(href))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Metadata, PublicationId, RenditionLayout, SourceAnchor, SpineItemId};

    struct Original(Book);
    impl BookSource for Original {
        fn book(&self) -> &Book {
            &self.0
        }
        fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
            Ok(Section {
                id: SpineItemId::new("chapter").unwrap(),
                href: PublicationUrl::parse("chapter.xhtml").unwrap(),
                blocks: vec![],
                anchors: vec![],
            })
        }
        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Ok(Resource {
                href: href.clone(),
                media_type: "image/png".into(),
                bytes: vec![1].into(),
            })
        }
        fn missing_font_resources(&self) -> bool {
            true
        }
        fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
            TableOfContentsOrigin::Generated
        }
        fn content_revision(&self) -> Option<u64> {
            Some(7)
        }
        fn cached_section_fingerprint(&self, _: usize) -> Option<[u8; 32]> {
            Some([7; 32])
        }
        fn cover_section(&self) -> Result<Option<Section>, PublicationError> {
            self.parse_section(0).map(Some)
        }
        fn image_resource_dimensions(
            &self,
            _: &PublicationUrl,
        ) -> Result<Option<[u32; 2]>, PublicationError> {
            Ok(Some([20, 30]))
        }
        fn raster_resource_dimensions(
            &self,
            _: &PublicationUrl,
        ) -> Result<Option<[u32; 2]>, PublicationError> {
            Ok(Some([2, 3]))
        }
        fn fixed_page_dimensions(
            &self,
            _: usize,
        ) -> Result<Option<FixedPageDimensions>, PublicationError> {
            Ok(Some(FixedPageDimensions {
                width: 40,
                height: 50,
            }))
        }
        fn raster_resource(
            &self,
            _: &PublicationUrl,
        ) -> Result<Option<RasterResource>, PublicationError> {
            Ok(Some(RasterResource {
                width: 1,
                height: 1,
                pixels: vec![0; 4].into(),
            }))
        }
        fn media_reference_revision(&self) -> u64 {
            9
        }
        fn prepare_media_references(&self, content: &mut Vec<Inline>, _: Option<&SourceRange>) {
            content.push(Inline::Break);
        }
        fn visible_formula_notes(&self, _: &SourceRange, _: &str) -> Vec<FormulaNote> {
            vec![FormulaNote {
                symbol: "x".into(),
                meaning: "position".into(),
                unit: "m".into(),
            }]
        }
    }
    struct Wrapped<S>(S);
    impl<S: BookSource> BookSource for Wrapped<S> {
        fn inner_source(&self) -> Option<&dyn BookSource> {
            Some(&self.0)
        }
        fn book(&self) -> &Book {
            self.0.book()
        }
        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            self.0.parse_section(index)
        }
        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            self.0.resource(href)
        }
    }

    #[test]
    fn nested_wrappers_keep_capabilities_without_reusing_transformed_content_fingerprints() {
        let source = Wrapped(Wrapped(Original(Book {
            id: PublicationId::new("book").unwrap(),
            metadata: Metadata {
                title: "Book".into(),
                authors: vec![],
                languages: vec![],
                layout: RenditionLayout::Reflowable,
            },
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        })));
        let href = PublicationUrl::parse("image.png").unwrap();
        assert!(source.missing_font_resources());
        assert_eq!(
            source.table_of_contents_origin(),
            TableOfContentsOrigin::Generated
        );
        assert!(source.cover_section().unwrap().is_some());
        assert_eq!(
            source.image_resource_dimensions(&href).unwrap(),
            Some([20, 30])
        );
        assert_eq!(
            source.raster_resource_dimensions(&href).unwrap(),
            Some([2, 3])
        );
        assert_eq!(source.fixed_page_dimensions(0).unwrap().unwrap().width, 40);
        assert_eq!(
            source.raster_resource(&href).unwrap().unwrap().pixels.len(),
            4
        );
        assert_eq!(source.media_reference_revision(), 9);
        let mut content = vec![];
        source.prepare_media_references(&mut content, None);
        assert_eq!(content, [Inline::Break]);
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        assert_eq!(
            source.visible_formula_notes(
                &SourceRange {
                    start: anchor.clone(),
                    end: anchor
                },
                "x"
            )[0]
            .meaning,
            "position"
        );
        assert_eq!(source.content_revision(), None);
        assert_eq!(source.cached_section_fingerprint(0), None);
    }
}
