use super::*;

#[test]
#[ignore = "requires TORTO_INLINE_IMAGE_BOOK; checks Text Entry replacement fidelity with the configured AI endpoint"]
fn live_text_entry_image_replacement_fidelity() {
    let opened = rebook_formats::open_file(std::env::var("TORTO_INLINE_IMAGE_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let settings = PluginSettings::load_default().unwrap();
    let model_override = std::env::var("TORTO_IMAGE_FIDELITY_MODEL").ok();
    let (provider, model) = if let Some(model) = model_override.as_deref() {
        (settings.providers.iter().find(|p| p.models.iter().any(|m| m.id==model))
            .expect("override model must already be configured"), model)
    } else { settings.semantic_layout_endpoint().unwrap() };
    println!("Image replacement fidelity model: {model}");
    let mut pending = Vec::new();
    let mut expected = Vec::new();
    for index in 0..source.book().sections.len() {
        let section = source.parse_section(index).unwrap();
        for candidate in candidates(&section) {
            let path = candidate.image.href.path();
            let visual_sequence = (1..=6).any(|n| path.ends_with(&format!("_u03-0{n}-9780123735911.jpg")));
            let kana = candidate.context.contains("Cree and Cherokee");
            if !visual_sequence && !kana { continue; }
            let resource = source.resource(&candidate.image.href).unwrap();
            let url = image_url(decode(&resource.bytes).unwrap()).unwrap();
            expected.push(kana);
            pending.push(batch::Pending { key: path.into(), path: None,
                aliases: vec![candidate.image.href.clone()], candidate, url });
        }
    }
    assert_eq!(expected.iter().filter(|safe| !**safe).count(), 6);
    assert_eq!(expected.iter().filter(|safe| **safe).count(), 2);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = crate::http::builder().timeout(Duration::from_secs(90)).build().unwrap();
    let mut mismatches = Vec::new();
    for (items, expected) in pending.chunks(batch::MAX_IMAGES).zip(expected.chunks(batch::MAX_IMAGES)) {
        let results = runtime.block_on(batch::request_batch(&client, provider, model,
            settings.semantic_layout.reasoning_effort, items)).unwrap();
        for ((item, safe), response) in items.iter().zip(expected).zip(results) {
            let response = response.expect("missing image judgment");
            let mut annotations = Vec::new();
            let mut failed = 0;
            record(Some(&response), &item.candidate.image.href, &mut annotations, &mut failed);
            println!("{}: status={}, replacement_safe={}, annotations={}", item.key,
                response.status, response.replacement_safe, annotations.len());
            assert_eq!(failed, 0);
            if !annotations.is_empty() != *safe { mismatches.push(item.key.clone()); }
        }
    }
    assert!(mismatches.is_empty(), "incorrect replacement judgments: {mismatches:?}");
}

#[test]
fn literal_radicals_strokes_and_punctuation_are_text_without_normalization() {
    for text in ["⺅", "㇏", "丨", "ㄅ", "ᄀ", "ə\u{301}", "↔", "<", ">", "⼀"] {
        let response=Response { table_html: None, revision: TEXT_REVISION, replacement_safe: true,status:"recognized_text".into(),text:Some(text.into()),literal:true,
            latex:None,equation_number:None,transient:false};
        assert!(response.formula().is_ok(),"{text}");
        assert_eq!(response.text.as_deref(),Some(text));
    }
    assert!(request_prompt().contains("CJK radicals"));
    assert!(validate_text("\u{fffd}").is_err());
}

#[test]
fn text_results_preserve_images_and_do_not_become_formulas() {
    use super::super::tests::{image, section};
    let original = section(vec![image("text")]);
    let candidate = candidates(&original).pop().unwrap();
    let response = Response { table_html: None, revision: TEXT_REVISION, replacement_safe: true, text: Some("か".into()), literal: true, transient: false,
        status: "recognized_text".into(), latex: None, equation_number: None };
    let mut annotations = Vec::new();
    let mut failed = 0;
    record(Some(&response), &candidate.image.href, &mut annotations, &mut failed);
    assert_eq!(failed, 0);
    assert!(validate_annotations(&original, &annotations));
    let mut working = original.blocks.clone();
    super::super::compose(&mut working, &annotations[0]);
    let Block::Text(text) = &working[0] else { panic!() };
    let Inline::Image(image) = &text.content[0] else { panic!() };
    assert_eq!(image.image.href, candidate.image.href);
    assert!(!image.image.formula_image);
    assert!(image.image.formula.is_none());
    assert_eq!(image.image.ocr_text.as_ref().unwrap().text, "か");
    assert_eq!(text.source, candidate.image.source);
    for invalid in ["", "\u{fffd}", "x\0y"] { assert!(validate_text(invalid).is_err()); }
    // Literal code and comparison symbols are safe text; the translation
    // protocol escapes them before wrapping them in t-img tags.
    assert!(validate_text("<b>x</b>").is_ok());
    assert!(validate_text("か き\nÁ").is_ok());
}

#[test]
#[ignore = "requires TORTO_INLINE_IMAGE_BOOK; renders real kana extraction without model calls"]
fn local_book_unicode_image_text_layout() {
    use anyrender::ImageRenderer;
    let opened = rebook_formats::open_file(std::env::var("TORTO_INLINE_IMAGE_BOOK").unwrap()).unwrap();
    let source = opened.source();
    for index in 0..source.book().sections.len() {
        let section = source.parse_section(index).unwrap();
        for block in &section.blocks {
            let Block::Text(original) = block else { continue };
            if !text_block_text(original).contains("Cree and Cherokee") { continue }
            let mut blocks = vec![block.clone()];
            let selected = candidates(&Section { blocks: blocks.clone(), ..section.clone() });
            assert_eq!(selected.len(), 2);
            for (candidate, value) in selected.iter().zip(["か","き"]) {
                compose_text(&mut blocks, &candidate.image.href, &ImageText { text: value.into(), literal: true, translated: None });
            }
            let mut style = rebook_layout::ReaderStyle::default();
            style.typesetting.mode = rebook_layout::TypesettingMode::Unified;
            style.spread = rebook_layout::SpreadMode::Single;
            let fonts = crate::fonts::embedded_reader_fonts();
            let mut engine = rebook_layout::LayoutEngine::with_fonts(fonts.iter().cloned());
            let layout = engine.layout_blocks(source.as_ref(), &blocks, rebook_layout::LayoutViewport::new(1100,600).unwrap(), &style).unwrap();
            let mut text = String::new();
            for item in &layout.pages[0].items {
                if let rebook_layout::PageItem::Text(placement) = item {
                    text.push_str(&placement.text);
                    assert!(placement.inline_images.is_empty(), "recognized text must shape as glyphs");
                    let zero = placement.source_spans.iter().filter(|span| span.source.start.text_offset == span.source.end.text_offset).collect::<Vec<_>>();
                    assert_eq!(zero.len(), 2);
                }
            }
            assert!(text.contains("か (ka) and き (ki)"));
            let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
            let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(1100,600);
            let mut pixels = Vec::new();
            renderer.render_to_vec(|scene| display.paint(scene), &mut pixels);
            image::RgbaImage::from_raw(1100,600,pixels).unwrap().save("F:/projects/ebook/tmp/ocr-kana-original.png").unwrap();
            println!("section={index}; recognized Unicode at original slots; source ranges retained");
            return;
        }
    }
    panic!("sample paragraph missing");
}

#[test]
fn formula_request_keeps_threefold_enlargement_and_white_padding() {
    let pixels = RgbaImage::from_pixel(3, 7, Rgba([0, 0, 0, 128]));
    let url = image_url(pixels.into()).unwrap();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(url.split_once(',').unwrap().1)
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap().into_rgba8();
    assert_eq!([image.width(), image.height()], [25, 37]);
    assert_eq!(image.get_pixel(0, 0).0, [255; 4]);
    let center = image.get_pixel(12, 18).0;
    // Image's floating-point compositing may round opaque alpha down by one.
    assert!((254..=255).contains(&center[3]));
    assert!(
        center[..3]
            .iter()
            .all(|channel| (126..=128).contains(channel))
    );
}

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
        Response { table_html: None, revision: TEXT_REVISION, replacement_safe: true,
            text: None,
            literal: false,
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
        Response { table_html: None, revision: TEXT_REVISION, replacement_safe: true,
            text: None,
            literal: false,
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
                image_text_revision: crate::plugins::semantic_layout::formulas::TEXT_REVISION,
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
    let client = crate::http::builder()
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
                ocr_text: None,
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
            ReasoningEffort::Default,
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

#[test]
fn table_results_require_safe_complete_grid_and_preserve_captioned_candidates() {
    use super::super::tests::{image, section};
    let original = section(vec![image("table")]);
    let href = candidates(&original)[0].image.href.clone();
    let mut response = Response {
        table_html: Some("<table><tr><th rowspan='2'>Category</th><th colspan='2'>Value</th></tr><tr><th>2023</th><th>2024</th></tr><tr><td>A</td><td>-1.2</td><td>3.4%</td></tr></table>".into()),
        revision: TEXT_REVISION, replacement_safe: true, text: None, literal: false, transient: false,
        status: "recognized_table".into(), latex: None, equation_number: None,
    };
    let mut annotations = Vec::new();
    let mut failed = 0;
    record(Some(&response), &href, &mut annotations, &mut failed);
    assert_eq!(failed, 0);
    assert!(matches!(&annotations[..], [Annotation::ImageTable { table, .. }] if table.rows[0].cells[0].row_span == 2));
    assert!(validate_annotations(&original, &annotations));
    response.table_html = Some("<table><tr><td rowspan='3'>Missing row</td></tr></table>".into());
    assert!(response.formula().is_err());
    let mut rejected = Vec::new();
    record(Some(&response), &href, &mut rejected, &mut failed);
    assert!(rejected.is_empty());
    response.replacement_safe = false;
    assert!(response.formula().unwrap().is_none());
    let descriptor = rebook_publication::SpineItem { id: rebook_publication::SpineItemId::new("captioned").unwrap(), href: PublicationUrl::parse("chapter.xhtml").unwrap(), media_type: "application/xhtml+xml".into(), linear: true, properties: vec![] };
    let section = rebook_html::parse_section("<html><body><figure><img src='table.png'/><figcaption>Table 1. Results.</figcaption></figure></body></html>", &descriptor, |_| None).unwrap();
    let tables = candidates(&section);
    assert_eq!(tables.len(), 1);
    assert!(!tables[0].inline);
}
