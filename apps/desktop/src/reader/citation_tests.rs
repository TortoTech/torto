use super::*;
use rebook_publication::{SpineItemId, TextRun, TextStyle};

#[test]
fn popup_keeps_footnotes_before_citations_and_merges_styled_parts() {
    let anchor = SourceAnchor {
        spine: SpineItemId::new("test").unwrap(),
        node: "p".into(),
        text_offset: 0,
    };
    let range = SourceRange {
        start: anchor.clone(),
        end: SourceAnchor {
            text_offset: 100,
            ..anchor
        },
    };
    let run = |text: &str, number, role| {
        Inline::Text(TextRun {
            text: text.into(),
            style: TextStyle {
                inline_citation: number,
                inline_role: role,
                ..Default::default()
            },
            link: None,
        })
    };
    let block = TextBlock {
        kind: TextBlockKind::Paragraph,
        style: Default::default(),
        source: Some(range.clone()),
        content: vec![
            run("Body ", 0, InlineRole::Normal),
            run("(Smith, ", 1, InlineRole::Normal),
            run("2020)", 1, InlineRole::Normal),
            run("Note content", 0, InlineRole::Footnote),
            run(" more ", 0, InlineRole::Normal),
            run("(Jones, 1990)", 2, InlineRole::Normal),
        ],
    };
    let items = text_block_focus_footnotes(&block);
    assert_eq!(items.len(), 3);
    assert!(matches!(&items[0],FocusFootnoteSource::Inline(t) if t=="Note content"));
    assert!(
        matches!(&items[1],FocusFootnoteSource::Citation{text,source,number:1} if text=="(Smith, 2020)" && source==&range)
    );
    assert!(matches!(
        &items[2],
        FocusFootnoteSource::Citation { number: 2, .. }
    ));
}
