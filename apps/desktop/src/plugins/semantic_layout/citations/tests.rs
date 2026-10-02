use super::super::tests::{section, text};
use super::*;
use rebook_publication::TextRun;

#[test]
fn heuristic_author_groups_handle_multiple_works_years_and_name_periods() {
    for value in [
        "(Just & Carpenter, 1992; Just & Varma, 2002)",
        "(McClelland et al., 1989; St. John & McClelland, 1990, 1992)",
        "(Smith,2020a, pp. 12–15)",
        "(van der Waals, 1910)",
        "(Smith, Jones, & Brown, 2020)",
        "（张三等，2020；李四与王五，2021）",
        "（张三，2020，2021）",
        "(e.g., Smith & Jones, 2020)",
    ] {
        assert!(heuristics::author_date(value), "{value}");
    }
    for value in [
        "(2020)",
        "(in 2020)",
        "(born in 2020)",
        "(updated 2020)",
        "(May, 2020)",
        "(Figure 2.3)",
        "(Table, 2020)",
        "(Chapter 2, 2020)",
        "(Smith 23–25)",
        "[1, 3–5]",
        "(P(W), 2020)",
        "(Smith, 2020, this is explanatory prose)",
    ] {
        assert!(!heuristics::author_date(value), "{value}");
    }
}

#[test]
fn unified_heuristics_work_without_ai_and_book_mode_preserves_original_text() {
    let original = section(vec![text(
        "p",
        "Claim (Just & Carpenter, 1992; Just & Varma, 2002). More (McClelland et al., 1989; St. John & McClelland, 1990, 1992).",
    )]);
    let inner = super::super::tests::original_source(original.clone());
    let source = SemanticLayoutSource::new(inner.clone(), inner);
    source.configure("local-citations", &PluginSettings::default());
    source.set_unified_citations(true);
    for _ in 0..2 {
        let displayed = source.parse_section(0).unwrap();
        let Block::Text(block) = &displayed.blocks[0] else {
            panic!()
        };
        assert_eq!(
            marked_citations(block),
            [
                (1, "(Just & Carpenter, 1992; Just & Varma, 2002)".into()),
                (
                    2,
                    "(McClelland et al., 1989; St. John & McClelland, 1990, 1992)".into()
                ),
            ]
        );
        assert_eq!(
            text_block_text(block),
            text_block_text(match &original.blocks[0] {
                Block::Text(t) => t,
                _ => unreachable!(),
            })
        );
    }
    source.set_unified_citations(false);
    assert_eq!(source.parse_section(0).unwrap(), original);
}

#[test]
fn numeric_heuristics_require_bibliographic_evidence_and_reject_array_context() {
    let mut original = section(vec![
        text("cue", "See [12] and cf. [1, 3–5]."),
        text(
            "plain",
            "A value [12]. Smith (2020) argues. An array [1, 2, 3].",
        ),
        text("math", "See array [12]."),
        text("linked", "A supported claim [24]."),
    ]);
    if let Block::Text(block) = &mut original.blocks[3] {
        if let Inline::Text(run) = &mut block.content[0] {
            run.link = Some(PublicationUrl::parse("references.xhtml#ref24").unwrap());
        }
    }
    citations::apply_heuristic_fallback(&mut original);
    let marked: Vec<_> = original
        .blocks
        .iter()
        .map(|block| match block {
            Block::Text(text) => marked_citations(text),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(marked[0], [(1, "[12]".into()), (2, "[1, 3–5]".into())]);
    assert!(marked[1].is_empty());
    assert!(marked[2].is_empty());
    assert_eq!(marked[3], [(1, "[24]".into())]);
}

#[test]
fn heuristic_and_ai_citations_merge_in_text_order_without_duplicate_marks() {
    let original = section(vec![text(
        "p",
        "Claim (Smith, 2020), supported (Jones 23–25).",
    )]);
    let Block::Text(block) = &original.blocks[0] else {
        panic!()
    };
    let inner = super::super::tests::original_source(original.clone());
    let source = SemanticLayoutSource::new(inner.clone(), inner);
    source.set_unified_citations(true);
    assert!(source.install(
        0,
        Recognition {
            fingerprint: fingerprint(&original),
            formulas_checked: true,
            skipped_groups: 0,
            annotations: vec![Annotation::InlineCitations {
                source: block.source.clone().unwrap(),
                spans: candidates(block),
            }],
        }
    ));
    let displayed = source.parse_section(0).unwrap();
    let Block::Text(block) = &displayed.blocks[0] else {
        panic!()
    };
    assert_eq!(
        marked_citations(block),
        [(1, "(Smith, 2020)".into()), (2, "(Jones 23–25)".into())]
    );
    let mut mixed = original;
    if let Block::Text(block) = &mut mixed.blocks[0] {
        let spans: Vec<_> = candidates(block)
            .into_iter()
            .filter(|c| c.text.contains("Jones"))
            .collect();
        apply(block, &spans);
    }
    citations::apply_heuristic_fallback(&mut mixed);
    let Block::Text(block) = &mixed.blocks[0] else {
        panic!()
    };
    assert_eq!(
        marked_citations(block),
        [(1, "(Smith, 2020)".into()), (2, "(Jones 23–25)".into())]
    );
}

#[test]
fn heuristic_translation_placeholders_survive_without_ai_layout_in_both_modes() {
    for mode in [
        crate::plugins::TranslationMode::Replace,
        crate::plugins::TranslationMode::Bilingual,
    ] {
        let mut original = section(vec![text(
            "p",
            "Claim (Just & Carpenter, 1992; Just & Varma, 2002).",
        )]);
        if let Block::Text(block) = &mut original.blocks[0] {
            block.content.push(Inline::Text(TextRun {
                text: "52".into(),
                style: rebook_publication::TextStyle {
                    baseline: rebook_publication::TextBaseline::Superscript,
                    link_role: LinkRole::FootnoteReference,
                    ..Default::default()
                },
                link: Some(PublicationUrl::parse("notes.xhtml#en52").unwrap()),
            }));
        }
        let inner = super::super::tests::original_source(original.clone());
        let translation = Arc::new(crate::plugins::TranslationBookSource::new(
            inner.clone(),
            mode,
        ));
        let source = SemanticLayoutSource::new(translation.clone(), inner);
        source.configure("heuristic-translation", &PluginSettings::default());
        source.set_unified_citations(true);
        let mut prepared = original.clone();
        source.prepare_citation_input(0, &fingerprint(&original), &mut prepared);
        let inputs = crate::plugins::prepare_translation_inputs(&prepared, false);
        assert!(inputs[0].0.text.contains("<citation id=\"1\">"));
        let Block::Text(prepared_text) = &prepared.blocks[0] else {
            panic!()
        };
        let note_id = rebook_layout::paragraph_footnotes(prepared_text)[0].start;
        translation.remember_translation_input(0, 0, &original.blocks[0], &prepared.blocks[0]);
        translation.store_batch(0, &[crate::plugins::BlockTranslation {
            block_index: 0, segment_index: None,
                    text: format!("论述<citation id=\"1\">贾斯特与卡彭特，1992；贾斯特与瓦尔马，2002</citation>。<t-note-{note_id}/>"),
        }]).unwrap();
        translation.set_enabled(true).unwrap();
        let displayed = source.parse_section(0).unwrap();
        let paragraphs: Vec<_> = displayed
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(t) => Some(t),
                _ => None,
            })
            .collect();
        assert!(
            paragraphs
                .iter()
                .all(|paragraph| marked_citations(paragraph).len() == 1)
        );
        assert!(
            paragraphs
                .iter()
                .all(|paragraph| rebook_layout::paragraph_footnotes(paragraph).len() == 1)
        );
        assert!(
            paragraphs
                .iter()
                .any(|paragraph| marked_citations(paragraph)
                    == [(1, "贾斯特与卡彭特，1992；贾斯特与瓦尔马，2002".into())])
        );
        source.set_unified_citations(false);
        assert!(
            source
                .parse_section(0)
                .unwrap()
                .blocks
                .iter()
                .all(|block| match block {
                    Block::Text(t) => marked_citations(t).is_empty(),
                    _ => true,
                })
        );
    }
}

#[test]
fn heuristic_fallback_handles_legacy_bilingual_companions_without_sources() {
    let mut original = section(vec![
        text("p", "Claim (Smith, 2020)."),
        text("translated", "论述（史密斯，2020）。"),
    ]);
    if let Block::Text(block) = &mut original.blocks[1] {
        block.source = None;
    }
    apply_heuristic_fallback(&mut original);
    for block in &original.blocks {
        let Block::Text(block) = block else { panic!() };
        assert_eq!(marked_citations(block).len(), 1);
    }
}

#[test]
fn authored_unbracketed_references_and_split_styles_keep_their_full_group() {
    let mut original = section(vec![text(
        "p",
        "Just & Carpenter, 1992; Just & Varma, 2002",
    )]);
    let Block::Text(block) = &mut original.blocks[0] else {
        panic!()
    };
    let Inline::Text(run) = &block.content[0] else {
        panic!()
    };
    let mut first = run.clone();
    first.text = "Just & Carpenter, 1992; ".into();
    first.style.citation = true;
    let mut second = run.clone();
    second.text = "Just & Varma, 2002".into();
    second.style.citation = true;
    second.style.bold = true;
    block.content = vec![Inline::Text(first), Inline::Text(second)];
    apply_heuristic_fallback(&mut original);
    let Block::Text(block) = &original.blocks[0] else {
        panic!()
    };
    assert_eq!(
        marked_citations(block),
        [(1, "Just & Carpenter, 1992; Just & Varma, 2002".into())]
    );
    assert!(block.content.iter().any(|inline| matches!(inline, Inline::Text(run) if run.style.bold && run.style.inline_citation == 1)));
}

#[test]
fn heuristic_cache_rechecks_rewritten_source_text() {
    let original = section(vec![text("p", "Claim (Smith, 2020).")]);
    let inner = Arc::new(crate::plugins::rewrite::RewriteBookSource::new(
        super::super::tests::original_source(original),
    ));
    let source = SemanticLayoutSource::new(inner.clone(), inner.clone());
    source.configure("rewrite-citations", &PluginSettings::default());
    source.set_unified_citations(true);
    assert!(
        text_block_text(match &source.parse_section(0).unwrap().blocks[0] {
            Block::Text(t) => t,
            _ => unreachable!(),
        })
        .contains("Smith")
    );
    inner
        .apply_rewrites(&[crate::plugins::rewrite::BlockRewrite {
            section_index: 0,
            block_id: "p".into(),
            text: "Claim (Jones, 2021).".into(),
        }])
        .unwrap();
    let displayed = source.parse_section(0).unwrap();
    let Block::Text(block) = &displayed.blocks[0] else {
        panic!()
    };
    assert_eq!(marked_citations(block), [(1, "(Jones, 2021)".into())]);
}

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
            format!("译文正文<citation id=\"1\">图3.11</citation>。<t-note-{note_id}/>"),
            format!("译文正文<citation id=\"1\">图3.11</citation>。<t-note-{note_id}/>后续句子。"),
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
                !text_block_text(text).contains("t-note"),
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
            &crate::http::client(),
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
