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
fn translated_quote_indent_follows_display_language_and_respects_centering() {
    let mut quote = text(TextBlockKind::Blockquote, "Quoted text.", "quote");
    quote.style.indent = -32.;
    let style = unified();
    assert_eq!(
        resolve_text_block(&quote, &style, TextContext::Flow)
            .style
            .indent,
        0.
    );
    for (system, em) in [(WritingSystem::Cjk, 2.), (WritingSystem::Latin, 1.5)] {
        let Inline::Text(run) = &mut quote.content[0] else {
            panic!()
        };
        run.style.display_writing_system = Some(system);
        assert_eq!(
            resolve_text_block(&quote, &style, TextContext::Flow)
                .style
                .indent,
            style.typography.font_size * em
        );
    }
    quote.style.semantic_alignment = Some(TextAlignment::Center);
    assert_eq!(
        resolve_text_block(&quote, &style, TextContext::Flow)
            .style
            .indent,
        0.
    );
    assert_eq!(
        resolve_text_block(&quote, &ReaderStyle::default(), TextContext::Flow)
            .style
            .indent,
        -32.
    );
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
