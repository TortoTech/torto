use super::*;
use rebook_app_core::plugins;
mod ai_parity;

fn root() -> PathBuf {
    std::env::temp_dir().join(format!("torto-android-test-{}", uuid::Uuid::new_v4()))
}

async fn focus_membership_and_heading_masks_follow_desktop_across_pages_and_resizes(core:&Arc<MobileCore>,root:&std::path::Path) {
        use std::io::Write;
        let path=root.join("focus-parity.epub");
        let body="<h1>Chapter title</h1><p>Earlier paragraph.</p><h2>Topic title</h2><h3>Subtopic title</h3><p>Following paragraph.</p><p>Later paragraph.</p><div><p>Grouped one.</p><p>Grouped two.</p><p>Grouped three.</p></div><p>After container.</p><ul><li>Parent item.<ul><li>Nested child.</li><li>Other child.</li></ul></li><li>Last root item.</li></ul><p>Final paragraph.</p>";
        let mut zip=zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name,text) in [
            ("mimetype","application/epub+zip".to_owned()),
            ("META-INF/container.xml",r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>"#.into()),
            ("book.opf",r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">focus-parity</dc:identifier><dc:title>Focus parity</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="body"/></spine></package>"#.into()),
            ("body.xhtml",format!("<html xmlns='http://www.w3.org/1999/xhtml'><body>{body}</body></html>")),
            ("nav.xhtml",r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="body.xhtml">Chapter</a></li></ol></nav></body></html>"#.into()),
        ] {zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(text.as_bytes()).unwrap();}
        zip.finish().unwrap();
        let book=core.clone().import_book(path.to_string_lossy().into_owned()).await.unwrap().into_iter().find(|b|b.title=="Focus parity").unwrap();
        let reader=core.clone().open_book(book.id,Viewport {width:360,height:640,scale:1.0}).await.unwrap();
        let mut options=core.clone().reading_options().await.unwrap();
        options.sentence_split=false;options.translated=false;
        reader.clone().apply_reading_options(options).await.unwrap();
        reader.clone().configure(20.0,true,false).await.unwrap();
        let following=reader.clone().search("Following paragraph".into()).await.unwrap().remove(0).source;
        reader.clone().focus_mode(true).await.unwrap();
        for height in [640,180] {
            reader.clone().resize(Viewport {width:360,height,scale:1.0}).await.unwrap();
            reader.clone().go_to_source(following.clone()).await.unwrap();
            let (body,headings)=reader.with(|s| {
                let unit=s.focus_units.iter().find(|u|u.ranges.iter().any(|r|serde_json::from_str::<SourceRange>(&following).unwrap().start.node==r.start.node)).unwrap();
                assert!(!unit.leading);
                assert!(!unit.headings.is_empty(),"both headings must survive retained logical-page assembly");
                assert_eq!(unit.paint_bounds().len(),2,"dim style clears the body and its preceding titles separately");
                Ok((unit.navigation_bounds().unwrap(),unit.headings.clone()))
            }).unwrap();
            let focused=reader.clone().page().await.unwrap().info();
            assert_eq!(focused.focus_bounds.len(),2);
            assert!(headings.iter().all(|heading|heading.y+heading.height<=body.y+1.0));
            let previous=reader.clone().turn(false).await.unwrap().info();
            assert!(previous.next_focus.is_some());
            let preview=previous.next_focus.unwrap();
            assert_eq!(preview.bounds.len(),2,"a drag preview uses the same title association as its final frame");
            reader.clone().turn(true).await.unwrap();
            reader.with(|s| {
                assert!(s.focus_anchor.as_ref().is_some_and(|r|r.start.node==serde_json::from_str::<SourceRange>(&following).unwrap().start.node),"turning never stops on either heading");Ok(())
            }).unwrap();
        }
        let grouped=reader.clone().search("Grouped two".into()).await.unwrap().remove(0).source;
        for (height,expected) in [(640,3),(120,1)] {
            reader.clone().resize(Viewport {width:360,height,scale:1.0}).await.unwrap();
            reader.clone().go_to_source(grouped.clone()).await.unwrap();
            reader.with(|s| {
                let group=s.focus_units.iter().find(|u|u.ranges.iter().any(|r|r.start.node==serde_json::from_str::<SourceRange>(&grouped).unwrap().start.node)).unwrap();
                assert_eq!(group.ranges.len(),expected,"only a fully visible group fitting the viewport forms one stop");Ok(())
            }).unwrap();
        }
        let child=reader.clone().search("Nested child".into()).await.unwrap().remove(0).source;
        reader.clone().go_to_source(child.clone()).await.unwrap();
        reader.with(|s| {
            let unit=s.focus_units.iter().find(|u|u.ranges.iter().any(|r|r.start.node==serde_json::from_str::<SourceRange>(&child).unwrap().start.node)).unwrap();
            assert_eq!(unit.ranges.len(),3,"a tall list splits by root items, retaining both nested children with their parent");Ok(())
        }).unwrap();
        reader.shutdown().await.unwrap();
}
fn fixture(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let paragraphs = (0..60).map(|i| format!("<p>Paragraph {i}: readers share stable source locations across devices. 中文搜索与选择。</p>")).collect::<String>();
    std::fs::write(path,format!(r#"<?xml version="1.0" encoding="UTF-8"?><FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Android reader fixture</book-title><lang>en</lang></title-info></description><body><section id="one"><title><p>One</p></title>{paragraphs}</section></body></FictionBook>"#)).unwrap();
}
async fn classic_previews_preserve_progress_and_focus_restores_the_saved_layout(reader: &Arc<MobileReader>) {
    let original = reader.clone().configure(20.0, false, false).await.unwrap();
    let locator = reader.with(|s| Ok(s.session.current_locator())).unwrap();
    let progress = reader.with(|s| Ok(s.store.load_progress(&s.book_id).map_err(error)?.unwrap().locator)).unwrap();
    assert!(reader.clone().preview_turn(false).await.unwrap().is_none());
    let preview = reader.clone().preview_turn(true).await.unwrap().unwrap();
    assert_ne!(preview.info().regions[0].source, original.info().regions[0].source);
    reader.with(|s| {
        assert_eq!(s.session.current_locator(), locator);
        assert_eq!(s.store.load_progress(&s.book_id).map_err(error)?.unwrap().locator, progress);
        assert!(s.history.is_empty());
        Ok(())
    }).unwrap();
    let turned = reader.clone().turn(true).await.unwrap();
    assert_eq!(preview.pixels, turned.pixels);
    // Old versions could persist false; that obsolete key must never restore paged focus.
    let mut legacy = reader.with(|s| Ok(serde_json::to_value(&s.options).unwrap())).unwrap();
    legacy["scroll_focus"] = serde_json::json!(false);
    let options: settings::ReadingOptions = serde_json::from_value(legacy).unwrap();
    assert!(serde_json::to_value(&options).unwrap().get("scroll_focus").is_none());
    reader.clone().apply_reading_options(options).await.unwrap();
    let focus = reader.clone().focus_mode(true).await.unwrap();
    assert!(focus.info().is_scroll);
    reader.clone().configure(22.0, false, true).await.unwrap();
    reader.with(|s| {
        assert_eq!(s.session.style().typesetting.mode, TypesettingMode::Unified);
        assert_eq!(s.session.style().typesetting.line_break_strategy, rebook_layout::LineBreakStrategy::Optimized);
        assert_eq!(s.classic_typesetting.mode, TypesettingMode::Book);
        Ok(())
    }).unwrap();
    assert!(reader.clone().preview_turn(true).await.unwrap().is_none());
    reader.clone().focus_mode(false).await.unwrap();
    reader.with(|s| {
        assert_eq!(s.session.style().typesetting.mode, TypesettingMode::Book);
        assert_eq!(s.session.style().typography.font_size, 22.0);
        Ok(())
    }).unwrap();
    reader.clone().configure(22.0, true, true).await.unwrap();
    reader.clone().focus_mode(true).await.unwrap();
    reader.clone().focus_mode(false).await.unwrap();
    reader.with(|s| {assert_eq!(s.session.style().typesetting.mode, TypesettingMode::Unified);Ok(())}).unwrap();
    reader.clone().configure(20.0, false, false).await.unwrap();
    reader.clone().focus_mode(false).await.unwrap();
    let end=reader.clone().search("Paragraph 59:".into()).await.unwrap().remove(0).source;
    reader.clone().go_to_source(end).await.unwrap();
    let at_end=reader.with(|s|Ok(s.session.current_locator())).unwrap();
    for _ in 0..2 {
        let result=reader.clone().turn_with_result(true,false).await.unwrap();
        assert!(result.reached_end,"only an explicit forward boundary reports the end");
        reader.with(|s|{assert_eq!(s.session.current_locator(),at_end);Ok(())}).unwrap();
    }
    assert!(!reader.clone().turn_with_result(false,false).await.unwrap().reached_end);
    reader.clone().focus_mode(true).await.unwrap();
    assert!(reader.clone().turn_with_result(true,true).await.unwrap().reached_end,"the final focus unit can end before its last focused paragraph");
    assert!(!reader.clone().turn_with_result(false,true).await.unwrap().reached_end);
    reader.clone().focus_mode(false).await.unwrap();
    reader.with(|s| {s.session.restore_locator(&locator).map_err(error)?;render(s)?;Ok(())}).unwrap();
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
        classic_previews_preserve_progress_and_focus_restores_the_saved_layout(&reader).await;
        let first = reader.clone().page().await.unwrap();
        let contents=reader.clone().toc().await.unwrap();
        reader.with(|s|{let expected=s.session.toc_items();assert_eq!(contents.len(),expected.len());
            for (actual,expected) in contents.iter().zip(expected){assert_eq!(actual.id,expected.id);assert_eq!(actual.ancestors,expected.ancestors);assert_eq!(actual.has_children,expected.has_children);}
            assert_eq!(first.info().active_toc_id,s.session.snapshot().active_toc_id);Ok(())}).unwrap();

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
        let count=core.clone().book_statistics(books[0].id.clone()).await.unwrap().history.iter().filter(|e|e.kind=="finished").count();
        reader.clone().mark_finished().await.unwrap();
        assert_eq!(core.clone().book_statistics(books[0].id.clone()).await.unwrap().history.iter().filter(|e|e.kind=="finished").count(),count,"marking an already finished book is idempotent");
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
        native_document_pages(&reader).await;
        reader.clone().shutdown().await.unwrap();
        assert!(matches!(
            reader.clone().page().await,
            Err(CoreError::Closed)
        ));
        drop(reader);
        drop(first);
        cached_translation_roundtrip(&core,&books[0].id).await;
        reflow::tests::verify(&core, &root).await;
        touch_and_focus(&core, &root).await;
        media_and_presentation(&core,&root).await;
        ai_parity::verify(&core, &root).await;
        ai_parity::driver_delivery(&core).await;
        ai_parity::linked_note_popup(&core,&root).await;
        feedback::tests::verify(&core,&books[0].id,&root).await;
        advanced_reading(&core, &books[0].id).await;
        long_focus_continuation(&core,&root).await;
        retained_focus_chapter_boundaries(&core,&root).await;
        statistics_cloud_and_diagnostics(&core, &root, &books[0].id).await;
        focus_membership_and_heading_masks_follow_desktop_across_pages_and_resizes(&core,&root).await;
        drop(core);
        // The process-wide statistics worker can still own its SQLite handle
        // after MobileCore is dropped. Windows cannot unlink that open file;
        // this is expected teardown behavior, not a reader shutdown failure.
        if let Err(error) = std::fs::remove_dir_all(root) {
            assert!(
                cfg!(windows) && error.raw_os_error() == Some(32),
                "could not clean up test profile: {error}"
            );
        }
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
    assert_eq!(action.marker,"1");
    let indicator=action.indicator.as_ref().expect("the active note retains the desktop baseline indicator");
    assert!((12.0..=16.0).contains(&indicator.width));
    assert_eq!(indicator.height,2.0);
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
    // Old Android settings must not restore the removed style or prefetch controls.
    let mut legacy = serde_json::to_value(&options).unwrap();
    legacy["focus_style"] = "highlight".into();
    legacy["ai_prefetch"] = false.into();
    rebook_app_core::persistence::write_json_atomic(&core.root.join("android-reading.json"), &legacy).unwrap();
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
    assert!(first.info().is_scroll && !first.info().focus_highlight, "Android always dims surroundings, including when old settings request highlighting");
    assert!(!first.info().focus_bounds.is_empty());
    focused_sentence_action_is_local_and_survives_reflow(&reader).await;
    let first = reader.clone().page().await.unwrap();
    let sources = first
        .info()
        .regions
        .iter()
        .map(|r| r.source.clone())
        .collect::<Vec<_>>();
    let moved = reader.clone().scroll_by(500.0).await.unwrap();
    assert_eq!(moved.info().scroll_distance, 500.0);
    assert_eq!(first.raster_id(), moved.raster_id(), "nearby scrolling must reuse retained pixels");
    assert!(Arc::ptr_eq(&first.pixels, &moved.pixels));
    assert!(moved.pixels.len() <= 24*1024*1024);
    assert!(moved.info().raster_top < 0.0 && moved.info().raster_height > 640.0);

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
    let target=reader.clone().search("Paragraph 0:".into()).await.unwrap()[0].source.clone();
    let origin=reader.clone().go_to_source(target).await.unwrap();
    let info=origin.info();
    let active=&info.focus_bounds[0];
    let tap=reader.clone().activate(active.x+2.0,active.y+active.height/2.0).await.unwrap();
    assert_eq!(tap.kind,"none","an active focus block toggles controls instead of navigating");
    assert!(tap.media.is_none(),"toggling controls must not render a replacement page");
    let next=reader.clone().turn(true).await.unwrap();
    assert_eq!(next.info().scroll_context,info.scroll_context);
    assert!((next.info().scroll_distance-info.scroll_distance-f64::from(info.next_focus.as_ref().unwrap().delta)).abs()<0.1);
    let reversed=reader.clone().turn(false).await.unwrap();
    assert!((reversed.info().scroll_distance-info.scroll_distance).abs()<0.1,"one-block movement must reverse to the same stop");
    assert_eq!(origin.raster_id(),next.raster_id(),"one-block navigation must reuse the strip");
    let target=origin.info().next_focus.unwrap();
    let r=&target.bounds[0];
    let action=reader.clone().activate(r.x+2.0,r.y+r.height/2.0).await.unwrap();
    assert_eq!(action.kind,"focus","inactive blocks retain the distinct focus navigation action");
    let clicked=action.media.unwrap();
    assert!((clicked.info().scroll_distance-info.scroll_distance-f64::from(target.delta)).abs()<0.1,"tapping a block must move the retained viewport as well as its focus mask");
    let context=clicked.info().scroll_context;
    reader.clone().trim_memory().await.unwrap();
    reader.with(|s| {assert!(s.scroll_cache.is_none());Ok(())}).unwrap();
    let rebuilt=reader.clone().page().await.unwrap();
    assert_eq!(rebuilt.info().scroll_context,context,"memory pressure must not reset motion coordinates");
    assert_ne!(rebuilt.raster_id(),clicked.raster_id());
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

async fn focused_sentence_action_is_local_and_survives_reflow(reader: &Arc<MobileReader>) {
    let page = reader.clone().page().await.unwrap();
    let control = page.info().controls.into_iter().find(|c| c.kind == "sentences").unwrap();
    assert!(!control.active, "legacy global sentence_split must not split the whole book");
    let anchor = reader.with(|s| Ok(s.focus_anchor.clone().unwrap())).unwrap();
    let original = reader.with(|s| s.pipeline.structure.parse_section(0).map_err(error)).unwrap();
    let structured = reader.clone().toggle_reading_control(control.clone()).await.unwrap();
    assert!(structured.info().controls.iter().any(|c| c.kind == "sentences" && c.active));
    reader.with(|s| {
        let changed = s.pipeline.structure.parse_section(0).map_err(error)?;
        let mut changed_count = 0;
        for (before, after) in original.blocks.iter().zip(&changed.blocks) {
            if before != after {
                assert!(before.text_blocks().any(|b| b.source.as_ref().is_some_and(|r| r.start.node == anchor.start.node)), "no neighbouring paragraph may be changed");
                assert!(after.text_blocks().any(|b| b.style.sentence_indents));
                changed_count += 1;
            }
        }
        assert_eq!(changed_count, 1);
        assert_eq!(s.focus_anchor.as_ref().unwrap().start.node, anchor.start.node);
        Ok(())
    }).unwrap();
    let options = reader.with(|s| Ok(s.options.clone())).unwrap();
    let reflowed = reader.clone().apply_reading_options(options).await.unwrap();
    assert!(reflowed.info().controls.iter().any(|c| c.kind == "sentences" && c.active));
    let classic = reader.clone().focus_mode(false).await.unwrap();
    assert!(!classic.info().controls.iter().any(|c| c.kind == "sentences"));
    reader.with(|s| { assert_eq!(s.pipeline.structure.parse_section(0).map_err(error)?.blocks, original.blocks); Ok(()) }).unwrap();
    reader.clone().focus_mode(true).await.unwrap();
    reader.clone().go_to_source(serde_json::to_string(&anchor).unwrap()).await.unwrap();
    reader.clone().turn(true).await.unwrap();
    let stale = reader.clone().toggle_reading_control(control.clone()).await.unwrap();
    assert!(!stale.info().controls.iter().any(|c| c.kind == "sentences" && c.active), "a stale action cannot split the next paragraph");
    reader.clone().go_to_source(serde_json::to_string(&anchor).unwrap()).await.unwrap();
    let restored = reader.clone().toggle_reading_control(control).await.unwrap();
    assert!(restored.info().controls.iter().any(|c| c.kind == "sentences" && !c.active));
    reader.with(|s| { assert_eq!(s.pipeline.structure.parse_section(0).map_err(error)?.blocks, original.blocks); Ok(()) }).unwrap();
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
        .sync_scheduled(
            saved.account.clone(),
            cloud::CloudSyncTrigger::Automatic,
            chat::ChatRequest::new(),
        )
        .await
        .unwrap();
    assert!(report.summary.unwrap().uploaded >= 1);
    let count_after_full = server.requests.lock().unwrap().len();
    let idle = core
        .clone()
        .sync_scheduled(
            saved.account.clone(),
            cloud::CloudSyncTrigger::Automatic,
            chat::ChatRequest::new(),
        )
        .await
        .unwrap();
    assert!(idle.summary.is_none() && idle.error.is_none());
    assert_eq!(
        server.requests.lock().unwrap().len(),
        count_after_full,
        "idle polling must not send HTTP requests"
    );
    cloud::forget_schedule_cache();
    let restarted = MobileCore::new(root.join("data").to_string_lossy().into_owned()).unwrap();
    let idle = restarted
        .sync_scheduled(
            saved.account.clone(),
            cloud::CloudSyncTrigger::Automatic,
            chat::ChatRequest::new(),
        )
        .await
        .unwrap();
    assert!(idle.summary.is_none());
    assert_eq!(
        server.requests.lock().unwrap().len(),
        count_after_full,
        "a new core must recover the successful schedule"
    );
    sync::mark_derived_dirty(book, sync::DerivedDataKind::Metadata).unwrap();
    let derived = core
        .clone()
        .sync_scheduled(
            saved.account.clone(),
            cloud::CloudSyncTrigger::Automatic,
            chat::ChatRequest::new(),
        )
        .await
        .unwrap();
    assert!(derived.error.is_none(), "{:?}", derived.error);
    assert_eq!(derived.summary.unwrap().uploaded, 0);
    assert!(
        server.requests.lock().unwrap()[count_after_full..]
            .iter()
            .all(|(_, path, _)| !path.ends_with("/library/devices/")),
        "dirty derived data must not rescan the library"
    );
    let count = server.requests.lock().unwrap().len();
    let cancelled = chat::ChatRequest::new();
    cancelled.cancel();
    assert!(
        core.clone()
            .sync_scheduled(
                saved.account.clone(),
                cloud::CloudSyncTrigger::Automatic,
                cancelled
            )
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
            .sync_scheduled(
                saved.account,
                cloud::CloudSyncTrigger::Automatic,
                chat::ChatRequest::new()
            )
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("配置已改变")
    );
    let mut unavailable = core.clone().cloud_options().await.unwrap();
    unavailable.url = "http://127.0.0.1:1/dav".into();
    let unavailable = core
        .clone()
        .save_cloud_options(unavailable, None)
        .await
        .unwrap();
    let failed = core
        .clone()
        .sync_scheduled(
            unavailable.account.clone(),
            cloud::CloudSyncTrigger::Automatic,
            chat::ChatRequest::new(),
        )
        .await
        .unwrap();
    assert!(failed.error.is_some());
    assert!(
        failed
            .retry_after_ms
            .is_some_and(|ms| (29_000..=30_000).contains(&ms))
    );
    cloud::forget_schedule_cache();
    let recovered = MobileCore::new(root.join("data").to_string_lossy().into_owned()).unwrap();
    let waiting = recovered
        .sync_scheduled(
            unavailable.account,
            cloud::CloudSyncTrigger::Automatic,
            chat::ChatRequest::new(),
        )
        .await
        .unwrap();
    assert!(waiting.summary.is_none() && waiting.error.is_none());
    assert!(
        waiting.next_poll_ms >= 29_000,
        "retry must survive a new core instance"
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

async fn retained_focus_chapter_boundaries(core: &Arc<MobileCore>, root: &std::path::Path) {
    use std::io::Write;
    let path=root.join("focus-boundaries.epub");
    let mut zip=zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let paragraphs=|prefix:&str| (0..10).map(|i|format!("<p>{prefix} block {i}: focus moves one paragraph at a time.</p>")).collect::<String>();
    let files=[
        ("mimetype","application/epub+zip".to_owned()),
        ("META-INF/container.xml",r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.into()),
        ("book.opf",r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">focus-boundaries</dc:identifier><dc:title>Focus boundaries</dc:title><dc:language>en</dc:language></metadata><manifest><item id="prelude" href="prelude.xhtml" media-type="application/xhtml+xml"/><item id="continuation" href="continuation.xhtml" media-type="application/xhtml+xml"/><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="prelude"/><itemref idref="continuation"/><itemref idref="a"/><itemref idref="b"/></spine></package>"#.into()),
        ("prelude.xhtml","<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h1>Heading-only prelude</h1></body></html>".into()),
        ("continuation.xhtml",r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Prelude continuation in an unlisted spine file.</p></body></html>"#.into()),
        ("a.xhtml",format!("<html xmlns=\"http://www.w3.org/1999/xhtml\"><body><h2 id=\"one\">First unit</h2>{}<h2 id=\"two\">Second unit</h2>{}</body></html>",paragraphs("A"),paragraphs("C"))),
        ("b.xhtml",format!("<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>{}</body></html>",paragraphs("B"))),
        ("nav.xhtml",r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="prelude.xhtml">Prelude</a><ol><li><a href="a.xhtml#one">First unit</a></li><li><a href="a.xhtml#two">Second unit</a></li></ol></li><li><a href="b.xhtml">Third unit</a></li></ol></nav></body></html>"#.into()),
    ];
    for (name,content) in files { zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(content.as_bytes()).unwrap(); }
    zip.finish().unwrap();
    let book=core.clone().import_book(path.to_string_lossy().into()).await.unwrap().into_iter().find(|b|b.title=="Focus boundaries").unwrap();
    let reader=core.clone().open_book(book.id.clone(),Viewport {width:360,height:640,scale:1.0}).await.unwrap();
    reader.clone().focus_mode(true).await.unwrap();
    reader.with(|s| {
        let expected=s.session.current_reading_unit_pages().map_err(error)?.iter().map(|p|p.position).collect::<Vec<_>>();
        assert!(s.scroll_slices.iter().any(|p|p.position.section_index==1),"a parent prelude and its unlisted continuation share one reading unit: core={expected:?}, strip={:?}",s.scroll_slices.iter().map(|p|p.position).collect::<Vec<_>>());
        assert!(s.scroll_slices.iter().all(|p|p.position.section_index<2));
        Ok(())
    }).unwrap();
    let middle=reader.clone().search("A block 3:".into()).await.unwrap()[0].source.clone();
    let from=reader.clone().go_to_source(middle).await.unwrap().info();
    let original=reader.clone().page().await.unwrap();
    let saved=reader.with(|s|Ok(serde_json::to_vec(&s.store.load_progress(&s.book_id).map_err(error)?.unwrap().locator).map_err(error)?)).unwrap();
    let prepared=reader.clone().preview_turn(true).await.unwrap().expect("focus prepares an adjacent semantic unit");
    assert_eq!((prepared.width(),prepared.height()),(360,640),"a chapter preview retains only one viewport");
    assert!(prepared.info().regions.iter().any(|r|r.text.contains("C block 0:")));
    reader.clone().preview_turn(false).await.unwrap();
    let restored=reader.clone().page().await.unwrap();
    assert_eq!(restored.raster_id(),original.raster_id(),"peeking restores the live retained strip");
    assert_eq!(restored.info().scroll_context,from.scroll_context);
    reader.with(|s|{assert_eq!(serde_json::to_vec(&s.store.load_progress(&s.book_id).map_err(error)?.unwrap().locator).map_err(error)?,saved);Ok(())}).unwrap();
    let actual=reader.clone().turn_reading_unit(true).await.unwrap();
    let next=actual.info();
    let start=-(next.raster_top.round() as i64);
    for row in 0..640i64 {
        let source=start+row;
        assert!(source>=0&&source<i64::from(actual.height()));
        let at=source as usize*360*4;let preview=row as usize*360*4;
        assert_eq!(&actual.pixels[at..at+360*4],&prepared.pixels[preview..preview+360*4],"preview and committed pixels must align without a jump");
    }
    assert_eq!(next.section,from.section,"adjacent subheadings in one XHTML must remain distinct units");
    assert_ne!(next.scroll_context,from.scroll_context);
    assert!(next.regions.iter().any(|r|r.text.contains("C block 0:")));
    assert!(next.regions.iter().all(|r|!r.text.contains("A block")));
    let back=reader.clone().turn_reading_unit(false).await.unwrap().info();
    assert!(back.regions.iter().any(|r|r.text.contains("A block 9:")),"returning to a previous unit selects its final block");
    for (from,to,same_spine) in [("A","C",true),("C","B",false)] {
        let source=reader.clone().search(format!("{from} block 9:")).await.unwrap()[0].source.clone();
        let edge=reader.clone().go_to_source(source.clone()).await.unwrap().info();
        assert!(edge.next_focus.is_none(),"{from}: an edge must not expose the next reading unit as another focus block; index={}/{}, texts={:?}",edge.focus_index,edge.focus_count,edge.regions.iter().map(|r|&r.text).collect::<Vec<_>>());
        assert!(edge.regions.iter().all(|r|!r.text.contains(&format!("{to} block"))),"neighbouring unit text must not enter the raster/accessibility window");
        let unit=reader.with(|s| Ok(s.session.current_reading_unit_anchor())).unwrap();
        let clamped=reader.clone().scroll_by(2000.0).await.unwrap().info();
        assert!(clamped.regions.iter().all(|r|!r.text.contains(&format!("{to} block"))));
        reader.with(|s| {assert_eq!(s.session.current_reading_unit_anchor(),unit,"selection-edge scrolling cannot change units");Ok(())}).unwrap();
        let last=reader.clone().go_to_source(source).await.unwrap().info();
        let next=reader.clone().turn(true).await.unwrap().info();
        assert_ne!(next.scroll_context,last.scroll_context,"a unit transition starts an independent scroll surface");
        assert_eq!(next.section==last.section,same_spine);
        assert!(next.previous_focus.is_none());
        assert!(next.regions.iter().all(|r|!r.text.contains(&format!("{from} block"))));
        reader.with(|s| {
            assert_ne!(s.session.current_reading_unit_anchor(),unit);
            let saved=s.store.load_progress(&s.book_id).map_err(error)?.unwrap().locator.source.unwrap();
            assert_eq!(saved.start.spine,s.focus_anchor.as_ref().unwrap().start.spine);
            assert_eq!(saved.start.node,s.focus_anchor.as_ref().unwrap().start.node);
            Ok(())
        }).unwrap();
        let back=reader.clone().turn(false).await.unwrap().info();
        assert_eq!(back.section,last.section);
        reader.with(|s| {assert_eq!(s.session.current_reading_unit_anchor(),unit);Ok(())}).unwrap();
        assert!(back.regions.iter().any(|r|r.text.contains(&format!("{from} block 9:"))));
    }
    let saved=reader.with(|s|Ok(s.store.load_progress(&s.book_id).map_err(error)?.unwrap().locator.source.unwrap())).unwrap();
    reader.shutdown().await.unwrap();
    let reopened=core.clone().open_book(book.id.clone(),Viewport {width:360,height:640,scale:1.0}).await.unwrap();
    reopened.clone().focus_mode(true).await.unwrap();
    reopened.with(|s| {
        let active=s.focus_anchor.as_ref().unwrap();
        assert_eq!(active.start.spine,saved.start.spine);
        assert_eq!(active.start.node,saved.start.node,"reopening must restore the focused block, not its page's leading paragraph");
        let mut stale=s.store.load_progress(&s.book_id).map_err(error)?.unwrap().locator;
        let range=stale.source.as_mut().unwrap();
        range.start.node="obsolete-bookmark-node".into();range.end.node="obsolete-bookmark-node".into();
        s.store.save_progress(&s.book_id,&stale).map_err(error)?;
        Ok(())
    }).unwrap();
    reopened.shutdown().await.unwrap();
    let fallback=core.clone().open_book(book.id.clone(),Viewport {width:360,height:640,scale:1.0}).await.unwrap();
    assert!(!fallback.clone().focus_mode(true).await.unwrap().info().regions.is_empty(),"an obsolete bookmark must still use the core locator fallback");
    fallback.shutdown().await.unwrap();
}


async fn native_document_pages(reader:&Arc<MobileReader>) {
    let before=reader.clone().page().await.unwrap().info();
    let markup=(0..45).map(|n|format!("Paragraph {n}: footnotes inherit the reader font and optimized composition.\n\n")).collect::<String>();
    let viewport=Viewport {width:300,height:240,scale:1.0};
    let first=reader.clone().note_page(markup.clone(),1,String::new(),String::new(),24.,0,viewport.clone(),0xff000000,0xfff8f5ee).await.unwrap();
    assert!(first.total>2,"long notes must remain scrollable");
    assert!(first.page.info().regions.iter().any(|r|r.text.contains("Paragraph 0")));
    let rect=first.page.info().regions.iter().flat_map(|r|&r.bounds).next().unwrap().clone();
    let selection=reader.clone().document_select(first.id.clone(),0,rect.x+2.0,rect.y+rect.height/2.0,rect.x+50.0,rect.y+rect.height/2.0,None).await.unwrap().unwrap();
    assert!(!selection.text.is_empty());assert!(!selection.bounds.is_empty());
    let last=reader.clone().note_page(markup,1,String::new(),String::new(),24.,first.total-1,viewport.clone(),0xff000000,0xfff8f5ee).await.unwrap();
    assert_eq!(last.id,first.id);
    assert!(last.page.info().regions.iter().any(|r|r.text.contains("44:")));
    assert_eq!(reader.clone().page().await.unwrap().info().progress,before.progress,"preview must not move book progress");
    reader.clone().close_document(first.id.clone()).await.unwrap();
    assert!(reader.clone().reference_page(first.id,0,viewport.clone(),0xff000000,0xfff8f5ee).await.is_err());
}

async fn media_and_presentation(core:&Arc<MobileCore>,root:&std::path::Path) {
    use std::io::Write;
    use rebook_publication::{Block,Inline};
    let path=root.join("media-controls.epub");
    let body=r##"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Media</title></head><body><p>See Table 1.1 for measurements.</p><table id="t"><caption>Table 1.1 Measurements</caption><tr><th>Name</th><th>Value</th></tr><tr><td>A</td><td>2</td></tr><tr><td>B</td><td>4</td></tr></table><p><span class="math math-display">x</span></p><p>x is the position.</p></body></html>"##;
    let mut zip=zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    for(name,content)in [
        ("mimetype","application/epub+zip"),
        ("META-INF/container.xml",r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf"/></rootfiles></container>"#),
        ("package.opf",r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">mobile-media-controls</dc:identifier><dc:title>Media controls</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="body"/></spine></package>"#),
        ("body.xhtml",body)
    ]{zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(content.as_bytes()).unwrap();}zip.finish().unwrap();
    let books=core.clone().import_book(path.to_string_lossy().into()).await.unwrap();
    let book=books.iter().find(|b|b.title=="Media controls").unwrap();
    let viewport=Viewport{width:360,height:640,scale:1.0};
    let reader=core.clone().open_book(book.id.clone(),viewport.clone()).await.unwrap();
    reader.clone().configure(20.0,true,false).await.unwrap();
    assert!(reader.clone().prepare_media_references().await.unwrap());
    assert!(!reader.clone().prepare_media_references().await.unwrap(),"catalog must be reused");
    reader.clone().refresh_media_references().await.unwrap();
    let (origin,key)=reader.with(|s|{
        let section=s.source.parse_section(0).map_err(error)?;
        let text=section.blocks.iter().flat_map(Block::text_blocks).next().unwrap();
        let key=rebook_publication::media_references::MediaKey{kind:rebook_publication::media_references::MediaKind::Table,number:"1.1".into()};
        Ok((text.source.clone().unwrap(),key))
    }).unwrap();
    let action=reader.with(|s|{let target=s.media.resolve(&origin,&key).unwrap();documents::reference(s,target)}).unwrap();
    assert_eq!(action.kind,"reference");
    let preview=reader.clone().reference_page(action.target.clone(),0,viewport.clone(),0xff000000,0xfff8f5ee).await.unwrap();
    assert!(preview.page.info().regions.iter().any(|r|r.text.contains("Measurements")));
    reader.with(|s|{
        let original=s.pipeline.rewrite.parse_section(0).map_err(error)?;
        let table=original.blocks.iter().find_map(|b|if let Block::Table(t)=b{t.source.clone()}else{None}).unwrap();
        let (formula,latex)=original.blocks.iter().flat_map(Block::text_blocks).find_map(|t|t.content.iter().find_map(|i|if let Inline::Math(m)=i{Some((t.source.clone().unwrap(),m.latex.clone()))}else{None})).unwrap();
        let fingerprint=plugins::semantic_layout::fingerprint(&original);
        let recognition=serde_json::from_value(serde_json::json!({"fingerprint":fingerprint,"formulas_checked":true,"image_text_revision":999,"annotations":[
            {"TableChart":{"source":table,"members":[table],"svg":"<svg xmlns='http://www.w3.org/2000/svg' width='200' height='100'><rect width='180' height='80' fill='green'/></svg>","language":"简体中文"}},
            {"FormulaNotes":{"source":formula,"latex":latex,"language":"简体中文","notes":[{"symbol":"x","meaning":"位置","unit":"m","evidence":"x is the position.","known":true}]}}
        ]})).unwrap();
        assert!(s.pipeline.semantic.install_scope(0,&fingerprint,0..original.blocks.len(),recognition));
        s.session.refresh_source().map_err(error)?;scroll::invalidate(s);Ok(())
    }).unwrap();
    let page=reader.clone().page().await.unwrap();
    let controls=page.info().controls;
    assert!(controls.iter().any(|c|c.kind=="formula"&&!c.active));
    let chart=controls.iter().find(|c|c.kind=="chart").unwrap().clone();
    assert!(!chart.active);
    let updated=reader.clone().toggle_document_control(action.target.clone(),chart).await.unwrap();
    assert!(updated.info().controls.iter().any(|c|c.kind=="chart"&&c.active));
    let preview=reader.clone().reference_page(action.target.clone(),0,viewport.clone(),0xff000000,0xfff8f5ee).await.unwrap();
    assert!(preview.controls.iter().any(|c|c.kind=="chart"&&c.active));
    let formula=reader.clone().page().await.unwrap().info().controls.into_iter().find(|c|c.kind=="formula").unwrap();
    let updated=reader.clone().toggle_reading_control(formula).await.unwrap();
    assert!(updated.info().controls.iter().any(|c|c.kind=="formula"&&c.active));
    reader.clone().close_document(action.target).await.unwrap();
    reflow::tests::media(&reader).await;
    reader.clone().shutdown().await.unwrap();
}
