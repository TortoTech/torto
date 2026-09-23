use super::*;

#[test]
fn formula_transcriptions_must_render_and_keep_negative_results_empty() {
    for latex in [
        r"\sigma=\sqrt{k\theta^2}",
        r"P_i=\frac{e^{U_i/t}}{\sum_j e^{U_j/t}}",
    ] {
        assert!(
            validate_formula(&ImageFormula {
                latex: latex.into(),
                equation_number: None
            })
            .is_ok()
        );
    }
    for latex in ["", r"\input{book}", "$x$", r"\frac{a}{"] {
        assert!(
            validate_formula(&ImageFormula {
                latex: latex.into(),
                equation_number: None
            })
            .is_err()
        );
    }
    assert!(
        Response {
            transient: false,
            status: "not_formula".into(),
            latex: None,
            equation_number: None
        }
        .formula()
        .unwrap()
        .is_none()
    );
    assert!(
        Response {
            transient: false,
            status: "unreadable".into(),
            latex: Some("x".into()),
            equation_number: None
        }
        .formula()
        .is_err()
    );
}

#[test]
fn formula_annotations_preserve_resources_and_protect_unreadable_images() {
    use super::super::tests::{image, original_source, section};
    let raw = section(vec![image("formula")]);
    let Block::Image(original) = &raw.blocks[0] else {
        panic!()
    };
    for readable in [true, false] {
        let annotation = if readable {
            Annotation::ImageFormula {
                href: original.href.clone(),
                formula: ImageFormula {
                    latex: r"x=\frac{a}{b}".into(),
                    equation_number: None,
                },
            }
        } else {
            Annotation::UnreadableFormula {
                href: original.href.clone(),
            }
        };
        assert!(validate_annotations(
            &raw,
            std::slice::from_ref(&annotation)
        ));
        let source = original_source(raw.clone());
        let overlay = SemanticLayoutSource::new(source.clone(), source);
        assert!(overlay.install(
            0,
            Recognition {
                formulas_checked: true,
                fingerprint: fingerprint(&raw),
                annotations: vec![annotation],
                skipped_groups: 0
            }
        ));
        let displayed = overlay.parse_section(0).unwrap();
        let Block::Image(image) = &displayed.blocks[0] else {
            panic!()
        };
        assert_eq!(image.href, original.href);
        assert_eq!(image.source, original.source);
        assert!(image.formula_image);
        assert_eq!(image.formula.is_some(), readable);
        assert!(!image_needs_caption(&displayed, 0));
        overlay.clear();
        assert_eq!(overlay.parse_section(0).unwrap(), raw);
    }
}

#[test]
#[ignore = "requires TORTO_FORMULA_BOOK; inspects actual second-chapter and appendix formula resources without model calls"]
fn local_formula_book_structure() {
    let opened = rebook_formats::open_file(std::env::var("TORTO_FORMULA_BOOK").unwrap()).unwrap();
    let source = opened.source();
    for (index, _item) in source.book().sections.iter().enumerate().filter(|(_, i)| {
        i.href.path().ends_with("09_chapter2.xhtml")
            || i.href.path().ends_with("15_appendix1.xhtml")
    }) {
        let section = source.parse_section(index).unwrap();
        for c in candidates(&section) {
            if [
                "002.gif", "009.gif", "010.gif", "268.gif", "269.gif", "272.gif",
            ]
            .iter()
            .any(|n| c.image.href.path().ends_with(n))
            {
                println!(
                    "{} inline={} source={:?} context={}",
                    c.image.href.path(),
                    c.inline,
                    c.image.source,
                    c.context.chars().take(90).collect::<String>()
                );
            }
        }
    }
}

#[test]
#[ignore = "requires TORTO_FORMULA_BOOK; checks actual appendix display/inline formula layout and original-image previews without model calls"]
fn local_computational_formula_layout() {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    let opened = rebook_formats::open_file(std::env::var("TORTO_FORMULA_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("15_appendix1.xhtml"))
        .unwrap();
    let section = source.parse_section(index).unwrap();
    let mut engine =
        LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned());
    for (name, latex, display) in [
        (
            "math-269.gif",
            r"P_i=\frac{e^{U_i/t}}{\sum_{j=1}^{n} e^{U_j/t}}",
            true,
        ),
        ("math-272.gif", r"\frac{m_i}{m_i+n_i}", false),
    ] {
        let original=section.blocks.iter().find(|b|matches!(b,Block::Text(t) if t.content.iter().any(|i|matches!(i,Inline::Image(r) if r.image.href.path().ends_with(name))))).unwrap().clone();
        let href = candidates(&Section {
            blocks: vec![original.clone()],
            ..section.clone()
        })
        .into_iter()
        .find(|c| c.image.href.path().ends_with(name))
        .unwrap()
        .image
        .href;
        let mut blocks = vec![original.clone()];
        compose(
            &mut blocks,
            &href,
            Some(&ImageFormula {
                latex: latex.into(),
                equation_number: None,
            }),
        );
        assert_eq!(
            super::super::source(&blocks[0]),
            super::super::source(&original)
        );
        for unified in [false, true] {
            let mut style = ReaderStyle {
                spread: SpreadMode::Single,
                ..ReaderStyle::default()
            };
            if unified {
                style.typesetting = ReaderTypesetting::unified();
            }
            let layout = engine
                .layout_blocks(
                    source.as_ref(),
                    &blocks,
                    LayoutViewport::new(900, 1200).unwrap(),
                    &style,
                )
                .unwrap();
            let converted = layout
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .any(|item| match item {
                    PageItem::Image(i) => i.formula_presentation.is_some(),
                    PageItem::Text(t) => t
                        .inline_images
                        .iter()
                        .any(|i| i.formula_presentation.is_some()),
                    _ => false,
                });
            assert_eq!(converted, unified, "{name}");
            if unified && display {
                let image = layout
                    .pages
                    .iter()
                    .flat_map(|p| &p.items)
                    .find_map(|i| match i {
                        PageItem::Image(i) => Some(i),
                        _ => None,
                    })
                    .unwrap();
                let list = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
                let preview = list
                    .image_at(image.x + image.width / 2.0, image.y + image.height / 2.0)
                    .unwrap();
                assert_eq!((preview.width, preview.height), (66, 58));
                assert_eq!(preview.formula.as_deref(), Some(latex));
                assert!(
                    image.width > 500.0,
                    "external A.2 label occupies the right side of the row"
                );
            }
        }
        println!(
            "{name}: original book layout retained, unified formula rendered; display={display}"
        );
    }
}

#[test]
#[ignore = "requires TORTO_FORMULA_BOOK; checks semantic gaps around the photographed chapter-two formula blocks"]
fn local_formula_block_spacing() {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    let opened = rebook_formats::open_file(std::env::var("TORTO_FORMULA_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|s| s.href.path().ends_with("09_chapter2.xhtml"))
        .unwrap();
    let section = source.parse_section(index).unwrap();
    let mut engine =
        LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned());
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        spread: SpreadMode::Single,
        horizontal_margin: 24.0,
        ..ReaderStyle::default()
    };
    let mut hyphens = 0;
    for file in ["math-005.gif", "math-006.gif"] {
        let i = section
            .blocks
            .iter()
            .position(|b| matches!(b,Block::Image(image) if image.href.path().ends_with(file)))
            .unwrap();
        assert!(
            matches!(section.blocks[i - 1], Block::Text(_))
                && matches!(section.blocks[i + 1], Block::Text(_))
        );
        for width in [450, 600, 750, 900] {
            let layout = engine
                .layout_blocks(
                    source.as_ref(),
                    &section.blocks[i - 1..=i + 1],
                    LayoutViewport::new(width, 1600).unwrap(),
                    &style,
                )
                .unwrap();
            let items = &layout.pages[0].items;
            let image_index = items
                .iter()
                .position(|i| matches!(i, PageItem::Image(_)))
                .unwrap();
            hyphens+=items[..image_index].iter().filter(|i|matches!(i,PageItem::Text(t) if t.source.is_none() && t.text.as_ref()=="\u{2010}")).count();
            let before = items[..image_index]
                .iter()
                .find_map(|i| match i {
                    PageItem::Text(t) if t.source.is_some() => Some(t),
                    _ => None,
                })
                .unwrap();
            let after = items[image_index + 1..]
                .iter()
                .find_map(|i| match i {
                    PageItem::Text(t) if t.source.is_some() => Some(t),
                    _ => None,
                })
                .unwrap();
            let PageItem::Image(image) = &items[image_index] else {
                panic!()
            };
            let line = before.layout.get(before.lines.end - 1).unwrap();
            let m = line.metrics();
            let before_gap = image.y
                - before.origin_y
                - m.block_max_coord.max(m.block_min_coord + m.line_height);
            let after_gap = after.origin_y
                + after
                    .layout
                    .get(after.lines.start)
                    .unwrap()
                    .metrics()
                    .block_min_coord
                - image.y
                - image.height;
            println!("{file} width={width}: before={before_gap:.2}, after={after_gap:.2}");
            assert!((before_gap - after_gap).abs() < 0.01);
        }
    }
    assert!(
        hyphens > 0,
        "real paragraphs must exercise discretionary hyphens"
    );
}

#[test]
#[ignore = "requires TORTO_FORMULA_BOOK; transcribes actual formula and diagram images with configured gemini/lite"]
fn live_computational_formula_images() {
    let opened = rebook_formats::open_file(std::env::var("TORTO_FORMULA_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let settings = PluginSettings::load_default().unwrap();
    let provider = settings
        .providers
        .iter()
        .find(|p| p.models.iter().any(|m| m.id == "gemini/lite"))
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap();
    let cases = [
        ("math-002.gif", true),
        ("math-009.gif", true),
        ("math-010.gif", true),
        ("math-269.gif", true),
        ("math-268.gif", true),
        ("graphic-008.gif", false),
    ];
    let mut pending = Vec::new();
    for (file, _) in cases {
        let href = PublicationUrl::parse(&format!("OEBPS/img/oso-9780195370669-{file}")).unwrap();
        let resource = source.resource(&href).unwrap();
        let candidate = Candidate {
            image: ImageBlock {
                formula_image: false,
                formula: None,
                href,
                alt: String::new(),
                style: Default::default(),
                source: None,
                text_layer: None,
            },
            inline: file == "math-010.gif",
            context: String::new(),
        };
        pending.push(batch::Pending {
            key: file.into(),
            path: None,
            aliases: vec![candidate.image.href.clone()],
            candidate,
            url: image_url(decode(&resource.bytes).unwrap()).unwrap(),
        });
    }
    let results = runtime
        .block_on(batch::request_batch(
            &client,
            provider,
            "gemini/lite",
            &pending,
        ))
        .unwrap();
    for ((file, expected), result) in cases.into_iter().zip(results) {
        let result = result.expect("every image must return a matched result");
        println!(
            "{file}: {} {}",
            result.status,
            result.latex.as_deref().unwrap_or("")
        );
        assert_eq!(result.formula().unwrap().is_some(), expected, "{file}");
        if file == "math-010.gif" {
            assert!(result.latex.as_ref().unwrap().contains("sqrt"));
        }
        if file == "math-269.gif" {
            let normalized = result.latex.as_ref().unwrap().replace(['{', '}', ' '], "");
            assert!(
                normalized.contains("U_i") && normalized.contains("U_j"),
                "numerator and denominator use different subscripts"
            );
        }
        assert!(
            result.equation_number.is_none(),
            "no number is printed inside these fixtures"
        );
    }
}
