use super::*;
use crate::plugins::semantic_layout::tests::{original_source, section, text};

fn pre(id: &str, value: &str) -> Block {
    let Block::Text(mut t) = text(id, value) else {
        unreachable!()
    };
    t.kind = TextBlockKind::Preformatted;
    Block::Text(t)
}
fn quotation(id: &str, value: &str) -> Block {
    let Block::Text(mut t) = text(id, value) else {
        unreachable!()
    };
    t.kind = TextBlockKind::Blockquote;
    Block::Quote(QuoteBlock {
        source: t.source.clone(),
        body: vec![t],
        attribution: None,
    })
}
fn group(body: Vec<usize>, presentation: Presentation, sources: Vec<usize>) -> Proposal {
    Proposal::CodeOrQuote {
        body,
        presentation,
        language: None,
        captions: vec![],
        sources,
        body_index: None,
    }
}
fn validate(p: &Proposal, s: &Section) -> Result<(), String> {
    validate_window(
        std::slice::from_ref(p),
        s,
        &RecognitionRoles::default(),
        0..s.blocks.len(),
        0..s.blocks.len(),
    )
}
fn retained(blocks: &[Block]) -> Vec<(Option<SourceRange>, String)> {
    blocks
        .iter()
        .flat_map(Block::text_blocks)
        .map(|t| (t.source.clone(), text_block_text(t)))
        .collect()
}

#[test]
fn grouped_response_uses_only_compact_keys_and_round_trips_the_schema() {
    let p = group(vec![0, 1], Presentation::Quote, vec![2]);
    let response = json!({"groups":[p],"citations":[],"formulas":[]});
    let compact = wire::encode(&response);
    assert_eq!(
        compact,
        json!({"g":[{"k":"cqk","d":[0,1],"pr":"q","lg":null,"ca":[],"so":[2],"bi":null}],"c":[],"f":[]})
    );
    let schema = wire::options(&completion_options(&RecognitionRoles::default()));
    jsonschema::validate(&schema["output_schema"], &compact).unwrap();
    let parsed: Response = serde_json::from_value(wire::decode(&compact)).unwrap();
    assert_eq!(
        serde_json::to_value(&parsed.groups).unwrap(),
        response["groups"]
    );
    let mut old = compact.clone();
    old["g"][0].as_object_mut().unwrap().remove("d");
    old["g"][0]["b"] = json!(0);
    assert!(jsonschema::validate(&schema["output_schema"], &old).is_err());
    let code = wire::encode(&json!({"presentation":"code"}));
    assert_eq!(code, json!({"pr":"cd"}));
}

#[test]
fn multiple_code_members_share_one_group_and_highlighting_state() {
    let mut s = section(vec![
        pre("open", "<style>"),
        pre("css", ".title { color: red; }"),
        pre("close", "</style>"),
        pre("html", "<h1 class=\"title\">Hello</h1>"),
    ]);
    let original = retained(&s.blocks);
    let p:Proposal=serde_json::from_value(json!({"kind":"code_or_quote","body":[0,1,2,3],"presentation":"code","language":"html","captions":[],"sources":[],"body_index":null})).unwrap();
    validate(&p, &s).unwrap();
    let a = annotation(&p, &s);
    let committed = visible::recognition_groups(
        &s,
        0..4,
        Recognition {
            annotations: vec![a.clone()],
            ..visible::empty_recognition(&s)
        },
    );
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].0, 0..4);
    super::super::compose(&mut s.blocks, &a);
    assert_eq!(s.blocks.len(), 1);
    let Block::Figure(f) = &s.blocks[0] else {
        panic!()
    };
    assert_eq!(f.body.len(), 4);
    assert!(
        f.text_blocks()
            .all(|t| t.kind == TextBlockKind::Preformatted)
    );
    assert!(
        f.text_blocks()
            .flat_map(|t| &t.content)
            .any(|i| matches!(i,Inline::Text(r) if r.style.syntax_colors.is_some()))
    );
    assert_eq!(retained(&s.blocks), original);
}

#[test]
fn multiple_quote_members_and_quote_encoded_credit_preserve_search_anchors() {
    use rebook_layout::{LayoutViewport, ReaderStyle, ReaderTypesetting, SpreadMode};
    use rebook_reader::ReaderSession;
    let mut s = section(vec![
        quotation("first", "The first quoted paragraph."),
        quotation("second", "The second quoted paragraph."),
        quotation("credit", "— Mira Vale"),
    ]);
    let original = retained(&s.blocks);
    let p = group(vec![0, 1], Presentation::Quote, vec![2]);
    validate(&p, &s).unwrap();
    let a = annotation(&p, &s);
    super::super::compose(&mut s.blocks, &a);
    let Block::Quote(q) = &s.blocks[0] else {
        panic!()
    };
    assert_eq!(q.body.len(), 2);
    assert_eq!(
        text_block_text(q.attribution.as_ref().unwrap()),
        "— Mira Vale"
    );
    assert_eq!(retained(&s.blocks), original);
    let mut reader = ReaderSession::open_with_fonts(
        original_source(s),
        LayoutViewport::new(1000, 700).unwrap(),
        ReaderStyle {
            spread: SpreadMode::Single,
            typesetting: ReaderTypesetting::unified(),
            ..Default::default()
        },
        crate::fonts::embedded_reader_fonts(),
    )
    .unwrap();
    for (range, _) in original {
        let anchor = range.unwrap().start;
        reader.go_to_source(&anchor).unwrap();
        assert!(reader.current_page().contains_source_anchor(&anchor));
    }
}

#[test]
fn sources_and_body_bilingual_companions_keep_their_order_and_roles() {
    let mut a = quotation("a", "An excerpt.");
    let Block::Quote(q) = &mut a else { panic!() };
    let mut companion = q.body[0].clone();
    companion.source = None;
    if let Inline::Text(r) = &mut companion.content[0] {
        r.text = "译文".into();
    }
    q.body.push(companion);
    let mut credit = quotation("credit", "— Mira Vale");
    let Block::Quote(q) = &mut credit else {
        panic!()
    };
    let mut companion = q.body[0].clone();
    companion.source = None;
    q.body.push(companion);
    let mut s = section(vec![a, credit]);
    let original = retained(&s.blocks);
    let p = group(vec![0], Presentation::Quote, vec![1]);
    validate(&p, &s).unwrap();
    let ann = annotation(&p, &s);
    super::super::compose(&mut s.blocks, &ann);
    assert_eq!(retained(&s.blocks), original);
    let Block::Figure(f) = &s.blocks[0] else {
        panic!()
    };
    let Block::Quote(q) = &f.body[0] else {
        panic!()
    };
    assert_eq!(q.body.len(), 2);
    assert!(
        f.body[1..]
            .iter()
            .flat_map(Block::text_blocks)
            .all(|t| t.kind == TextBlockKind::QuoteAttribution)
    );
}

#[test]
fn grouping_cannot_cross_narrative_headings_duplicate_members_or_explicit_credits() {
    let s = section(vec![
        quotation("a", "First"),
        text("narrative", "An explanation"),
        quotation("b", "Second"),
    ]);
    assert!(validate(&group(vec![0, 2], Presentation::Quote, vec![]), &s).is_err());
    assert!(validate(&group(vec![0, 0], Presentation::Quote, vec![]), &s).is_err());
    assert!(validate(&group(vec![0], Presentation::Quote, vec![0]), &s).is_err());
    let mut heading = s.clone();
    if let Block::Text(t) = &mut heading.blocks[1] {
        t.kind = TextBlockKind::Heading(2);
    }
    assert!(validate(&group(vec![0], Presentation::Quote, vec![1]), &heading).is_err());
    let mut a = quotation("a", "First credited quotation.");
    let Block::Quote(q) = &mut a else { panic!() };
    let Block::Text(t) = text("credit", "— First author") else {
        panic!()
    };
    q.attribution = Some(t);
    let s = section(vec![a, quotation("b", "Independent second quotation.")]);
    assert!(validate(&group(vec![0, 1], Presentation::Quote, vec![]), &s).is_err());
    let p1 = group(vec![0], Presentation::Quote, vec![]);
    let p2 = group(vec![1], Presentation::Quote, vec![]);
    validate_window(&[p1, p2], &s, &RecognitionRoles::default(), 0..2, 0..2).unwrap();
}

#[test]
fn candidate_runs_stay_in_one_batch_and_oversized_runs_are_not_partially_classified() {
    let s = section(vec![
        text("before", &"x".repeat(4900)),
        pre("a", &"a".repeat(3000)),
        pre("b", &"b".repeat(3000)),
        text("after", "Narrative"),
    ]);
    assert_eq!(batching::fixed_batches(&s, 0..4), vec![0..1, 1..3, 3..4]);
    let input = unified_window_input(&s, &RecognitionRoles::default(), 1..3, 0..4, &[]);
    assert_eq!(input["container_runs"], json!([[1, 2]]));
    assert_eq!(wire::encode(&input)["cg"], json!([[1, 2]]));
    let huge = section(vec![
        pre("a", &"a".repeat(MAX_RUN_CHARS)),
        pre("b", "continuation"),
    ]);
    assert!(eligible_ids(&huge).is_empty());
    assert!(!visible::needs_recognition(&huge, 0..2));
    assert!(validate(&group(vec![0], Presentation::Code, vec![]), &huge).is_err());
}

#[test]
fn code_protection_applies_to_every_merged_member() {
    let mut window = WindowResult {
        groups: vec![group(vec![0, 1], Presentation::Code, vec![])],
        formulas: (0..2)
            .map(|block| text_formulas::Proposal {
                block,
                paragraph: 0,
                original: "x=1".into(),
                latex: "x=1".into(),
                before: String::new(),
                after: String::new(),
            })
            .collect(),
        ..Default::default()
    };
    protect(&mut window, &[]);
    assert!(window.formulas.is_empty());
}

#[test]
fn compact_multi_container_response_is_recognized_and_committed_in_one_request() {
    use std::io::{BufRead, Read, Write};
    use std::time::{Duration, Instant};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let started = Instant::now();
        let mut socket = loop {
            if let Ok((s, _)) = listener.accept() {
                break s;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
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
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; len];
        reader.read_exact(&mut bytes).unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let message = &request["messages"][1]["content"];
        let text = message.as_str().map(str::to_owned).unwrap_or_else(|| {
            message
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect()
        });
        let input: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(input["cg"], json!([[0, 1, 2]]));
        assert!(input.get("container_runs").is_none());
        assert!(request.to_string().contains("sources")); // schema descriptions explain so
        let response = json!({"g":[{"k":"cqk","d":[0,1,2],"pr":"cd","lg":"css","ca":[],"so":[],"bi":null}],"c":[],"f":[]});
        let body=json!({"choices":[{"index":0,"finish_reason":"stop","message":{"content":response.to_string()}}]}).to_string();
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
    });
    let s = section(vec![
        pre("a", "body {"),
        pre("b", "  direction: rtl;"),
        pre("c", "}"),
    ]);
    let mut settings = PluginSettings::default().with_test_model();
    settings.providers[0].kind = crate::plugins::AiProviderKind::OpenAi;
    settings.providers[0].base_url = format!("http://{address}/v1");
    settings.providers[0].api_key = "fixture".into();
    settings.semantic_layout.enabled = true;
    settings.semantic_layout.provider = settings.providers[0].id.clone();
    settings.semantic_layout.model = settings.providers[0].models[0].id.clone();
    let book_id = format!("joint-groups-{}", uuid::Uuid::new_v4());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime
        .block_on(recognize_visible(
            &s,
            0..3,
            &book_id,
            &settings,
            original_source(s.clone()).as_ref(),
        ))
        .unwrap();
    server.join().unwrap();
    // The listener is closed: a second successful recognition must use the
    // persisted multi-member result rather than issuing another model call.
    let cached = runtime
        .block_on(recognize_visible(
            &s,
            0..3,
            &book_id,
            &settings,
            original_source(s.clone()).as_ref(),
        ))
        .unwrap();
    assert_eq!(cached.annotations, result.annotations);
    assert!(result.annotations.iter().any(|a|matches!(a,Annotation::CodeOrQuote{body,presentation:Presentation::Code,..} if body.len()==3)));
    let committed = visible::recognition_groups(&s, 0..3, result);
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].0, 0..3);
}

#[test]
#[ignore = "requires TORTO_ACCESSIBILITY_BOOK local EPUB; optional TORTO_GROUP_RENDER_DIR; no online model"]
fn local_accessibility_adjacent_code_and_quote_groups_keep_all_sources() {
    use anyrender::ImageRenderer;
    use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle, ReaderTypesetting, SpreadMode};
    let opened =
        rebook_formats::open_file(std::env::var("TORTO_ACCESSIBILITY_BOOK").unwrap()).unwrap();
    let source = opened.source();
    // Source indexes are asserted against this edition to catch parser changes.
    let cases = [
        ("-6.xhtml", 9, 1, 1, Presentation::Quote, None),
        ("-6.xhtml", 124, 3, 0, Presentation::Code, Some("css")),
        ("-8.xhtml", 219, 2, 0, Presentation::Quote, None),
        ("-8.xhtml", 222, 4, 0, Presentation::Quote, None),
        ("-8.xhtml", 242, 13, 0, Presentation::Code, None),
        ("-9.xhtml", 107, 5, 0, Presentation::Code, Some("html")),
        ("-9.xhtml", 141, 14, 0, Presentation::Code, Some("html")),
        ("-11.xhtml", 17, 2, 1, Presentation::Quote, None),
    ];
    for (case, (suffix, start, count, credits, presentation, language)) in
        cases.into_iter().enumerate()
    {
        let si = source
            .book()
            .sections
            .iter()
            .position(|s| s.href.path().ends_with(suffix))
            .unwrap();
        let mut s = source.parse_section(si).unwrap();
        s.blocks = s.blocks[start..start + count + credits].to_vec();
        let original = retained(&s.blocks);
        let p:Proposal=serde_json::from_value(json!({"kind":"code_or_quote","body":(0..count).collect::<Vec<_>>(),
            "presentation":presentation,"language":language,"captions":[],"sources":(count..count+credits).collect::<Vec<_>>(),"body_index":null})).unwrap();
        validate(&p, &s).unwrap();
        let input = unified_window_input(
            &s,
            &RecognitionRoles::default(),
            0..s.blocks.len(),
            0..s.blocks.len(),
            &[],
        );
        assert_eq!(
            input["container_runs"][0].as_array().unwrap().len(),
            count + credits
        );
        let a = annotation(&p, &s);
        super::super::compose(&mut s.blocks, &a);
        assert_eq!(retained(&s.blocks), original, "case {case}");
        assert_eq!(s.blocks.len(), 1);
        if presentation == Presentation::Quote {
            assert!(
                matches!(&s.blocks[0],Block::Quote(q) if q.body.len()==count && q.attribution.is_some()==(credits>0))
            );
        } else {
            assert!(
                s.blocks[0]
                    .text_blocks()
                    .all(|t| t.kind == TextBlockKind::Preformatted)
            );
        }
        let mut engine =
            LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned());
        let mut style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            spread: SpreadMode::Single,
            ..Default::default()
        };
        engine
            .available_reader_font_families()
            .repair_typography(&mut style.typography);
        let layout = engine
            .layout_blocks(
                source.as_ref(),
                &s.blocks,
                LayoutViewport::new(1100, 1000).unwrap(),
                &style,
            )
            .unwrap();
        let displays: Vec<_> = layout
            .pages
            .iter()
            .map(|p| rebook_renderer::DisplayListCompiler.compile(p))
            .collect();
        for (range, _) in &original {
            if let Some(range) = range {
                assert!(
                    displays
                        .iter()
                        .any(|d| d.contains_source_anchor(&range.start)),
                    "case {case}: {}",
                    range.start.node
                );
            }
        }
        if let Some(dir) = std::env::var_os("TORTO_GROUP_RENDER_DIR") {
            std::fs::create_dir_all(&dir).unwrap();
            let mut pixels = Vec::new();
            let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(1100, 1000);
            renderer.render_to_vec(|scene| displays[0].paint(scene), &mut pixels);
            image::RgbaImage::from_raw(1100, 1000, pixels)
                .unwrap()
                .save(std::path::Path::new(&dir).join(format!("joint-{case}.png")))
                .unwrap();
        }
    }
    println!(
        "Verified 8 real-book groups: WHO, RNIB, dialogue, stage directions, CSS, HTML, embedded CSS and WEBVTT"
    );
}
