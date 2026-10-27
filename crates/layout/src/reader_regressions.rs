use super::*;

fn text(value: &str) -> TextBlock {
    TextBlock {
        kind: TextBlockKind::Paragraph,
        content: vec![Inline::Text(TextRun {
            text: value.into(),
            style: Default::default(),
            link: None,
        })],
        style: Default::default(),
        source: None,
    }
}

#[test]
fn ordinary_paragraph_groups_do_not_share_note_numbers() {
    let mut paragraphs = vec![];
    for _ in 0..2 {
        let mut paragraph = text("Body");
        paragraph.style.paragraph_group = Some(7);
        paragraph.content.push(Inline::Text(TextRun {
            text: "Note".into(),
            style: TextStyle {
                inline_role: InlineRole::Footnote,
                ..Default::default()
            },
            link: None,
        }));
        paragraphs.push(Block::Text(paragraph));
    }
    number_list_footnotes(&mut paragraphs);
    for paragraph in &paragraphs {
        let numbered = numbered_semantic_footnotes(paragraph);
        let Block::Text(text) = numbered.as_ref() else {
            panic!();
        };
        let Inline::Text(run) = &text.content[1] else {
            panic!();
        };
        assert_eq!(run.style.footnote_number, 1);
    }
}

#[test]
fn unified_body_removes_uniform_styling_and_keeps_local_emphasis() {
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let mut paragraph = text("Entire paragraph");
    let Inline::Text(run) = &mut paragraph.content[0] else {
        panic!();
    };
    run.style.bold = true;
    run.style.italic = true;
    let resolved = resolve_text_block(&paragraph, &style, TextContext::Flow);
    let Inline::Text(run) = &resolved.content[0] else {
        panic!();
    };
    assert!(!run.style.bold && !run.style.italic);
    assert_eq!(
        resolve_text_block(&paragraph, &ReaderStyle::default(), TextContext::Flow).as_ref(),
        &paragraph
    );
    paragraph.content.extend(text(" ordinary text").content);
    let resolved = resolve_text_block(&paragraph, &style, TextContext::Flow);
    let Inline::Text(run) = &resolved.content[0] else {
        panic!();
    };
    assert!(run.style.bold && run.style.italic);
}

#[test]
fn copyright_heading_keeps_its_authored_center_alignment() {
    let mut heading = text("版权信息");
    heading.kind = TextBlockKind::Heading(1);
    heading.style.authored_alignment = Some(TextAlignment::Center);
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    assert_eq!(
        resolve_text_block(&heading, &style, TextContext::Flow)
            .style
            .align,
        TextAlignment::Center
    );
}

#[test]
fn translated_cjk_list_markers_start_inside_the_paragraph_text_column() {
    let mut item = text("翻译后的列表项目");
    item.kind = TextBlockKind::ListItem {
        depth: 0,
        ordered: false,
        ordinal: 1,
        marker_visible: true,
    };
    let style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    if let Inline::Text(run) = &mut item.content[0] {
        run.style.display_writing_system = Some(WritingSystem::Cjk);
    }
    let resolved = resolve_text_block(&item, &style, TextContext::Flow);
    assert!(
        resolved.style.margin_start
            > style.typography.font_size * style.typesetting.paragraph_indent_em
    );
}
