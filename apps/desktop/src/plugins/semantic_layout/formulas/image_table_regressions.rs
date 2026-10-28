use super::*;
use rebook_publication::{Book, PublicationError, Resource, SpineItem, SpineItemId};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const TABLE: &str = "<table><tr><th rowspan='5'>投机</th><td>大机构——小机构</td></tr><tr><td>老股民——新股民</td></tr><tr><td>庄家——散户</td></tr><tr><td>高技巧者——低技巧者</td></tr><tr><td>心态好者——心态坏者/有耐心者——没有耐心者</td></tr><tr><th>投资</th><td>上市公司创造财富，共同富裕</td></tr></table>";

struct PictureSource {
    inner: Arc<dyn BookSource>,
    bytes: Arc<[u8]>,
}
impl BookSource for PictureSource {
    fn book(&self) -> &Book {
        self.inner.book()
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        self.inner.parse_section(index)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Ok(Resource {
            href: href.clone(),
            media_type: "image/png".into(),
            bytes: self.bytes.clone(),
        })
    }
}
fn synthetic(with_inline: bool) -> (Arc<dyn BookSource>, Section) {
    let descriptor = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    let inline = if with_inline {
        "<p>Symbol <img src='bg.png' style='height:1em'/> example.</p>"
    } else {
        ""
    };
    let section=rebook_html::parse_section(&format!("<html><body>{inline}<img src='bg.png' width='800' height='600' style='height:auto;max-width:100%;display:block'/></body></html>"),&descriptor,|_|None).unwrap();
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::from_pixel(1050, 347, Rgba([255, 255, 255, 255])))
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let source = PictureSource {
        inner: crate::plugins::semantic_layout::tests::original_source(section.clone()),
        bytes: png.into_inner().into(),
    };
    (Arc::new(source), section)
}

/// Exercise the real recognizer entry, not only table parsing or record(). A
/// preexisting current-generation negative chapter result must be rechecked.
fn check(source: Arc<dyn BookSource>, original: Section, expected_images: usize) -> Section {
    use std::io::{BufRead, Read, Write};
    use std::time::{Duration, Instant};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let active = running.clone();
    let server = std::thread::spawn(move || {
        let started = Instant::now();
        let mut images = 0;
        while active.load(Ordering::Relaxed) {
            assert!(started.elapsed() < Duration::from_secs(15));
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(n) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = n.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; len];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            let content = request["messages"][1]["content"].as_array().unwrap();
            let schema = &request["response_format"]["json_schema"]["schema"];
            let properties = &schema["properties"]["r"]["items"]["properties"];
            assert!(properties.get("th").is_some());
            assert!(properties.get("rs").is_some());
            assert!(properties.get("lt").is_some());
            assert!(properties.get("table_html").is_none());
            assert!(properties.get("replacement_safe").is_none());
            let mut results = Vec::new();
            for pair in content[1..].chunks_exact(2) {
                let metadata: Value =
                    serde_json::from_str(pair[0]["text"].as_str().unwrap()).unwrap();
                let data = pair[1]["image_url"]["url"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("data:image/png;base64,")
                    .unwrap();
                let pixels = image::load_from_memory(
                    &base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap(),
                )
                .unwrap();
                assert!(pixels.height() > 240);
                assert!(pixels.width() <= 1616 && pixels.height() <= 1616);
                let inline = metadata["md"]["in"].as_bool().unwrap();
                results.push(json!({"ii":metadata["ii"],"ss":if inline {"no"} else {"tb"},"rs":!inline,"th":if inline {Value::Null} else {json!(TABLE)},"t":null,"lt":false,"l":null,"n":null}));
                images += 1;
            }
            let result = json!({"r":results});
            jsonschema::validate(schema, &result).unwrap();
            let body=json!({"choices":[{"index":0,"finish_reason":"stop","message":{"content":result.to_string()}}]}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        }
        images
    });
    let mut settings = PluginSettings::default().with_test_model();
    settings.providers[0].kind = crate::plugins::AiProviderKind::OpenAi;
    settings.providers[0].base_url = format!("http://{address}/v1");
    settings.providers[0].api_key = "fixture".into();
    settings.semantic_layout.enabled = true;
    settings.semantic_layout.provider = settings.providers[0].id.clone();
    settings.semantic_layout.model = settings.providers[0].models[0].id.clone();
    let book = format!("issue56-{}", uuid::Uuid::new_v4());
    let hash = fingerprint(&original);
    let identity = recognition_identity(&book, &settings).unwrap();
    let path = recognition_path(&identity, &hash).unwrap();
    let old = Recognition {
        fingerprint: hash.clone(),
        formulas_checked: true,
        image_text_revision: 6,
        annotations: vec![],
        skipped_groups: 0,
    };
    crate::persistence::write_json_atomic(&path, &old).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(recognize_with_source(
        &original,
        &book,
        &settings,
        source.as_ref(),
    ));
    running.store(false, Ordering::Relaxed);
    let submitted = server.join().unwrap();
    assert_eq!(
        submitted, expected_images,
        "large images must reach the model, with independent inline/standalone decisions"
    );
    let result = result.unwrap();
    assert_eq!(result.image_text_revision, TEXT_REVISION);
    assert!(
        result
            .annotations
            .iter()
            .any(|a| matches!(a, Annotation::ImageTable { .. }))
    );
    // Server is gone: reopening must restore the upgraded table without retrying.
    let cached = rt
        .block_on(recognize_with_source(
            &original,
            &book,
            &settings,
            source.as_ref(),
        ))
        .unwrap();
    assert_eq!(cached.annotations, result.annotations);
    let mut converted = original.clone();
    for a in &result.annotations {
        super::super::compose(&mut converted.blocks, a);
    }
    let table = converted
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Table(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(table.rows.len(), 6);
    assert_eq!(table.rows[0].cells[0].row_span, 5);
    let source_range = original
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Image(i) => i.source.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(table.source.as_ref(), Some(source_range));
    assert!(table.rows.iter().flat_map(|r| &r.cells).all(|c| {
        c.text
            .source
            .as_ref()
            .unwrap()
            .start
            .node
            .starts_with(&format!("{}-table-", source_range.start.node))
    }));
    // Each synthetic test owns this cache entry, never remove a user's cache.
    let _ = std::fs::remove_file(path);
    converted
}

#[test]
fn tall_standalone_table_rechecks_old_negative_cache_and_restores_merged_cells() {
    let (source, section) = synthetic(false);
    check(source, section, 1);
}

#[test]
fn inline_negative_result_cannot_poison_the_same_standalone_picture() {
    let (source, section) = synthetic(true);
    let converted = check(source, section, 2);
    assert!(
        converted
            .blocks
            .iter()
            .flat_map(Block::text_blocks)
            .flat_map(|t| &t.content)
            .any(|i| matches!(i,Inline::Image(r) if r.image.text_layer.is_none()))
    );
}

fn ratio(source: &dyn BookSource, section: &Section) {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    let image = section
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Image(i) => Some(i),
            _ => None,
        })
        .unwrap();
    assert_eq!(image.style.height, None);
    for unified in [false, true] {
        for width in [360, 1200] {
            let mut style = ReaderStyle {
                spread: SpreadMode::Single,
                ..Default::default()
            };
            if unified {
                style.typesetting = ReaderTypesetting::unified();
            }
            let layout = LayoutEngine::new()
                .layout_blocks(
                    source,
                    &[Block::Image(image.clone())],
                    LayoutViewport::new(width, 900).unwrap(),
                    &style,
                )
                .unwrap();
            let placed = layout
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .find_map(|i| match i {
                    PageItem::Image(i) => Some(i),
                    _ => None,
                })
                .unwrap();
            assert!((placed.width / placed.height - 1050.0 / 347.0).abs() < 0.001);
            if width == 1200 {
                assert!((placed.width - 800.0).abs() < 0.01);
                assert!((placed.height - 264.38095).abs() < 0.01);
            }
        }
    }
}

#[test]
fn html_auto_preserves_intrinsic_ratio_in_book_and_unified_layout() {
    let (source, section) = synthetic(false);
    ratio(source.as_ref(), &section);
}

#[test]
#[ignore = "requires TORTO_ISSUE56_BOOK local EPUB; mock model, optional TORTO_ISSUE56_RENDER_DIR"]
fn local_issue56_bg_preserves_aspect_and_becomes_one_six_row_table() {
    use anyrender::ImageRenderer;
    use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle, ReaderTypesetting, SpreadMode};
    let opened = rebook_formats::open_file(std::env::var("TORTO_ISSUE56_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("Text/part1.xhtml"))
        .unwrap();
    let mut section = source.parse_section(index).unwrap();
    section.blocks.retain(
        |b| matches!(b,Block::Image(i) if i.source.as_ref().is_some_and(|s|s.start.node=="n56")),
    );
    assert_eq!(section.blocks.len(), 1);
    ratio(source.as_ref(), &section);
    let converted = check(source.clone(), section, 1);
    if let Some(dir) = std::env::var_os("TORTO_ISSUE56_RENDER_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        let mut engine =
            LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned());
        let mut style = ReaderStyle {
            spread: SpreadMode::Single,
            typesetting: ReaderTypesetting::unified(),
            ..Default::default()
        };
        engine
            .available_reader_font_families()
            .repair_typography(&mut style.typography);
        let layout = engine
            .layout_blocks(
                source.as_ref(),
                &converted.blocks,
                LayoutViewport::new(1200, 900).unwrap(),
                &style,
            )
            .unwrap();
        let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
        let mut pixels = Vec::new();
        let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(1200, 900);
        renderer.render_to_vec(|scene| display.paint(scene), &mut pixels);
        image::RgbaImage::from_raw(1200, 900, pixels)
            .unwrap()
            .save(std::path::Path::new(&dir).join("converted-table.png"))
            .unwrap();
    }
    println!(
        "n56: 1050x347 -> 800x264.38 in both layouts; submitted to mock vision model, six rows and rowspan=5 preserved, old cache upgraded"
    );
}
