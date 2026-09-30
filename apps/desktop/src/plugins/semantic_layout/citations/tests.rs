use super::super::tests::{section, text};
use super::*;
use rebook_publication::TextRun;

fn assert_prepared_note_translation(original: Section) {
    let Block::Text(block) = &original.blocks[0] else {
        panic!()
    };
    let raw_notes = rebook_layout::paragraph_footnotes(block);
    assert_eq!(raw_notes.len(), 1);
    let recognition = Recognition {
        fingerprint: fingerprint(&original),
        annotations: vec![Annotation::InlineCitations {
            source: block.source.clone().unwrap(),
            spans: candidates(block)
                .into_iter()
                .filter(|span| span.text.contains("figure 3.11"))
                .collect(),
        }],
        formulas_checked: true,
        skipped_groups: 0,
    };
    let mut prepared = original.clone();
    apply_translation_citations(&mut prepared, &recognition);
    let Block::Text(prepared_text) = &prepared.blocks[0] else {
        panic!()
    };
    let prepared_notes = rebook_layout::paragraph_footnotes(prepared_text);
    assert_ne!(
        raw_notes[0].start, prepared_notes[0].start,
        "citation preparation must split source runs"
    );
    let note_id = prepared_notes[0].start;
    for mode in [
        crate::plugins::TranslationMode::Replace,
        crate::plugins::TranslationMode::Bilingual,
    ] {
        let source = super::super::tests::original_source(original.clone());
        let translation = Arc::new(crate::plugins::TranslationBookSource::new(
            source.clone(),
            mode,
        ));
        translation.remember_translation_input(0, 0, &original.blocks[0], &prepared.blocks[0]);
        let overlay = SemanticLayoutSource::new(translation.clone(), source);
        assert!(overlay.install(0, recognition.clone()));
        for translated in [
            format!("译文正文<citation id=\"1\">图3.11</citation>。<torto-note-{note_id}/>"),
            format!(
                "译文正文<citation id=\"1\">图3.11</citation>。<torto-note-{note_id}/>后续句子。"
            ),
        ] {
            let input = crate::plugins::prepare_translation_inputs(&prepared, false);
            assert!(
                crate::plugins::translation::validate_translation_footnotes(
                    &input[0].0.text,
                    &translated
                )
                .is_ok()
            );
            translation
                .store_batch(
                    0,
                    &[crate::plugins::BlockTranslation {
                        block_index: 0,
                        segment_index: None,
                        text: translated,
                    }],
                )
                .unwrap();
            translation.set_enabled(true).unwrap();
            let displayed = overlay.parse_section(0).unwrap();
            let Block::Text(text) = displayed.blocks.last().unwrap() else {
                panic!()
            };
            assert!(
                !text_block_text(text).contains("torto-note"),
                "footnote source identity was resolved against the wrong inline snapshot: {}",
                text_block_text(text)
            );
            let notes = rebook_layout::paragraph_footnotes(text);
            assert_eq!(notes.len(), 1);
            let identity = |inlines: &[Inline]| {
                inlines
                    .iter()
                    .filter_map(|inline| match inline {
                        Inline::Text(run) => Some((
                            run.text.clone(),
                            run.link.clone(),
                            run.style.link_role,
                            run.style.baseline,
                        )),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                identity(&text.content[notes[0].clone()]),
                identity(&block.content[raw_notes[0].clone()])
            );
            assert!(text_block_text(text).starts_with("译文正文"));
        }
    }
}

#[test]
fn translated_tail_note_survives_citation_run_splitting() {
    let Block::Text(mut block) = text("tail-note", "Body (figure 3.11).52") else {
        panic!()
    };
    let Inline::Text(template) = block.content[0].clone() else {
        panic!()
    };
    block.content = (0..20)
        .map(|index| {
            Inline::Text(TextRun {
                text: format!("Part {index}. "),
                ..template.clone()
            })
        })
        .collect();
    block.content.push(Inline::Text(TextRun {
        text: "Body (figure 3.11).".into(),
        ..template.clone()
    }));
    block.content.push(Inline::Text(TextRun {
        text: "52".into(),
        link: Some(PublicationUrl::parse("notes.xhtml#en231").unwrap()),
        style: rebook_publication::TextStyle {
            baseline: rebook_publication::TextBaseline::Superscript,
            link_role: rebook_publication::LinkRole::FootnoteReference,
            ..Default::default()
        },
    }));
    block.source.as_mut().unwrap().end.text_offset =
        text_block_text(&block).chars().count().try_into().unwrap();
    assert_prepared_note_translation(section(vec![Block::Text(block)]));
}

#[test]
#[ignore = "requires TORTO_PERF_BOOK; offline Chinese Computer figure 3.11 footnote regression"]
fn local_chinese_computer_translated_tail_note() {
    let book = rebook_formats::open_file(std::path::PathBuf::from(
        std::env::var_os("TORTO_PERF_BOOK").unwrap(),
    ))
    .unwrap();
    let source = book.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.to_string().ends_with("10992_Mullaney-0008.xhtml"))
        .unwrap();
    let mut original = source.parse_section(index).unwrap();
    let target = original.blocks.iter().find(|block| matches!(block, Block::Text(text) if text_block_text(text).contains("Divisible type") && text_block_text(text).contains("1830s"))).unwrap().clone();
    original.blocks = vec![target];
    assert_prepared_note_translation(original);
}

#[test]
fn citations_keep_original_text_and_number_across_style_runs() {
    let Block::Text(mut t) = text("p", "Some evidence (Smith, 2020) and more (Jones 23–25).")
    else {
        panic!()
    };
    let original = text_block_text(&t);
    let found = candidates(&t);
    assert_eq!(found.len(), 2);
    let Inline::Text(r) = t.content[0].clone() else {
        panic!()
    };
    let split = original.find("2020").unwrap();
    let mut first = r.clone();
    first.text = original[..split].into();
    let mut second = r;
    second.text = original[split..].into();
    second.style.italic = true;
    t.content = vec![Inline::Text(first), Inline::Text(second)];
    apply(&mut t, &found);
    assert_eq!(text_block_text(&t), original);
    assert!(
        t.content
            .iter()
            .any(|i| matches!(i,Inline::Text(r) if r.style.inline_citation==1 && r.style.italic))
    );
    assert!(
        t.content
            .iter()
            .any(|i| matches!(i,Inline::Text(r) if r.style.inline_citation==2))
    );
}

#[test]
fn candidates_protect_footnotes_and_narrative_years() {
    let Block::Text(mut t) = text(
        "p",
        "Smith (2020) argues this [12] (ordinary aside) (张三，2020).",
    ) else {
        panic!()
    };
    let c = candidates(&t);
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].text, "[12]");
    let Inline::Text(r) = &mut t.content[0] else {
        panic!()
    };
    r.style.inline_role = InlineRole::Footnote;
    assert!(candidates(&t).is_empty());
}

#[test]
fn cached_citations_validate_their_source_and_ranges() {
    let b = text("p", "Evidence (Smith, 2020).");
    let Block::Text(t) = &b else { panic!() };
    let a = Annotation::InlineCitations {
        source: t.source.clone().unwrap(),
        spans: candidates(t),
    };
    let s = section(vec![b]);
    let cached: Annotation = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert!(validate_annotations(&s, std::slice::from_ref(&cached)));
    let source = super::super::tests::original_source(s.clone());
    let overlay = SemanticLayoutSource::new(source.clone(), source.clone());
    assert!(overlay.install(
        0,
        Recognition {
            formulas_checked: true,
            fingerprint: fingerprint(&s),
            annotations: vec![cached.clone()],
            skipped_groups: 0
        }
    ));
    let displayed = overlay.parse_section(0).unwrap();
    assert_ne!(displayed.blocks, s.blocks);
    overlay.clear();
    assert_eq!(overlay.parse_section(0).unwrap().blocks, s.blocks);
    let mut changed = s.clone();
    changed.blocks[0] = text("p", "Other text (Jones, 1990).");
    assert!(!validate_annotations(&changed, &[cached]));
}

#[test]
#[ignore = "requires TORTO_CITATION_BOOK; uses the configured gemini/lite model on the photographed paragraph"]
fn live_computational_models_inline_citations() {
    let opened = rebook_formats::open_file(std::env::var("TORTO_CITATION_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("09_chapter2.xhtml"))
        .unwrap();
    let mut s = source.parse_section(index).unwrap();
    let target=s.blocks.iter().find(|b| matches!(b,Block::Text(t) if text_block_text(t).contains("Lewandowsky, 1991") && text_block_text(t).contains("McClosky"))).unwrap().clone();
    s.blocks = vec![target];
    let settings = PluginSettings::load_default().unwrap();
    let provider = settings
        .providers
        .iter()
        .find(|p| p.models.iter().any(|m| m.id == "gemini/lite"))
        .unwrap();
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(recognize(
            &reqwest::Client::new(),
            provider,
            "gemini/lite",
            &s,
        ))
        .unwrap();
    assert!(validate_annotations(&s, &result));
    assert_eq!(result.len(), 1);
    let Annotation::InlineCitations {
        source: range,
        spans,
    } = &result[0]
    else {
        panic!()
    };
    for c in spans {
        println!("citation: {}", c.text);
    }
    assert_eq!(spans.len(), 4);
    for (c, name) in spans
        .iter()
        .zip(["Rumelhart", "Crick", "Hebb", "Lewandowsky"])
    {
        assert!(c.text.contains(name));
    }
    let mut blocks = s.blocks.clone();
    compose(&mut blocks, range, spans);
    let Block::Text(before) = &s.blocks[0] else {
        panic!()
    };
    let Block::Text(after) = &blocks[0] else {
        panic!()
    };
    assert_eq!(text_block_text(before), text_block_text(after));
    assert_eq!(after.source, before.source);
    let numbered: Vec<_> = after
        .content
        .iter()
        .filter_map(|i| match i {
            Inline::Text(r) if r.style.inline_citation > 0 => Some(r.style.inline_citation),
            _ => None,
        })
        .collect();
    for n in 1..=4 {
        assert!(numbered.contains(&n));
    }
    println!(
        "Verified four citation groups; ordinary asides and original paragraph text preserved"
    );
}

fn marked_citations(block: &TextBlock) -> Vec<(u32, String)> {
    let mut result: Vec<(u32, String)> = Vec::new();
    for inline in &block.content {
        if let Inline::Text(run) = inline
            && run.style.inline_citation > 0
        {
            if let Some((number, text)) = result.last_mut()
                && *number == run.style.inline_citation
            {
                text.push_str(&run.text);
            } else {
                result.push((run.style.inline_citation, run.text.clone()));
            }
        }
    }
    result
}

#[test]
fn translated_photographed_citations_keep_all_three_markers() {
    let Block::Text(original) = text(
        "p",
        "Model (Pacht & Rayner, 1993; Rayner & Duffy, 1986; Rayner & Frazier, 1989; Sereno, Pacht, & Rayner, 1992). Context (e.g., Onifer & Swinney, 1981; Swinney, 1979). Access (e.g., Glucksberg, Kreuz, & Rho, 1986; Van Petten & Kutas, 1987).",
    ) else {
        panic!()
    };
    let spans = candidates(&original);
    let translated = "\u{6a21}\u{578b} (Pacht & Rayner, 1993; Rayner & Duffy, 1986; Rayner & Frazier, 1989;  Sereno, Pacht, & Rayner, 1992)\u{3002}\u{8bed}\u{5883}\u{ff08}\u{4f8b}\u{5982}\u{ff0c} Onifer & Swinney, 1981;  Swinney, 1979\u{ff09}\u{3002}\u{8bbf}\u{95ee} (\u{4f8b}\u{5982}, Glucksberg, Kreuz, & Rho, 1986\u{ff1b} Van Petten & Kutas, 1987)\u{3002}";
    let Block::Text(mut target) = text("p", translated) else {
        panic!()
    };
    // Translation may introduce independent bold/italic runs within a citation.
    let Inline::Text(run) = target.content[0].clone() else {
        panic!()
    };
    let split = run.text.find("Swinney").unwrap();
    let mut head = run.clone();
    head.text = run.text[..split].into();
    let mut tail = run.clone();
    tail.text = run.text[split..].into();
    tail.style.bold = true;
    target.content = vec![Inline::Text(head), Inline::Text(tail)];
    apply(&mut target, &spans);
    assert_eq!(text_block_text(&target), translated);
    assert_eq!(
        marked_citations(&target)
            .iter()
            .map(|(n, _)| *n)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(target.content.iter().any(|inline| matches!(inline,Inline::Text(run) if run.style.bold && run.style.inline_citation==2)));
    let once = target.clone();
    apply(&mut target, &spans);
    assert_eq!(target, once);
    // Bilingual translation companions carry no independent source range.
    let mut companion = text("p", translated);
    if let Block::Text(t) = &mut companion {
        t.source = None;
    }
    let mut blocks = vec![Block::Text(original.clone()), companion];
    compose(&mut blocks, original.source.as_ref().unwrap(), &spans);
    for block in blocks {
        let Block::Text(t) = block else { panic!() };
        assert_eq!(marked_citations(&t).len(), 3);
    }
}

#[test]
fn unmatched_or_changed_citation_does_not_disable_other_citations() {
    let Block::Text(original) = text("p", "A (Smith, 2020), B (Jones, 2021), C (Brown, 2022).")
    else {
        panic!()
    };
    let Block::Text(mut target) = text(
        "p",
        "Translated (Brown, 2022), changed (Jones, 2023), missing Smith.",
    ) else {
        panic!()
    };
    let before = text_block_text(&target);
    apply(&mut target, &candidates(&original));
    assert_eq!(marked_citations(&target), [(1, "(Brown, 2022)".into())]);
    assert_eq!(text_block_text(&target), before);
}

#[test]
fn ambiguous_translations_and_protected_footnotes_stay_unmarked() {
    let Block::Text(original) = text("p", "Evidence (e.g., Smith, 2020). Other (Jones, 2021).")
    else {
        panic!()
    };
    let Block::Text(mut target) = text(
        "p",
        "Translated (Smith, 2020) and (Smith, 2020), plus (Jones, 2021).",
    ) else {
        panic!()
    };
    apply(&mut target, &candidates(&original));
    assert_eq!(marked_citations(&target), [(1, "(Jones, 2021)".into())]);
    let Block::Text(mut protected) =
        text("p", "Evidence (e.g., Smith, 2020). Other (Jones, 2021).")
    else {
        panic!()
    };
    let Inline::Text(run) = &mut protected.content[0] else {
        panic!()
    };
    run.style.inline_role = InlineRole::Footnote;
    apply(&mut protected, &candidates(&original));
    assert!(marked_citations(&protected).is_empty());
}

#[test]
fn matched_citations_are_renumbered_in_translated_order() {
    let Block::Text(original) = text("p", "First (Smith, 2020), then (Jones, 2021).") else {
        panic!()
    };
    let Block::Text(mut target) = text("p", "Translation first (Jones, 2021), then (Smith, 2020).")
    else {
        panic!()
    };
    apply(&mut target, &candidates(&original));
    assert_eq!(
        marked_citations(&target),
        [(1, "(Jones, 2021)".into()), (2, "(Smith, 2020)".into())]
    );
}

#[test]
fn translation_pipeline_restores_tagged_citations_and_toggle_hides_only_markers() {
    let original = section(vec![text("p", "Claim (Smith, 2020).")]);
    let Block::Text(block) = &original.blocks[0] else {
        panic!()
    };
    let recognition = Recognition {
        formulas_checked: true,
        fingerprint: fingerprint(&original),
        annotations: vec![Annotation::InlineCitations {
            source: block.source.clone().unwrap(),
            spans: candidates(block),
        }],
        skipped_groups: 0,
    };
    let source = super::super::tests::original_source(original.clone());
    let translation = Arc::new(crate::plugins::TranslationBookSource::new(
        source.clone(),
        crate::plugins::TranslationMode::Replace,
    ));
    let semantic = SemanticLayoutSource::new(translation.clone(), source);
    let mut input = original.clone();
    apply_translation_citations(&mut input, &recognition);
    let inputs = crate::plugins::prepare_translation_inputs(&input, false);
    assert!(inputs[0].0.text.contains("<citation id=\"1\">"));
    translation
        .store_batch(
            0,
            &[crate::plugins::BlockTranslation {
                block_index: 0,
                segment_index: None,
                text: "Translated <citation id=\"1\">(Author translated, 2020)</citation>.".into(),
            }],
        )
        .unwrap();
    translation.set_enabled(true).unwrap();
    assert!(semantic.install(0, recognition));
    let displayed = semantic.parse_section(0).unwrap();
    let Block::Text(block) = &displayed.blocks[0] else {
        panic!()
    };
    assert_eq!(
        marked_citations(block),
        [(1, "(Author translated, 2020)".into())]
    );
    let mut settings = PluginSettings::default();
    settings.semantic_layout.enabled = false;
    semantic.configure("citation-test", &settings);
    let disabled = semantic.parse_section(0).unwrap();
    let Block::Text(disabled) = &disabled.blocks[0] else {
        panic!()
    };
    assert!(marked_citations(disabled).is_empty());
    assert_eq!(text_block_text(block), text_block_text(disabled));
}
