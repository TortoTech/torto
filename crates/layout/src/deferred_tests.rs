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
        ocr_text: None,
        formula_image: false,
        formula: None,
        text_layer: None,
    }
}

#[test]
fn source_formula_images_defer_pixels_in_both_presentation_modes() {
    let source = Source::new("deferred-source-formula-pixels");
    let viewport = LayoutViewport::new(500, 700).unwrap();
    for unified in [false, true] {
        let mut style = ReaderStyle::default();
        if unified {
            style.typesetting = ReaderTypesetting::unified();
        }
        let mut block = image("formula.png");
        block.formula_image = true;
        block.formula = Some(rebook_publication::ImageFormula {
            latex: r"\frac{x+1}{2}".into(),
            equation_number: Some("1.2".into()),
        });
        let mut engine = LayoutEngine::new();
        engine.set_deferred_images(true);
        let scope = timing::TimingScope::start().unwrap();
        let layout = engine
            .layout_blocks(&source, &[Block::Image(block)], viewport, &style)
            .unwrap();
        let timings = scope.finish();
        assert_eq!(timings.calls(timing::TimingStage::ImageDecode), 0);
        assert_eq!(timings.calls(timing::TimingStage::FormulaRaster), 0);
        let placed = layout
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .find_map(|item| {
                if let PageItem::Image(image) = item {
                    Some(image)
                } else {
                    None
                }
            })
            .unwrap();
        let presentation = placed.formula_presentation.as_ref().unwrap();
        assert!(presentation.original.pixels.is_empty());
        assert_eq!(
            [presentation.original.width, presentation.original.height],
            [240, 160]
        );
        assert!(placed.image.pixels.is_empty());
        let request = placed.image.deferred.as_ref().unwrap();
        request.load(&source).unwrap();
        let ready = request.ready_for_test().unwrap();
        assert!(!ready.pixels.is_empty());
        let blob = ready
            .blob
            .as_ref()
            .expect("loaded formulas need a paint blob");
        assert_eq!(blob.as_ref(), ready.pixels.as_ref());
        assert_eq!(
            blob.id(),
            request.ready_for_test().unwrap().blob.unwrap().id(),
            "repainting must reuse the cached image identity"
        );
        retire_publication_rasters(source.book.id.as_str(), false);
    }
}

#[test]
fn formulas_reserve_exact_geometry_and_rasterize_only_when_requested() {
    let source = Source::new("deferred-formula-pixels");
    let typography = ReaderTypography::default();
    let options = resvg::usvg::Options::default();
    for (display, original) in [
        (false, None),
        (true, None),
        (
            true,
            Some(vec![TextRun {
                text: "original formula".into(),
                style: Default::default(),
                link: None,
            }]),
        ),
    ] {
        let block = text(
            TextBlockKind::Paragraph,
            vec![
                Inline::Math(MathRun {
                    latex: r"\frac{x_1+1}{2}".into(),
                    display,
                    original: original.clone(),
                    size_scale: 1.0,
                }),
                Inline::Math(MathRun {
                    latex: r"y^2+z".into(),
                    display,
                    original,
                    size_scale: 1.0,
                }),
            ],
        );
        let (_, _, eager, _, _, _, _) = prepare_inline_content(
            &block,
            Rgba::BLACK,
            &typography,
            320.0,
            &options,
            false,
            true,
            false,
            &[],
            &mut [],
            None,
        );
        let capture = timing::TimingScope::start().unwrap();
        let (_, _, lazy, _, _, _, _) = prepare_inline_content(
            &block,
            Rgba::BLACK,
            &typography,
            320.0,
            &options,
            false,
            true,
            false,
            &[],
            &mut [],
            Some((&source, raster_cache::generation())),
        );
        assert_eq!(
            capture.finish().calls(timing::TimingStage::FormulaRaster),
            0
        );
        assert_eq!(eager.len(), 2);
        for (eager, lazy) in eager.iter().zip(&lazy) {
            assert_eq!(
                (eager.width, eager.height, eager.box_height, eager.offset_y),
                (lazy.width, lazy.height, lazy.box_height, lazy.offset_y)
            );
            assert!(lazy.image.pixels.is_empty());
            let pixels = lazy
                .image
                .deferred
                .as_ref()
                .unwrap()
                .render_generated()
                .unwrap();
            assert_eq!(
                (pixels.width, pixels.height),
                (eager.image.width, eager.image.height)
            );
            assert_eq!(pixels.pixels, eager.image.pixels);
        }
        let first = lazy[0].image.deferred.as_ref().unwrap();
        let offscreen = lazy[1].image.deferred.as_ref().unwrap();
        first.load(&source).unwrap();
        let ready = first.ready_for_test().unwrap();
        assert_eq!(ready.blob.as_ref().unwrap().as_ref(), ready.pixels.as_ref());
        assert!(offscreen.ready_for_test().is_none());
        retire_publication_rasters(source.book.id.as_str(), false);
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
            body: Vec::new(),
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
        Block::Table(TableBlock { visualization: None,
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

struct MetadataSource {
    inner: Source,
    dimensions: [u32; 2],
    reads: std::sync::atomic::AtomicUsize,
}
impl BookSource for MetadataSource {
    fn book(&self) -> &Book {
        self.inner.book()
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        unreachable!()
    }
    fn image_resource_dimensions(
        &self,
        _: &PublicationUrl,
    ) -> Result<Option<[u32; 2]>, PublicationError> {
        Ok(Some(self.dimensions))
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.resource(href)
    }
}

#[test]
fn cached_encoded_dimensions_reserve_geometry_without_reading_pixels_and_keep_resizing() {
    let source = MetadataSource {
        inner: Source::new("cached-encoded-image-size"),
        dimensions: [240, 160],
        reads: Default::default(),
    };
    let mut engine = LayoutEngine::new();
    engine.set_deferred_images(true);
    engine.raster_target = [60, 40];
    let block = image("picture.png");
    let prepared = engine.load_raster(&source, &block).unwrap();
    assert_eq!([prepared.width, prepared.height], [240, 160]);
    assert!(prepared.pixels.is_empty());
    assert_eq!(source.reads.load(std::sync::atomic::Ordering::SeqCst), 0);
    let request = prepared.deferred.as_ref().unwrap();
    request.load(&source).unwrap();
    let ready = request.ready_for_test().unwrap();
    assert_eq!([ready.width, ready.height], [60, 40]);
    assert_eq!(source.reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        ready.origin.as_ref().map(|o| [o.width, o.height]),
        Some([240, 160])
    );
    // Eager consumers (e.g. popups) reuse the same pixel variant and upload ID.
    engine.set_deferred_images(false);
    let eager = engine.load_raster(&source, &block).unwrap();
    assert_eq!(
        eager.blob.as_ref().unwrap().id(),
        ready.blob.as_ref().unwrap().id()
    );
    retire_publication_rasters(source.book().id.as_str(), false);
}

#[test]
fn encoded_dimensions_mismatch_is_rejected_before_publishing_pixels() {
    let source = MetadataSource {
        inner: Source::new("mismatched-encoded-image-size"),
        dimensions: [999, 999],
        reads: Default::default(),
    };
    let mut engine = LayoutEngine::new();
    engine.set_deferred_images(true);
    let prepared = engine.load_raster(&source, &image("wrong.png")).unwrap();
    let request = prepared.deferred.as_ref().unwrap();
    assert!(request.load(&source).is_err());
    assert!(request.ready_for_test().is_none());
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
    assert!(first.ready_for_test().is_none());
    assert!(second.ready_for_test().is_none());
    first.load(&source).unwrap();
    let loaded = first.ready_for_test().unwrap();
    assert_eq!((loaded.width, loaded.height), (120, 80));
    assert!(second.ready_for_test().is_none());
    let weak = Arc::downgrade(&loaded.pixels);
    let blob = loaded.blob.as_ref().unwrap().id();
    first.load(&source).unwrap();
    assert_eq!(first.ready_for_test().unwrap().blob.unwrap().id(), blob);
    drop(loaded);
    retire_publication_rasters(source.book.id.as_str(), false);
    assert!(weak.upgrade().is_none());
    assert!(first.ready_for_test().is_none());
    assert!(second.ready_for_test().is_none());
    // Returning to a view can reuse already-prepared geometry after retirement.
    first.load(&source).unwrap();
    assert!(first.ready_for_test().is_some());
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
    assert!(request.ready_for_test().is_none());
    request.load(&source).unwrap();
    assert!(request.ready_for_test().is_some());
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
    assert!(request.ready_for_test().is_none());
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
    assert!(request.ready_for_test().is_none());
}
