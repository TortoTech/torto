use super::*;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    pause: Arc<AtomicBool>,
    pause_translation: Arc<AtomicBool>,
    fail_translation: Arc<AtomicBool>,
    pause_structure: Arc<AtomicBool>,
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let pause = Arc::new(AtomicBool::new(false));
        let pause_translation = Arc::new(AtomicBool::new(false));
        let fail_translation = Arc::new(AtomicBool::new(false));
        let pause_structure = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (records, paused, stopped) = (requests.clone(), pause.clone(), stop.clone());
        let body_paused = pause_translation.clone();
        let fail_once = fail_translation.clone();
        let structure_paused = pause_structure.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                        length = n.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                let user = text(&payload["messages"][1]["content"]);
                let input: Value =
                    serde_json::Deserializer::from_str(&user).into_iter::<Value>().next().unwrap().unwrap();
                records.lock().unwrap().push(payload);
                let enhancement = input.get("tl").is_some() && input.get("tb").is_some();
                let structure = input.get("bs").is_some() || input.get("tg").is_some();
                if !enhancement && !structure && fail_once.swap(false,Ordering::AcqRel) {
                    let _=write!(socket,"HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
                    continue;
                }
                if (structure
                    && input["tg"]["cqk"]
                        .as_array()
                        .is_some_and(|ids| !ids.is_empty())
                    && structure_paused.swap(false, Ordering::AcqRel))
                    || (!enhancement && !structure && body_paused.swap(false, Ordering::AcqRel))
                {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                }
                let output = if input.get("tl").is_some() && input.get("tb").is_some() {
                    if paused.swap(false, Ordering::AcqRel) {
                        entered_tx.send(()).unwrap();
                        release_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                    }
                    let charts = if input["tb"].as_array().is_some_and(|v| !v.is_empty()) {
                        json!([{"tb":[0],"k":"bar","ti":"测量结果","pn":[{"ti":"测量结果","un":"","ctg":["甲","乙"],"sr":[{"nm":"数值","cs":[[0,1,1],[0,2,1]]}]}]}])
                    } else {
                        json!([])
                    };
                    let notes = if input["f"].as_array().is_some_and(|v| !v.is_empty()) {
                        json!([{"i":0,"nt":[{"sy":"x","me":"位置","un":"","ev":"x is the position.","kn":true}]}])
                    } else {
                        json!([])
                    };
                    let output = json!({"vz":charts,"nt":notes});
                    if input["tl"] == "English" {
                        serde_json::from_str(
                            &output
                                .to_string()
                                .replace("测量结果", "Measurements")
                                .replace("位置", "Position"),
                        )
                        .unwrap()
                    } else {
                        output
                    }
                } else if input.get("bs").is_some() || input.get("tg").is_some() {
                    json!({"g":[],"c":[],"f":[]})
                } else {
                    let mut result = serde_json::Map::new();
                    for key in input
                        .as_object()
                        .unwrap()
                        .keys()
                        .filter(|key| key.chars().next().is_some_and(|c| c.is_ascii_digit()))
                    {
                        let value=input[key].as_str().unwrap_or_default();
                        result.insert(key.clone(), if value.contains("<inlinefootnote ") {json!(value.replace("Inline definition", "内嵌脚注译文").replace("Inline body", "正文"))}else{json!("共享源位置")},
                        );
                    }
                    result.insert("g".into(), json!([{"s":"source locations","t":"源位置"}]));
                    Value::Object(result)
                };
                let body = json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":output.to_string()}}]}).to_string();
                // A cancelled request can close its socket before the delayed response.
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            url,
            requests,
            pause,
            pause_translation,
            fail_translation,
            pause_structure,
            entered,
            release,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.release.send(());
        if let Some(thread) = self.thread.take() {
            let joined = thread.join();
            if !std::thread::panicking() {
                joined.unwrap();
            }
        }
    }
}
fn text(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| {
        value
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    })
}
async fn drain(reader: Arc<MobileReader>, request: Arc<chat::ChatRequest>) -> Result<()> {
    loop {
        let update = reader
            .clone()
            .enhance_page_cancellable(request.clone())
            .await?;
        if !update.pending {
            return Ok(());
        }
    }
}

pub(super) async fn verify(core: &Arc<MobileCore>, root: &std::path::Path) {
    let original = plugins::PluginSettings::load_default().unwrap();
    let server = Server::new();
    let mut settings = original.clone();
    settings.providers = vec![plugins::AiProvider {
        id: "mobile-parity".into(),
        kind: plugins::AiProviderKind::Custom,
        base_url: server.url.clone(),
        api_key: "inert-test-key".into(),
        models: vec![plugins::AiModelConfig::language("test")],
        ..Default::default()
    }];
    settings.translation_provider = "mobile-parity".into();
    settings.translation_model = "test".into();
    settings.semantic_layout.provider = "mobile-parity".into();
    settings.semantic_layout.model = "test".into();
    settings.semantic_layout.enabled = true;
    settings.target_language = "简体中文".into();
    settings.expert_translation = true;
    settings.translate_toc = true;
    settings.save_default().unwrap();

    let path = root.join("expert-mobile.fb2");
    let paragraphs = (0..40)
        .map(|i| {
            format!("<p>Paragraph {i}: source locations must remain stable across devices.</p>")
        })
        .collect::<String>();
    std::fs::write(&path,format!(r#"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Expert parity</book-title><lang>en</lang></title-info></description><body><section><title><p>Source locations</p></title>{paragraphs}</section></body></FictionBook>"#)).unwrap();
    let book = core
        .clone()
        .import_book(path.to_string_lossy().into())
        .await
        .unwrap()
        .remove(0);
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
    reader.clone().page().await.unwrap();
    server.pause_translation.store(true, Ordering::Release);
    let cancelled = chat::ChatRequest::new();
    let worker = runtime().spawn(
        reader
            .clone()
            .translate_page_cancellable(true, cancelled.clone()),
    );
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), reader.clone().page())
            .await
            .is_ok(),
        "body translation must not own the reading mutex during HTTP"
    );
    cancelled.cancel();
    assert!(
        worker
            .await
            .unwrap()
            .err()
            .unwrap()
            .to_string()
            .contains("已停止")
    );
    server.release.send(()).unwrap();
    let start = server.requests.lock().unwrap().len();
    server.pause_translation.store(true, Ordering::Release);
    let worker = runtime().spawn(reader.clone().translate_page(true));
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    reader.clone().focus_mode(true).await.unwrap();
    let later = reader
        .clone()
        .search("Paragraph 4:".into())
        .await
        .unwrap()
        .remove(0)
        .source;
    reader.clone().go_to_source(later).await.unwrap();
    let latest = reader
        .with(|s| Ok(s.focus_anchor.clone().unwrap()))
        .unwrap();
    server.release.send(()).unwrap();
    worker.await.unwrap().unwrap();
    reader.with(|s| {
        assert_eq!(s.focus_anchor.as_ref().unwrap().start.node, latest.start.node,
            "a delayed response must retain the latest focus, not the HTTP request's original page");
        Ok(())
    }).unwrap();
    reader.clone().focus_mode(false).await.unwrap();
    reader.clone().turn(true).await.unwrap();
    reader.clone().translate_page(true).await.unwrap();
    reader
        .clone()
        .translate_toc_cancellable(chat::ChatRequest::new())
        .await
        .unwrap();
    let requests = server.requests.lock().unwrap()[start..].to_vec();
    assert!(
        requests.len() >= 3,
        "body, next body, and contents use real shared transport"
    );
    assert!(
        text(&requests[0]["messages"][0]["content"]).contains("Expert translation and glossary")
    );
    for request in requests.iter().skip(1) {
        assert!(
            request["messages"].to_string().contains("源位置"),
            "body and TOC reuse the same book/language terminology"
        );
    }
    reader.shutdown().await.unwrap();

    let file = root.join("layout-lock.epub");
    let body = format!(
        "<html xmlns='http://www.w3.org/1999/xhtml'><body>{}</body></html>",
        (0..3)
            .map(|i| format!(
                "<blockquote><p>Batch{i} {}</p></blockquote><p>End of example {i}.</p>",
                "for (let i=0; i&lt;10; i++) {{ run(); }} ".repeat(150)
            ))
            .collect::<String>()
    );
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
    for (name, content) in [
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf"/></rootfiles></container>"#,
        ),
        (
            "package.opf",
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">layout-lock</dc:identifier><dc:title>Layout lock</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="body"/></spine></package>"#,
        ),
        ("body.xhtml", body.as_str()),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    let book = core
        .clone()
        .import_book(file.to_string_lossy().into())
        .await
        .unwrap()
        .remove(0);
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
    reader.clone().page().await.unwrap();
    server.pause_structure.store(true, Ordering::Release);
    let cancelled = chat::ChatRequest::new();
    let worker = runtime().spawn(reader.clone().recognize_page_cancellable(cancelled.clone()));
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), reader.clone().page())
            .await
            .is_ok(),
        "base recognition must release the reading mutex during HTTP"
    );
    cancelled.cancel();
    assert!(
        worker
            .await
            .unwrap()
            .err()
            .unwrap()
            .to_string()
            .contains("已停止")
    );
    server.release.send(()).unwrap();
    reader.shutdown().await.unwrap();

    let book = core
        .clone()
        .books()
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.title == "Media controls")
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
    reader.clone().configure(20.0, true, false).await.unwrap();
    reader
        .with(|s| {
            s.session.go_to_section(0).map_err(error)?;
            Ok(())
        })
        .unwrap();
    // A tall viewport includes the authored table, formula and its definition.
    reader
        .clone()
        .resize(Viewport {
            width: 360,
            height: 1200,
            scale: 1.0,
        })
        .await
        .unwrap();
    reader.clone().recognize_page().await.unwrap();
    let before = reader.with(|s| Ok(s.session.current_locator())).unwrap();
    let request = chat::ChatRequest::new();
    server.pause.store(true, Ordering::Release);
    let worker = runtime().spawn(drain(reader.clone(), request.clone()));
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), reader.clone().page())
            .await
            .is_ok(),
        "model wait must release the reading mutex"
    );
    request.cancel();
    assert!(
        worker
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("已停止")
    );
    server.release.send(()).unwrap();
    drain(reader.clone(), chat::ChatRequest::new())
        .await
        .unwrap();
    let page = reader.clone().page().await.unwrap();
    assert!(
        page.info()
            .controls
            .iter()
            .any(|c| c.kind == "chart" && !c.active)
    );
    assert!(
        page.info()
            .controls
            .iter()
            .any(|c| c.kind == "formula" && !c.active)
    );
    reader
        .with(|s| {
            assert_eq!(before, s.session.current_locator());
            Ok(())
        })
        .unwrap();
    // Repeating a base pass must not erase completed charts and notes.
    reader.clone().recognize_page().await.unwrap();
    let page = reader.clone().page().await.unwrap();
    assert!(page.info().controls.iter().any(|c| c.kind == "chart"));
    assert!(page.info().controls.iter().any(|c| c.kind == "formula"));
    // Changing language invalidates the prepared work and schedules new metadata.
    settings.target_language = "日本語".into();
    settings.save_default().unwrap();
    reader.clone().reload_ai_options().await.unwrap();
    reader.clone().recognize_page().await.unwrap();
    server.pause.store(true, Ordering::Release);
    let worker = runtime().spawn(drain(reader.clone(), chat::ChatRequest::new()));
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    settings.target_language = "English".into();
    settings.save_default().unwrap();
    server.release.send(()).unwrap();
    worker.await.unwrap().unwrap();
    assert!(
        !reader
            .clone()
            .page()
            .await
            .unwrap()
            .info()
            .controls
            .iter()
            .any(|c| c.kind == "chart" || c.kind == "formula"),
        "stale language metadata must not be installed"
    );
    reader.clone().reload_ai_options().await.unwrap();
    assert!(
        !reader
            .clone()
            .page()
            .await
            .unwrap()
            .info()
            .controls
            .iter()
            .any(|c| c.kind == "chart" || c.kind == "formula")
    );
    reader.clone().recognize_page().await.unwrap();
    server.pause.store(true, Ordering::Release);
    let worker = runtime().spawn(drain(reader.clone(), chat::ChatRequest::new()));
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    reader
        .with(|s| {
            let section = s.pipeline.rewrite.parse_section(0).map_err(error)?;
            let source = section
                .blocks
                .iter()
                .flat_map(rebook_publication::Block::text_blocks)
                .find_map(|t| t.source.clone())
                .unwrap();
            s.pipeline
                .rewrite
                .apply_rewrites(&[plugins::BlockRewrite {
                    section_index: 0,
                    block_id: source.start.node,
                    text: "Updated source context.".into(),
                }])
                .map_err(error)?;
            Ok(())
        })
        .unwrap();
    server.release.send(()).unwrap();
    worker.await.unwrap().unwrap();
    assert!(
        !reader
            .clone()
            .page()
            .await
            .unwrap()
            .info()
            .controls
            .iter()
            .any(|c| c.kind == "chart" || c.kind == "formula"),
        "source edits reject in-flight metadata"
    );
    reader.shutdown().await.unwrap();
    original.save_default().unwrap();
}

pub(super) async fn driver_delivery(core: &Arc<MobileCore>) {
    let original = plugins::PluginSettings::load_default().unwrap();
    let server = Server::new();
    let mut settings = original.clone();
    settings.providers = vec![plugins::AiProvider {
        id: "driver".into(),
        kind: plugins::AiProviderKind::Custom,
        base_url: server.url.clone(),
        api_key: "inert-key".into(),
        models: vec![plugins::AiModelConfig::language("driver")],
        ..Default::default()
    }];
    settings.semantic_layout.enabled = true;
    settings.semantic_layout.provider = "driver".into();
    settings.semantic_layout.model = "driver".into();
    settings.target_language = "简体中文".into();
    settings.save_default().unwrap();
    let book = core
        .clone()
        .books()
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.title == "Media controls")
        .unwrap();
    let reader = core
        .clone()
        .open_book(
            book.id,
            Viewport {
                width: 360,
                height: 1200,
                scale: 1.0,
            },
        )
        .await
        .unwrap();
    reader.clone().configure(20.0, true, false).await.unwrap();
    #[derive(Default)]
    struct Listener {
        ready: AtomicBool,
        notify: tokio::sync::Notify,
        updates: Mutex<Vec<(u64, bool, bool)>>,
    }
    impl services::driver::ReaderAiListener for Listener {
        fn update(&self, update: services::driver::ReaderAiUpdate) {
            let controls = update
                .page
                .as_ref()
                .map(|p| p.info().controls)
                .unwrap_or_default();
            if controls.iter().any(|c| c.kind == "chart")
                && controls.iter().any(|c| c.kind == "formula")
            {
                self.ready.store(true, Ordering::Release);
            }
            self.updates.lock().unwrap().push((
                update.version,
                update.processing,
                update.enhancing,
            ));
            self.notify.notify_waiters();
        }
    }
    let listener = Arc::new(Listener::default());
    let activity = services::driver::ReaderAiActivity {
        active: true,
        interacting: false,
        manual_layout: false,
        ui_epoch: 7,
    };
    reader
        .clone()
        .update_ai_demand(activity.clone(), listener.clone())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wait = listener.notify.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            if listener.ready.load(Ordering::Acquire) {
                break;
            }
            wait.await;
        }
    })
    .await
    .expect("the demand adapter must deliver enhanced native pages, not discard them");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wait = listener.notify.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            if listener
                .updates
                .lock()
                .unwrap()
                .last()
                .is_some_and(|(_, working, enhancing)| !*working && !*enhancing)
            {
                break;
            }
            wait.await;
        }
    })
    .await
    .unwrap();
    let count = server.requests.lock().unwrap().len();
    for _ in 0..4 {
        reader
            .clone()
            .update_ai_demand(activity.clone(), listener.clone())
            .await
            .unwrap();
    }
    tokio::time::sleep(reading_ai_settle()).await;
    assert_eq!(
        count,
        server.requests.lock().unwrap().len(),
        "unchanged demand must remain idle without model requests"
    );
    // A callback from an obsolete configuration must not acquire the new
    // listener's epoch and overwrite its page while the transport is completing.
    settings.target_language = "English".into();
    settings.save_default().unwrap();
    reader.clone().reload_ai_options().await.unwrap();
    listener.ready.store(false, Ordering::Release);
    server.pause.store(true, Ordering::Release);
    reader
        .clone()
        .update_ai_demand(
            services::driver::ReaderAiActivity {
                ui_epoch: 8,
                ..activity.clone()
            },
            listener.clone(),
        )
        .await
        .unwrap();
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    settings.target_language = "日本語".into();
    settings.save_default().unwrap();
    reader.clone().reload_ai_options().await.unwrap();
    reader
        .clone()
        .update_ai_demand(
            services::driver::ReaderAiActivity {
                ui_epoch: 9,
                ..activity
            },
            listener.clone(),
        )
        .await
        .unwrap();
    server.release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wait = listener.notify.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            if listener.ready.load(Ordering::Acquire) {
                break;
            }
            wait.await;
        }
    })
    .await
    .unwrap();
    reader
        .with(|s| {
            let raw = s.pipeline.rewrite.parse_section(0).map_err(error)?;
            let (origin, latex) = raw
                .blocks
                .iter()
                .find_map(plugins::semantic_layout::enhancements::display_formula)
                .unwrap();
            let notes = s.pipeline.semantic.formula_notes(&origin, &latex);
            assert!(notes.iter().any(|n| n.meaning == "位置"));
            assert!(notes.iter().all(|n| n.meaning != "Position"));
            Ok(())
        })
        .unwrap();
    let versions = listener
        .updates
        .lock()
        .unwrap()
        .iter()
        .map(|(v, _, _)| *v)
        .collect::<Vec<_>>();
    assert!(versions.windows(2).all(|p| p[0] < p[1]));
    reader.shutdown().await.unwrap();
    // An in-flight prefetched batch becomes visible before its HTTP completes.
    // Completing it must automatically hand off to translation, without another
    // gesture or UI demand update, and without a duplicate structure request.
    let reading = core.clone().reading_options().await.unwrap();
    core.clone()
        .save_reading_options(settings::ReadingOptions {
            translated: true,
            ..reading.clone()
        })
        .await
        .unwrap();
    settings.translation_provider = "driver".into();
    let mut legacy = serde_json::to_value(settings::load_reading(&core.root)).unwrap();
    legacy["ai_prefetch"] = false.into();
    rebook_app_core::persistence::write_json_atomic(&core.root.join("android-reading.json"), &legacy,
    ).unwrap();
    settings.translation_model = "driver".into();
    settings.translate_toc = false;
    settings.expert_translation = false;
    settings.save_default().unwrap();
    let book = core
        .clone()
        .books()
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.title == "Layout lock")
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
    reader.clone().set_translation_enabled(true)
        .await
        .unwrap();
    reader.clone().configure(20.0, true, false).await.unwrap();
    reader.clone().page().await.unwrap();
    let listener = Arc::new(Listener::default());
    let activity = services::driver::ReaderAiActivity {
        active: true,
        interacting: false,
        manual_layout: false,
        ui_epoch: 20,
    };
    server.pause_structure.store(true, Ordering::Release);
    reader
        .clone()
        .update_ai_demand(activity.clone(), listener.clone())
        .await
        .unwrap();
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    server.pause_structure.store(true, Ordering::Release);
    server.release.send(()).unwrap();
    server
        .entered
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let marker = {
        let records = server.requests.lock().unwrap();
        let input = text(&records.last().unwrap()["messages"][1]["content"]);
        let input: Value = serde_json::from_str(input.split("\n\n").next().unwrap()).unwrap();
        let target = input["tg"]["cqk"].as_array().unwrap().first().unwrap();
        let block = input["bs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["i"] == *target)
            .unwrap()
            .to_string();
        (0..3)
            .map(|i| format!("Batch{i}"))
            .find(|m| block.contains(m))
            .unwrap()
    };
    let source = reader
        .clone()
        .search(marker.clone())
        .await
        .unwrap()
        .remove(0)
        .source;
    reader.clone().go_to_source(source).await.unwrap();
    reader
        .clone()
        .update_ai_demand(
            services::driver::ReaderAiActivity {
                ui_epoch: 21,
                ..activity
            },
            listener.clone(),
        )
        .await
        .unwrap();
    let count = server.requests.lock().unwrap().len();
    server.release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wait = listener.notify.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            if server.requests.lock().unwrap().iter().skip(count).any(|r| {
                let body = text(&r["messages"][1]["content"]);
                let input: Value =
                    serde_json::from_str(body.split("\n\n").next().unwrap()).unwrap();
                input.get("bs").is_none() && input.get("tg").is_none() && body.contains(&marker)
            }) {
                break;
            }
            wait.await;
        }
    })
    .await
    .expect("prefetch completion must resume visible translation without another UI event");
    reader.shutdown().await.unwrap();
    core.clone().save_reading_options(reading).await.unwrap();
    original.save_default().unwrap();
}
fn reading_ai_settle() -> std::time::Duration {
    rebook_app_core::reading_ai::SETTLE + std::time::Duration::from_millis(30)
}

/// Opening a linked endnote uses the shared retry/cache path and publishes only
/// document pixels. Other entries in the note chapter remain untranslated.
pub(super) async fn linked_note_popup(core:&Arc<MobileCore>,root:&std::path::Path) {
    let original=plugins::PluginSettings::load_default().unwrap();
    let reading=core.clone().reading_options().await.unwrap();
    let server=Server::new();
    let mut settings=original.clone();
    settings.providers=vec![plugins::AiProvider {id:"notes-test".into(),kind:plugins::AiProviderKind::Custom,base_url:server.url.clone(),models:vec![plugins::AiModelConfig::language("test")],..Default::default()}];
    settings.translation_provider="notes-test".into();settings.translation_model="test".into();
    settings.target_language="简体中文".into();settings.translate_toc=false;settings.expert_translation=false;
    settings.semantic_layout.enabled=false;settings.save_default().unwrap();
    core.clone().save_reading_options(settings::ReadingOptions {translated:true,..reading.clone()}).await.unwrap();
    let path=root.join("linked-notes.epub");
    let mut zip=zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    for (name,body) in [
        ("mimetype",r#"application/epub+zip"#),
        ("META-INF/container.xml",r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>"#,
        ),
        ("book.opf",r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">linked-notes</dc:identifier><dc:title>Linked notes</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="body"/><itemref idref="notes"/></spine></package>"#,
        ),
        ("body.xhtml",r##"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><p>Body remains still<a epub:type="noteref" href="notes.xhtml#one">1</a>.</p><p>Inline body<span class="footnote">Inline definition</span></p></body></html>"##,
        ),
        ("notes.xhtml",r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><section epub:type="endnotes"><aside epub:type="footnote" id="one"><p>First linked definition.</p></aside><aside epub:type="footnote" id="two"><p>Unrelated second definition.</p></aside></section></body></html>"#,
        ),
    ] {zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();zip.write_all(body.as_bytes()).unwrap();}
    zip.finish().unwrap();
    let book=core.clone().import_book(path.to_string_lossy().into()).await.unwrap().remove(0);
    let viewport=Viewport {width:300,height:300,scale:1.,
    };
    let reader=core.clone().open_book(book.id,viewport.clone()).await.unwrap();
    reader.clone().configure(24.,true,false).await.unwrap();
    let before=reader.clone().page().await.unwrap().info();
    let mut expanded=reader.with(|s| {
        let raw=s.pipeline.rewrite.parse_section(0).map_err(error)?;
        Ok(vec![(0,raw.blocks.iter().flat_map(plugins::semantic_layout::block_ranges).collect(),
            )])
    }).unwrap();
    reader.with(|s| {rebook_app_core::reading_text::append_linked_footnote_translation_ranges(s.pipeline.rewrite.as_ref(),&mut expanded,
            );Ok(())}).unwrap();
    assert_eq!(expanded.len(),2,"linked definitions participate in body translation demand");
    assert_eq!(expanded[1].1.len(),1,"one note must not promote the whole notes container"
    );
    reader.clone().set_translation_enabled(true).await.unwrap();
    let target="notes.xhtml#one".to_string();
    let first=reader.clone().note_page("First linked definition.".into(),42,target.clone(),"1".into(),24.,0,viewport.clone(),0xff000000,0xfff4f5f7,
        ).await.unwrap();
    assert!(first.page.info().regions.iter().any(|r|r.text.contains("First linked")));
    let bounds=first.page.info().regions[0].bounds.clone();
    assert!(bounds[0].x>=25.,"only the first line reserves the visible marker");
    #[derive(Default)] struct Listener {revision:std::sync::atomic::AtomicU64,notify:tokio::sync::Notify,pages:std::sync::atomic::AtomicUsize,
    }
    impl services::driver::ReaderAiListener for Listener {
        fn update(&self,u:services::driver::ReaderAiUpdate){
            self.revision.fetch_max(u.document_revision,Ordering::Release);
            if u.page.is_some(){self.pages.fetch_add(1,Ordering::Relaxed);}
            self.notify.notify_waiters();
        }
    }
    let listener=Arc::new(Listener::default());
    server.fail_translation.store(true,Ordering::Release);
    let activity=services::driver::ReaderAiActivity {active:true,interacting:false,manual_layout:false,ui_epoch:100,
    };
    reader.clone().update_ai_demand(activity.clone(),listener.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {let wait=listener.notify.notified();tokio::pin!(wait);wait.as_mut().enable();if listener.revision.load(Ordering::Acquire)>0 {break;}wait.await;}
    }).await.expect("translated popup must publish its revision without a body page");
    let translated=reader.clone().note_page("First linked definition.".into(),42,target.clone(),"1".into(),24.,0,viewport.clone(),0xff000000,0xfff4f5f7,
        ).await.unwrap();
    assert!(translated.page.info().regions.iter().any(|r|r.text.contains("共享源位置")));
    assert_eq!(listener.pages.load(Ordering::Relaxed),0,"translation must not reflow the covered body");
    let requests=server.requests.lock().unwrap();
    assert_eq!(requests.len(),2,"mobile must reuse the shared first request plus one retry");
    let payload=text(&requests[0]["messages"][1]["content"]);
    assert!(payload.contains("First linked definition"));assert!(!payload.contains("Unrelated second"));drop(requests);
    reader.with(|s| {
        assert_eq!(s.session.location().section_index,before.section as usize);
        let raw=s.pipeline.rewrite.parse_section(1).map_err(error)?;
        let target=PublicationUrl::parse("notes.xhtml#two").unwrap();
        let ranges=rebook_app_core::reading_text::focus_footnote_translation_ranges_in_section(&raw,&target,
                );
        let prepared=plugins::prepare_translation_inputs(&raw,false);
        assert!(!s.pipeline.translation.untranslated_prepared(1,&prepared,&ranges).map_err(error)?.is_empty());Ok(())
    }).unwrap();
    reader.clone().close_document(translated.id).await.unwrap();
    reader.clone().note_page("First linked definition.".into(),43,target,"1".into(),24.,0,viewport,0xff000000,0xfff4f5f7,
        ).await.unwrap();
    reader.clone().update_ai_demand(activity,listener.clone()).await.unwrap();
    assert_eq!(server.requests.lock().unwrap().len(),2,"reopening uses translation cache");
    // Embedded footnotes must also resolve from the translated source rather
    // than retaining the original string captured on the first tap.
    reader.with(|s|{s.document=None;Ok(())}).unwrap();
    let (owner,range)=reader.with(|s| {
        let raw=s.pipeline.rewrite.parse_section(0).map_err(error)?;
        let source=raw.blocks.iter().flat_map(|b|b.text_blocks()).find(|t| {
                    t.content.iter().flat_map(rebook_publication::Inline::text_runs).any(|r|r.text.contains("Inline body"))
                }).unwrap().source.clone().unwrap();
        Ok((rebook_publication::source_block_identity(&source),source))
    }).unwrap();
    let target=format!("inline-note:0:{owner}:{}",0x2000_0001u32);
    reader.clone().note_page("Inline definition".into(),44,target.clone(),"1".into(),24.,0,Viewport {width:300,height:300,scale:1.,
            },0xff000000,0xfff4f5f7,
        ).await.unwrap();
    let revision=listener.revision.load(Ordering::Acquire);
    reader.clone().update_ai_demand(services::driver::ReaderAiActivity {active:true,interacting:false,manual_layout:false,ui_epoch:101,
            },listener.clone(),
        ).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {let wait=listener.notify.notified();tokio::pin!(wait);wait.as_mut().enable();if listener.revision.load(Ordering::Acquire)>revision {break;}wait.await;}
    }).await.expect("inline notes must publish translated popup content");
    let page=reader.clone().note_page("Inline definition".into(),44,target,"1".into(),24.,0,Viewport {width:300,height:300,scale:1.,
            },0xff000000,0xfff4f5f7,
        ).await.unwrap();
    assert!(page.page.info().regions.iter().any(|r|r.text.contains("内嵌脚注译文")));
    assert_eq!(listener.pages.load(Ordering::Relaxed),0);
    assert!(reader.with(|s|Ok(documents::translation_target(s).unwrap().ranges.contains(&range))).unwrap());
    reader.clone().close_document(page.id).await.unwrap();
    reader.clone().update_ai_demand(services::driver::ReaderAiActivity {active:true,interacting:false,manual_layout:false,ui_epoch:101,
            },listener.clone(),
        ).await.unwrap();
    assert!(listener.pages.load(Ordering::Relaxed)>0,"closing a popup publishes body results held during preview without another scroll");
    assert_eq!(reader.with(|s|Ok(s.session.snapshot().location.section_index)).unwrap(),before.section as usize);

    reader.shutdown().await.unwrap();core.clone().save_reading_options(reading).await.unwrap();original.save_default().unwrap();
}
