use super::*;
use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};
use rebook_publication::{
    ImageBlock, Metadata, PublicationId, SourceAnchor, SpineItem, SpineItemId, TextRun,
};

pub(super) fn text(id: &str, content: &str) -> Block {
    let start = SourceAnchor {
        spine: SpineItemId::new("chapter").unwrap(),
        node: id.into(),
        text_offset: 0,
    };
    let end = SourceAnchor {
        text_offset: content.chars().count() as u64,
        ..start.clone()
    };
    Block::Text(TextBlock {
        kind: TextBlockKind::Paragraph,
        content: vec![Inline::Text(TextRun {
            text: content.into(),
            style: rebook_publication::TextStyle::default(),
            link: None,
        })],
        style: rebook_publication::BlockStyle::default(),
        source: Some(SourceRange { start, end }),
    })
}

pub(super) fn image(id: &str) -> Block {
    Block::Image(ImageBlock {
        formula_image: false,
        formula: None,
        href: PublicationUrl::parse("image.png").unwrap(),
        alt: String::new(),
        style: rebook_publication::ImageStyle::default(),
        source: source(&text(id, "image")).cloned(),
        text_layer: None,
    })
}

pub(super) fn section(blocks: Vec<Block>) -> Section {
    Section {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        blocks,
        anchors: vec![],
    }
}

fn validate(groups: &[Proposal], section: &Section) -> Result<(), String> {
    validate_window(
        groups,
        section,
        &RecognitionRoles::default(),
        0..section.blocks.len(),
        0..section.blocks.len(),
    )
}

#[test]
fn protected_semantics_are_not_classification_targets() {
    let mut quote = text("q", "Previously recognized quotation");
    if let Block::Text(text) = &mut quote {
        text.kind = TextBlockKind::Blockquote;
    }
    assert_eq!(
        input_block(0, &quote),
        json!({"id":0,"type":"protected_boundary"})
    );
    let section = section(vec![quote]);
    assert!(
        validate(
            &[Proposal::Quote {
                alignment: None,
                body: vec![0],
                attribution: None
            }],
            &section
        )
        .is_err()
    );
    let figure = Block::Figure(FigureBlock {
        images: vec![],
        captions: vec![],
        caption_position: CaptionPosition::default(),
        style: rebook_publication::BlockStyle::default(),
        source: None,
    });
    assert_eq!(input_block(1, &figure)["type"], "protected_boundary");
}

#[test]
fn recognized_standalone_caption_protects_its_adjacent_images_and_cached_proposals() {
    for before in [false, true] {
        let mut caption = text("caption", "Figure 4.3. Existing caption");
        if let Block::Text(text) = &mut caption {
            text.kind = TextBlockKind::Caption;
        }
        let blocks = if before {
            vec![
                caption,
                image("one"),
                image("two"),
                text("body", "Narrative"),
            ]
        } else {
            vec![
                text("body", "Narrative mentioning the figure"),
                image("one"),
                image("two"),
                caption,
            ]
        };
        let section = section(blocks);
        assert_eq!(
            section_input_block(&section, 1)["type"],
            "protected_boundary"
        );
        assert_eq!(
            section_input_block(&section, 2)["type"],
            "protected_boundary"
        );
        assert!(
            validate(
                &[Proposal::Figure {
                    images: vec![1, 2],
                    captions: vec![if before { 3 } else { 0 }]
                }],
                &section
            )
            .is_err()
        );
    }
}

#[test]
fn one_invalid_caption_does_not_discard_a_valid_quote_or_relax_validation() {
    let section = section(vec![
        text("q", "Epigraph"),
        text("a", "Author"),
        image("i"),
        text("p", "Ordinary prose"),
        text("c", "Caption"),
    ]);
    let proposals = vec![
        Proposal::Quote {
            alignment: None,
            body: vec![0],
            attribution: Some(1),
        },
        Proposal::Figure {
            images: vec![2],
            captions: vec![4],
        },
        Proposal::Quote {
            alignment: None,
            body: vec![0],
            attribution: Some(1),
        },
    ];
    let safe = retain_valid_groups(
        &proposals,
        &section,
        &RecognitionRoles::default(),
        0..5,
        0..5,
    );
    assert_eq!(safe.groups.len(), 1);
    assert_eq!(safe.skipped_groups, 2);
    validate(&safe.groups, &section).unwrap();
}

#[test]
fn rejects_hallucinated_overlapping_discontinuous_and_reversed_groups() {
    let section = section(vec![
        text("a", "First"),
        text("b", "Second"),
        text("c", "Credit"),
    ]);
    for body in [vec![], vec![99], vec![0, 2], vec![1, 0], vec![0, 0]] {
        assert!(
            validate(
                &[Proposal::Quote {
                    body,
                    attribution: None,
                    alignment: None,
                }],
                &section
            )
            .is_err()
        );
    }
    assert!(
        validate(
            &[
                Proposal::Quote {
                    alignment: None,
                    body: vec![0, 1],
                    attribution: Some(2)
                },
                Proposal::Quote {
                    alignment: None,
                    body: vec![1],
                    attribution: None
                }
            ],
            &section
        )
        .is_err()
    );
    assert!(
        validate(
            &[Proposal::Quote {
                alignment: None,
                body: vec![1],
                attribution: Some(0)
            }],
            &section
        )
        .is_err()
    );
}

#[test]
fn captions_require_adjacent_images_and_respect_disabled_types() {
    let section = section(vec![
        image("i"),
        text("c", "Figure 1. A diagram."),
        text("p", "Narrative"),
    ]);
    let good = Proposal::Figure {
        images: vec![0],
        captions: vec![1],
    };
    assert!(validate(std::slice::from_ref(&good), &section).is_ok());
    assert!(
        validate(
            &[Proposal::Figure {
                images: vec![0],
                captions: vec![2]
            }],
            &section
        )
        .is_err()
    );
    assert!(
        validate_window(
            &[good],
            &section,
            &RecognitionRoles {
                captions: false,
                ..Default::default()
            },
            0..3,
            0..3
        )
        .is_err()
    );
}

#[test]
fn quote_composition_preserves_sources_and_bilingual_attribution() {
    let mut original = section(vec![
        text("a", "Quotation"),
        text("b", "Second paragraph"),
        text("c", "Author"),
        text("p", "Narrative"),
    ]);
    for block in &mut original.blocks {
        if let Block::Text(text) = block {
            for inline in &mut text.content {
                if let Inline::Text(run) = inline {
                    run.style.keyword_size_scale = Some(0.75);
                }
            }
        }
    }
    let group = Proposal::Quote {
        alignment: None,
        body: vec![0, 1],
        attribution: Some(2),
    };
    let annotation = annotation(&group, &original);
    let mut blocks = Vec::new();
    for block in &original.blocks {
        blocks.push(block.clone());
        if let Block::Text(mut translated) = block.clone() {
            translated.source = None;
            translated.content = vec![Inline::Text(TextRun {
                text: "翻译".into(),
                style: rebook_publication::TextStyle {
                    keyword_size_scale: Some(0.75),
                    ..Default::default()
                },
                link: None,
            })];
            blocks.push(Block::Text(translated));
        }
    }
    compose(&mut blocks, &annotation);
    let Block::Quote(quote) = &blocks[0] else {
        panic!("quote missing");
    };
    assert_eq!(quote.body.len(), 4);
    assert_eq!(quote.body[0].source.as_ref(), source(&original.blocks[0]));
    let credit = quote.attribution.as_ref().unwrap();
    assert!(
        quote
            .body
            .iter()
            .chain(std::iter::once(credit))
            .all(|text| text.content.iter().all(|inline| match inline {
                Inline::Text(run) => run.style.keyword_size_scale == Some(0.75),
                _ => true,
            }))
    );
    assert_eq!(credit.source.as_ref(), source(&original.blocks[2]));
    assert!(text_block_text(credit).contains("翻译"));
    assert_eq!(blocks.len(), 3); // untouched narrative and companion
}

#[test]
fn figure_composition_preserves_order_and_all_images() {
    for before in [true, false] {
        let blocks = if before {
            vec![text("c", "Caption"), image("a"), image("b")]
        } else {
            vec![image("a"), image("b"), text("c", "Caption")]
        };
        let original = section(blocks);
        let group = if before {
            Proposal::Figure {
                images: vec![1, 2],
                captions: vec![0],
            }
        } else {
            Proposal::Figure {
                images: vec![0, 1],
                captions: vec![2],
            }
        };
        validate(std::slice::from_ref(&group), &original).unwrap();
        let mut blocks = original.blocks.clone();
        compose(&mut blocks, &annotation(&group, &original));
        let Block::Figure(figure) = &blocks[0] else {
            panic!("figure missing");
        };
        assert_eq!(figure.images.len(), 2);
        assert_eq!(figure.captions[0].kind, TextBlockKind::Caption);
        assert_eq!(
            figure.caption_position,
            if before {
                CaptionPosition::Before
            } else {
                CaptionPosition::After
            }
        );
    }
}

struct TestSource {
    book: Book,
    section: Section,
}
impl BookSource for TestSource {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        Ok(self.section.clone())
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Err(PublicationError::ResourceNotFound(href.to_string()))
    }
}

pub(super) fn original_source(section: Section) -> Arc<dyn BookSource> {
    Arc::new(TestSource {
        book: Book {
            id: PublicationId::new("semantic-test").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![SpineItem {
                id: section.id.clone(),
                href: section.href.clone(),
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: vec![],
            }],
            table_of_contents: vec![],
        },
        section,
    })
}

#[test]
fn translation_indices_survive_semantic_toggle_and_changed_content_is_rejected() {
    let section = section(vec![
        text("a", "Quotation"),
        text("c", "Credit"),
        text("n", "Narrative"),
    ]);
    let original = original_source(section.clone());
    let translations = Arc::new(TranslationBookSource::new(
        original.clone(),
        TranslationMode::Bilingual,
    ));
    translations.set_enabled(true).unwrap();
    translations
        .store_batch(
            0,
            &[BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "引用翻译".into(),
            }],
        )
        .unwrap();
    let overlay = SemanticLayoutSource::new(translations.clone(), original);
    let recognition = Recognition {
        formulas_checked: true,
        fingerprint: fingerprint(&section),
        skipped_groups: 0,
        annotations: vec![annotation(
            &Proposal::Quote {
                alignment: None,
                body: vec![0],
                attribution: Some(1),
            },
            &section,
        )],
    };
    assert!(overlay.install(0, recognition.clone()));
    let composed = overlay.parse_section(0).unwrap();
    assert!(matches!(&composed.blocks[0],Block::Quote(q) if q.body.len()==2));
    overlay.clear();
    assert_eq!(
        overlay.parse_section(0).unwrap(),
        translations.parse_section(0).unwrap()
    );
    assert!(!overlay.install(
        0,
        Recognition {
            formulas_checked: true,
            fingerprint: "stale".into(),
            ..recognition
        }
    ));
}

#[test]
fn scoped_layout_and_translation_commute_without_completing_the_whole_chapter() {
    for mode in [TranslationMode::Replace, TranslationMode::Bilingual] {
        let mut results = Vec::new();
        for translate_first in [false, true] {
            let section = section(vec![
                text("a", "Quotation"),
                text("c", "Credit"),
                text("n", "Narrative"),
            ]);
            let original = original_source(section.clone());
            let translations = Arc::new(TranslationBookSource::new(original.clone(), mode));
            translations.set_enabled(true).unwrap();
            let overlay = SemanticLayoutSource::new(translations.clone(), original);
            let translate = || {
                translations
                    .store_batch(
                        0,
                        &[BlockTranslation {
                            block_index: 0,
                            segment_index: None,
                            text: "Translated quotation".into(),
                        }],
                    )
                    .unwrap()
            };
            if translate_first {
                translate();
            }
            let scoped = scope_section(&section, 0..2);
            let result = Recognition {
                formulas_checked: true,
                fingerprint: fingerprint(&scoped),
                skipped_groups: 0,
                annotations: vec![annotation(
                    &Proposal::Quote {
                        alignment: None,
                        body: vec![0],
                        attribution: Some(1),
                    },
                    &scoped,
                )],
            };
            assert!(!overlay.install_scope(0, "stale", 0..2, result.clone()));
            assert!(overlay.install_scope(0, &fingerprint(&section), 0..2, result));
            assert!(!overlay.has_recognition(0, &fingerprint(&section)));
            if !translate_first {
                translate();
            }
            let composed = overlay.parse_section(0).unwrap();
            assert!(
                matches!(&composed.blocks[0], Block::Quote(q) if q.body.len() == if mode == TranslationMode::Bilingual { 2 } else { 1 })
            );
            assert_eq!(composed.blocks.last(), section.blocks.last());
            let remainder = scope_section(&section, 2..3);
            overlay.install_scope(
                0,
                &fingerprint(&section),
                2..3,
                Recognition {
                    formulas_checked: true,
                    fingerprint: fingerprint(&remainder),
                    skipped_groups: 0,
                    annotations: vec![],
                },
            );
            assert_eq!(overlay.parse_section(0).unwrap(), composed);
            // A previously cached whole chapter must not mask fresh subsection
            // results after its original text has changed.
            overlay.state.write().unwrap().insert(
                0,
                Recognition {
                    formulas_checked: true,
                    fingerprint: "old-content".into(),
                    skipped_groups: 0,
                    annotations: vec![],
                },
            );
            assert_eq!(overlay.parse_section(0).unwrap(), composed);
            results.push(composed);
            overlay.clear();
            assert_eq!(
                overlay.parse_section(0).unwrap(),
                translations.parse_section(0).unwrap()
            );
        }
        assert_eq!(results[0], results[1]);
    }
}

#[test]
fn visible_targets_accept_related_credit_but_ignore_context_only_changes() {
    let original = section(vec![
        text("credit", "Mira Vale wrote:"),
        text("quote", "Words to remember."),
        text("other", "Unrelated context."),
    ]);
    let proposal = Proposal::QuoteBefore {
        body: vec![1],
        attribution: 0,
        alignment: None,
    };
    validate_window(
        std::slice::from_ref(&proposal),
        &original,
        &RecognitionRoles::default(),
        1..2,
        0..3,
    )
    .unwrap();
    assert!(
        validate_window(
            std::slice::from_ref(&proposal),
            &original,
            &RecognitionRoles::default(),
            2..3,
            0..3
        )
        .is_err()
    );
    let mut result = empty_recognition(&original);
    result.annotations = vec![
        annotation(&proposal, &original),
        Annotation::SectionHeading {
            source: source(&original.blocks[2]).unwrap().clone(),
        },
    ];
    let groups = recognition_groups(&original, 1..2, result);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].0, 0..2);
    assert_eq!(groups[0].1.annotations.len(), 1);
}

#[test]
fn independent_visible_paragraphs_get_independent_commit_groups() {
    let original = section(vec![
        text("a", "First"),
        text("b", "Second"),
        text("c", "Offscreen"),
    ]);
    let groups = recognition_groups(&original, 0..2, empty_recognition(&original));
    assert_eq!(
        groups
            .iter()
            .map(|group| group.0.clone())
            .collect::<Vec<_>>(),
        vec![0..1, 1..2]
    );
}

#[test]
fn visible_request_sends_context_without_targeting_the_next_paragraph() {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut socket = loop {
            if let Ok((socket, _)) = listener.accept() {
                break socket;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        };
        socket.set_nonblocking(false).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(&mut socket);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let input: Value =
            serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["target_start"], 1);
        assert_eq!(input["target_end_exclusive"], 2);
        assert_eq!(input["blocks"].as_array().unwrap().len(), 3);
        let body = json!({"choices":[{"message":{"content":"{\"groups\":[]}"}}]}).to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    let original = section(vec![
        text("a", "Before context."),
        text("b", "Visible paragraph."),
        text("c", "After context."),
    ]);
    let mut settings = PluginSettings::default();
    settings.semantic_layout.enabled = true;
    settings.semantic_layout.provider = settings.providers[0].id.clone();
    settings.semantic_layout.model = settings.providers[0].models[0].id.clone();
    settings.providers[0].api_key = "fixture".into();
    settings.providers[0].base_url = format!("http://{address}/v1");
    let source = original_source(original.clone());
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(recognize_visible(
            &original,
            1..2,
            "visible-only-test",
            &settings,
            source.as_ref(),
        ))
        .unwrap();
    assert!(result.annotations.is_empty());
    server.join().unwrap();
}

#[test]
fn settings_migrate_disabled_and_missing_selection_is_not_replaced() {
    let mut settings: PluginSettings = serde_json::from_value(json!({})).unwrap();
    assert!(!settings.semantic_layout.enabled);
    settings.semantic_layout.provider = "deleted".into();
    settings.semantic_layout.model = "missing".into();
    settings.normalize();
    assert_eq!(settings.semantic_layout.provider, "deleted");
    assert!(settings.semantic_layout_endpoint().is_err());
    let roundtrip: PluginSettings =
        serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
    assert_eq!(roundtrip.semantic_layout, settings.semantic_layout);
    let legacy: SemanticLayoutSettings =
        serde_json::from_value(json!({"enabled":true,"quotes":false,"captions":false})).unwrap();
    assert!(legacy.enabled);
    let serialized = serde_json::to_value(legacy).unwrap();
    assert!(serialized.get("quotes").is_none() && serialized.get("captions").is_none());
}

#[test]
#[ignore = "requires TORTO_SEMANTIC_BOOK; diagnoses a full chapter using the current configured model"]
fn live_semantic_layout_full_chapter() {
    let path = std::env::var("TORTO_SEMANTIC_BOOK").expect("set book path");
    let index: usize = std::env::var("TORTO_SEMANTIC_SECTION")
        .unwrap_or_else(|_| "6".into())
        .parse()
        .unwrap();
    let opened = rebook_formats::open_file(path).unwrap();
    let source = opened.source();
    let section = source.parse_section(index).unwrap();
    let settings = PluginSettings::load_default().unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(recognize_with_source(
            &section,
            source.book().id.as_str(),
            &settings,
            source.as_ref(),
        ));
    println!(
        "section {index}, {} blocks: {}",
        section.blocks.len(),
        result
            .as_ref()
            .map_or_else(|e| e.clone(), |r| format!("{} groups", r.annotations.len()))
    );
    assert!(
        result.is_ok(),
        "inspect semantic-layout.log for window and failure details"
    );
}

#[test]
#[ignore = "requires TORTO_SEMANTIC_BOOK pointing to local The Hand; no network requests"]
fn local_hand_reflow_geometry() {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    let opened = rebook_formats::open_file(std::env::var("TORTO_SEMANTIC_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().contains("_c04_"))
        .unwrap();
    let original = source.parse_section(index).unwrap();
    let target = original
        .blocks
        .iter()
        .position(|b| {
            paragraph(b)
                .is_some_and(|t| text_block_text(t).starts_with("The obligatory lengthening"))
        })
        .unwrap();
    println!("target paragraph={target}, image={}", target + 1);
    assert!(
        !image_needs_caption(&original, target + 1),
        "existing Figure 4.3 caption must protect the image"
    );
    let mut augmented = original.clone();
    assert!(matches!(original.blocks[target + 1], Block::Image(_)));
    compose(
        &mut augmented.blocks,
        &annotation(
            &Proposal::Figure {
                images: vec![target + 1],
                captions: vec![target],
            },
            &original,
        ),
    );
    let mut engine =
        LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned());
    let mut style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        spread: SpreadMode::Scroll,
        writing_system: source.book().metadata.writing_system(),
        horizontal_margin: 40.0,
        top_margin: 0.0,
        bottom_margin: 0.0,
        ..ReaderStyle::default()
    };
    style.typography = crate::preferences::load_reader_preferences()
        .unwrap()
        .typography;
    for width in [800, 880, 1000] {
        for (name, section) in [
            ("original", &original),
            ("augmented", &augmented),
            ("restored", &original),
        ] {
            let layout = engine
                .layout_section(
                    source.as_ref(),
                    section,
                    LayoutViewport::new(width, 700).unwrap(),
                    &style,
                )
                .unwrap();
            for (page_index, page) in layout.pages.iter().enumerate() {
                let display = rebook_renderer::DisplayListCompiler.compile(page);
                for (item_index, item) in page.items.iter().enumerate() {
                    if let PageItem::Image(image) = item {
                        let prior = page.items[..item_index]
                            .iter()
                            .filter_map(|item| match item {
                                PageItem::Text(text) if text.source.is_some() => Some(text),
                                _ => None,
                            })
                            .last();
                        if let Some(text) = prior {
                            let rects = display
                                .source_rects(std::slice::from_ref(text.source.as_ref().unwrap()));
                            let bottom =
                                rects.iter().map(|r| r.y1).fold(f64::NEG_INFINITY, f64::max);
                            assert!(
                                bottom <= f64::from(image.y) + 0.1,
                                "{name} width={width} page={page_index} text bottom={bottom} image y={}",
                                image.y
                            );
                        }
                    }
                }
            }
            println!("{width} {name}: {} pages checked", layout.pages.len());
        }
    }
}

#[test]
fn cached_negative_results_load_without_a_request_and_content_changes_miss() {
    let section = section(vec![text("a", "Ordinary narrative")]);
    let identity = json!(["test", uuid::Uuid::new_v4().to_string()]);
    let hash = fingerprint(&section);
    let path = recognition_path(&identity, &hash).unwrap();
    let result = Recognition {
        formulas_checked: true,
        fingerprint: hash.clone(),
        skipped_groups: 0,
        annotations: vec![],
    };
    crate::persistence::write_json_atomic(&path, &result).unwrap();
    let original = original_source(section.clone());
    let overlay = SemanticLayoutSource::new(original.clone(), original);
    *overlay.cache_identity.write().unwrap() = Some(identity.clone());
    assert_eq!(overlay.parse_section(0).unwrap(), section);
    assert!(overlay.has_recognition(0, &hash));
    let mut edited = section.clone();
    edited.blocks.push(text("b", "New content"));
    assert!(load_recognition(&edited, &identity).is_none());
    assert!(load_recognition(&section, &json!(["different-model"])).is_none());
    std::fs::write(&path, b"invalid json").unwrap();
    assert!(load_recognition(&section, &identity).is_none());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn window_ownership_prevents_duplicate_groups_and_protected_boundaries_cannot_be_crossed() {
    let mut protected = text("b", "Existing caption");
    if let Block::Text(text) = &mut protected {
        text.kind = TextBlockKind::Caption;
    }
    let section = section(vec![text("a", "Quote"), protected, text("c", "Credit")]);
    let spanning = Proposal::Quote {
        alignment: None,
        body: vec![0, 1],
        attribution: Some(2),
    };
    assert!(validate(&[spanning], &section).is_err());
    let outside_target = Proposal::Quote {
        alignment: None,
        body: vec![0],
        attribution: None,
    };
    assert!(
        validate_window(
            &[outside_target],
            &section,
            &RecognitionRoles::default(),
            2..3,
            0..3
        )
        .is_err()
    );
}

#[test]
fn structured_prompt_examples_match_the_output_types() {
    let examples: Vec<Value> = serde_json::from_str(include_str!("prompt_examples.json")).unwrap();
    assert!(!examples.is_empty());
    for value in examples {
        for group in value["groups"].as_array().unwrap() {
            if group["kind"] == "quote" {
                assert!(group.get("alignment").is_some());
            }
        }
        let response: Response = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            response.groups.len(),
            value["groups"].as_array().unwrap().len()
        );
    }
}

#[test]
fn quote_alignment_survives_annotations_and_applies_only_to_body() {
    use rebook_publication::TextAlignment;
    let mut original = section(vec![text("q", "Quoted text"), text("credit", "An author")]);
    if let Block::Text(text) = &mut original.blocks[0] {
        text.style.align = TextAlignment::Center;
        text.style.authored_alignment = Some(TextAlignment::Center);
    }
    let proposal = Proposal::Quote {
        body: vec![0],
        attribution: Some(1),
        alignment: Some(QuoteAlignment::Start),
    };
    let annotation = annotation(&proposal, &original);
    let restored: Annotation =
        serde_json::from_slice(&serde_json::to_vec(&annotation).unwrap()).unwrap();
    assert_eq!(restored, annotation);
    let mut blocks = original.blocks.clone();
    compose(&mut blocks, &restored);
    let Block::Quote(quote) = &blocks[0] else {
        panic!("missing quote");
    };
    assert_eq!(
        quote.body[0].style.semantic_alignment,
        Some(TextAlignment::Start)
    );
    assert_eq!(quote.body[0].style.align, TextAlignment::Center);
    assert_eq!(
        quote.body[0].style.authored_alignment,
        Some(TextAlignment::Center)
    );
    assert_eq!(
        quote.attribution.as_ref().unwrap().style.semantic_alignment,
        None
    );
    assert_eq!(quote.body[0].source.as_ref(), source(&original.blocks[0]));
    assert!(serde_json::from_value::<Response>(json!({"groups":[{"kind":"quote","body":[0],"attribution":null,"alignment":"arbitrary-css"}]})).is_err());
}

#[test]
fn semantic_request_sends_json_schema_and_structured_instructions() {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for attempt in 0..2 {
            let started = std::time::Instant::now();
            let (mut socket, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            started.elapsed() < std::time::Duration::from_secs(5),
                            "request did not arrive"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
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
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["response_format"]["type"], "json_schema");
            let contract = &request["response_format"]["json_schema"];
            assert_eq!(contract["strict"], true);
            assert_eq!(contract["schema"]["additionalProperties"], false);
            let alternatives = contract["schema"]["properties"]["groups"]["items"]["anyOf"]
                .as_array()
                .unwrap();
            let item = alternatives
                .iter()
                .find(|item| item["properties"]["kind"]["enum"] == json!(["quote"]))
                .unwrap();
            assert!(
                alternatives
                    .iter()
                    .any(|item| item["properties"]["kind"]["enum"] == json!(["quote_attribution"]))
            );
            assert_eq!(item["properties"]["kind"]["enum"], json!(["quote"]));
            assert_eq!(
                item["required"],
                json!(["kind", "body", "attribution", "alignment"])
            );
            assert_eq!(item["properties"]["attribution"]["type"], json!("integer"));
            assert!(
                request["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .starts_with("# AI layout")
            );
            let input: Value =
                serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(input["blocks"][0]["id"], 0);
            if attempt == 1 {
                let last = request["messages"].as_array().unwrap().last().unwrap();
                let feedback = last["content"].as_str().unwrap();
                assert!(feedback.starts_with("# Correct the response"));
                assert!(feedback.contains("## Structural error") && feedback.contains("99"));
            }
            let result = json!({"groups":[{"kind":"quote_inline","body":[if attempt == 0 {99} else {0}],"credit":"-- A poet","alignment":"center"}]});
            let body = json!({"choices":[{"message":{"content":result.to_string()}}]}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let provider = crate::plugins::AiProvider {
        base_url: format!("http://{address}/v1"),
        api_key: "test-key".into(),
        ..Default::default()
    };
    let section = section(vec![text("p", "A line of verse.\n-- A poet")]);
    let input = json!({"target_start":0,"target_end_exclusive":1,"blocks":[section_input_block(&section,0)]});
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(request_groups(
            &client,
            (&provider, "fixture", ReasoningEffort::High),
            &input,
            &section,
            &RecognitionRoles {
                quotes: true,
                captions: false,
                headings: false,
            },
            0..1,
            0..1,
        ));
    server.join().unwrap();
    assert_eq!(result.unwrap().groups.len(), 1);
    let figure = completion_options(&RecognitionRoles {
        quotes: false,
        captions: true,
        headings: false,
    });
    assert_eq!(
        figure
            .pointer(
                "/response_format/json_schema/schema/properties/groups/items/properties/kind/enum"
            )
            .unwrap(),
        &json!(["figure"])
    );
}

#[test]
#[ignore = "requires local The Hand and configured AI layout model; sends the full fifth chapter"]
fn live_hand_epigraph_with_inline_credit() {
    let book = rebook_formats::open_file(std::env::var("TORTO_SEMANTIC_BOOK").unwrap()).unwrap();
    let source = book.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().contains("_c05_"))
        .unwrap();
    let mut section = source.parse_section(index).unwrap();
    let target = section
        .blocks
        .iter()
        .position(|block| {
            paragraph(block).is_some_and(|p| text_block_text(p).contains("Octavio Paz"))
        })
        .unwrap();
    let range = super::source(&section.blocks[target]).unwrap().clone();
    let settings = PluginSettings::load_default().unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(recognize_with_source(
            &section,
            source.book().id.as_str(),
            &settings,
            source.as_ref(),
        ))
        .unwrap();
    assert!(
        result
            .annotations
            .iter()
            .any(|a| matches!(a, Annotation::QuoteInline { body, .. } if body.contains(&range))),
        "Paz epigraph should be recognized without a separate attribution paragraph"
    );
    let alignment = result
        .annotations
        .iter()
        .find_map(|a| match a {
            Annotation::QuoteInline {
                body, alignment, ..
            } if body.contains(&range) => *alignment,
            _ => None,
        })
        .expect("the epigraph should receive an alignment recommendation");
    println!("Recommended epigraph alignment: {alignment:?}");
    let overlay = SemanticLayoutSource::new(source.clone(), source.clone());
    overlay.configure(source.book().id.as_str(), &settings);
    // Exercise the same persisted chapter path used when opening the reader.
    if result.skipped_groups == 0 {
        let cached = overlay.parse_section(index).unwrap();
        assert!(cached.blocks.iter().any(|block| matches!(block, Block::Quote(q) if q.attribution.as_ref().is_some_and(|p| text_block_text(p).contains("Octavio Paz")))), "reader cache must expose the epigraph as a quote");
    }
    assert!(overlay.install(index, result.clone()));
    let displayed = overlay.parse_section(index).unwrap();
    assert!(displayed.blocks.iter().any(|block| matches!(block,Block::Quote(q) if q.attribution.as_ref().is_some_and(|p| text_block_text(p).contains("Octavio Paz")) && q.body.iter().all(|p| p.style.semantic_alignment==Some(alignment.text_alignment())))));
    assert!(displayed.blocks.iter().any(|block| matches!(block, Block::Quote(q) if q.attribution.as_ref().is_some_and(|p| text_block_text(p).contains("Octavio Paz")))));
    for a in &result.annotations {
        compose(&mut section.blocks, a);
    }
    assert!(section.blocks.iter().any(|block| matches!(block, Block::Quote(q) if q.attribution.as_ref().is_some_and(|p| text_block_text(p).contains("Octavio Paz")))));
    println!(
        "Full fifth chapter: Paz epigraph recognized, installed and exposed through the reader source"
    );
}

/// Explicit opt-in: uses local credentials and sends bounded excerpts to the
/// configured gemini/lite model. No credentials or book text are logged.
#[test]
#[ignore = "requires TORTO_SEMANTIC_BOOK and configured gemini/lite; makes paid requests"]
fn live_ramachandran_gemini_lite() {
    let path = std::env::var("TORTO_SEMANTIC_BOOK").expect("set TORTO_SEMANTIC_BOOK");
    let opened = rebook_formats::open_file(path).unwrap();
    let source = opened.source();
    let mut settings = PluginSettings::load_default().unwrap();
    let provider = settings
        .providers
        .iter()
        .find(|p| p.models.iter().any(|m| m.id == "gemini/lite"))
        .expect("gemini/lite must already be configured");
    settings.semantic_layout = SemanticLayoutSettings {
        enabled: true,
        provider: provider.id.clone(),
        model: "gemini/lite".into(),
        ..Default::default()
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut report = Vec::new();
    for index in [4, 5, 7, 8, 9, 11, 15] {
        let mut section = source.parse_section(index).unwrap();
        if index != 11 {
            section.blocks.truncate(24);
        }
        let candidates = section
            .blocks
            .iter()
            .filter(|b| paragraph(b).is_some())
            .count();
        let result = runtime.block_on(recognize_with_source(
            &section,
            source.book().id.as_str(),
            &settings,
            source.as_ref(),
        ));
        let input = section
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| input_block(i, b))
            .collect::<Vec<_>>();
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                println!("section {index}: {error}");
                report.push(json!({"section":index,"groups":0,"error":error,"input":input}));
                continue;
            }
        };
        let groups = result
            .annotations
            .iter()
            .filter(|a| {
                !matches!(
                    a,
                    Annotation::InlineCitations { .. }
                        | Annotation::ImageFormula { .. }
                        | Annotation::UnreadableFormula { .. }
                )
            })
            .count();
        // Manually checked against the book, including negative narrative/dialogue
        // paragraphs. Chapter 5 also contains a cartoon caption and a quoted letter.
        let quote = |body: Vec<usize>, attribution| Proposal::Quote {
            body,
            attribution,
            alignment: None,
        };
        let expected = match index {
            4 | 15 => vec![quote(vec![2], Some(3)), quote(vec![4], Some(5))],
            5 => vec![quote(vec![2, 3], Some(4))],
            7 => vec![quote(vec![2], Some(3))],
            8 => vec![
                quote(vec![2], Some(3)),
                Proposal::Figure {
                    images: vec![5],
                    captions: vec![6, 7, 8, 9],
                },
            ],
            9 => vec![quote(vec![3], Some(4))],
            11 => vec![quote(vec![3], Some(4)), quote(vec![5], Some(6))],
            _ => unreachable!(),
        };
        let expected: Vec<_> = expected.iter().map(|p| annotation(p, &section)).collect();
        // This benchmark labels semantic groups; alignment is verified separately.
        let recognized: Vec<_> = result
            .annotations
            .iter()
            .filter(|a| {
                !matches!(
                    a,
                    Annotation::InlineCitations { .. }
                        | Annotation::ImageFormula { .. }
                        | Annotation::UnreadableFormula { .. }
                )
            })
            .cloned()
            .map(|mut a| {
                if let Annotation::Quote { alignment, .. } = &mut a {
                    *alignment = None;
                }
                a
            })
            .collect();
        let matches_expected =
            expected.len() == groups && expected.iter().all(|a| recognized.contains(a));
        let no_false_positives = recognized.iter().all(|a| expected.contains(a));
        // Explicitly sourced epigraphs and the caption must work. The former
        // unattributed-letter positive is now a negative example by design.
        let required_found = expected
            .iter()
            .filter(|a| {
                !matches!(
                    a,
                    Annotation::Quote {
                        attribution: None,
                        ..
                    }
                )
            })
            .all(|a| recognized.contains(a));
        let missed = expected.iter().filter(|a| !recognized.contains(a)).count();
        let mut composed = section.blocks.clone();
        for a in &result.annotations {
            compose(&mut composed, a);
        }
        let entry = json!({"section":index,"candidates":candidates,"groups":groups,"matches_expected":matches_expected,"no_false_positives":no_false_positives,"required_found":required_found,"missed":missed,"annotations":result.annotations,"input":input,
            "before":section.blocks.iter().map(|b| match b {Block::Text(t)=>format!("{:?}",t.kind),_=>format!("{:?}",std::mem::discriminant(b))}).collect::<Vec<_>>()});
        println!(
            "section {index}: {candidates} candidates, {groups} new semantic groups, {missed} missed benchmark groups"
        );
        report.push(entry);
    }
    if let Ok(path) = std::env::var("TORTO_SEMANTIC_REPORT") {
        crate::persistence::write_json_atomic(std::path::Path::new(&path), &report).unwrap();
    }
    assert!(
        report.iter().all(|r| r.get("error").is_none()),
        "recognition errors; inspect the report"
    );
    assert!(
        report
            .iter()
            .all(|r| r["no_false_positives"] == true && r["required_found"] == true),
        "false positive or missing chapter epigraph/caption; inspect report"
    );
    assert!(
        report
            .iter()
            .filter(|r| r["groups"].as_u64().unwrap() > 0)
            .count()
            >= 4,
        "expected missing epigraphs in multiple chapters"
    );
}

#[test]
fn unified_request_returns_groups_and_citations_in_one_call() {
    use std::io::{BufRead, Read, Write};
    let mut section = section(vec![
        text("p", "Evidence (Smith, 2020). Rate x = y."),
        text("q", "A line of verse.\n-- A poet"),
        image("img"),
        text("caption", "Figure 1. A diagram."),
        text("context", "Other (Jones, 2022)."),
    ]);
    let Block::Text(heading) = &mut section.blocks[4] else {
        unreachable!()
    };
    heading.kind = TextBlockKind::Heading(2);
    let candidates = citations::window_candidates(&section, 0..4);
    assert_eq!(candidates.len(), 1);
    let roles = RecognitionRoles::default();
    let input = unified_window_input(&section, &roles, 0..4, 0..5, &candidates);
    assert_eq!(input["targets"]["classify_citations"], json!(["c0_0_0"]));
    assert_eq!(input["blocks"][4]["citation_candidates"], json!([]));
    assert!(validate_citation_ids(&["c4_0_0".into()], &candidates).is_err());
    assert!(validate_citation_ids(&["c0_0_0".into(), "c0_0_0".into()], &candidates).is_err());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(&mut socket);
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse::<usize>().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request["reasoning_effort"], "high");
        assert_eq!(
            request["response_format"]["json_schema"]["schema"]["required"],
            json!(["groups", "citations", "formulas"])
        );
        let input: Value =
            serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input["blocks"][0]["citation_candidates"][0]["id"], "c0_0_0");
        assert_eq!(input["quotes_enabled"], true);
        assert_eq!(input["captions_enabled"], true);
        let mut response = json!({"groups":[{"kind":"quote_inline","body":[1],"credit":"-- A poet","alignment":"start"},{"kind":"figure","images":[2],"captions":[3]}],"citations":["c0_0_0"],"formulas":[{"block":0,"paragraph":0,"original":"x = y","latex":"x=y","before":"","after":""}]});
        let duplicate = response["groups"][0].clone();
        response["groups"]
            .as_array_mut()
            .unwrap()
            .extend([duplicate, json!({"kind":"section_heading","block":4})]);
        response["citations"]
            .as_array_mut()
            .unwrap()
            .push(json!("c0_0_0"));
        let duplicate = response["formulas"][0].clone();
        response["formulas"].as_array_mut().unwrap().push(duplicate);
        let body = json!({"choices":[{"message":{"content":response.to_string()}}]}).to_string();
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        // Listener drops after this one response. A second pass would fail.
    });
    let provider = crate::plugins::AiProvider {
        base_url: format!("http://{address}/v1"),
        api_key: "fixture".into(),
        ..Default::default()
    };
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(request_window_groups(
            &reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            (&provider, "fixture", ReasoningEffort::High),
            &input,
            &section,
            &roles,
            0..4,
            0..5,
        ))
        .unwrap();
    server.join().unwrap();
    assert_eq!(result.groups.len(), 2);
    assert_eq!(result.citations, ["c0_0_0"]);
    assert_eq!(result.formulas.len(), 1);
    let mut annotations = citations::window_annotations(&candidates, &result.citations);
    annotations
        .extend(resolve_text_formulas(&section, 0..4, &result.formulas, &result.citations).0);
    assert!(citations::validate_annotations(&section, &annotations));
    let mut displayed = section.clone();
    for a in &annotations {
        compose(&mut displayed.blocks, a);
    }
    for group in &result.groups {
        compose(&mut displayed.blocks, &annotation(group, &section));
    }
    let inputs = crate::plugins::prepare_translation_inputs(&displayed, false);
    assert!(inputs[0].0.text.contains("<torto-math-0/>"));
    assert!(
        inputs[0]
            .0
            .text
            .contains("<citation id=\"1\">(Smith, 2020)</citation>")
    );
}

#[test]
fn layout_reasoning_defaults_roundtrips_and_changes_cache_identity() {
    let old: SemanticLayoutSettings =
        serde_json::from_value(json!({"enabled":true,"provider":"old","model":"old"})).unwrap();
    assert_eq!(old.reasoning_effort, ReasoningEffort::Default);
    let mut settings = PluginSettings::default();
    settings.providers[0].api_key = "fixture".into();
    settings.providers[0].base_url = "http://127.0.0.1:9/v1".into();
    settings.semantic_layout.enabled = true;
    settings.semantic_layout.provider = settings.providers[0].id.clone();
    settings.semantic_layout.model = settings.providers[0].models[0].id.clone();
    let original = recognition_identity("reasoning-fixture", &settings).unwrap();
    settings.semantic_layout.reasoning_effort = ReasoningEffort::High;
    assert_ne!(
        recognition_identity("reasoning-fixture", &settings).unwrap(),
        original
    );
    let restored: SemanticLayoutSettings =
        serde_json::from_value(serde_json::to_value(&settings.semantic_layout).unwrap()).unwrap();
    assert_eq!(restored.reasoning_effort, ReasoningEffort::High);
}
