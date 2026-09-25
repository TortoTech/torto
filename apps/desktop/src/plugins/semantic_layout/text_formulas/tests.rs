use super::*;
use crate::plugins::semantic_layout::tests::{original_source, section, text};
use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};

fn formula_section() -> Section {
    let Block::Text(mut block) = text("p", "Relation: N ~ I0.301 continues.") else {
        panic!()
    };
    let run = |value: &str, italic, baseline| {
        Inline::Text(TextRun {
            text: value.into(),
            style: rebook_publication::TextStyle {
                italic,
                baseline,
                ..Default::default()
            },
            link: None,
        })
    };
    block.content = vec![
        run("Relation: ", false, TextBaseline::Normal),
        run("N", true, TextBaseline::Normal),
        run(" ~ ", false, TextBaseline::Normal),
        run("I", true, TextBaseline::Normal),
        run("0.301", false, TextBaseline::Superscript),
        run(" continues.", false, TextBaseline::Normal),
    ];
    section(vec![Block::Text(block)])
}
fn proposal() -> Proposal {
    Proposal {
        block: 0,
        paragraph: 0,
        original: "<i>N</i> ~ <i>I</i><sup>0.301</sup>".into(),
        latex: r"N \sim I^{0.301}".into(),
        before: String::new(),
        after: String::new(),
    }
}

#[test]
fn unified_request_uses_one_formatted_paragraph_for_all_roles() {
    let section = formula_section();
    let roles = RecognitionRoles {
        quotes: true,
        captions: true,
        headings: true,
    };
    let request = unified_window_input(&section, &roles, 0..1, 0..1, &[]);
    let block = &request["blocks"][0];
    assert!(block.get("text").is_none());
    assert!(block.get("citation_paragraphs").is_none());
    assert_eq!(block["math_texts"], json!(input(&section.blocks[0])));
}

#[test]
fn rejects_empty_flattened_and_detached_script_proposals() {
    let section = formula_section();
    for original in [
        "",
        "N ~ I0.301",
        "0.301",
        "<sup>0.301</sup>",
        "<i>N</i> ~ <i>I</i>",
        "<i>N</i> ~ <i>I</i><sup>0.3",
    ] {
        let p = Proposal {
            original: original.into(),
            latex: "1".into(),
            ..proposal()
        };
        let (accepted, skipped) = resolve(&section, 0..1, &[p]);
        assert!(accepted.is_empty(), "accepted {original}");
        assert_eq!(skipped, 1);
    }
    assert_eq!(resolve(&section, 0..1, &[proposal()]).1, 0);
}

#[test]
fn chained_relation_crosses_styled_runs_as_one_formula() {
    let mut section = formula_section();
    let Block::Text(t) = &mut section.blocks[0] else {
        panic!()
    };
    let run = |text: &str, italic, baseline| {
        Inline::Text(TextRun {
            text: text.into(),
            link: None,
            style: rebook_publication::TextStyle {
                italic,
                baseline,
                ..Default::default()
            },
        })
    };
    t.content = vec![
        run("Ratio: ", false, TextBaseline::Normal),
        run("Q/Q", true, TextBaseline::Normal),
        run("b", false, TextBaseline::Subscript),
        run(" = 0.5 = 8", false, TextBaseline::Normal),
        run("0.4", false, TextBaseline::Superscript),
        run("/", false, TextBaseline::Normal),
        run("s", true, TextBaseline::Normal),
        run("0.4", false, TextBaseline::Superscript),
        run(". Next sentence.", false, TextBaseline::Normal),
    ];
    let p = Proposal {
        original: "<i>Q/Q</i><sub>b</sub> = 0.5 = 8<sup>0.4</sup>/<i>s</i><sup>0.4</sup>".into(),
        latex: r"Q/Q_b = 0.5 = 8^{0.4}/s^{0.4}".into(),
        ..proposal()
    };
    let (annotations, skipped) = resolve(&section, 0..1, &[p]);
    assert_eq!(skipped, 0);
    assert_eq!(annotations.len(), 1);
    for a in &annotations {
        super::super::compose(&mut section.blocks, a);
    }
    let Block::Text(t) = &section.blocks[0] else {
        panic!()
    };
    assert_eq!(
        t.content
            .iter()
            .filter(|i| matches!(i, Inline::Math(_)))
            .count(),
        1
    );
}

#[test]
fn model_selected_formatted_span_preserves_text_styles_and_offsets() {
    let original = formula_section();
    let input = input(&original.blocks[0]);
    assert!(
        input[0]["text"]
            .as_str()
            .unwrap()
            .contains(&proposal().original)
    );
    let (annotations, skipped) = resolve(&original, 0..1, &[proposal()]);
    assert_eq!(skipped, 0);
    assert!(validate_annotations(&original, &annotations));
    let mut prepared = original.clone();
    for a in &annotations {
        super::super::compose(&mut prepared.blocks, a);
    }
    let Block::Text(text) = &prepared.blocks[0] else {
        panic!()
    };
    let Inline::Math(math) = &text.content[1] else {
        panic!("{:?}", text.content)
    };
    assert_eq!(math.original_text().unwrap(), "N ~ I0.301");
    assert_eq!(math.source_char_len(), 10);
    assert_eq!(
        text.source,
        match &original.blocks[0] {
            Block::Text(t) => t.source.clone(),
            _ => None,
        }
    );
    assert!(
        math.original
            .as_ref()
            .unwrap()
            .iter()
            .any(|r| r.style.baseline == TextBaseline::Superscript)
    );
    restore_originals(&mut prepared.blocks);
    assert_eq!(prepared, original);
}

#[test]
fn repeated_spans_require_context_and_invalid_overlaps_are_isolated() {
    let section = section(vec![text("p", "x = y and x = y")]);
    let mut p = Proposal {
        original: "x = y".into(),
        latex: "x=y".into(),
        ..proposal()
    };
    assert_eq!(resolve(&section, 0..1, &[p.clone()]).1, 1);
    p.before = " and ".into();
    assert_eq!(resolve(&section, 0..1, &[p.clone()]).1, 0);
    assert_eq!(resolve(&section, 0..1, &[p.clone(), p.clone()]).1, 2);
    p.latex = r"\frac{".into();
    assert_eq!(resolve(&section, 0..1, &[p]).1, 1);
}

#[test]
fn unique_formulas_ignore_redundant_context_without_losing_safety_checks() {
    let mut original = formula_section();
    let Block::Text(t) = &mut original.blocks[0] else {
        unreachable!()
    };
    let run = |text: &str, italic| {
        Inline::Text(TextRun {
            text: text.into(),
            link: None,
            style: rebook_publication::TextStyle {
                italic,
                ..Default::default()
            },
        })
    };
    t.content = vec![
        run("From ", false),
        run("v", true),
        run(" = d/t; so that", true),
        run(" v = d/t ", false),
        run("before and after", true),
    ];
    let proposals = [
        Proposal {
            original: "<i>v</i><i> = d/t".into(),
            latex: "v=d/t".into(),
            before: "From ".into(),
            after: "; so that".into(),
            ..proposal()
        },
        Proposal {
            original: "v = d/t".into(),
            latex: "v=d/t".into(),
            before: "so that ".into(),
            after: " <i>before and after</i>".into(),
            ..proposal()
        },
    ];
    let (annotations, skipped) = resolve(&original, 0..1, &proposals);
    assert_eq!(skipped, 0);
    let mut displayed = original.clone();
    for a in &annotations {
        super::super::compose(&mut displayed.blocks, a);
    }
    let Block::Text(t) = &displayed.blocks[0] else {
        unreachable!()
    };
    assert_eq!(
        t.content
            .iter()
            .filter(|i| matches!(i, Inline::Math(_)))
            .count(),
        2
    );

    let repeated = section(vec![text("p", "x = y and x = y")]);
    let p = Proposal {
        original: "x = y".into(),
        latex: "x=y".into(),
        before: "wrong".into(),
        ..proposal()
    };
    assert_eq!(resolve(&repeated, 0..1, &[p]).1, 1);
    let p = Proposal {
        original: "<sup>0.301</sup>".into(),
        latex: "0.301".into(),
        before: "wrong".into(),
        ..proposal()
    };
    assert_eq!(resolve(&formula_section(), 0..1, &[p]).1, 1);
}

#[test]
fn selected_citations_cannot_also_become_formulas() {
    let section = section(vec![text("p", "Evidence (Smith, 2020).")]);
    let p = Proposal {
        original: "(Smith, 2020)".into(),
        latex: r"\text{Smith, 2020}".into(),
        ..proposal()
    };
    let (accepted, skipped) =
        super::super::resolve_text_formulas(&section, 0..1, &[p], &["c0_0_0".into()]);
    assert!(accepted.is_empty());
    assert_eq!(skipped, 1);
}

#[test]
fn translation_restores_prepared_math_and_disabled_layout_restores_source_runs() {
    let original = formula_section();
    let (annotations, _) = resolve(&original, 0..1, &[proposal()]);
    let recognition = Recognition {
        fingerprint: fingerprint(&original),
        annotations,
        formulas_checked: true,
        skipped_groups: 0,
    };
    let mut prepared = original.clone();
    apply_translation_citations(&mut prepared, &recognition);
    let source = original_source(original.clone());
    let translation = Arc::new(TranslationBookSource::new(
        source.clone(),
        TranslationMode::Replace,
    ));
    translation.remember_formula_input(0, 0, &original.blocks[0], &prepared.blocks[0]);
    let inputs = crate::plugins::prepare_translation_inputs(&prepared, false);
    assert!(inputs[0].0.text.contains("<torto-math-0/>"));
    let overlay = SemanticLayoutSource::new(translation.clone(), source);
    assert!(overlay.install(0, recognition));
    translation.set_enabled(true).unwrap();
    translation
        .store_batch(
            0,
            &[BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "Translated <torto-math-0/> continues.".into(),
            }],
        )
        .unwrap();
    let displayed = overlay.parse_section(0).unwrap();
    let Block::Text(t) = &displayed.blocks[0] else {
        panic!()
    };
    assert_eq!(
        t.content
            .iter()
            .filter(|i| matches!(i, Inline::Math(_)))
            .count(),
        1
    );
    let mut settings = PluginSettings::default();
    settings.semantic_layout.enabled = false;
    overlay.configure("text-math", &settings);
    let disabled = overlay.parse_section(0).unwrap();
    let Block::Text(t) = &disabled.blocks[0] else {
        panic!()
    };
    assert!(!t.content.iter().any(|i| matches!(i, Inline::Math(_))));
    assert_eq!(text_block_text(t), "Translated N ~ I0.301 continues.");
    translation
        .store_batch(
            0,
            &[BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "Legacy translation without formula tokens.".into(),
            }],
        )
        .unwrap();
    let legacy = overlay.parse_section(0).unwrap();
    let Block::Text(t) = &legacy.blocks[0] else {
        panic!()
    };
    assert_eq!(
        text_block_text(t),
        "Legacy translation without formula tokens."
    );
}

#[test]
#[ignore = "requires TORTO_TEXT_FORMULA_BOOK local EPUB; no model requests"]
fn local_hearing_text_formulas_preserve_original_math_styles() {
    let book =
        rebook_formats::open_file(std::env::var("TORTO_TEXT_FORMULA_BOOK").unwrap()).unwrap();
    let source = book.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path() == "ops/xhtml/ch02.html")
        .unwrap();
    let original = source.parse_section(index).unwrap();
    let block=original.blocks.iter().find(|b|matches!(b,Block::Text(t) if text_block_text(t).starts_with("The tenfold rule specifies"))).unwrap().clone();
    let scoped = section(vec![block]);
    let mut first = proposal();
    first.before = "related by ".into();
    let mut last = proposal();
    last.before = "Since ".into();
    let third = Proposal {
        original: "2 = 10<sup>0.301</sup>".into(),
        latex: "2=10^{0.301}".into(),
        before: String::new(),
        after: String::new(),
        ..proposal()
    };
    let (annotations, skipped) = resolve(&scoped, 0..1, &[first, last, third]);
    assert_eq!(skipped, 0);
    assert!(validate_annotations(&scoped, &annotations));
    let mut rendered = scoped.clone();
    for annotation in &annotations {
        super::super::compose(&mut rendered.blocks, annotation);
    }
    let Block::Text(text) = &rendered.blocks[0] else {
        panic!()
    };
    assert_eq!(
        text.content
            .iter()
            .filter(|i| matches!(i, Inline::Math(_)))
            .count(),
        3
    );
    let inputs = crate::plugins::prepare_translation_inputs(&rendered, false);
    assert_eq!(inputs[0].0.text.matches("<torto-math-").count(), 3);
    restore_originals(&mut rendered.blocks);
    let Block::Text(restored) = &rendered.blocks[0] else {
        panic!()
    };
    let Block::Text(original) = &scoped.blocks[0] else {
        panic!()
    };
    let characters = |text: &TextBlock| {
        text.content
            .iter()
            .flat_map(|inline| match inline {
                Inline::Text(run) => run
                    .text
                    .chars()
                    .map(|ch| (ch, run.style, run.link.clone()))
                    .collect::<Vec<_>>(),
                _ => panic!("expected restored text"),
            })
            .collect::<Vec<_>>()
    };
    assert!(
        characters(restored) == characters(original),
        "every original character and style must survive; run boundaries may be split"
    );
    assert_eq!(restored.source, original.source);
    assert_eq!(restored.style, original.style);
    println!(
        "Photographed chapter paragraph: three styled expressions mapped without changing original source or text"
    );
}
