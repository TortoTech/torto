use super::*;
use rebook_publication::{
    Book, FigureBlock, InlineImageAlignment, InlineImageRun, Metadata, PublicationId, Resource,
    TableCell, TableRow,
};

struct Source {
    book: Book,
    bytes: Arc<[u8]>,
}
impl Source {
    fn new(id: &str) -> Self {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            240,
            160,
            image::Rgba([20, 80, 40, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        Self {
            book: Book {
                id: PublicationId::new(id).unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
            bytes: png.into_inner().into(),
        }
    }
}
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        unreachable!()
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Ok(Resource {
            href: href.clone(),
            media_type: "image/png".into(),
            bytes: self.bytes.clone(),
        })
    }
}
fn image(name: &str) -> ImageBlock {
    ImageBlock {
        href: PublicationUrl::parse(name).unwrap(),
        alt: name.into(),
        style: ImageStyle::default(),
        source: None,
        formula_image: false,
        formula: None,
        text_layer: None,
    }
}
fn text(kind: TextBlockKind, content: Vec<Inline>) -> TextBlock {
    TextBlock {
        kind,
        content,

        style: BlockStyle::default(),
        source: None,
    }
}
fn inline(name: &str) -> Inline {
    Inline::Image(Box::new(InlineImageRun {
        image: image(name),
        size_scale: 1.0,
        intrinsic_sizing: true,
        presentation: false,
        vertical_align: InlineImageAlignment::Middle,
    }))
}
fn geometry(layout: &SectionLayout) -> Vec<(f32, f32, f32, f32)> {
    layout
        .pages
        .iter()
        .flat_map(|page| {
            page.items.iter().map(|item| match item {
                PageItem::Image(i) => (i.x, i.y, i.width, i.height),
                PageItem::Text(t) => (t.origin_x, t.origin_y, t.layout.width(), t.layout.height()),
                PageItem::Table(t) => (0.0, t.y, 0.0, t.height),
                _ => (0.0, 0.0, 0.0, 0.0),
            })
        })
        .collect()
}

#[test]
fn figure_caption_inline_and_table_geometry_does_not_depend_on_pixels() {
    let source = Source::new("deferred-geometry");
    let blocks = vec![
        Block::Figure(FigureBlock {
            source: None,
            images: vec![image("figure.png")],
            captions: vec![text(
                TextBlockKind::Caption,
                vec![
                    Inline::Text(TextRun {
                        text: "A caption".into(),
                        style: TextStyle::default(),
                        link: None,
                    }),
                    Inline::Break,
                    Inline::Text(TextRun {
                        text: "Forced second line".into(),
                        style: TextStyle::default(),
                        link: None,
                    }),
                ],
            )],
            caption_position: CaptionPosition::After,
            style: BlockStyle::default(),
        }),
        Block::Text(text(TextBlockKind::Paragraph, vec![inline("inline.png")])),
        Block::Table(TableBlock {
            before: Vec::new(),
            after: Vec::new(),
            source: None,
            rows: vec![TableRow {
                cells: vec![TableCell {
                    text: text(TextBlockKind::Paragraph, vec![inline("cell.png")]),
                    authored_alignment: None,
                    column_span: 1,
                    row_span: 1,
                    header: false,
                }],
            }],
        }),
    ];
    let mut engine = LayoutEngine::new();
    let style = ReaderStyle {
        spread: SpreadMode::Scroll,
        ..ReaderStyle::default()
    };
    let viewport = LayoutViewport::new(600, 800).unwrap();
    let eager = engine
        .layout_blocks(&source, &blocks, viewport, &style)
        .unwrap();
    engine.set_deferred_images(true);
    let capture = timing::TimingScope::start().unwrap();
    let lazy = engine
        .layout_blocks(&source, &blocks, viewport, &style)
        .unwrap();
    let timings = capture.finish();
    assert_eq!(timings.calls(timing::TimingStage::ImageDecode), 0);
    assert_eq!(timings.calls(timing::TimingStage::ImagePixels), 0);
    assert_eq!(timings.calls(timing::TimingStage::ImageMetadata), 3);
    assert_eq!(geometry(&eager), geometry(&lazy));
    for page in &lazy.pages {
        for item in &page.items {
            match item {
                PageItem::Image(i) => {
                    assert!(i.image.deferred.is_some());
                    assert!(i.image.pixels.is_empty());
                }
                PageItem::Text(t) => {
                    for i in t.inline_images.iter() {
                        assert!(i.image.deferred.is_some());
                        assert!(i.image.pixels.is_empty());
                    }
                }
                PageItem::Table(t) => {
                    for cell in &t.cells {
                        if let Some(t) = &cell.text {
                            for i in t.inline_images.iter() {
                                assert!(i.image.deferred.is_some());
                                assert!(i.image.pixels.is_empty());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[test]
fn only_requested_pixels_are_loaded_and_view_retirement_releases_pixels() {
    let source = Source::new("deferred-retirement");
    let first = raster_cache::prepare(
        &source,
        &image("first.png"),
        [120, 120],
        raster_cache::generation(),
    )
    .unwrap();
    let second = raster_cache::prepare(
        &source,
        &image("second.png"),
        [120, 120],
        raster_cache::generation(),
    )
    .unwrap();
    let first = first.deferred.unwrap();
    let second = second.deferred.unwrap();
    assert!(first.ready().is_none());
    assert!(second.ready().is_none());
    first.load(&source).unwrap();
    let loaded = first.ready().unwrap();
    assert_eq!((loaded.width, loaded.height), (120, 80));
    assert!(second.ready().is_none());
    let weak = Arc::downgrade(&loaded.pixels);
    let blob = loaded.blob.as_ref().unwrap().id();
    first.load(&source).unwrap();
    assert_eq!(first.ready().unwrap().blob.unwrap().id(), blob);
    drop(loaded);
    retire_publication_rasters(source.book.id.as_str(), false);
    assert!(weak.upgrade().is_none());
    assert!(first.ready().is_none());
    assert!(second.ready().is_none());
    // Returning to a view can reuse already-prepared geometry after retirement.
    first.load(&source).unwrap();
    assert!(first.ready().is_some());
}

#[test]
fn retirement_during_decode_rejects_the_result_but_allows_a_new_visible_request() {
    let source = Source::new("deferred-retirement-race");
    let raster = raster_cache::prepare(
        &source,
        &image("image.png"),
        [120, 120],
        raster_cache::generation(),
    )
    .unwrap();
    let request = raster.deferred.unwrap();
    let checks = std::cell::Cell::new(0);
    request
        .load_guarded(&source, || {
            checks.set(checks.get() + 1);
            if checks.get() == 3 {
                retire_publication_rasters(source.book.id.as_str(), false);
            }
            true
        })
        .unwrap();
    assert!(request.ready().is_none());
    request.load(&source).unwrap();
    assert!(request.ready().is_some());
}

#[test]
fn a_cancelled_decode_cannot_publish_pixels() {
    let source = Source::new("deferred-cancel");
    let raster = raster_cache::prepare(
        &source,
        &image("image.png"),
        [120, 120],
        raster_cache::generation(),
    )
    .unwrap();
    let request = raster.deferred.unwrap();
    let checks = std::cell::Cell::new(0);
    request
        .load_guarded(&source, || {
            checks.set(checks.get() + 1);
            checks.get() < 3
        })
        .unwrap();
    assert!(request.ready().is_none());
}

#[test]
fn changed_resource_fails_once_instead_of_publishing_obsolete_pixels() {
    let mut source = Source::new("deferred-source-change");
    let raster = raster_cache::prepare(
        &source,
        &image("image.png"),
        [120, 120],
        raster_cache::generation(),
    )
    .unwrap();
    let request = raster.deferred.unwrap();
    source.bytes = Source::new("different").bytes.to_vec().into();
    let mut bytes = source.bytes.to_vec();
    bytes.push(0);
    source.bytes = bytes.into();
    assert!(request.load(&source).is_err());
    assert!(request.ready().is_none());
}
