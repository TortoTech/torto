use super::*;
use crate::plugins::semantic_layout::tests::{original_source, section, text};
use crate::plugins::{BlockTranslation, TranslationBookSource, TranslationMode};

#[test]
fn hmm_book_formula_quotes_become_display_formulas_and_restore_when_disabled() {
    let descriptor = rebook_publication::SpineItem {
        id: rebook_publication::SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    // Text Entry Systems, formula15 / formula16: centered text formulas, not images.
    let original = rebook_html::parse_section(r#"<html><head><style>
        .fig {margin-top:2em;margin-bottom:0.5em;padding-left:5px;padding-right:5px;padding-top:5px;text-align:center}
        </style></head><body>
        <p class="fig"><a id="formula15"/><em>P</em>(article | prep)<em>P</em>(“the” | article)<em>P</em>(noun | article)<em>P</em>(“sky” | noun),</p>
        <p>whereas that of “tie sly” is obtained by</p>
        <p class="fig"><a id="formula16"/><em>P</em>(verb | prep) <em>P</em>(“tie” | verb) <em>P</em>(adjective | verb) <em>P</em>(“sly” | adjective) + <em>P</em>(noun | prep) <em>P</em>(“tie” | noun) <em>P</em>(adjective | noun) <em>P</em>(“sly” | adjective),</p>
        </body></html>"#, &descriptor, |_| None).unwrap();
    let latex = [
        r"P(\mathrm{article}\mid\mathrm{prep})P(\text{the}\mid\mathrm{article})P(\mathrm{noun}\mid\mathrm{article})P(\text{sky}\mid\mathrm{noun})",
        r"P(\mathrm{verb}\mid\mathrm{prep})P(\text{tie}\mid\mathrm{verb})P(\mathrm{adjective}\mid\mathrm{verb})P(\text{sly}\mid\mathrm{adjective})+P(\mathrm{noun}\mid\mathrm{prep})P(\text{tie}\mid\mathrm{noun})P(\mathrm{adjective}\mid\mathrm{noun})P(\text{sly}\mid\mathrm{adjective})",
    ];
    let mut proposals = Vec::new();
    let mut originals = Vec::new();
    for (block, latex) in [0, 2].into_iter().zip(latex) {
        let Block::Quote(q) = &original.blocks[block] else {
            panic!("fixture should reproduce quote inference")
        };
        originals.push(q.body[0].clone());
        proposals.push(Proposal {
            block,
            paragraph: 0,
            original: encode(&q.body[0]).value.trim_end_matches(',').into(),
            latex: latex.into(),
            before: String::new(),
            after: ",".into(),
        });
    }
    let (annotations, skipped) = resolve(&original, 0..original.blocks.len(), &proposals);
    assert_eq!(skipped, 0);
    assert_eq!(annotations.len(), 2);
    let source = original_source(original.clone());
    let overlay = SemanticLayoutSource::new(source.clone(), source);
    assert!(overlay.install(
        0,
        Recognition {
            fingerprint: fingerprint(&original),
            annotations,
            formulas_checked: true,
            skipped_groups: 0
        }
    ));
    let displayed = overlay.parse_section(0).unwrap();
    let mut engine = rebook_layout::LayoutEngine::new();
    let layout = engine
        .layout_blocks(
            &overlay,
            &displayed.blocks,
            rebook_layout::LayoutViewport::new(800, 1200).unwrap(),
            &rebook_layout::ReaderStyle {
                typesetting: rebook_layout::ReaderTypesetting::unified(),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !layout
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .any(|item| matches!(item, rebook_layout::PageItem::Quote(_)))
    );
    let rendered_formulas = layout
        .pages
        .iter()
        .flat_map(|p| &p.items)
        .filter_map(|item| {
            if let rebook_layout::PageItem::Text(t) = item {
                Some(t.inline_images.len())
            } else {
                None
            }
        })
        .sum::<usize>();
    assert_eq!(rendered_formulas, 2);
    for (index, original) in [0, 2].into_iter().zip(originals) {
        let Block::Text(t) = &displayed.blocks[index] else {
            panic!("quote wrapper remains")
        };
        assert_eq!(t.kind, TextBlockKind::Paragraph);
        assert_eq!(t.source, original.source);
        assert!(rebook_layout::is_display_formula(t));
        let bounds = layout
            .pages
            .iter()
            .flat_map(|page| {
                rebook_renderer::DisplayListCompiler
                    .compile(page)
                    .image_source_rects(std::slice::from_ref(t.source.as_ref().unwrap()))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            bounds.len(),
            1,
            "text formula must expose a single image activation outline"
        );
        assert!(bounds[0].width() > 0.0 && bounds[0].height() > 0.0);
        let [Inline::Math(math)] = t.content.as_slice() else {
            panic!()
        };
        assert!(math.display);
        assert!(math.latex.ends_with(r"\text{,}"));
        assert_eq!(math.original_text().unwrap(), text_block_text(&original));
    }
    let mut settings = PluginSettings::default();
    settings.semantic_layout.enabled = false;
    overlay.configure("formula-quotes", &settings);
    assert_eq!(overlay.parse_section(0).unwrap().blocks, original.blocks);
}

#[test]
fn formula_quote_conversion_is_atomic_and_preserves_prose_and_attribution() {
    let Block::Text(mut body) = text("formula", "x=y,") else {
        panic!()
    };
    apply(
        &mut body,
        &[Span {
            start: 0,
            end: 3,
            original: "x=y".into(),
            latex: "x=y".into(),
        }],
    );
    let Block::Text(prose) = text("prose", "An explanation.") else {
        panic!()
    };
    for quote in [
        QuoteBlock {
            body: vec![body.clone(), prose.clone()],
            attribution: None,
            source: None,
        },
        QuoteBlock {
            body: vec![body.clone()],
            attribution: Some(prose),
            source: None,
        },
    ] {
        let mut blocks = vec![Block::Quote(quote.clone())];
        normalize_formula_quotes(&mut blocks);
        assert_eq!(blocks, vec![Block::Quote(quote)]);
    }
    let mut blocks = vec![Block::Quote(QuoteBlock {
        body: vec![body.clone(), body],
        attribution: None,
        source: None,
    })];
    normalize_formula_quotes(&mut blocks);
    assert_eq!(blocks.len(), 2);
    assert!(
        blocks
            .iter()
            .all(|b| matches!(b, Block::Text(t) if rebook_layout::is_display_formula(t)))
    );
    let once = blocks.clone();
    normalize_formula_quotes(&mut blocks);
    assert_eq!(blocks, once);
}

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
