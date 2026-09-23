use super::*;
use crate::plugins::semantic_layout::tests::{original_source, section, text};
use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};

#[test]
#[ignore = "uses configured AI model and local TORTO_SEMANTIC_BOOK; makes paid requests"]
fn live_numbered_headings_and_book_page_numbers() {
    let settings = PluginSettings::load_default().unwrap();
    let endpoint = settings.semantic_layout_endpoint().unwrap();
    let client = reqwest::Client::builder()
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
            let input = json!({"target_start":start,"target_end_exclusive":end,
                "blocks":(lo..hi).map(|i| section_input_block(section,i)).collect::<Vec<_>>()});
            let result = runtime
                .block_on(request_window_groups(
                    &client,
                    endpoint,
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
        let proposed = groups.iter().flat_map(proposal_ids).collect::<Vec<_>>();
        let accepted = runtime
            .block_on(review(&client, endpoint, section, &proposed))
            .unwrap();
        groups.retain(|group| proposal_ids(group).iter().all(|id| accepted.contains(id)));
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
        .is_err()
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
    for value in ["", "0", "1.2", "-2", "2024 report", "1.."] {
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
    assert_eq!(items.len(), 64);
    assert!(items.first().unwrap()["id"].as_u64().unwrap() < 50);
    assert!(items.last().unwrap()["id"].as_u64().unwrap() > 50);
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
