use super::*;
use rebook_publication::*;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Source {
    book: Book,
    renders: AtomicUsize,
}
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let item = &self.book.sections[index];
        Ok(Section {
            id: item.id.clone(),
            href: item.href.clone(),
            anchors: vec![],
            blocks: vec![Block::Image(ImageBlock {
                formula_image: false,
                formula: None,
                href: item.href.clone(),
                alt: String::new(),
                style: Default::default(),
                source: None,
                text_layer: Some(FixedPageTextLayer {
                    width: 10.0,
                    height: 10.0,
                    text: format!("Chapter {} contents book title", index + 1),
                    spans: vec![],
                    replacement: None,
                }),
            })],
        })
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Err(PublicationError::ResourceNotFound(href.to_string()))
    }
    fn raster_resource(
        &self,
        _: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.renders.fetch_add(1, Ordering::SeqCst);
        Ok(Some(RasterResource {
            width: 10,
            height: 10,
            pixels: vec![255; 400].into(),
        }))
    }
}
fn source() -> Arc<Source> {
    let id = format!(
        "pdf-agent-test-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    Arc::new(Source {
        book: Book {
            id: PublicationId::new(id).unwrap(),
            metadata: Default::default(),
            cover: None,
            sections: (1..=5)
                .map(|p| SpineItem {
                    id: SpineItemId::new(format!("p{p}")).unwrap(),
                    href: PublicationUrl::parse(&format!("p{p}.png")).unwrap(),
                    media_type: "image/png".into(),
                    linear: true,
                    properties: vec![],
                })
                .collect(),
            table_of_contents: vec![],
        },
        renders: AtomicUsize::new(0),
    })
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn connection_error(status: u16, kind: &str) -> Value {
    json!({"status_code":status,"error":{"type":kind,"message":"failed to execute HTTP request to provider API"}})
}

#[test]
fn connection_retry_classification_excludes_auth_schema_and_generic_server_errors() {
    for status in [502, 503, 504] {
        assert!(transient_connection_failure(&format!(
            "ProviderResponseError: status {status}: {}\n (request id: test)",
            connection_error(status, "provider_connection_failed")
        )));
    }
    for (status, kind) in [
        (401, "provider_connection_failed"),
        (429, "provider_connection_failed"),
        (500, "provider_connection_failed"),
        (502, "invalid_request_error"),
        (502, "provider_response_unmarshal"),
    ] {
        assert!(!transient_connection_failure(
            &connection_error(status, kind).to_string()
        ));
    }
    assert!(!transient_connection_failure(
        "JsonError: missing field data"
    ));
    assert!(!transient_connection_failure("AI 请求超时"));
}

#[test]
fn connection_retry_resends_same_round_before_tool_execution() {
    let reply = json!({"id":"reply","object":"chat.completion","created":0,"model":"test","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
    let (url, handle) = crate::plugins::llm::tests::server(vec![
        (502, connection_error(502, "provider_connection_failed")),
        (200, reply),
    ]);
    let provider = AiProvider {
        base_url: url,
        ..Default::default()
    };
    let mut progress = Vec::new();
    let result = runtime()
        .block_on(complete_round(
            &provider,
            "test",
            ReasoningEffort::High,
            &[json!({"role":"user","content":"inspect"})],
            &tools(),
            &mut |p| progress.push(p),
        ))
        .unwrap();
    assert_eq!(result["content"], "done");
    assert_eq!(progress.len(), 1);
    let requests = handle.join().unwrap();
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[1]["reasoning_effort"], "high");
}

#[test]
fn connection_retry_is_bounded_and_preserves_final_error() {
    let (url, handle) = crate::plugins::llm::tests::server(vec![
        (
            502,
            connection_error(502, "provider_connection_failed")
        );
        3
    ]);
    let provider = AiProvider {
        base_url: url,
        ..Default::default()
    };
    let mut progress = Vec::new();
    let error = runtime()
        .block_on(complete_round(
            &provider,
            "test",
            ReasoningEffort::Default,
            &[json!({"role":"user","content":"inspect"})],
            &tools(),
            &mut |p| progress.push(p),
        ))
        .unwrap_err();
    assert!(error.contains("provider_connection_failed"));
    assert_eq!(progress.len(), 2);
    assert_eq!(handle.join().unwrap().len(), 3);
}

#[test]
fn connection_retry_does_not_retry_authentication_errors() {
    let (url, handle) = crate::plugins::llm::tests::server(vec![(
        401,
        connection_error(401, "provider_connection_failed"),
    )]);
    let provider = AiProvider {
        base_url: url,
        ..Default::default()
    };
    let error = runtime()
        .block_on(complete_round(
            &provider,
            "test",
            ReasoningEffort::Default,
            &[json!({"role":"user","content":"inspect"})],
            &tools(),
            &mut |_| panic!("must not retry"),
        ))
        .unwrap_err();
    assert!(error.contains("401"));
    assert_eq!(handle.join().unwrap().len(), 1);
}
fn goals() -> Goals {
    Goals {
        toc: true,
        metadata: true,
        page_roles: true,
    }
}
fn patch(entry: Value) -> Value {
    json!({"entries":[entry],"delete_entries":[],"metadata":null,"roles":[],"delete_roles":[]})
}
fn entry(target: usize) -> Value {
    json!({"id":"chapter","order":0,"depth":0,"title":"Chapter","printed_page":"iv","physical_page":target,"source_page":2,"verified":true})
}
#[test]
fn draft_rejects_unseen_evidence_and_updates_atomically() {
    let mut s = Session::new(source(), goals()).unwrap();
    let p = patch(entry(4));
    assert!(s.patch(serde_json::from_value(p.clone()).unwrap()).is_err());
    assert!(s.draft.entries.is_empty());
    s.draft.viewed.insert(2);
    s.draft.read.insert(4);
    s.patch(serde_json::from_value(p).unwrap()).unwrap();
    let old = serde_json::to_value(&s.draft).unwrap();
    assert!(
        s.patch(serde_json::from_value(patch(entry(99))).unwrap())
            .is_err()
    );
    assert_eq!(serde_json::to_value(&s.draft).unwrap(), old);
    let resumed = Session::new(s.source.clone(), goals()).unwrap();
    assert_eq!(resumed.draft.entries["chapter"].printed_page, "iv");
}
#[test]
fn image_cache_and_search_do_not_call_a_model() {
    runtime().block_on(async {
        let source = source();
        let mut s = Session::new(source.clone(), goals()).unwrap();
        let read = json!({"pages":[3],"crop":null});
        assert_eq!(
            s.execute("read_pages", read.clone()).await.unwrap().value["cache_hits"],
            0
        );
        assert_eq!(
            s.execute("read_pages", read).await.unwrap().value["cache_hits"],
            1
        );
        assert_eq!(source.renders.load(Ordering::SeqCst), 1);
        let results = s
            .execute(
                "search_text",
                json!({"query":"chapter 3","start":1,"end":5}),
            )
            .await
            .unwrap();
        assert_eq!(results.value["hits"][0]["page"], 3);
        assert!(
            s.execute("read_pages", json!({"pages":[1],"crop":[1.0,0.0,0.1,1.0]}))
                .await
                .is_err()
        );
    });
}

#[test]
fn crop_cache_uses_actual_pixels_and_survives_a_new_session() {
    runtime().block_on(async {
        let source = source();
        let mut session = Session::new(source.clone(), goals()).unwrap();
        let crop = [0.1_f64, 0.1, 0.5, 0.5];
        let first = session
            .execute("read_pages", json!({"pages":[3],"crop":crop}))
            .await
            .unwrap();
        assert_eq!(first.value["cache_hits"], 0);
        let mut adjacent = crop;
        adjacent[0] = f64::from_bits(crop[0].to_bits() + 1);
        let mut reopened = Session::new(source.clone(), goals()).unwrap();
        let cached = reopened
            .execute("read_pages", json!({"pages":[3],"crop":adjacent}))
            .await
            .unwrap();
        assert_eq!(cached.value["cache_hits"], 1);
        assert_eq!(cached.images, first.images);
        assert_eq!(source.renders.load(Ordering::SeqCst), 1);
        adjacent[0] = 0.1005;
        let changed = reopened
            .execute("read_pages", json!({"pages":[3],"crop":adjacent}))
            .await
            .unwrap();
        assert_eq!(changed.value["cache_hits"], 0);
        assert_eq!(source.renders.load(Ordering::SeqCst), 2);
    });
}
#[test]
fn schemas_are_valid_and_finish_rejects_unverified_navigation() {
    for tool in tools().as_array().unwrap() {
        jsonschema::validator_for(&tool["function"]["parameters"]).unwrap();
    }
    runtime().block_on(async {
        let mut s=Session::new(source(),Goals {toc:true,metadata:false,page_roles:false}).unwrap();
        s.draft.viewed.insert(2);
        let mut e=entry(4);e["verified"]=json!(false);
        s.patch(serde_json::from_value(patch(e)).unwrap()).unwrap();
        let finish=json!({"toc":"complete","metadata":"not_requested","page_roles":"not_requested","summary":"done"});
        assert!(s.execute("finish",finish.clone()).await.is_err());
        let mut finish=finish;finish["toc"]=json!("partial");
        assert!(s.execute("finish",finish).await.unwrap().finished);
    });
}

#[test]
fn partial_navigation_promotes_children_of_unresolved_parents() {
    let mut s = Session::new(source(), goals()).unwrap();
    s.draft.viewed.insert(2);
    s.draft.read.insert(4);
    let mut parent = entry(3);
    parent["verified"] = json!(false);
    let mut child = entry(4);
    child["id"] = json!("child");
    child["order"] = json!(1);
    child["depth"] = json!(1);
    let mut patch = patch(parent);
    patch["entries"].as_array_mut().unwrap().push(child);
    s.patch(serde_json::from_value(patch).unwrap()).unwrap();
    let result = s.result("fixture", "fixture", "partial").unwrap();
    let entries = result.toc.unwrap().entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].depth, 0);
    assert_eq!(entries[0].physical_page, 4);
}

#[test]
fn invalid_tool_arguments_fail_schema_before_execution() {
    let tools = tools();
    let schema = &tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "read_pages")
        .unwrap()["function"]["parameters"];
    let validator = jsonschema::validator_for(schema).unwrap();
    for args in [
        json!({"pages":[0],"crop":null}),
        json!({"pages":[1,2,3,4,5,6],"crop":null}),
        json!({"pages":[1,1],"crop":null}),
    ] {
        assert!(!validator.is_valid(&args));
    }
}
fn call(name: &str, args: Value) -> (u16, Value) {
    (
        200,
        json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":format!("call-{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]}),
    )
}
#[test]
fn agent_chooses_pages_saves_then_finishes_using_real_tool_history() {
    let mut update = patch(entry(4));
    update["metadata"] = json!({"title":"The Book","authors":["Writer"],"evidence_pages":[1]});
    update["roles"] = json!([{"page":1,"role":"title-page"}]);
    let (url, server) = crate::plugins::llm::tests::server(vec![
        call("overview_pages", json!({"pages":[1,2,5]})),
        call("read_pages", json!({"pages":[1,2,4],"crop":null})),
        call("update_draft", update),
        call(
            "finish",
            json!({"toc":"complete","metadata":"complete","page_roles":"complete","summary":"confirmed"}),
        ),
    ]);
    let mut settings = PluginSettings::default().with_test_model();
    settings.ocr_reasoning_effort = ReasoningEffort::High;
    settings.providers[0].base_url = url;
    settings.providers[0].api_key = "fixture".into();
    let result = runtime()
        .block_on(run(source(), settings, goals(), |_| {}))
        .unwrap();
    assert_eq!(result.toc.unwrap().entries[0].physical_page, 4);
    assert_eq!(result.metadata.unwrap().title, "The Book");
    assert_eq!(result.page_roles.len(), 1);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|r| r["reasoning_effort"] == "high"));
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "tool")
    );
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("data:image/jpeg;base64")
    );
    assert!(requests.iter().all(|r| r.get("response_format").is_none()));
}

#[test]
#[ignore = "sends selected local PDF pages to the explicitly configured vision/tool model"]
fn live_configured_pdf_discovery() {
    let query = std::env::var("TORTO_PDF_AGENT_BOOK")
        .expect("set book title query")
        .to_lowercase();
    let model = std::env::var("TORTO_PDF_AGENT_MODEL").expect("set configured model ID");
    let library = crate::library::LocalLibrary::load_default().unwrap();
    let matches: Vec<_> = library
        .books()
        .iter()
        .filter(|b| {
            b.title.to_lowercase().contains(&query)
                && b.path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        })
        .collect();
    assert_eq!(matches.len(), 1, "book query must select exactly one PDF");
    let book = matches[0];
    let opened = rebook_formats::open_file(&book.path).unwrap();
    let source = opened.source();
    let mut settings = PluginSettings::load_default().unwrap();
    let provider = settings
        .providers
        .iter()
        .find(|p| p.models.iter().any(|m| m.id == model))
        .expect("model must already be configured");
    eprintln!(
        "LIVE book={} pages={} provider={} model={}",
        book.title,
        source.book().sections.len(),
        provider.name,
        model
    );
    settings.ocr_provider = provider.id.clone();
    settings.ocr_model = model.clone();
    let started = Instant::now();
    let mut progress = Vec::new();
    let result = runtime().block_on(run(source.clone(), settings, goals(), |message| {
        eprintln!("{:.1}s {message}", started.elapsed().as_secs_f32());
        progress.push(message);
    }));
    let snapshot = Session::new(source, goals()).unwrap();
    let report = match &result {
        Ok(r) => {
            json!({"book":book.title,"model":model,"elapsed_seconds":started.elapsed().as_secs_f32(),"progress":progress,"metadata":r.metadata,"toc":r.toc.as_ref().map(|t|&t.entries),"toc_error":r.toc_error,"page_roles":r.page_roles,"warnings":r.warnings,"draft":snapshot.draft,"draft_path":snapshot.directory})
        }
        Err(error) => {
            json!({"book":book.title,"model":model,"elapsed_seconds":started.elapsed().as_secs_f32(),"progress":progress,"error":error,"draft":snapshot.draft,"draft_path":snapshot.directory})
        }
    };
    let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/pdf-agent-live/report.json");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    crate::persistence::write_json_atomic(&output, &report).unwrap();
    eprintln!(
        "LIVE report={} success={}",
        output.display(),
        result.is_ok()
    );
    assert!(result.is_ok(), "{}", result.err().unwrap_or_default());
}
