use super::*;
use crate::plugins::semantic_layout::tests::{image, original_source, section, text};
use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};

fn fixture() -> (Section, Recognition) {
    let mut authored = text("authored", "Existing publication heading");
    if let Block::Text(t) = &mut authored {
        t.kind = TextBlockKind::Heading(2);
    }
    let original = section(vec![
        text("number", "2"),
        text("candidate", "Chapter 2 Background"),
        text("quote", "A borrowed passage."),
        text("credit", "-- Mira Vale"),
        image("image"),
        text("caption", "Figure 1. The original caption"),
        authored,
    ]);
    let mut result = empty_recognition(&original);
    result.annotations = vec![
        Annotation::SectionHeading {
            source: source(&original.blocks[0]).unwrap().clone(),
        },
        Annotation::SectionHeading {
            source: source(&original.blocks[1]).unwrap().clone(),
        },
        annotation(
            &Proposal::Quote {
                body: vec![2],
                attribution: Some(3),
                alignment: None,
            },
            &original,
        ),
        annotation(
            &Proposal::Figure {
                images: vec![4],
                captions: vec![5],
            },
            &original,
        ),
    ];
    (original, result)
}
fn identity() -> Value {
    // Exercise this specific persisted contract independently of later prompts.
    json!([
        CACHE_FORMAT_VERSION,
        MIGRATED_CONTRACT,
        uuid::Uuid::new_v4().to_string(),
        "provider",
        "https://example.invalid/v1",
        "model",
        "none",
        "enhancements",
        "Chinese",
        true,
        true
    ])
}

#[test]
fn requests_keep_existing_boundaries_without_heading_tasks_or_output_types() {
    let (original, _) = fixture();
    for roles in [
        RecognitionRoles::default(),
        RecognitionRoles {
            quotes: false,
            captions: false,
        },
    ] {
        let input = unified_window_input(&original, &roles, 0..7, 0..7, &[]);
        for key in [
            "classify_headings",
            "headings_enabled",
            "numbered_candidates",
            "bold_ratio",
        ] {
            assert!(
                !input.to_string().contains(key),
                "Unexpected heading task: {key}"
            );
        }
        assert!(input["blocks"][0].get("style").is_none());
        assert_eq!(input["blocks"][6]["type"], "boundary");
        assert_eq!(
            input["blocks"][6]["heading"],
            "Existing publication heading"
        );
        let prompt = window_prompt(&original, &roles, 0..7, 0..7);
        assert!(prompt.contains("Never add headings"));
        assert!(!prompt.contains("## Headings"));
        let options = completion_options(&roles);
        assert!(!options.to_string().contains("section_heading"));
        let validator = jsonschema::validator_for(&options["output_schema"]).unwrap();
        assert!(!validator.is_valid(
            &json!({"groups":[{"kind":"section_heading","block":0}],"citations":[],"formulas":[]})
        ));
        assert!(
            !wire::options(&options)
                .to_string()
                .contains("section_heading")
        );
    }
}

#[test]
fn unsolicited_headings_are_ignored_before_deserialization_and_conflict_checks() {
    let (original, _) = fixture();
    let value = json!({"groups":[
        {"kind":"section_heading","block":99999},
        {"kind":"section_heading","block":"invalid","extra":"legacy"},
        {"kind":"section_heading"},
        {"kind":"quote","body":[2],"attribution":3,"alignment":null},
        {"kind":"figure","images":[4],"captions":[5]}
    ],"citations":["c0_0_0"],"formulas":[{"block":1,"paragraph":0,"original":"F = ma","latex":"F=ma","before":"","after":""}]});
    for wire_value in [value.clone(), wire::encode(&value)] {
        let mut response = normalize::decode_response(&wire_value).unwrap();
        assert_eq!(response.groups.len(), 2);
        assert_eq!(response.citations, vec!["c0_0_0"]);
        assert_eq!(response.formulas.len(), 1);
        assert_eq!(normalize::response(&mut response, &original, 0..7), 0);
        validate_window(
            &response.groups,
            &original,
            &RecognitionRoles::default(),
            0..7,
            0..7,
        )
        .unwrap();
    }
    assert!(
        validate_window(
            &[Proposal::SectionHeading { block: 0 }],
            &original,
            &RecognitionRoles::default(),
            0..7,
            0..7
        )
        .is_err()
    );
}

#[test]
fn installed_and_direct_legacy_headings_do_not_promote_original_or_translated_prose() {
    let (original, mut result) = fixture();
    result.annotations.truncate(2);
    assert!(!result.has_annotations());
    assert!(
        recognition_groups(&original, 0..7, result.clone())
            .iter()
            .all(|(_, r)| !r.has_annotations())
    );
    assert!(!merge_recognition(&original, 0..7, vec![result.clone()]).has_annotations());
    let source_book = original_source(original.clone());
    let translations = Arc::new(TranslationBookSource::new(
        source_book.clone(),
        TranslationMode::Replace,
    ));
    translations
        .store_batch(
            0,
            &[BlockTranslation {
                block_index: 1,
                segment_index: None,
                text: "第二章 背景".into(),
            }],
        )
        .unwrap();
    let overlay = SemanticLayoutSource::new(translations.clone(), source_book.clone());
    for mode in [
        None,
        Some(TranslationMode::Replace),
        Some(TranslationMode::Bilingual),
    ] {
        translations.set_enabled(mode.is_some()).unwrap();
        if let Some(mode) = mode {
            translations.set_mode(mode).unwrap();
        }
        assert!(
            !overlay.install(0, result.clone()),
            "Headings alone never count as an applied edit"
        );
        assert!(!overlay.install_scope(0, &fingerprint(&original), 0..7, result.clone()));
        assert_eq!(
            overlay.parse_section(0).unwrap(),
            translations.parse_section(0).unwrap()
        );
        assert_eq!(
            overlay.book().table_of_contents,
            source_book.book().table_of_contents
        );
        let mut direct = translations.parse_section(0).unwrap();
        let before = direct.clone();
        for a in &result.annotations {
            compose(&mut direct.blocks, a);
        }
        assert_eq!(
            direct, before,
            "The composer itself must also reject legacy headings"
        );
        overlay.clear();
    }
}

#[test]
fn old_chapter_and_manual_scope_caches_keep_independent_annotations() {
    let (original, result) = fixture();
    let id = identity();
    let legacy = legacy_identity(&id).expect("known heading-only contract migration");
    let hash = fingerprint(&original);
    let chapter = recognition_path(&legacy, &hash).unwrap();
    let scope = scope_presentation_path(&legacy, &hash).unwrap();
    crate::persistence::write_json_atomic(&chapter, &result).unwrap();
    crate::persistence::write_json_atomic(&scope, &vec![(0, 7, result.clone())]).unwrap();
    let loaded = load_recognition(&original, &id).unwrap();
    assert_eq!(loaded.annotations, result.annotations[2..]);
    let scopes = load_scope_presentation(&id, &original, &hash).unwrap();
    assert_eq!(scopes[&(0, 7)].annotations, loaded.annotations);
    let book = original_source(original.clone());
    let overlay = SemanticLayoutSource::new(book.clone(), book);
    assert!(overlay.install(0, loaded));
    let displayed = overlay.parse_section(0).unwrap();
    assert_eq!(displayed.blocks[0], original.blocks[0]);
    assert_eq!(displayed.blocks[1], original.blocks[1]);
    assert!(matches!(&displayed.blocks[2], Block::Quote(_)));
    assert!(matches!(&displayed.blocks[3], Block::Figure(_)));
    assert_eq!(displayed.blocks[4], original.blocks[6]);
    let mut different = id.clone();
    different[5] = json!("different-model");
    assert!(load_recognition(&original, &different).is_none());
    let mut future = id.clone();
    future[1] = json!("a-different-request-contract");
    assert!(legacy_identity(&future).is_none());
    let current = recognition_path(&id, &hash).unwrap();
    std::fs::write(&current, b"invalid new cache").unwrap();
    assert!(
        load_recognition(&original, &id).is_none(),
        "Invalid new data must not fall back to old data"
    );
    for path in [chapter, scope, current] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn cached_misses_notice_legacy_results_arriving_later() {
    let (original, result) = fixture();
    let id = identity();
    let hash = fingerprint(&original);
    let book = original_source(original.clone());
    let overlay = SemanticLayoutSource::new(book.clone(), book);
    *overlay.cache_identity.write().unwrap() = Some(id.clone());
    assert!(
        overlay
            .cached_recognition(0, &original, &hash, &id)
            .is_none()
    );
    assert_eq!(
        overlay.recognition_misses.read().unwrap()[&0].paths.len(),
        2
    );
    let legacy_path = recognition_path(&legacy_identity(&id).unwrap(), &hash).unwrap();
    crate::persistence::write_json_atomic(&legacy_path, &result).unwrap();
    let loaded = overlay
        .cached_recognition(0, &original, &hash, &id)
        .unwrap();
    assert_eq!(loaded.annotations, result.annotations[2..]);
    std::fs::remove_file(legacy_path).unwrap();
}
