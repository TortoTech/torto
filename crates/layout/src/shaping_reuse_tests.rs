// Included in the layout tests to use their source fixtures.
fn reuse_test_block(value: &str) -> TextBlock {
    TextBlock {
        kind: TextBlockKind::Paragraph,
        content: vec![Inline::Text(TextRun {
            text: value.into(),
            style: TextStyle::default(),
            link: None,
        })],
        style: BlockStyle::default(),
        source: None,
    }
}

fn reuse_test_geometry(text: &PreparedText) -> Vec<String> {
    text.layout
        .lines()
        .map(|line| {
            let mut value = format!("{:?} {:?}", line.text_range(), line.metrics());
            for item in line.items() {
                match item {
                    crate::text_layout::PositionedLayoutItem::GlyphRun(run) => {
                        use std::fmt::Write as _;
                        write!(
                            value,
                            " {:?} {:?} {:?}",
                            run.style(),
                            run.run().normalized_coords(),
                            run.positioned_glyphs().collect::<Vec<_>>()
                        )
                        .unwrap();
                    }
                    crate::text_layout::PositionedLayoutItem::InlineBox(_) => {
                        panic!("plain cell has a box")
                    }
                }
            }
            value
        })
        .collect()
}

#[test]
fn rtl_translation_uses_public_paragraph_direction_without_synthetic_source_text() {
    let original="אבג אבג אבג";
    let translated="English translation, including a second wrapped line.";
    let mut block=reuse_test_block(original);
    block.style.direction=rebook_publication::TextDirection::Rtl;
    block.style.align=TextAlignment::End;
    block.content.push(Inline::Break);
    block.content.push(Inline::Text(TextRun {text:translated.into(),style:TextStyle {display_writing_system:Some(rebook_publication::WritingSystem::Latin),..Default::default()},link:None}));
    let prepared=LayoutEngine::new().shape_text(&block,&ReaderStyle::default(),190.);
    assert_eq!(prepared.text.as_ref(),format!("{original}\n{translated}"));
    let start=original.len()+1;
    let lines=prepared.layout.lines().filter(|l|l.text_range().start>=start).collect::<Vec<_>>();
    assert!(lines.len()>1);
    assert_eq!(lines[0].text_range().start,start);
    for line in lines {
        assert!(line.metrics().offset.abs()<0.001);
        assert!(line.runs().all(|run|!run.is_rtl()));
        for run in line.runs(){for cluster in run.clusters(){assert!(cluster.text_range().start>=start);}}
    }
}

#[test]
fn table_measurement_reuse_matches_fresh_shaping_including_bidi_and_hard_breaks() {
    let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
        include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
    ))]);
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    for value in [
        "longer words and spacing to wrap across several lines",
        "中文与 English 混合内容。",
        "שלום עולם some Latin text",
        " \t ",
    ] {
        for align in [
            TextAlignment::Start,
            TextAlignment::Center,
            TextAlignment::End,
            TextAlignment::Justify,
        ] {
            for logical in [false, true] {
                let mut block = reuse_test_block(value);
                block.style.align = align;
                block.style.logical_alignment = logical;
                block.style.indent = 12.0;
                block.content.push(Inline::Break);
                block.content.push(Inline::Text(TextRun {
                    text: value.into(),
                    style: TextStyle {
                        bold: true,
                        italic: true,
                        size_scale: 0.8,
                        ..Default::default()
                    },
                    link: None,
                }));
                assert!(table_shaping::reusable_cell(&block));
                for width in [50.0, 115.0, 320.0] {
                    let measured = engine.shape_table_cell(&block, &style, 16_384.0, 8.0, &[]);
                    let scope = timing::TimingScope::start().unwrap();
                    let actual = LayoutEngine::reflow_table_cell(measured, &block, width, 8.0);
                    let timings = scope.finish();
                    assert_eq!(timings.calls(timing::TimingStage::GlyphShape), 0);
                    let expected = engine.shape_text_with_sources(
                        &block,
                        &style,
                        width,
                        8.0,
                        &[],
                        Vec::new(),
                        false,
                    );
                    assert_eq!(
                        reuse_test_geometry(&actual),
                        reuse_test_geometry(&expected),
                        "{value} {align:?} {width}"
                    );
                    assert_eq!(actual.text, expected.text);
                    assert_eq!(actual.source_text_start, expected.source_text_start);
                    assert_eq!(
                        actual.available_width.to_bits(),
                        expected.available_width.to_bits()
                    );
                    assert_eq!(
                        actual.start_offset.to_bits(),
                        expected.start_offset.to_bits()
                    );
                }
            }
        }
    }
}

#[test]
fn naturally_wrapped_measurements_keep_fresh_shaping_to_avoid_round_trip_rounding() {
    let mut engine = LayoutEngine::new();
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let mut block =
        reuse_test_block("A longer cell that wraps when its measurement is too narrow.");
    block.style.align = TextAlignment::Justify;
    let narrow = engine.shape_table_cell(&block, &style, 60.0, 8.0, &[]);
    assert!(!table_shaping::reusable_measurement(&block, &narrow));
    let wide = engine.shape_table_cell(&block, &style, 16_384.0, 8.0, &[]);
    assert!(table_shaping::reusable_measurement(&block, &wide));
    block.content.push(Inline::Break);
    block
        .content
        .extend(reuse_test_block("Another authored line.").content);
    let broken = engine.shape_table_cell(&block, &style, 16_384.0, 8.0, &[]);
    assert!(table_shaping::reusable_measurement(&block, &broken));
}

#[test]
fn empty_table_cells_skip_shaping_without_consuming_whitespace_or_breaks() {
    let mut engine = LayoutEngine::new();
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let empty = reuse_test_block("");
    let scope = timing::TimingScope::start().unwrap();
    let actual = engine.shape_table_cell(&empty, &style, 100.0, 8.0, &[]);
    let timings = scope.finish();
    assert_eq!(timings.calls(timing::TimingStage::GlyphShape), 0);
    assert_eq!(timings.calls(timing::TimingStage::LineBreak), 0);
    assert!(actual.text.is_empty());
    assert!(actual.lines.is_empty());
    assert_eq!(prepared_text_height(&actual).to_bits(), 0.0_f32.to_bits());
    let expected =
        engine.shape_text_with_sources(&empty, &style, 100.0, 8.0, &[], Vec::new(), false);
    assert!(edge_content_lines(&expected).is_empty());
    assert_eq!(
        actual.available_width.to_bits(),
        expected.available_width.to_bits()
    );
    assert!(LayoutEngine::empty_table_cell(&empty, &ReaderStyle::default(), 100.0, 8.0).is_none());
    for value in [" ", "\t", "\n", "\u{a0}", "\u{200b}"] {
        assert!(
            LayoutEngine::empty_table_cell(&reuse_test_block(value), &style, 100.0, 8.0).is_none()
        );
    }
    let mut broken = empty.clone();
    broken.content.push(Inline::Break);
    assert!(LayoutEngine::empty_table_cell(&broken, &style, 100.0, 8.0).is_none());
}

#[test]
fn complex_table_content_keeps_width_dependent_preparation() {
    let plain = reuse_test_block("cell");
    for changed in [
        TextStyle {
            inline_citation: 1,
            ..Default::default()
        },
        TextStyle {
            link_role: LinkRole::FootnoteReference,
            ..Default::default()
        },
        TextStyle {
            inline_role: InlineRole::Footnote,
            ..Default::default()
        },
        TextStyle {
            display_writing_system: Some(WritingSystem::Cjk),
            ..Default::default()
        },
    ] {
        let mut block = plain.clone();
        if let Inline::Text(run) = &mut block.content[0] {
            run.style = changed;
        }
        assert!(!table_shaping::reusable_cell(&block));
    }
    let mut linked = plain.clone();
    if let Inline::Text(run) = &mut linked.content[0] {
        run.link = Some(PublicationUrl::website("https://example.com").unwrap());
    }
    assert!(!table_shaping::reusable_cell(&linked));
    let mut formula = plain.clone();
    formula
        .content
        .push(Inline::Math(rebook_publication::MathRun {
            original: None,
            latex: "x^2".into(),
            display: false,
            size_scale: 1.0,
        }));
    assert!(!table_shaping::reusable_cell(&formula));
    let mut ruby = plain.clone();
    ruby.content
        .push(Inline::Ruby(Box::new(rebook_publication::RubyRun {
            base: vec![],
            annotation: vec![],
            below: false,
        })));
    assert!(!table_shaping::reusable_cell(&ruby));
    let mut sentence = plain;
    sentence.style.preserve_sentence_prefix = true;
    assert!(!table_shaping::reusable_cell(&sentence));
}

#[test]
fn hyphen_measurements_are_reused_across_paragraphs_and_separate_font_settings() {
    let mut engine = LayoutEngine::new();
    engine.publication_languages = vec!["en-US".into()];
    let typography = ReaderTypography::default();
    let style = TextStyle::default();
    let candidates = |engine: &mut LayoutEngine, word: &str, typography: &ReaderTypography| {
        let span = StyledRange {
            ruby: None,
            range: 0..word.len(),
            style,
            footnote_reference_group: 0,
            hyphenation_suppressed: false,
        };
        let scope = timing::TimingScope::start().unwrap();
        let glyphs = engine.prepare_hyphen_candidates(
            word,
            &[span],
            &[],
            "serif",
            typography,
            1.5,
            Rgba::BLACK,
        );
        (glyphs, scope.finish())
    };
    let (first, first_time) = candidates(&mut engine, "typographical", &typography);
    assert!(!first.is_empty());
    assert_eq!(first_time.calls(timing::TimingStage::GlyphShape), 1);
    let (second, second_time) = candidates(&mut engine, "representation", &typography);
    assert!(!second.is_empty());
    assert_eq!(second_time.calls(timing::TimingStage::GlyphShape), 0);
    assert!(Arc::ptr_eq(
        &first.values().next().unwrap().layout,
        &second.values().next().unwrap().layout
    ));
    let mut enlarged = typography.clone();
    enlarged.font_size *= 2.0;
    let (third, third_time) = candidates(&mut engine, "representation", &enlarged);
    assert_eq!(third_time.calls(timing::TimingStage::GlyphShape), 1);
    assert!(third.values().next().unwrap().width > first.values().next().unwrap().width);
}
