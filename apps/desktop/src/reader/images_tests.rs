use super::super::render::VelloScene;
use super::*;
use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle};
use rebook_publication::{
    Block, Book, ImageBlock, ImageStyle, Metadata, PublicationError, PublicationId, PublicationUrl,
    Resource, Section,
};
use rebook_renderer::DisplayListCompiler;
use std::sync::Mutex;
use std::time::Duration;

struct Gate {
    href: String,
    started: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}
struct Source {
    book: Book,
    bytes: Arc<[u8]>,
    reads: Mutex<Vec<String>>,
    gate: Mutex<Option<Arc<Gate>>>,
}
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        unreachable!()
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.reads.lock().unwrap().push(href.to_string());
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate.filter(|g| g.href == href.to_string()) {
            gate.started.send(()).unwrap();
            gate.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        Ok(Resource {
            href: href.clone(),
            media_type: "image/png".into(),
            bytes: self.bytes.clone(),
        })
    }
}
fn fixture(id: &str, count: usize) -> (Arc<Source>, Vec<DeferredRaster>) {
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        20,
        10,
        image::Rgba([20, 80, 40, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let source = Arc::new(Source {
        book: Book {
            id: PublicationId::new(id).unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: Vec::new(),
            table_of_contents: Vec::new(),
        },
        bytes: png.into_inner().into(),
        reads: Mutex::new(Vec::new()),
        gate: Mutex::new(None),
    });
    let mut engine = LayoutEngine::new();
    engine.set_deferred_images(true);
    let mut requests = Vec::new();
    for index in 0..count {
        let block = Block::Image(ImageBlock {
            href: PublicationUrl::parse(&format!("image-{index}.png")).unwrap(),
            alt: String::new(),
            style: ImageStyle::default(),
            source: None,
            ocr_text: None,
            formula_image: false,
            formula: None,
            text_layer: None,
        });
        let layout = engine
            .layout_blocks(
                source.as_ref(),
                &[block],
                LayoutViewport::new(400, 600).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        let page = DisplayListCompiler.compile(&layout.pages[0]);
        requests.push(page.deferred_rasters().next().unwrap().0.clone());
    }
    source.reads.lock().unwrap().clear();
    (source, requests)
}
fn interest(request: &DeferredRaster, visible: bool, distance: f64) -> Interest {
    Interest {
        request: request.clone(),
        visible,
        distance,
        queued: Instant::now(),
    }
}

// UI cache probes deliberately return None on lock contention. Parallel tests
// must allow a later frame to observe residency instead of treating one busy
// probe as lost pixels. Keep the timeout so real failures still fail the test.
fn when_available<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "image stayed unavailable across later probes"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn visible_work_precedes_prefetch_and_completion_wakes_the_ui() {
    let (source, requests) = fixture("viewport-priority", 3);
    let (started, start) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    *source.gate.lock().unwrap() = Some(Arc::new(Gate {
        href: "image-1.png".into(),
        started,
        release: Mutex::new(wait),
    }));
    let mut loader = ImageLoader::default();
    let context = egui::Context::default();
    for _ in 0..8 {
        let mut output = context.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
    }
    let (wake, woke) = mpsc::channel();
    context.set_request_repaint_callback(move |_| {
        let _ = wake.send(());
    });
    loader.replace(vec![
        interest(&requests[0], false, 0.0),
        interest(&requests[1], true, 20.0),
    ]);
    let erased: Arc<dyn BookSource> = source.clone();
    loader.dispatch(&context, &erased, true, true);
    start.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(*source.reads.lock().unwrap(), ["image-1.png"]);
    assert_eq!(loader.running.len(), 1);
    release.send(()).unwrap();
    woke.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(loader.drain());
    let _ = when_available(|| requests[1].ready());
    assert!(requests[0].ready().is_none());
    assert!(requests[2].ready().is_none());
    *source.gate.lock().unwrap() = None;
    // Once the foreground is ready, exactly one speculative decode may run.
    loader.dispatch(&context, &erased, true, true);
    assert_eq!(loader.running.len(), 1);
    assert!(loader.running.values().all(|r| !r.visible));
}

#[test]
fn fast_navigation_cancels_old_pixels_and_replaces_the_queued_window() {
    let (source, requests) = fixture("viewport-cancel", 3);
    let (started, start) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    *source.gate.lock().unwrap() = Some(Arc::new(Gate {
        href: "image-0.png".into(),
        started,
        release: Mutex::new(wait),
    }));
    let mut loader = ImageLoader::default();
    let context = egui::Context::default();
    let erased: Arc<dyn BookSource> = source.clone();
    loader.replace(vec![
        interest(&requests[0], true, 0.0),
        interest(&requests[1], false, 1.0),
    ]);
    loader.dispatch(&context, &erased, true, true);
    start.recv_timeout(Duration::from_secs(5)).unwrap();
    let flag = loader.running.values().next().unwrap().needed.clone();
    loader.replace(vec![interest(&requests[2], true, 0.0)]);
    assert!(!flag.load(Ordering::Acquire));
    assert_eq!(loader.interests.len(), 1);
    release.send(()).unwrap();
    let completed = loader.results.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(completed.key, requests[0].key());
    assert!(completed.error.is_none());
    assert!(requests[0].ready().is_none());
    assert!(requests[1].ready().is_none());
}

#[test]
fn prefetch_is_bounded_coalesced_and_pauses_without_focus() {
    let (source, requests) = fixture("viewport-budget", 10);
    let mut loader = ImageLoader::default();
    let mut interests = requests
        .iter()
        .enumerate()
        .map(|(n, r)| interest(r, n == 0, n as f64))
        .collect::<Vec<_>>();
    interests.push(interest(&requests[0], true, 0.0));
    loader.replace(interests);
    assert_eq!(loader.interests.len(), 1 + PREFETCH_IMAGES);
    assert_eq!(loader.interests.iter().filter(|i| i.visible).count(), 1);
    assert!(
        loader
            .interests
            .iter()
            .filter(|i| !i.visible)
            .map(|i| i.request.estimated_bytes())
            .sum::<usize>()
            <= PREFETCH_BYTES
    );
    requests[0].load(source.as_ref()).unwrap();
    let context = egui::Context::default();
    let erased: Arc<dyn BookSource> = source.clone();
    loader.dispatch(&context, &erased, true, false);
    assert!(loader.running.is_empty());
    loader.dispatch(&context, &erased, false, true);
    assert!(loader.running.is_empty());
    loader.dispatch(&context, &erased, true, true);
    assert_eq!(loader.running.len(), 1);
}

#[test]
fn painting_uses_only_visible_ready_blobs_and_preserves_geometry_and_hits() {
    let (source, requests) = fixture("viewport-paint", 2);
    let raster = |request: DeferredRaster| rebook_layout::RasterImage {
        deferred: Some(request),
        origin: None,
        blob: None,
        width: 20,
        height: 10,
        pixels: Arc::from([]),
    };
    let placement = |request, y| {
        rebook_layout::PageItem::Image(rebook_layout::ImagePlacement {
            clip: None,
            formula_presentation: None,
            image: raster(request),
            x: 20.0,
            y,
            width: 200.0,
            height: 100.0,
            source: None,
            text_layer: None,
            replacement: None,
        })
    };
    let layout = rebook_layout::PageLayout {
        viewport: LayoutViewport::new(400, 1200).unwrap(),
        background: rebook_publication::Rgba::BLACK,
        leading_gap: 0.0,
        items: vec![
            placement(requests[0].clone(), 100.0),
            placement(requests[1].clone(), 900.0),
        ],
    };
    let page = DisplayListCompiler.compile(&layout);
    let bounds = page.image_bounds();
    let mut scene = vello::Scene::new();
    assert!(
        page.paint_image_layer_in(&mut VelloScene::new(&mut scene), 0.0, None)
            .is_empty()
    );
    assert!(page.image_at(50.0, 150.0).unwrap().pixels.is_empty());
    // Exercise the retained desktop scene too: an initially empty image
    // layer must resolve again after decode, without evicting its text scene.
    let (mut desktop, _, _) = super::super::semantic_layout::tests::fixture();
    let mut scroll = (*desktop.current_scroll_layout().unwrap()).clone();
    scroll.pages = vec![rebook_reader::ReaderSectionPage {
        position: scroll.pages[0].position,
        page: Arc::new(page.clone()),
        placeholder: false,
        visible_top: None,
        visible_bottom: None,
    }];
    scroll.page_tops = vec![0.0];
    scroll.page_origins = vec![0.0];
    scroll.page_heights = vec![1200.0];
    scroll.content_height = 1200.0;
    scroll.quote_bridges.clear();
    desktop.scroll_section = Some(Arc::new(scroll));
    desktop.scroll_viewport = Some(super::super::ScrollViewportState {
        offset_y: 400.0,
        size: egui::vec2(400.0, 500.0),
    });
    let pending_scene = desktop.page_scene();
    assert!(pending_scene.images.is_empty());
    let retained = desktop.page_scenes.values().next().unwrap().clone();
    let ready = requests
        .iter()
        .map(|request| {
            request.load(source.as_ref()).unwrap();
            when_available(|| request.ready())
        })
        .collect::<Vec<_>>();
    let ready_scene = when_available(|| {
        let scene = desktop.page_scene();
        (scene.images.len() == 1).then_some(scene)
    });
    assert_eq!(ready_scene.images.len(), 1);
    assert_eq!(
        ready_scene.images[0].data.id(),
        ready[0].blob.as_ref().unwrap().id()
    );
    assert!(ready_scene.refresh_image_atlas);
    assert!(Arc::ptr_eq(
        &retained,
        desktop.page_scenes.values().next().unwrap()
    ));
    desktop.scroll_viewport.as_mut().unwrap().offset_y = 1150.0;
    let next_scene = when_available(|| {
        let scene = desktop.page_scene();
        (scene.images.len() == 1).then_some(scene)
    });
    assert_eq!(next_scene.images.len(), 1);
    assert_eq!(
        next_scene.images[0].data.id(),
        ready[1].blob.as_ref().unwrap().id()
    );
    let images = when_available(|| {
        let images = page.paint_image_layer_in(
            &mut VelloScene::new(&mut scene),
            0.0,
            Some(Rect::new(0.0, 150.0, 400.0, 500.0)),
        );
        (images.len() == 1).then_some(images)
    });
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].data.id(), ready[0].blob.as_ref().unwrap().id());
    assert_eq!(page.image_bounds(), bounds);
    let _ = when_available(|| {
        page.image_at(50.0, 150.0)
            .filter(|hit| !hit.pixels.is_empty())
    });
    assert_eq!(page.raster_allocations().count(), 0);
}

#[test]
fn deferred_formulas_paint_after_loading_with_stable_blob_identity() {
    let (source, _) = fixture("viewport-generated-formulas", 0);
    let mut formula = ImageBlock {
        href: PublicationUrl::parse("formula.png").unwrap(),
        alt: String::new(),
        style: ImageStyle::default(),
        source: None,
        ocr_text: None,
        formula_image: true,
        formula: Some(rebook_publication::ImageFormula {
            latex: r"\frac{x+1}{2}".into(),
            equation_number: Some("2.4".into()),
        }),
        text_layer: None,
    };
    for numbered in [false, true] {
        formula.formula.as_mut().unwrap().equation_number = numbered.then(|| "2.4".into());
        let mut engine = LayoutEngine::new();
        engine.set_deferred_images(true);
        let style = ReaderStyle {
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            ..Default::default()
        };
        let layout = engine
            .layout_blocks(
                source.as_ref(),
                &[Block::Image(formula.clone())],
                LayoutViewport::new(400, 600).unwrap(),
                &style,
            )
            .unwrap();
        let page = DisplayListCompiler.compile(&layout.pages[0]);
        let request = page.deferred_rasters().next().unwrap().0;
        let bounds = page.image_bounds().unwrap();
        assert!(
            page.paint_image_layer_in(&mut VelloScene::new(&mut vello::Scene::new()), 0.0, None)
                .is_empty()
        );
        request.load(source.as_ref()).unwrap();
        let mut scene = vello::Scene::new();
        let images = page.paint_image_layer_in(&mut VelloScene::new(&mut scene), 0.0, Some(bounds));
        assert_eq!(images.len(), 1, "loaded formula must enter the paint scene");
        assert!(images[0].data.as_ref().chunks_exact(4).any(|p| p[3] != 0));
        let repaint =
            page.paint_image_layer_in(&mut VelloScene::new(&mut scene), 0.0, Some(bounds));
        assert_eq!(images[0].data.id(), repaint[0].data.id());
        assert_eq!(page.image_bounds(), Some(bounds));
    }
}

#[test]
#[ignore = "requires TORTO_IMAGE_BOOK; offline local book image paint regression"]
fn local_book_deferred_images_enter_the_paint_scene() {
    let path = std::path::PathBuf::from(std::env::var_os("TORTO_IMAGE_BOOK").unwrap());
    let store = crate::sync::SyncStore::open_at(
        std::env::temp_dir().join(format!(
            "torto-image-paint-{}.sqlite3",
            uuid::Uuid::new_v4()
        )),
        "image-paint-regression",
    )
    .unwrap();
    let mut desktop = super::super::open_reader_in_viewport(
        &path,
        crate::fonts::embedded_reader_fonts(),
        None,
        None,
        store,
        LayoutViewport::new(1216, 789).unwrap(),
    )
    .unwrap();
    desktop.progress_store = None;
    let label = std::env::var("TORTO_IMAGE_TOC").unwrap();
    let target = desktop
        .reader
        .toc_items()
        .iter()
        .find(|item| item.label.contains(&label))
        .unwrap()
        .target
        .clone()
        .unwrap();
    desktop.reader.go_to_href(&target).unwrap();
    let pages = desktop.reader.current_reading_unit_pages().unwrap();
    let output = std::env::var_os("TORTO_IMAGE_OUTPUT").map(std::path::PathBuf::from);
    if let Some(output) = &output {
        std::fs::create_dir_all(output).unwrap();
    }
    let mut count = 0;
    for entry in pages {
        let requests = entry.page.deferred_rasters().collect::<Vec<_>>();
        if requests.is_empty() {
            continue;
        }
        for (request, _) in &requests {
            request.load(desktop.source.as_ref()).unwrap();
        }
        let images = entry.page.paint_image_layer_in(
            &mut VelloScene::new(&mut vello::Scene::new()),
            0.0,
            None,
        );
        assert_eq!(images.len(), requests.len());
        for image in images {
            assert_eq!(
                image.data.len(),
                image.width as usize * image.height as usize * 4
            );
            assert!(image.data.as_ref().chunks_exact(4).any(|p| p[3] != 0));
            if let Some(output) = &output {
                image::save_buffer(
                    output.join(format!("paint-image-{count}.png")),
                    image.data.as_ref(),
                    image.width,
                    image.height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            count += 1;
        }
    }
    assert!(count > 0, "target subsection must contain deferred images");
    println!("LOCAL_IMAGE_PAINT toc={label} images={count}");
}
