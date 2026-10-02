use super::*;
use crate::plugins::semantic_layout::tests::{original_source, section, text};
use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};

#[test]
#[ignore = "uses configured AI model and local TORTO_SEMANTIC_BOOK; makes paid requests"]
fn live_numbered_headings_and_book_page_numbers() {
    let settings = PluginSettings::load_default().unwrap();
    let endpoint = settings.semantic_layout_endpoint().unwrap();
    let client = crate::http::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let roles = RecognitionRoles {
        quotes: false,
        captions: false,
        headings: true,
    };
    let recognize = |section: &Section| {
        let mut groups = Vec::new();
        let mut start = 0;
        while start < section.blocks.len() {
            let end = window_end(section, start);
            let lo = start.saturating_sub(OVERLAP);
            let hi = (end + OVERLAP).min(section.blocks.len());
            let input = unified_window_input(section, &roles, start..end, lo..hi, &[]);
            let result = runtime
                .block_on(request_window_groups(
                    &client,
                    (
                        endpoint.0,
                        endpoint.1,
                        settings.semantic_layout.reasoning_effort,
                    ),
                    &input,
                    section,
                    &roles,
                    start..end,
                    lo..hi,
                ))
                .unwrap();
            assert_eq!(result.skipped_groups, 0);
            groups.extend(result.groups);
            start = end;
        }
        groups
    };
    // Independent synthetic positive; never included in the production prompt.
    let positive = section(vec![
        text("one", "1"),
        text(
            "a",
            "Forests regulate the local water cycle. Their roots retain rainwater, while their leaves release moisture into the air. These processes connect vegetation to regional rainfall.",
        ),
        text(
            "b",
            "The loss of tree cover therefore changes more than the appearance of a landscape. Rivers become less predictable and surrounding farms face a different climate.",
        ),
        text("two", "2"),
        text(
            "c",
            "Urban transport presents a different planning problem. Reliable rail services allow a city to grow without devoting most of its land to roads and parking.",
        ),
        text(
            "d",
            "The choice of a transport network shapes where people live and work. Planning must consider the needs of residents who cannot drive.",
        ),
        text("three", "3"),
        text(
            "e",
            "Public health depends on accessible preventive care. Regular screening and vaccination can reduce the burden of disease before hospital treatment becomes necessary.",
        ),
    ]);
    let found = recognize(&positive)
        .iter()
        .flat_map(proposal_ids)
        .collect::<Vec<_>>();
    assert_eq!(found, vec![0, 3, 6], "independent section-heading fixture");
    let opened = rebook_formats::open_file(std::env::var("TORTO_SEMANTIC_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path() == "index_split_005.html")
        .unwrap();
    let chapter = source.parse_section(index).unwrap();
    let candidates = chapter
        .blocks
        .iter()
        .filter(|b| candidate(b).is_some())
        .count();
    assert!(
        candidates >= 5,
        "expected page-number regression candidates"
    );
    let found = recognize(&chapter);
    assert!(
        found.is_empty(),
        "residual page numbers must not become headings: {found:?}"
    );
    println!(
        "3 synthetic headings recognized; {candidates} real-book page numbers rejected across the full chapter"
    );
}

#[test]
fn headings_validate_ids_roles_and_conflicts() {
    let section = section(vec![
        text("h", "2"),
        text("p", "2 examples"),
        text("q", "3"),
    ]);
    let heading = Proposal::SectionHeading { block: 0 };
    let validate = |groups: &[Proposal], roles: &RecognitionRoles| {
        validate_window(groups, &section, roles, 0..3, 0..3)
    };
    assert!(validate(std::slice::from_ref(&heading), &RecognitionRoles::default()).is_ok());
    assert!(
        validate(
            &[Proposal::SectionHeading { block: 1 }],
            &RecognitionRoles::default()
        )
        .is_ok()
    );
    assert!(
        validate(
            &[Proposal::SectionHeading { block: 9 }],
            &RecognitionRoles::default()
        )
        .is_err()
    );
    assert!(
        validate(
            std::slice::from_ref(&heading),
            &RecognitionRoles {
                headings: false,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        validate(
            &[
                heading,
                Proposal::Quote {
                    body: vec![0],
                    attribution: None,
                    alignment: None
                }
            ],
            &RecognitionRoles::default()
        )
        .is_err()
    );
    assert!(
        validate_window(
            &[Proposal::SectionHeading { block: 2 }],
            &section,
            &RecognitionRoles::default(),
            0..2,
            0..3
        )
        .is_err()
    );
    for value in ["1", "12.", "3)"] {
        assert!(candidate(&text("n", value)).is_some());
    }
    for value in ["", "0", "1.2", "-2", "1.."] {
        assert!(candidate(&text("n", value)).is_none(), "{value}");
    }
    let mut protected = text("h", "2");
    if let Block::Text(t) = &mut protected {
        t.kind = TextBlockKind::Heading(2);
    }
    assert!(candidate(&protected).is_none());
}

#[test]
fn numbering_context_reaches_across_windows_and_is_bounded() {
    let section = section(
        (1..=100)
            .map(|n| text(&format!("n{n}"), &n.to_string()))
            .collect(),
    );
    let summary = context(&section, 50);
    let items = summary["items"].as_array().unwrap();
    assert_eq!(summary["total"], 100);
    assert_eq!(items.len(), 8);
    assert!(items.first().unwrap()["id"].as_u64().unwrap() < 50);
    assert!(items.last().unwrap()["id"].as_u64().unwrap() > 50);
}

#[test]
fn stale_unnumbered_heading_annotations_do_not_apply_or_discard_other_headings() {
    let mut known = text("known", "Chapter 3 Existing heading");
    if let Block::Text(t) = &mut known {
        t.kind = TextBlockKind::Heading(2);
    }
    let original = section(vec![
        text("old", "Historical background"),
        text("new", "Chapter 2 Background"),
        known,
    ]);
    let result = Recognition {
        fingerprint: fingerprint(&original),
        formulas_checked: true,
        skipped_groups: 0,
        annotations: original
            .blocks
            .iter()
            .map(|block| Annotation::SectionHeading {
                source: source(block).unwrap().clone(),
            })
            .collect(),
    };
    let source = original_source(original.clone());
    let overlay = SemanticLayoutSource::new(source.clone(), source);
    assert!(overlay.install(0, result.clone()));
    let displayed = overlay.parse_section(0).unwrap();
    assert!(matches!(&displayed.blocks[0], Block::Text(t) if t.kind == TextBlockKind::Paragraph));
    assert!(matches!(&displayed.blocks[1], Block::Text(t) if t.kind == TextBlockKind::Heading(3)));
    assert_eq!(displayed.blocks[2], original.blocks[2]);
    let request = unified_window_input(&original, &RecognitionRoles::default(), 0..3, 0..3, &[]);
    assert_eq!(request["targets"]["classify_headings"], json!([1]));
    let identity = json!(["restricted-heading-cache", std::process::id()]);
    let path = recognition_path(&identity, &result.fingerprint).unwrap();
    crate::persistence::write_json_atomic(&path, &result).unwrap();
    let cached = load_recognition(&original, &identity).unwrap();
    assert_eq!(cached.annotations, vec![result.annotations[1].clone()]);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn headings_preserve_translation_sources_toc_and_toggle() {
    let section = section(vec![text("h", "2"), text("p", "Body")]);
    let range = source(&section.blocks[0]).unwrap().clone();
    let original = original_source(section.clone());
    let translations = Arc::new(TranslationBookSource::new(
        original.clone(),
        TranslationMode::Replace,
    ));
    translations
        .store_batch(
            0,
            &[BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "二".into(),
            }],
        )
        .unwrap();
    let overlay = SemanticLayoutSource::new(translations.clone(), original.clone());
    let result = Recognition {
        formulas_checked: true,
        fingerprint: fingerprint(&section),
        skipped_groups: 0,
        annotations: vec![Annotation::SectionHeading {
            source: range.clone(),
        }],
    };
    for mode in [
        None,
        Some(TranslationMode::Replace),
        Some(TranslationMode::Bilingual),
    ] {
        translations.set_enabled(mode.is_some()).unwrap();
        if let Some(mode) = mode {
            translations.set_mode(mode).unwrap();
        }
        assert!(overlay.install(0, result.clone()));
        let rendered = overlay.parse_section(0).unwrap();
        let heading_count = if mode == Some(TranslationMode::Bilingual) {
            2
        } else {
            1
        };
        for block in &rendered.blocks[..heading_count] {
            assert!(matches!(block, Block::Text(t) if t.kind == TextBlockKind::Heading(3)));
        }
        assert_eq!(source(&rendered.blocks[0]), Some(&range));
        assert_eq!(
            overlay.book().table_of_contents,
            original.book().table_of_contents
        );
        overlay.clear();
        assert_eq!(
            overlay.parse_section(0).unwrap(),
            translations.parse_section(0).unwrap()
        );
    }
    let identity = json!(["heading-cache-test", std::process::id()]);
    let path = recognition_path(&identity, &result.fingerprint).unwrap();
    crate::persistence::write_json_atomic(&path, &result).unwrap();
    assert_eq!(
        load_recognition(&section, &identity).unwrap().annotations,
        result.annotations
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn numbered_candidates_use_original_styles_and_keep_protected_roles() {
    let title = "1.2 Historical background to medical knowledge and treatments";
    let mut heading = text("h", title);
    if let Block::Text(t) = &mut heading {
        if let Inline::Text(run) = &mut t.content[0] {
            run.style.bold = true;
        }
        t.style.margin_before = 24.0;
    }
    assert!(candidate(&heading).is_some());
    for title in [
        "2024 report",
        "1.2 Background",
        "Chapter 1",
        "Part II Background",
        "Section 2.3 Introduction",
        "第1章 引言",
        "第三节",
        "（1）标题",
        "1、标题",
        "１．２ 标题",
    ] {
        assert!(candidate(&text("t", title)).is_some());
    }
    for title in [
        "Historical background",
        "Why does this happen?",
        "历史背景",
        "See Chapter 1",
        "Chapter one",
        "Chapter 1abc",
        "Particular problems",
        "(2020)",
    ] {
        assert!(candidate(&text("t", title)).is_none(), "{title}");
    }
    for kind in [
        TextBlockKind::Heading(2),
        TextBlockKind::Caption,
        TextBlockKind::Blockquote,
        TextBlockKind::QuoteAttribution,
        TextBlockKind::ListItem {
            ordered: false,
            ordinal: 1,
            depth: 0,
            marker_visible: true,
        },
    ] {
        let mut protected = heading.clone();
        if let Block::Text(t) = &mut protected {
            t.kind = kind;
        }
        assert!(candidate(&protected).is_none());
    }
    assert!(candidate(&text("long", &"x".repeat(241))).is_none());
    let section = section(vec![
        heading,
        text(
            "body",
            "This passage discusses the history of medical understanding.",
        ),
    ]);
    let input = unified_window_input(&section, &RecognitionRoles::default(), 0..1, 0..2, &[]);
    assert_eq!(input["blocks"][0]["style"]["bold_ratio"], 1.0);
    assert_eq!(input["blocks"][0]["style"]["margin_before"], 24.0);
    assert_eq!(input["targets"]["classify_headings"], json!([0]));
    assert!(input.get("numbered_candidates").is_none());
    let prompt = window_prompt(&section, &RecognitionRoles::default(), 0..1, 0..2);
    assert!(prompt.contains("## Headings"));
    assert!(!prompt.contains("## Captions"));
    assert!(!prompt.contains("## Inline citations"));
    assert!(PROMPT.chars().count() < 7000);
}

#[test]
#[ignore = "requires TORTO_HEADING_BOOK local EPUB; no model requests"]
fn local_tinnitus_plain_paragraph_heading() {
    let book = rebook_formats::open_file(std::env::var("TORTO_HEADING_BOOK").unwrap()).unwrap();
    let source = book.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("9781847091666_epub-10.html"))
        .unwrap();
    let section = source.parse_section(index).unwrap();
    let id=section.blocks.iter().position(|block| matches!(block,Block::Text(t) if text_block_text(t)=="Historical background to medical knowledge and treatments")).unwrap();
    assert!(candidate(&section.blocks[id]).is_none());
    let input = unified_window_input(
        &section,
        &RecognitionRoles::default(),
        id..id + 1,
        id.saturating_sub(1)..(id + 2).min(section.blocks.len()),
        &[],
    );
    let block = input["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["id"] == id)
        .unwrap();
    assert_eq!(block["id"], id);
    assert_eq!(input["targets"]["classify_headings"], json!([]));
    let proposal = Proposal::SectionHeading { block: id };
    assert!(
        validate_window(
            &[proposal.clone()],
            &section,
            &RecognitionRoles::default(),
            id..id + 1,
            id..id + 1,
        )
        .is_err()
    );
    println!(
        "book section={index} block={id}; unnumbered paragraph remains ineligible despite bold styling"
    );
}
