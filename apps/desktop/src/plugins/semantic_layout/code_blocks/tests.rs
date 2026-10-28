use super::*;
use crate::plugins::semantic_layout::tests::{section, text};

fn quote(lines: &[&str]) -> Block {
    let body: Vec<_> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let Block::Text(mut t) = text(&format!("line-{i}"), line) else {
                unreachable!()
            };
            t.kind = TextBlockKind::Blockquote;
            t.style.margin_start = if i == 0 { 16.0 } else { 32.0 };
            t
        })
        .collect();
    let source = Some(SourceRange {
        start: body[0].source.as_ref().unwrap().start.clone(),
        end: body.last().unwrap().source.as_ref().unwrap().end.clone(),
    });
    Block::Quote(QuoteBlock {
        body,
        attribution: None,
        source,
    })
}

fn proposal(presentation: Presentation, captions: Vec<usize>, language: Option<&str>) -> Proposal {
    Proposal::CodeOrQuote {
        block: 0,
        presentation,
        language: language.map(str::to_owned),
        captions,
        body_index: None,
    }
}

#[test]
fn complete_xml_container_is_offered_to_the_model_with_each_source_line() {
    let s = section(vec![quote(&[
        "<span",
        "id=\"page001\"",
        "epub:type=\"pagebreak\"",
        "role=\"doc-pagebreak\"",
        "aria-label=\"1\"/>",
    ])]);
    let input = unified_window_input(&s, &RecognitionRoles::default(), 0..1, 0..1, &[]);
    assert_eq!(input["targets"]["classify_code_or_quote"], json!([0]));
    assert_eq!(input["blocks"][0]["body"].as_array().unwrap().len(), 5);
    assert_eq!(
        input["blocks"][0]["body"][2]["text"],
        "epub:type=\"pagebreak\""
    );
    assert!(visible::needs_recognition(&s, 0..1));
    let options = wire::options(&completion_options(&RecognitionRoles::default()));
    assert!(options.to_string().contains("cqk"));
    assert_eq!(wire::decode(&wire::encode(&input)), input);
    assert!(PROMPT.contains("punctuation specimens"));
}

#[test]
fn code_classification_preserves_lines_sources_links_and_caption_association() {
    let mut s = section(vec![
        quote(&[
            "<metadata>",
            "<summary>中文 accessibility statement</summary>",
            "</metadata>",
        ]),
        text("caption", "Example 3. Accessibility metadata"),
        text("source", "Source: https://example.com/a"),
        text("after", "Unrelated discussion."),
    ]);
    let Block::Text(t) = &mut s.blocks[2] else {
        unreachable!()
    };
    let Inline::Text(run) = &mut t.content[0] else {
        unreachable!()
    };
    run.link = Some(PublicationUrl::website("https://example.com/a").unwrap());
    let original: Vec<_> = s
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .map(|t| (t.source.clone(), text_block_text(t)))
        .collect();
    let p = proposal(Presentation::Code, vec![1, 2], Some("xml"));
    validate_window(&[p.clone()], &s, &RecognitionRoles::default(), 0..4, 0..4).unwrap();
    let response = json!({"groups":[p],"citations":[],"formulas":[]});
    let compact = wire::encode(&response);
    jsonschema::validate(
        &wire::options(&completion_options(&RecognitionRoles::default()))["output_schema"],
        &compact,
    )
    .unwrap();
    let response: Response = serde_json::from_value(wire::decode(&compact)).unwrap();
    let a = annotation(&response.groups[0], &s);
    let groups = visible::recognition_groups(
        &s,
        0..4,
        Recognition {
            annotations: vec![a.clone()],
            ..visible::empty_recognition(&s)
        },
    );
    assert_eq!(groups[0].0, 0..3);
    super::super::compose(&mut s.blocks, &a);
    let Block::Figure(f) = &s.blocks[0] else {
        panic!("code with attached captions must be one group")
    };
    assert_eq!(f.body.len(), 5);
    for t in f.body[..3].iter().flat_map(Block::text_blocks) {
        assert_eq!(t.kind, TextBlockKind::Preformatted);
    }
    let Block::Text(first) = &f.body[0] else {
        unreachable!()
    };
    let Block::Text(second) = &f.body[1] else {
        unreachable!()
    };
    assert_eq!(first.style.margin_start, 0.0);
    assert_eq!(second.style.margin_start, 16.0);
    assert!(
        f.text_blocks()
            .flat_map(|t| &t.content)
            .any(|i| matches!(i, Inline::Text(r) if r.style.syntax_colors.is_some()))
    );
    assert!(f.text_blocks().all(|t| t.style.literal_links));
    assert!(
        f.text_blocks()
            .flat_map(|t| &t.content)
            .any(|i| matches!(i,Inline::Text(r) if r.link.is_some()))
    );
    let after: Vec<_> = s
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .map(|t| (t.source.clone(), text_block_text(t)))
        .collect();
    assert_eq!(after, original);
    assert_eq!(s.blocks.len(), 2);
}

#[test]
fn authored_label_before_and_source_after_stay_outside_the_body() {
    let mut s = section(vec![
        text("label", "Example 1. XML"),
        quote(&["<span/>"]),
        text("source", "Source: a specification"),
    ]);
    for i in [0, 2] {
        if let Block::Text(t) = &mut s.blocks[i] {
            t.kind = TextBlockKind::Caption;
        }
    }
    let p = Proposal::CodeOrQuote {
        block: 1,
        presentation: Presentation::Code,
        language: Some("xml".into()),
        captions: vec![0, 2],
        body_index: None,
    };
    let input = unified_window_input(&s, &RecognitionRoles::default(), 0..3, 0..3, &[]);
    assert_eq!(input["blocks"][0]["type"], "attribution_candidate");
    validate_window(&[p.clone()], &s, &RecognitionRoles::default(), 0..3, 0..3).unwrap();
    let a = annotation(&p, &s);
    super::super::compose(&mut s.blocks, &a);
    let Block::Figure(f) = &s.blocks[0] else {
        panic!()
    };
    assert_eq!(
        f.body
            .iter()
            .flat_map(Block::text_blocks)
            .map(|t| t.kind)
            .collect::<Vec<_>>(),
        vec![
            TextBlockKind::Caption,
            TextBlockKind::Preformatted,
            TextBlockKind::Caption
        ]
    );
}

#[test]
fn prose_keeps_quote_presentation_and_unsupported_syntax_keeps_plain_code() {
    let mut s = section(vec![quote(&[
        "An accessibility statement with an inline code term.",
    ])]);
    let p = proposal(Presentation::Quote, vec![], None);
    let a = annotation(&p, &s);
    super::super::compose(&mut s.blocks, &a);
    assert!(matches!(&s.blocks[0], Block::Quote(q) if q.body[0].kind == TextBlockKind::Blockquote));
    let mut s = section(vec![quote(&["Unrecognized illustrative syntax"])]);
    let a = annotation(
        &proposal(Presentation::Code, vec![], Some("unknown-language")),
        &s,
    );
    super::super::compose(&mut s.blocks, &a);
    assert!(
        s.blocks[0]
            .text_blocks()
            .all(|t| t.kind == TextBlockKind::Preformatted)
    );
    assert!(
        s.blocks[0]
            .text_blocks()
            .flat_map(|t| &t.content)
            .all(|i| matches!(i,Inline::Text(r) if r.style.syntax_colors.is_none()))
    );
}

#[test]
fn rejects_partial_overlapping_remote_or_invalid_classification() {
    let s = section(vec![
        quote(&["<metadata>", "value", "</metadata>"]),
        text("near", "Caption"),
        text("far", "Other"),
    ]);
    for p in [
        proposal(Presentation::Code, vec![2], Some("xml")),
        proposal(Presentation::Quote, vec![], Some("xml")),
        Proposal::CodeOrQuote {
            block: 1,
            presentation: Presentation::Code,
            language: None,
            captions: vec![],
            body_index: None,
        },
        Proposal::CodeOrQuote {
            block: 0,
            presentation: Presentation::Code,
            language: Some("xml".into()),
            captions: vec![],
            body_index: Some(2),
        },
        Proposal::CodeOrQuote {
            block: 0,
            presentation: Presentation::Quote,
            language: None,
            captions: vec![],
            body_index: Some(usize::MAX),
        },
    ] {
        assert!(validate_window(&[p], &s, &RecognitionRoles::default(), 0..3, 0..3).is_err());
    }
    let p = proposal(Presentation::Code, vec![], Some("xml"));
    assert!(
        validate_window(
            &[p.clone(), p],
            &s,
            &RecognitionRoles::default(),
            0..3,
            0..3
        )
        .is_err()
    );
    let mut window = WindowResult {
        groups: vec![proposal(Presentation::Code, vec![], Some("xml"))],
        formulas: vec![text_formulas::Proposal {
            block: 0,
            paragraph: 0,
            original: "x=1".into(),
            latex: "x=1".into(),
            before: String::new(),
            after: String::new(),
        }],
        ..Default::default()
    };
    protect(&mut window, &[]);
    assert!(window.formulas.is_empty());
}

#[test]
fn highlighting_preserves_unicode_indentation_breaks_and_multiline_state() {
    let mut body = body(&quote(&[
        "{\n  \"说明\": \"中文\",",
        "  \"enabled\": true\n}",
    ]))
    .unwrap()
    .to_vec();
    let original: Vec<_> = body.iter().map(text_block_text).collect();
    highlight::apply(&mut body, "json");
    assert_eq!(
        body.iter().map(text_block_text).collect::<Vec<_>>(),
        original
    );
    let colors: Vec<_> = body
        .iter()
        .flat_map(|t| &t.content)
        .filter_map(|i| match i {
            Inline::Text(r) => r.style.syntax_colors,
            _ => None,
        })
        .collect();
    assert!(colors.len() > 2);
    assert!(colors.windows(2).any(|pair| pair[0] != pair[1]));
}

#[test]
#[ignore = "requires TORTO_BARRIERS_BOOK local EPUB; optional TORTO_CODE_RENDER_DIR"]
fn local_barriers_authored_code_and_prose_keep_sources_after_ai_composition() {
    use anyrender::{ImageRenderer, PaintScene};
    use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle, ReaderTypesetting, SpreadMode};
    let opened = rebook_formats::open_file(std::env::var("TORTO_BARRIERS_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let mut count = 0;
    for (chapter, languages) in [
        ("chapter-11.xhtml", vec![None]),
        ("chapter-12.xhtml", vec![Some("xml"), Some("xml"), None]),
        ("chapter-13.xhtml", vec![Some("json")]),
    ] {
        let index = source
            .book()
            .sections
            .iter()
            .position(|s| s.href.path().ends_with(chapter))
            .unwrap();
        let section = source.parse_section(index).unwrap();
        let authored: Vec<_> = section
            .blocks
            .iter()
            .filter(|b| body(b).is_some())
            .collect();
        assert_eq!(authored.len(), languages.len());
        for (block, language) in authored.into_iter().zip(languages) {
            let original: Vec<_> = block
                .text_blocks()
                .map(|t| (t.source.clone(), text_block_text(t)))
                .collect();
            let mut fixture = section.clone();
            fixture.blocks = vec![block.clone()];
            let p = proposal(
                if language.is_some() {
                    Presentation::Code
                } else {
                    Presentation::Quote
                },
                vec![],
                language,
            );
            validate_window(
                &[p.clone()],
                &fixture,
                &RecognitionRoles::default(),
                0..1,
                0..1,
            )
            .unwrap();
            let a = annotation(&p, &fixture);
            super::super::compose(&mut fixture.blocks, &a);
            let actual: Vec<_> = fixture
                .blocks
                .iter()
                .flat_map(Block::text_blocks)
                .map(|t| (t.source.clone(), text_block_text(t)))
                .collect();
            assert_eq!(original, actual);
            if language.is_some() {
                assert!(
                    fixture
                        .blocks
                        .iter()
                        .flat_map(Block::text_blocks)
                        .all(|t| t.kind == TextBlockKind::Preformatted)
                );
                assert!(
                    fixture
                        .blocks
                        .iter()
                        .flat_map(Block::text_blocks)
                        .flat_map(|t| &t.content)
                        .any(|i| matches!(i,Inline::Text(r) if r.style.syntax_colors.is_some()))
                );
            } else {
                assert!(matches!(&fixture.blocks[0], Block::Quote(_)));
            }
            if let Some(dir) = std::env::var_os("TORTO_CODE_RENDER_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                for dark in [false, true] {
                    let mut engine = LayoutEngine::with_fonts(
                        crate::fonts::embedded_reader_fonts().iter().cloned(),
                    );
                    let mut style = ReaderStyle {
                        background: if dark {
                            rebook_publication::Rgba {
                                red: 24,
                                green: 27,
                                blue: 30,
                                alpha: 255,
                            }
                        } else {
                            ReaderStyle::default().background
                        },
                        typesetting: ReaderTypesetting::unified(),
                        spread: SpreadMode::Single,
                        foreground: if dark {
                            rebook_publication::Rgba {
                                red: 220,
                                green: 220,
                                blue: 220,
                                alpha: 255,
                            }
                        } else {
                            rebook_publication::Rgba::BLACK
                        },
                        ..Default::default()
                    };
                    engine
                        .available_reader_font_families()
                        .repair_typography(&mut style.typography);
                    let layout = engine
                        .layout_blocks(
                            source.as_ref(),
                            &fixture.blocks,
                            LayoutViewport::new(1000, 1000).unwrap(),
                            &style,
                        )
                        .unwrap();
                    let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
                    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(1000, 1000);
                    let mut pixels = Vec::new();
                    renderer.render_to_vec(
                        |scene| {
                            scene.fill(
                                peniko::Fill::NonZero,
                                kurbo::Affine::IDENTITY,
                                if dark {
                                    peniko::color::palette::css::BLACK
                                } else {
                                    peniko::color::palette::css::WHITE
                                },
                                None,
                                &kurbo::Rect::new(0.0, 0.0, 1000.0, 1000.0),
                            );
                            display.paint(scene);
                        },
                        &mut pixels,
                    );
                    image::RgbaImage::from_raw(1000, 1000, pixels)
                        .unwrap()
                        .save(std::path::Path::new(&dir).join(format!(
                            "case-{count}-{}.png",
                            if dark { "dark" } else { "light" }
                        )))
                        .unwrap();
                }
            }
            count += 1;
        }
    }
    assert_eq!(count, 5);
    println!(
        "Verified 3 complete XML/JSON code examples and 2 genuine prose/specimen quotes; no live model request"
    );
}
