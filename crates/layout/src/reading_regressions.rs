// Regressions for split publisher structures and translated presentation.
use super::*;

fn text(kind: TextBlockKind, value: &str, node: &str) -> TextBlock {
    let spine = SpineItemId::new("chapter").unwrap();
    TextBlock {
        kind,
        content: vec![Inline::Text(TextRun {
            text: value.into(),
            style: Default::default(),
            link: None,
        })],
        style: Default::default(),
        source: Some(SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: node.into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine,
                node: node.into(),
                text_offset: value.chars().count() as u64,
            },
        }),
    }
}
fn unified() -> ReaderStyle {
    ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    }
}

#[test]
fn split_figure_caption_joins_per_language_and_preserves_both_source_nodes() {
    let source = EmptySource::new("caption-regression");
    let mut engine = LayoutEngine::new();
    let label = text(TextBlockKind::Caption, "Fig. 1.1", "label");
    let body = text(TextBlockKind::Caption, "Examples of legibility.", "caption");
    let mut translated_label = label.clone();
    translated_label.style.reference_owner = Some(rebook_publication::source_block_identity(
        label.source.as_ref().unwrap(),
    ));
    translated_label.source = None;
    let mut translated_body = body.clone();
    translated_body.content = vec![Inline::Text(TextRun {
        text: "字形辨识示例".into(),
        style: TextStyle {
            display_writing_system: Some(WritingSystem::Cjk),
            ..Default::default()
        },
        link: None,
    })];
    translated_body.style.reference_owner = Some(rebook_publication::source_block_identity(
        body.source.as_ref().unwrap(),
    ));
    translated_body.source = None;
    let parts = [
        label.clone(),
        translated_label,
        body.clone(),
        translated_body,
    ];
    let shaped = engine
        .shape_figure_captions(&source, &parts, &unified(), 500., true)
        .unwrap();
    assert_eq!(shaped.len(), 2);
    assert_eq!(
        shaped[0].0.text.as_ref(),
        "Fig. 1.1 Examples of legibility."
    );
    assert_eq!(shaped[0].0.source_spans.len(), 2);
    assert_eq!(shaped[0].0.source_spans[0].source.start.node, "label");
    assert_eq!(shaped[0].0.source_spans[1].source.start.node, "caption");
    assert_eq!(shaped[1].0.text.as_ref(), "Fig. 1.1 字形辨识示例");
    assert!(shaped[1].0.source_spans.is_empty());
    let classic = engine
        .shape_figure_captions(&source, &parts, &ReaderStyle::default(), 500., false)
        .unwrap();
    assert_eq!(classic.len(), 4);
    let mut explicit = label;
    explicit.style.hard_break_after = true;
    let separated = [explicit, body];
    assert_eq!(
        engine
            .shape_figure_captions(&source, &separated, &unified(), 500., true)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn translated_quote_indent_preserves_authored_presence_and_follows_display_language() {
    let mut quote = text(TextBlockKind::Blockquote, "Quoted text.", "quote");
    for source_system in [WritingSystem::Latin, WritingSystem::Cjk] {
        let mut style = unified();
        style.writing_system = source_system;
        for display in [None, Some(WritingSystem::Cjk), Some(WritingSystem::Latin)] {
            let Inline::Text(run) = &mut quote.content[0] else { panic!() };
            run.style.display_writing_system = display;
            for source_indent in [-32., 0., 32.] {
                quote.style.indent = source_indent;
                for custom in [None, Some(0.75)] {
                    style.typesetting.paragraph_indent_mode = if custom.is_some() {
                        ParagraphIndentMode::Custom
                    } else { ParagraphIndentMode::Auto };
                    style.typesetting.paragraph_indent_em = custom.unwrap_or(2.);
                    let em = custom.unwrap_or(if display.unwrap_or(source_system) == WritingSystem::Cjk { 2. } else { 1.5 });
                    for alignment in [TextAlignment::Start, TextAlignment::Center, TextAlignment::End] {
                        quote.style.semantic_alignment = Some(alignment);
                        let expected = if source_indent > 0. && alignment == TextAlignment::Start {
                            style.typography.font_size * em
                        } else { 0. };
                        assert_eq!(resolve_text_block(&quote, &style, TextContext::Flow).style.indent, expected);
                        assert_eq!(resolve_text_block(&quote, &ReaderStyle::default(), TextContext::Flow).style.indent, source_indent);
                    }
                }
            }
        }
    }
}

#[test]
fn bilingual_quote_padding_and_wrapped_lines_follow_each_displayed_language() {
    let source = EmptySource::new("bilingual-quote-insets");
    let mut body = Vec::new();
    for indent in [-32., 0., 32.] {
        let mut original = text(TextBlockKind::Blockquote, &"Original quoted prose with several words. ".repeat(12), "quote");
        original.style.indent = indent;
        let mut translated = original.clone();
        translated.content = vec![Inline::Text(TextRun {
            text: "注意：引用正文保留原文是否首行缩进的区别，容器内边距按实际显示语言计算。".repeat(8),
            style: TextStyle { display_writing_system: Some(WritingSystem::Cjk), ..Default::default() },
            link: None,
        })];
        translated.source = None;
        body.extend([original, translated]);
    }
    let mut attribution = text(TextBlockKind::QuoteAttribution, "中文引源", "credit");
    let Inline::Text(run) = &mut attribution.content[0] else { panic!() };
    run.style.display_writing_system = Some(WritingSystem::Cjk);
    let block = Block::Quote(rebook_publication::QuoteBlock { body, attribution: Some(attribution), source: None });
    let before = block.clone();
    let mut engine = LayoutEngine::with_fonts([
        ReaderFontBlob::new(Arc::new(include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice())),
        ReaderFontBlob::new(Arc::new(include_bytes!("../../../assets/fonts/LXGWWenKaiGBScreen.ttf").as_slice())),
    ]);
    for quote_style in [QuoteStyle::Accent, QuoteStyle::Filled] {
        for width in [390, 900] {
            let mut style = unified();
            style.writing_system = WritingSystem::Latin;
            style.typesetting.quote_style = quote_style;
            style.spread = SpreadMode::Scroll;
            let layout = engine.layout_blocks(&source, std::slice::from_ref(&block), LayoutViewport::new(width, 9000).unwrap(), &style).unwrap();
            let left = reading_content_left(width as f32, &style);
            let placed = layout.pages.iter().flat_map(|page| &page.items).filter_map(|item| match item {
                PageItem::Text(text) if text.text.len() > 10 => Some(text), _ => None,
            }).collect::<Vec<_>>();
            assert_eq!(placed.len(), 7);
            for (index, text) in placed[..6].iter().enumerate() {
                let em = if index % 2 == 0 { 1.5 } else { 2.0 };
                let padding = if quote_style == QuoteStyle::Filled { 8.0 } else { style.typography.font_size * em };
                let first_indent = if index >= 4 { style.typography.font_size * em } else { 0.0 };
                assert!((text.origin_x - left - padding).abs() < 0.01);
                let first = text.layout.get(text.lines.start).unwrap().metrics();
                let continuation = text.layout.get(text.lines.start + 1).unwrap().metrics();
                assert!((first.offset - first_indent).abs() < 0.01);
                assert!(continuation.offset.abs() < 0.01);
            }
            let credit_padding = if quote_style == QuoteStyle::Filled { 8.0 } else { style.typography.font_size * 2.0 };
            assert!((placed[6].origin_x - left - credit_padding).abs() < 0.01);
        }
    }
    assert_eq!(block, before, "layout must not mutate source styles or anchors");
}

#[test]
fn chinese_title_brackets_suppress_only_automatic_emphasis_bold() {
    let emphasized = TextStyle {
        emphasis: true,
        italic: true,
        ..Default::default()
    };
    let run = |value: &str, style| {
        Inline::Text(TextRun {
            text: value.into(),
            style,
            link: None,
        })
    };
    let mut content = vec![run("强调《书", emphasized), run("名》重点", emphasized)];
    resolve_semantic_inline_presentation(&mut content, WritingSystem::Cjk);
    let mut characters = Vec::new();
    for inline in &content {
        for run in inline.text_runs() {
            characters.extend(
                run.text
                    .chars()
                    .map(|c| (c, run.style.bold, run.style.italic)),
            );
        }
    }
    assert!(
        characters
            .iter()
            .filter(|(c, _, _)| "《书名》".contains(*c))
            .all(|(_, bold, italic)| !bold && !italic)
    );
    assert!(
        characters
            .iter()
            .filter(|(c, _, _)| "强调重点".contains(*c))
            .all(|(_, bold, _)| *bold)
    );
    let mut explicit = vec![run(
        "《书名》",
        TextStyle {
            bold: true,
            ..emphasized
        },
    )];
    resolve_semantic_inline_presentation(&mut explicit, WritingSystem::Cjk);
    assert!(
        explicit
            .iter()
            .flat_map(Inline::text_runs)
            .all(|r| r.style.bold)
    );
}

#[test]
fn unified_heading_removes_only_a_thin_presentation_rule() {
    let image = Inline::Image(Box::new(rebook_publication::InlineImageRun {
        image: ImageBlock {
            href: PublicationUrl::parse("rule.jpg").unwrap(),
            alt: String::new(),
            style: ImageStyle {
                width: Some(rebook_publication::ImageLength::Fraction(0.2)),
                height: Some(rebook_publication::ImageLength::Pixels(2.4)),
                ..Default::default()
            },
            source: None,
            ocr_text: None,
            formula_image: false,
            formula: None,
            text_layer: None,
        },
        size_scale: 1.,
        intrinsic_sizing: false,
        vertical_align: InlineImageAlignment::Middle,
        presentation: true,
    }));
    let mut heading = text(TextBlockKind::Heading(1), "Chapter 1", "heading");
    heading.content.push(image);
    assert_eq!(
        resolve_text_block(&heading, &ReaderStyle::default(), TextContext::Flow).content,
        heading.content
    );
    let resolved = resolve_text_block(&heading, &unified(), TextContext::Flow).into_owned();
    assert!(
        !resolved
            .content
            .iter()
            .any(|i| matches!(i, Inline::Image(_)))
    );
    let original_chars: usize = heading
        .content
        .iter()
        .map(|i| match i {
            Inline::Text(r) => r.text.chars().count(),
            _ => 1,
        })
        .sum();
    assert_eq!(
        resolved
            .content
            .iter()
            .flat_map(Inline::text_runs)
            .map(|r| r.text.chars().count())
            .sum::<usize>(),
        original_chars
    );
    let Inline::Image(image) = &mut heading.content[1] else {
        panic!()
    };
    image.presentation = false;
    assert!(
        resolve_text_block(&heading, &unified(), TextContext::Flow)
            .content
            .iter()
            .any(|i| matches!(i, Inline::Image(_)))
    );
}
