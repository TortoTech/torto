use super::*;
use rebook_app_core::plugins;

fn root() -> PathBuf {
    std::env::temp_dir().join(format!("torto-android-test-{}", uuid::Uuid::new_v4()))
}
fn fixture(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let paragraphs = (0..60).map(|i| format!("<p>Paragraph {i}: readers share stable source locations across devices. 中文搜索与选择。</p>")).collect::<String>();
    std::fs::write(path,format!(r#"<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Android reader fixture</book-title><lang>en</lang></title-info></description><body><section id="one"><title><p>One</p></title>{paragraphs}</section></body></FictionBook>"#)).unwrap();
}
#[test]
fn imported_book_search_navigation_render_and_close_use_shared_core() {
    runtime().block_on(async {
        let root = root();
        let path = root.join("sample.fb2");
        fixture(&path);
        let core = MobileCore::new(root.join("data").to_string_lossy().into_owned()).unwrap();
        let books = core
            .clone()
            .import_book(path.to_string_lossy().into_owned())
            .await
            .unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(
            core.clone()
                .import_book(path.to_string_lossy().into_owned())
                .await
                .unwrap()
                .len(),
            1
        );
        let viewport = Viewport {
            width: 360,
            height: 640,
            scale: 1.0,
        };
        let reader = core
            .clone()
            .open_book(books[0].id.clone(), viewport)
            .await
            .unwrap();
        let first = reader.clone().page().await.unwrap();
        assert_eq!(first.pixels.len(), 360 * 640 * 4);
        assert!(
            first
                .info()
                .regions
                .iter()
                .any(|r| r.text.contains("Paragraph"))
        );
        let found = reader.clone().search("Paragraph 45".into()).await.unwrap();
        assert_eq!(found.len(), 1);
        let jumped = reader
            .clone()
            .go_to_source(found[0].source.clone())
            .await
            .unwrap();
        assert!(
            jumped
                .info()
                .regions
                .iter()
                .any(|r| r.text.contains("Paragraph 45"))
        );
        let region = jumped
            .info()
            .regions
            .into_iter()
            .find(|r| r.text.contains("Paragraph 45"))
            .unwrap();
        let rect = &region.bounds[0];
        let selected = reader
            .clone()
            .select(
                rect.x + 2.0,
                rect.y + rect.height / 2.0,
                rect.x + 45.0,
                rect.y + rect.height / 2.0,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!selected.text.is_empty());
        let notes = reader
            .clone()
            .save_annotation(Some("迁移测试批注".into()))
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        let annotation = notes[0].clone();
        assert_eq!(annotation.quote, selected.text);
        assert!(!reader.clone().page().await.unwrap().info().marks.is_empty());
        let notes = reader
            .clone()
            .edit_annotation(annotation.id.clone(), Some("edited".into()))
            .await
            .unwrap();
        assert_eq!(notes[0].note.as_deref(), Some("edited"));
        reader
            .clone()
            .resize(Viewport {
                width: 600,
                height: 360,
                scale: 1.0,
            })
            .await
            .unwrap();
        let marked = reader
            .clone()
            .go_to_source(annotation.source.clone())
            .await
            .unwrap();
        assert!(!marked.info().marks.is_empty());
        reader.clone().clear_selection().await.unwrap();
        assert!(reader.clone().save_annotation(None).await.is_err());

        // Cancel is independent of the session mutex and needs no AI credentials.
        struct Listener;
        impl chat::ChatListener for Listener {
            fn update(&self, _: String, _: String, _: String) {
                panic!("cancelled request emitted content");
            }
        }
        let request = chat::ChatRequest::new();
        request.cancel();
        let result = reader
            .clone()
            .chat("summarize".into(), request, Arc::new(Listener))
            .await;
        assert!(result.err().unwrap().to_string().contains("已停止"));
        assert!(reader.clone().chat_history().await.unwrap().is_empty());
        let cancelled=chat::ChatRequest::new();cancelled.cancel();
        assert!(reader.clone().translate_page_cancellable(true,cancelled.clone()).await.err().unwrap().to_string().contains("已停止"));
        assert!(reader.clone().recognize_page_cancellable(cancelled).await.err().unwrap().to_string().contains("已停止"));

        // Exercise actual streamed transport through the shared chat service, using
        // an isolated loopback endpoint (never a paid provider or personal key).
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut socket = loop {
                if let Ok((socket, _)) = listener.accept() { break socket; }
                assert!(std::time::Instant::now() < deadline, "mock chat request timeout");
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096]; let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0); request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers.lines().find_map(|h| h.to_lowercase().strip_prefix("content-length:").and_then(|n| n.trim().parse::<usize>().ok())).unwrap_or(0);
                    if request.len() >= end + 4 + length { break; }
                }
            }
            let headers_end = request.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
            let payload: serde_json::Value = serde_json::from_slice(&request[headers_end..]).unwrap();
            assert!(payload.to_string().contains("data:image/png;base64,"));
            let body = concat!("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"共享阅读助手\"}}]}\n\n", "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n", "data: [DONE]\n\n");
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
            }
        });
        let mut settings = rebook_app_core::plugins::PluginSettings::default();
        settings.providers = vec![rebook_app_core::plugins::AiProvider {
            id: "mock".into(), kind: rebook_app_core::plugins::AiProviderKind::Custom,
            base_url: format!("http://{address}/v1"), api_key: "test-only".into(),
            models: vec![rebook_app_core::plugins::AiModelConfig::language("mock")], ..Default::default()
        }];
        settings.chat_provider = "mock".into(); settings.chat_model = "mock".into();
        settings.save_default().unwrap();
        #[derive(Default)] struct Stream(Mutex<Vec<String>>);
        impl chat::ChatListener for Stream { fn update(&self, content: String, _: String, _: String) { self.0.lock().unwrap().push(content); } }
        let stream = Arc::new(Stream::default());
        let image_path = root.join("chat-fixture.png");
        image::RgbaImage::from_pixel(80, 60, image::Rgba([30, 120, 70, 255])).save(&image_path).unwrap();
        let attachment = reader.clone().add_chat_image(image_path.to_string_lossy().into_owned(), "fixture".into()).await.unwrap();
        assert!(reader.clone().chat_image("../../secret".into()).await.is_err());
        let preview = reader.clone().chat_image(attachment.id.clone()).await.unwrap();
        assert_eq!((preview.width(), preview.height()), (80,60));
        let citation_range: SourceRange = serde_json::from_str(&annotation.source).unwrap();
        let link = rebook_app_core::plugins::chat_citation_link(0, Some(&citation_range.start.node));
        assert!(reader.clone().follow_chat_citation("link://j/999999/unknown".into()).await.is_err());
        assert!(reader.clone().follow_chat_citation(link).await.unwrap().info().can_go_back);
        reader.clone().go_back().await.unwrap();
        let reply = reader.clone().chat_with_images("summarize".into(), vec![attachment.clone()], chat::ChatRequest::new(), stream.clone()).await.unwrap();
        let followup = reader.clone().chat("describe the attached image again".into(), chat::ChatRequest::new(), stream.clone()).await.unwrap();
        server.join().unwrap();
        assert_eq!(followup.messages.len(), 4);
        assert_eq!(reply.messages[0].images[0].id, attachment.id);
        assert_eq!(reply.messages.len(), 2);
        assert_eq!(reply.messages[1].content, "共享阅读助手");
        assert!(stream.0.lock().unwrap().iter().any(|s| s.contains("共享阅读助手")));
        reader.with(|s| { assert_eq!(chat::load_messages(&s.chat_path)[0].images[0].id, attachment.id); Ok(()) }).unwrap();
        reader.clone().clear_chat().await.unwrap();
        assert!(reader.clone().chat_history().await.unwrap().is_empty());
        assert!(reader.clone().chat_image(attachment.id).await.is_err());

        // Reviewing/discarding tools must not mutate saved notes or temporary rewrites.
        let response = || rebook_app_core::plugins::ChatResponse {
            content: "proposed deletion".into(),
            rewrites: vec![],
            rewrite_transactions: vec![],
            annotation_actions: vec![rebook_app_core::plugins::ChatAnnotationAction::Delete {
                annotation_id: annotation.id.clone(),
            }],
        };
        reader
            .with(|s| {
                s.pending = Some(chat::PendingChanges {
                    response: response(),
                    rewrites: vec![],
                    replace_rewrites: false,
                });
                Ok(())
            })
            .unwrap();
        reader.clone().resolve_chat_changes(false).await.unwrap();
        assert_eq!(reader.clone().annotations().await.unwrap().len(), 1);
        reader
            .with(|s| {
                s.pending = Some(chat::PendingChanges {
                    response: response(),
                    rewrites: vec![],
                    replace_rewrites: false,
                });
                Ok(())
            })
            .unwrap();
        reader.clone().resolve_chat_changes(true).await.unwrap();
        assert!(reader.clone().annotations().await.unwrap().is_empty());
        assert!(reader.clone().page().await.unwrap().info().marks.is_empty());
        // An AI-requested clear has an empty final rewrite set but still needs applying.
        reader
            .with(|s| {
                s.pipeline
                    .rewrite
                    .apply_rewrites(&[rebook_app_core::plugins::BlockRewrite {
                        section_index: 0,
                        block_id: "test-block".into(),
                        text: "temporary".into(),
                    }])
                    .map_err(error)?;
                let mut proposal = response();
                proposal.annotation_actions.clear();
                s.pending = Some(chat::PendingChanges {
                    response: proposal,
                    rewrites: vec![],
                    replace_rewrites: true,
                });
                Ok(())
            })
            .unwrap();
        reader.clone().resolve_chat_changes(true).await.unwrap();
        reader
            .with(|s| {
                assert!(s.pipeline.rewrite.list_rewrites(None).unwrap().is_empty());
                Ok(())
            })
            .unwrap();
        reader.clone().reading_activity(false, false).await.unwrap();
        reader.clone().mark_finished().await.unwrap();
        let stats = core.clone().statistics().await.unwrap();
        assert_eq!(stats.distribution.len(), 7);
        assert!(
            stats
                .books
                .iter()
                .any(|b| b.id == books[0].id && b.finished)
        );
        assert_eq!(stats.total, 0, "inactive reader must not accrue time");
        assert!(core.clone().books().await.unwrap()[0].last_read > 0);
        let id = jumped.id();
        drop(jumped);
        assert!(!pixels::registry().lock().unwrap().contains_key(&id));
        reader.clone().shutdown().await.unwrap();
        assert!(matches!(
            reader.clone().page().await,
            Err(CoreError::Closed)
        ));
        drop(reader);
        drop(first);
        cached_translation_roundtrip(&core,&books[0].id).await;
        touch_and_focus(&core, &root).await;
        advanced_reading(&core, &books[0].id).await;
        long_focus_continuation(&core,&root).await;
        drop(core);
        std::fs::remove_dir_all(root).unwrap();
    });
}
#[test]
fn rejects_unbounded_and_nonfinite_viewports() {
    for viewport in [
        Viewport {
            width: 0,
            height: 640,
            scale: 1.0,
        },
        Viewport {
            width: 4096,
            height: 4096,
            scale: 4.0,
        },
        Viewport {
            width: 360,
            height: 640,
            scale: f32::NAN,
        },
    ] {
        assert!(viewport.validate().is_err());
    }
}

async fn touch_and_focus(core: &Arc<MobileCore>, root: &std::path::Path) {
    use std::io::Write;
    let path = root.join("interactions.epub");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(80, 60, image::Rgba([30, 120, 70, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    for (name,bytes) in [
        ("mimetype", b"application/epub+zip".as_slice()),
        ("META-INF/container.xml", br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.as_slice()),
        ("package.opf", br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">torto-interactions</dc:identifier><dc:title>Interaction fixture</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="image" href="image.png" media-type="image/png"/></manifest><spine><itemref idref="body"/></spine></package>"#.as_slice()),
        ("body.xhtml", br##"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Touch</title></head><body><p><a href="#destination">Jump internally</a> with a note<a epub:type="noteref" href="#note">1</a>.</p><p>Independent paragraph for focus navigation.</p><p><img src="image.png" alt="Preview fixture"/></p><p id="destination">Destination text.</p><aside epub:type="footnote" id="note"><p>1. <strong>A multi-paragraph note.</strong> <span class="math">\frac{1}{2}</span></p><p>Note continuation.</p></aside></body></html>"##.as_slice()),
        ("image.png", png.get_ref().as_slice()),
    ] {zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(bytes).unwrap();}
    zip.finish().unwrap();
    let books = core
        .clone()
        .import_book(path.to_string_lossy().into())
        .await
        .unwrap();
    let book = books
        .iter()
        .find(|b| b.title == "Interaction fixture")
        .unwrap();
    let reader = core
        .clone()
        .open_book(
            book.id.clone(),
            Viewport {
                width: 360,
                height: 640,
                scale: 1.0,
            },
        )
        .await
        .unwrap();
    let page = reader.clone().page().await.unwrap();
    assert!(page.info().focus_bounds.is_empty());
    let (x, y) = reader
        .with(|s| {
            let bounds = s.session.current_page().image_bounds().unwrap();
            Ok((
                (bounds.x0 + bounds.x1) as f32 / 2.0,
                (bounds.y0 + bounds.y1) as f32 / 2.0,
            ))
        })
        .unwrap();
    let action = reader.clone().activate(x, y).await.unwrap();
    assert_eq!(action.kind, "image");
    assert!(!action.target.is_empty());
    let attached = reader
        .clone()
        .add_chat_book_images(action.target)
        .await
        .unwrap();
    assert_eq!(attached.len(), 1);
    assert!(
        reader
            .clone()
            .chat_image(attached[0].id.clone())
            .await
            .is_ok()
    );
    reader
        .clone()
        .remove_chat_image(attached[0].id.clone())
        .await
        .unwrap();
    assert!(
        reader
            .clone()
            .chat_image(attached[0].id.clone())
            .await
            .is_err()
    );
    let image = action.media.unwrap();
    assert!(image.width() <= 2048);
    assert!(!image.pixels.is_empty());
    let formula = reader
        .with(|s| {
            let hit = rebook_reader::ReaderImage {
                source: None,
                origin: None,
                formula: Some(r"\frac{1}{2}".into()),
                position: rebook_reader::ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index: 0,
                },
                x: 0.0,
                y: 0.0,
                display_width: 10.0,
                display_height: 10.0,
                width: 1,
                height: 1,
                pixels: Arc::from([0u8, 0, 0, 0]),
            };
            interactions::image_page(s, hit)
        })
        .unwrap();
    assert!(formula.width() > 80 && formula.height() > 80);
    let focus = reader.clone().focus_mode(true).await.unwrap();
    assert!(focus.info().focus_count >= 2);
    assert!(!focus.info().focus_bounds.is_empty());
    let next = reader.clone().turn(true).await.unwrap();
    assert_eq!(next.info().focus_index, 1);
    let image_focus = reader.clone().turn(true).await.unwrap();
    assert_eq!(
        image_focus.info().focus_index,
        2,
        "zero-length image source ranges must remain focusable"
    );
    reader.clone().turn(false).await.unwrap();
    let previous = reader.clone().turn(false).await.unwrap();
    assert_eq!(previous.info().focus_index, 0);
    reader.clone().focus_mode(false).await.unwrap();
    let (x,y)=reader.with(|s|{
        let section=s.source.parse_section(0).map_err(error)?;
        let text=section.blocks.iter().flat_map(rebook_publication::Block::text_blocks).find(|b|b.content.iter().any(|i|matches!(i,rebook_publication::Inline::Text(t) if t.text.contains("Jump internally")))).unwrap();
        let bounds=s.session.current_page().source_rects(std::slice::from_ref(text.source.as_ref().unwrap()));
        Ok((bounds[0].x0 as f32+10.0,(bounds[0].y0+bounds[0].y1)as f32/2.0))
    }).unwrap();
    let action = reader.clone().activate(x, y).await.unwrap();
    assert_eq!(action.kind, "page");
    assert!(action.media.unwrap().info().can_go_back);
    assert!(!reader.clone().go_back().await.unwrap().info().can_go_back);
    let bounds=reader.with(|s|{
        let section=s.source.parse_section(0).map_err(error)?;
        let range=section.blocks.iter().flat_map(rebook_publication::Block::text_blocks).find(|b|b.content.iter().any(|i|matches!(i,rebook_publication::Inline::Text(t) if t.text.contains("Jump internally")))).unwrap().source.as_ref().unwrap();
        Ok(s.session.current_page().footnote_reference_bounds(range,0x2000_0001))
    }).unwrap();
    assert!(
        !bounds.is_empty(),
        "semantic note icon must retain touch geometry"
    );
    let b = bounds[0];
    let action = reader
        .clone()
        .activate((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0)
        .await
        .unwrap();
    assert_eq!(action.kind, "note");
    assert!(action.text.contains("multi-paragraph"));
    assert!(action.text.contains("continuation"));
    assert!(action.markup.starts_with("**A multi-paragraph note.**"));
    assert!(action.markup.contains(r"$\frac{1}{2}$"));
    assert!(reader.clone().activate(f32::NAN, 0.0).await.is_err());
    reader.shutdown().await.unwrap();
}

async fn cached_translation_roundtrip(core: &Arc<MobileCore>, book: &str) {
    let viewport = Viewport {
        width: 360,
        height: 640,
        scale: 1.0,
    };
    let reader = core
        .clone()
        .open_book(book.into(), viewport.clone())
        .await
        .unwrap();
    let mut settings = rebook_app_core::plugins::PluginSettings::load_default().unwrap();
    settings.target_language = "简体中文".into();
    settings.providers[0].kind = rebook_app_core::plugins::AiProviderKind::OpenAi;
    settings.providers[0].api_key.clear();
    settings.providers[0].models = vec![
        rebook_app_core::plugins::AiModelConfig::language("offline-a"),
        rebook_app_core::plugins::AiModelConfig::language("offline-b"),
    ];
    settings.translation_provider = settings.providers[0].id.clone();
    settings.translation_model = "offline-a".into();
    settings.save_default().unwrap();
    for (model, text) in [("offline-a", "缓存译文"), ("offline-b", "新模型译文")] {
        settings.translation_model = model.into();
        settings.save_default().unwrap();
        let mut resolved = rebook_app_core::plugins::PluginSettings::load_default().unwrap();
        resolved.target_language = resolved.resolved_target_language("简体中文");
        reader
            .with(|s| {
                let index = s.session.snapshot().location.section_index;
                let section = s.pipeline.rewrite.parse_section(index).map_err(error)?;
                let prepared =
                    rebook_app_core::plugins::prepare_translation_inputs(&section, false);
                let mut cache = rebook_app_core::translation_cache::TranslationCache::open(
                    book, index, &resolved, &prepared,
                )
                .unwrap();
                cache
                    .store(
                        &prepared
                            .iter()
                            .map(|(input, _)| rebook_app_core::plugins::BlockTranslation {
                                block_index: input.block_index,
                                segment_index: input.segment_index,
                                text: text.into(),
                            })
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
        let translated = reader.clone().translate_page(true).await.unwrap();
        assert!(
            translated
                .info()
                .regions
                .iter()
                .any(|r| r.text.contains(text))
        );
    }
    reader.shutdown().await.unwrap();
    let reopened = core.clone().open_book(book.into(), viewport).await.unwrap();
    let translated = reopened.clone().translate_page(true).await.unwrap();
    assert!(
        translated
            .info()
            .regions
            .iter()
            .any(|r| r.text.contains("新模型译文"))
    );
    reopened.shutdown().await.unwrap();
}

async fn advanced_reading(core: &Arc<MobileCore>, book: &str) {
    // All files/credentials use the existing isolated test profile.
    let mut options = core.clone().reading_options().await.unwrap();
    options.font_weight = 575;
    options.focus_style = "highlight".into();
    options.scroll_focus = true;
    options.sentence_split = true;
    core.clone()
        .save_reading_options(options.clone())
        .await
        .unwrap();
    assert_eq!(
        core.clone().reading_options().await.unwrap().font_weight,
        575
    );
    let fonts = core.clone().font_families().await.unwrap();
    assert!(fonts.monospace.iter().any(|f| f == "monospace"));
    let reader = core
        .clone()
        .open_book(
            book.into(),
            Viewport {
                width: 360,
                height: 640,
                scale: 1.0,
            },
        )
        .await
        .unwrap();
    reader
        .clone()
        .go_to_source(
            reader.clone().search("Paragraph 0:".into()).await.unwrap()[0]
                .source
                .clone(),
        )
        .await
        .unwrap();
    let first = reader.clone().focus_mode(true).await.unwrap();
    assert!(first.info().is_scroll && first.info().focus_highlight);
    assert!(!first.info().focus_bounds.is_empty());
    let sources = first
        .info()
        .regions
        .iter()
        .map(|r| r.source.clone())
        .collect::<Vec<_>>();
    let moved = reader.clone().scroll_by(500.0).await.unwrap();
    assert_eq!(moved.info().scroll_distance, 500.0);
    assert_ne!(
        sources,
        moved
            .info()
            .regions
            .iter()
            .map(|r| r.source.clone())
            .collect::<Vec<_>>()
    );
    reader
        .with(|s| {
            assert!(s.scroll_slices.len() <= 8);
            Ok(())
        })
        .unwrap();
    let visible = moved
        .info()
        .regions
        .into_iter()
        .filter(|r| {
            r.bounds
                .iter()
                .any(|b| b.y > 20.0 && b.y + b.height < 600.0)
        })
        .collect::<Vec<_>>();
    assert!(visible.len() >= 2, "scroll window must contain nearby text");
    let a = &visible[0].bounds[0];
    let b = &visible[1].bounds[0];
    let selected = reader
        .clone()
        .select(
            a.x + 2.0,
            a.y + a.height / 2.0,
            a.x + 35.0,
            a.y + a.height / 2.0,
        )
        .await
        .unwrap()
        .unwrap();
    reader.clone().begin_selection_handle(false).await.unwrap();
    let extended = reader
        .clone()
        .extend_selection(b.x + 45.0, b.y + b.height / 2.0)
        .await
        .unwrap()
        .unwrap();
    assert!(extended.text.chars().count() > selected.text.chars().count());
    reader.clone().scroll_by(60.0).await.unwrap();
    assert_eq!(
        reader.clone().selection().await.unwrap().unwrap().text,
        extended.text
    );
    reader.clone().clear_selection().await.unwrap();
    assert!(reader.clone().scroll_by(f32::NAN).await.is_err());
    let before = reader
        .with(|s| Ok((scroll::position(s), s.scroll_offset, s.focus_anchor.clone())))
        .unwrap();
    // Error after stepping forward must restore the reader, including its scroll offset.
    assert!(
        reader
            .clone()
            .prefetch_ai(false, true, chat::ChatRequest::new())
            .await
            .is_err()
    );
    let after = reader
        .with(|s| Ok((scroll::position(s), s.scroll_offset, s.focus_anchor.clone())))
        .unwrap();
    assert_eq!(before, after);
    struct CancelPrefetch(std::sync::Weak<chat::ChatRequest>);
    impl chat::AiProgressListener for CancelPrefetch {
        fn update(&self, stage: String, _: u32, _: u32) {
            if stage == "prefetch" {
                self.0.upgrade().unwrap().cancel();
            }
        }
    }
    let request = chat::ChatRequest::new();
    request.observe_ai(Arc::new(CancelPrefetch(Arc::downgrade(&request))));
    assert!(
        reader
            .clone()
            .prefetch_ai(true, false, request)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("已停止")
    );
    assert_eq!(
        before,
        reader
            .with(|s| Ok((scroll::position(s), s.scroll_offset, s.focus_anchor.clone())))
            .unwrap()
    );
    reader.shutdown().await.unwrap();

    let providers = core.clone().providers().await.unwrap();
    let original = providers[0].clone();
    core.clone()
        .save_provider(
            settings::Provider {
                id: String::new(),
                kind: "custom".into(),
                name: "temporary".into(),
                base_url: "http://127.0.0.1:1/v1".into(),
                models: vec!["test-model".into()],
                key_saved: false,
            },
            Some("test-only-secret".into()),
            false,
        )
        .await
        .unwrap();
    let added = core
        .clone()
        .providers()
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.name == "temporary")
        .unwrap();
    assert!(added.key_saved);
    core.clone()
        .save_provider(added.clone(), None, false)
        .await
        .unwrap();
    assert!(
        core.clone()
            .providers()
            .await
            .unwrap()
            .iter()
            .find(|p| p.id == added.id)
            .unwrap()
            .key_saved
    );
    core.clone().delete_provider(added.id).await.unwrap();
    assert!(
        core.clone()
            .providers()
            .await
            .unwrap()
            .iter()
            .any(|p| p.id == original.id)
    );
    let mut ai = core.clone().ai_options().await.unwrap();
    ai.show_original = true;
    ai.translate_toc = true;
    ai.expert_translation = true;
    ai.layout_enabled = false;
    ai.chat_steps = 13;
    ai.chat_history = 11;
    let saved = core.clone().save_ai_options(ai).await.unwrap();
    assert!(saved.show_original && saved.translate_toc && saved.expert_translation);
    assert_eq!(saved.chat_steps, 13);
    let mut options = core.clone().reading_options().await.unwrap();
    options.translated = true;
    core.clone().save_reading_options(options).await.unwrap();
    assert!(core.clone().reading_options().await.unwrap().translated);
    let reader = core
        .clone()
        .open_book(
            book.into(),
            Viewport {
                width: 360,
                height: 640,
                scale: 1.0,
            },
        )
        .await
        .unwrap();
    let mut resolved = rebook_app_core::plugins::PluginSettings::load_default().unwrap();
    resolved.target_language = resolved.resolved_target_language(
        rebook_app_core::preferences::AppLanguage::system_translation_target(),
    );
    reader
        .with(|s| {
            let rows = s.session.toc_items();
            let inputs = rows
                .iter()
                .enumerate()
                .map(|(index, row)| plugins::TranslationBlockInput {
                    block_index: index,
                    segment_index: None,
                    text: row.label.clone(),
                })
                .collect::<Vec<_>>();
            let mut cache = rebook_app_core::translation_cache::TranslationCache::open_text_inputs(
                &format!("{}-toc", s.book_id),
                &resolved,
                &inputs,
            )
            .unwrap();
            cache
                .store(
                    &inputs
                        .iter()
                        .map(|input| plugins::BlockTranslation {
                            block_index: input.block_index,
                            segment_index: None,
                            text: "目录译文".into(),
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    let toc = reader
        .clone()
        .translate_toc_cancellable(chat::ChatRequest::new())
        .await
        .unwrap();
    assert!(
        toc.iter()
            .any(|t| t.title.contains("目录译文") && t.title.contains('\n'))
    );
    reader.shutdown().await.unwrap();
}

async fn long_focus_continuation(core: &Arc<MobileCore>, root: &std::path::Path) {
    let path = root.join("long-focus.fb2");
    let text = "Readers keep a stable place within this long paragraph when its font changes. "
        .repeat(180);
    std::fs::write(&path,format!(r#"<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Long focus continuation</book-title><lang>en</lang></title-info></description><body><section><title><p>Long focus</p></title><p>{text}</p></section></body></FictionBook>"#)).unwrap();
    let book = core
        .clone()
        .import_book(path.to_string_lossy().into_owned())
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.title == "Long focus continuation")
        .unwrap();
    let mut options = core.clone().reading_options().await.unwrap();
    options.translated = false;
    options.sentence_split = false;
    core.clone()
        .save_reading_options(options.clone())
        .await
        .unwrap();
    let reader = core
        .clone()
        .open_book(
            book.id,
            Viewport {
                width: 360,
                height: 640,
                scale: 1.0,
            },
        )
        .await
        .unwrap();
    reader.clone().focus_mode(true).await.unwrap();
    let scrolled = reader.clone().scroll_by(1800.0).await.unwrap();
    assert!(scrolled.info().page > 0);
    assert!(
        scrolled
            .info()
            .regions
            .iter()
            .all(|r| r.text.len() < text.len() / 2),
        "each accessibility region must describe its page fragment"
    );
    options.font_weight = 650;
    let reflowed = reader.clone().apply_reading_options(options).await.unwrap();
    assert!(
        reflowed.info().page > 0,
        "changing the font must retain the paragraph continuation"
    );
    reader.shutdown().await.unwrap();
}

async fn statistics_cloud_and_diagnostics(
    core: &Arc<MobileCore>,
    root: &std::path::Path,
    book: &str,
) {
    use rebook_app_core::{statistics as st, sync};
    st::flush();
    let now = st::now_ms();
    let offset = chrono::Local::now().offset().local_minus_utc();
    for i in 0..3 {
        st::record(
            book,
            st::EventKind::Reading {
                session: "statistics-fixture".into(),
                start: now - 135_000 + i * 45_000,
                end: now - 90_000 + i * 45_000,
                offset,
                from: 0.1,
                to: 0.4,
            },
        );
    }
    st::flush();
    let month = core
        .clone()
        .statistics_period("month".into(), 0)
        .await
        .unwrap();
    assert!(month.total >= 135_000);
    assert!(month.days >= 1);
    assert!(month.books.iter().any(|b| b.id == book));
    let all = core
        .clone()
        .statistics_period("all".into(), 0)
        .await
        .unwrap();
    assert!(all.total >= month.total);
    let previous = core
        .clone()
        .statistics_period("year".into(), -1)
        .await
        .unwrap();
    assert_eq!(previous.distribution.len(), 12);
    assert!(
        core.clone()
            .statistics_period("week".into(), i32::MIN)
            .await
            .is_err()
    );
    let detail = core.clone().book_statistics(book.into()).await.unwrap();
    assert!(detail.total >= 135_000);
    assert!(detail.started.is_some() && detail.last.is_some());
    assert!(detail.history.iter().any(|e| e.duration == 45_000));

    let original = core.clone().cloud_options().await.unwrap();
    let server = sync::FakeWebDav::start();
    let mut o = original.clone();
    o.enabled = true;
    o.provider = "custom".into();
    o.url = server.base_url();
    o.username = "test-user".into();
    o.device_name = "Native test device".into();
    o.background = true;
    o.foreground = true;
    let saved = core
        .clone()
        .save_cloud_options(o, Some("cloud-test-secret".into()))
        .await
        .unwrap();
    assert!(saved.password_saved);
    assert_eq!(saved.device_name, "Native test device");
    let preserved = core
        .clone()
        .save_cloud_options(saved.clone(), None)
        .await
        .unwrap();
    assert!(preserved.password_saved);
    let report = core
        .clone()
        .sync_cancellable(saved.account.clone(), false, chat::ChatRequest::new())
        .await
        .unwrap();
    assert!(report.uploaded >= 1);
    let count = server.requests.lock().unwrap().len();
    let cancelled = chat::ChatRequest::new();
    cancelled.cancel();
    assert!(
        core.clone()
            .sync_cancellable(saved.account.clone(), true, cancelled)
            .await
            .is_err()
    );
    assert_eq!(server.requests.lock().unwrap().len(), count);
    let mut changed = saved.clone();
    changed.username = "other-account".into();
    core.clone()
        .save_cloud_options(changed, None)
        .await
        .unwrap();
    assert!(
        core.clone()
            .sync_cancellable(saved.account, true, chat::ChatRequest::new())
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("配置已改变")
    );
    let log = root.join("data/logs/android.log");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(
        &log,
        "Authorization: Bearer hidden-secret\ncloud-test-secret\nhealthy event\n",
    )
    .unwrap();
    let exported = core
        .clone()
        .export_diagnostics("test device".into())
        .await
        .unwrap();
    let report = std::fs::read_to_string(exported).unwrap();
    assert!(!report.contains("hidden-secret") && !report.contains("cloud-test-secret"));
    assert!(report.contains("healthy event"));
    core.clone()
        .save_cloud_options(original, Some(String::new()))
        .await
        .unwrap();
    server.stop();
}
