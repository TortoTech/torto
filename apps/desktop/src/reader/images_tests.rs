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
    assert!(requests[1].ready().is_some());
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
    for request in &requests {
        request.load(source.as_ref()).unwrap();
    }
    let ready_scene = desktop.page_scene();
    assert_eq!(ready_scene.images.len(), 1);
    assert_eq!(
        ready_scene.images[0].data.id(),
        requests[0].ready().unwrap().blob.unwrap().id()
    );
    assert!(ready_scene.refresh_image_atlas);
    assert!(Arc::ptr_eq(
        &retained,
        desktop.page_scenes.values().next().unwrap()
    ));
    desktop.scroll_viewport.as_mut().unwrap().offset_y = 1150.0;
    let next_scene = desktop.page_scene();
    assert_eq!(next_scene.images.len(), 1);
    assert_eq!(
        next_scene.images[0].data.id(),
        requests[1].ready().unwrap().blob.unwrap().id()
    );
    let images = page.paint_image_layer_in(
        &mut VelloScene::new(&mut scene),
        0.0,
        Some(Rect::new(0.0, 150.0, 400.0, 500.0)),
    );
    assert_eq!(images.len(), 1);
    assert_eq!(
        images[0].data.id(),
        requests[0].ready().unwrap().blob.unwrap().id()
    );
    assert_eq!(page.image_bounds(), bounds);
    assert!(!page.image_at(50.0, 150.0).unwrap().pixels.is_empty());
    assert_eq!(page.raster_allocations().count(), 0);
}
