use super::tests::{original_source, section, text};
use super::*;
use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};

fn body(id: &str, value: &str) -> TextBlock {
    let Block::Text(mut text) = text(id, value) else {
        unreachable!()
    };
    text.kind = TextBlockKind::Blockquote;
    text.style.align = rebook_publication::TextAlignment::Center;
    text.style.authored_alignment = Some(rebook_publication::TextAlignment::Center);
    text
}

fn fixture(inside: bool) -> Section {
    let first = body(
        "quote",
        "Each small discovery changes the way we see the world.",
    );
    let quote = Block::Quote(QuoteBlock {
        source: first.source.clone(),
        body: vec![first],
        attribution: None,
    });
    if inside {
        let Block::Quote(mut quote) = quote else {
            unreachable!()
        };
        quote.body.push(body("credit", "— Mira Vale"));
        section(vec![Block::Quote(quote)])
    } else {
        section(vec![
            quote,
            text("credit", "— Mira Vale"),
            text("next", "The next paragraph is ordinary prose."),
        ])
    }
}

fn proposal(inside: bool) -> Proposal {
    Proposal::QuoteAttribution {
        quote: 0,
        attribution: (!inside).then_some(1),
        body_index: inside.then_some(1),
    }
}

fn validate(group: &Proposal, section: &Section) -> Result<(), String> {
    validate_window(
        std::slice::from_ref(group),
        section,
        &RecognitionRoles::default(),
        0..section.blocks.len(),
        0..section.blocks.len(),
    )
}

#[test]
fn only_quotes_missing_credit_are_offered_for_completion() {
    let mut original = fixture(false);
    assert_eq!(
        section_input_block(&original, 0)["type"],
        "quote_missing_attribution"
    );
    validate(&proposal(false), &original).unwrap();
    assert!(
        validate(
            &Proposal::Quote {
                body: vec![0],
                attribution: Some(1),
                alignment: None
            },
            &original
        )
        .is_err()
    );
    let Block::Text(credit) = original.blocks[1].clone() else {
        unreachable!()
    };
    let Block::Quote(quote) = &mut original.blocks[0] else {
        unreachable!()
    };
    quote.attribution = Some(credit);
    assert_eq!(
        section_input_block(&original, 0)["type"],
        "protected_boundary"
    );
    assert!(validate(&proposal(false), &original).is_err());
}

#[test]
fn completion_preserves_quote_body_styles_and_source_links() {
    for inside in [false, true] {
        let original = fixture(inside);
        let item = proposal(inside);
        validate(&item, &original).unwrap();
        let annotation = annotation(&item, &original);
        let restored: Annotation =
            serde_json::from_slice(&serde_json::to_vec(&annotation).unwrap()).unwrap();
        assert_eq!(restored, annotation);
        let mut blocks = original.blocks.clone();
        compose(&mut blocks, &restored);
        let Block::Quote(before) = &original.blocks[0] else {
            unreachable!()
        };
        let Block::Quote(after) = &blocks[0] else {
            unreachable!()
        };
        assert_eq!(after.body, vec![before.body[0].clone()]);
        let credit = after.attribution.as_ref().unwrap();
        assert_eq!(text_block_text(credit), "— Mira Vale");
        assert_eq!(credit.kind, TextBlockKind::QuoteAttribution);
        assert_eq!(credit.source.as_ref().unwrap().start.node, "credit");
        assert_eq!(blocks.len(), if inside { 1 } else { 2 });
        if !inside {
            assert_eq!(blocks[1], original.blocks[2]);
        }

        let identity = json!(["attribution-cache-test", uuid::Uuid::new_v4().to_string()]);
        let result = Recognition {
            formulas_checked: true,
            fingerprint: fingerprint(&original),
            annotations: vec![annotation],
            skipped_groups: 0,
        };
        let path = recognition_path(&identity, &result.fingerprint).unwrap();
        crate::persistence::write_json_atomic(&path, &result).unwrap();
        assert_eq!(
            load_recognition(&original, &identity).unwrap().annotations,
            result.annotations
        );
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn completion_cannot_empty_a_quote_or_take_nonadjacent_prose() {
    let original = fixture(false);
    for group in [
        Proposal::QuoteAttribution {
            quote: 0,
            attribution: Some(2),
            body_index: None,
        },
        Proposal::QuoteAttribution {
            quote: 0,
            attribution: None,
            body_index: Some(0),
        },
        Proposal::QuoteAttribution {
            quote: 0,
            attribution: Some(1),
            body_index: Some(0),
        },
        Proposal::QuoteAttribution {
            quote: 0,
            attribution: None,
            body_index: None,
        },
    ] {
        assert!(validate(&group, &original).is_err());
    }
}

#[test]
fn completion_keeps_bilingual_body_and_credit_together() {
    for inside in [false, true] {
        let section = fixture(inside);
        let original = original_source(section.clone());
        let translations = Arc::new(TranslationBookSource::new(
            original.clone(),
            TranslationMode::Bilingual,
        ));
        translations.set_enabled(true).unwrap();
        translations
            .store_batch(
                0,
                &[
                    BlockTranslation {
                        block_index: 0,
                        segment_index: Some(0),
                        text: "Translated quotation".into(),
                    },
                    BlockTranslation {
                        block_index: if inside { 0 } else { 1 },
                        segment_index: inside.then_some(1),
                        text: "Translated credit".into(),
                    },
                ],
            )
            .unwrap();
        let overlay = SemanticLayoutSource::new(translations.clone(), original);
        assert!(overlay.install(
            0,
            Recognition {
                formulas_checked: true,
                fingerprint: fingerprint(&section),
                annotations: vec![annotation(&proposal(inside), &section)],
                skipped_groups: 0
            }
        ));
        let after = overlay.parse_section(0).unwrap();
        let Block::Quote(quote) = &after.blocks[0] else {
            unreachable!()
        };
        assert_eq!(quote.body.len(), 2);
        assert_eq!(text_block_text(&quote.body[1]), "Translated quotation");
        let credit = text_block_text(quote.attribution.as_ref().unwrap());
        assert!(credit.contains("Mira Vale") && credit.contains("Translated credit"));
        overlay.clear();
        assert_eq!(
            overlay.parse_section(0).unwrap(),
            translations.parse_section(0).unwrap()
        );
    }
}

#[test]
fn standalone_blockquote_can_gain_a_known_attribution_paragraph() {
    let mut credit = body("credit", "— Mira Vale");
    credit.kind = TextBlockKind::QuoteAttribution;
    let original = section(vec![
        Block::Text(body("quote", "An existing quotation.")),
        Block::Text(credit),
    ]);
    assert_eq!(
        section_input_block(&original, 1)["type"],
        "attribution_candidate"
    );
    validate(&proposal(false), &original).unwrap();
    let mut blocks = original.blocks.clone();
    compose(&mut blocks, &annotation(&proposal(false), &original));
    assert!(matches!(&blocks[0],Block::Quote(q) if q.attribution.is_some()));
    assert_eq!(blocks.len(), 1);
}

#[test]
#[ignore = "uses current configured AI layout model for synthetic attribution fixtures"]
fn live_existing_quote_attribution_completion() {
    let mut settings = PluginSettings::load_default().unwrap();
    settings.semantic_layout.enabled = true;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for inside in [false, true] {
        let original = fixture(inside);
        let result = runtime
            .block_on(recognize(
                &original,
                &format!("attribution-live-{}", uuid::Uuid::new_v4()),
                &settings,
            ))
            .unwrap();
        assert!(result.annotations.iter().any(
            |a| matches!(a,Annotation::QuoteAttribution {inside:actual,..} if *actual==inside)
        ));
        let mut blocks = original.blocks.clone();
        for item in &result.annotations {
            compose(&mut blocks, item);
        }
        let Block::Quote(quote) = &blocks[0] else {
            unreachable!()
        };
        assert_eq!(
            text_block_text(quote.attribution.as_ref().unwrap()),
            "— Mira Vale"
        );
        assert_eq!(
            quote.body[0],
            unattributed_quote_body(&original.blocks[0]).unwrap()[0]
        );
        println!(
            "Existing quote attribution completed (inside={inside}) without changing its body"
        );
    }
}
