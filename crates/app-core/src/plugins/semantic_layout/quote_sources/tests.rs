use super::super::tests::{section, text};
use super::*;

fn valid(group: &Proposal, section: &Section) -> bool {
    validate_window(
        std::slice::from_ref(group),
        section,
        &RecognitionRoles::default(),
        0..section.blocks.len(),
        0..section.blocks.len(),
    )
    .is_ok()
}

#[test]
fn new_quotes_require_valid_adjacent_credit() {
    let s = section(vec![
        text("q", "An independent passage."),
        text("a", "— Mira Vale"),
        text("p", "Narrative continues."),
    ]);
    for attribution in [None, Some(0), Some(2), Some(99)] {
        assert!(!valid(
            &Proposal::Quote {
                body: vec![0],
                attribution,
                alignment: None
            },
            &s
        ));
    }
    assert!(valid(
        &Proposal::Quote {
            body: vec![0],
            attribution: Some(1),
            alignment: None
        },
        &s
    ));
    assert!(!credit_like(
        "Nothing worth reading has been written on it.\""
    ));
}

#[test]
fn introducing_source_stays_before_body() {
    let s = section(vec![
        text("a", "Mira Vale writes:"),
        text("q", "A borrowed passage."),
    ]);
    let p = Proposal::QuoteBefore {
        body: vec![1],
        attribution: 0,
        alignment: Some(QuoteAlignment::Start),
    };
    assert!(valid(&p, &s));
    let mut blocks = s.blocks.clone();
    compose(&mut blocks, &annotation(&p, &s));
    assert_eq!(blocks[0], s.blocks[0]);
    let Block::Quote(q) = &blocks[1] else {
        panic!()
    };
    assert!(q.attribution.is_none());
    assert_eq!(text_block_text(&q.body[0]), "A borrowed passage.");
    assert!(!introduces_quote("Someone said:"));
    assert!(!introduces_quote("Mira Vale walked away."));
    assert!(introduces_quote("In The Example, Mira Vale writes."));
}

#[test]
fn inline_credit_is_split_with_unicode_source_offsets() {
    let s = section(vec![text("q", "你好世界。——某作者")]);
    let p = Proposal::QuoteInline {
        body: vec![0],
        credit: "——某作者".into(),
        alignment: None,
    };
    assert!(valid(&p, &s));
    let mut blocks = s.blocks.clone();
    compose(&mut blocks, &annotation(&p, &s));
    let Block::Quote(q) = &blocks[0] else {
        panic!()
    };
    assert_eq!(text_block_text(&q.body[0]), "你好世界。");
    let a = q.attribution.as_ref().unwrap();
    assert_eq!(text_block_text(a), "——某作者");
    assert_eq!(a.source.as_ref().unwrap().start.text_offset, 5);
    assert_eq!(
        q.body[0].source.as_ref().unwrap().end,
        a.source.as_ref().unwrap().start
    );
    for credit in ["某作者", "——另一个作者", "你好世界。——某作者"] {
        assert!(!valid(
            &Proposal::QuoteInline {
                body: vec![0],
                credit: credit.into(),
                alignment: None
            },
            &s
        ));
    }
}

#[test]
fn existing_quote_can_extract_inline_credit_without_restyling_body() {
    let mut block = text("q", "A quotation.\n— Mira Vale");
    let Block::Text(t) = &mut block else { panic!() };
    t.kind = TextBlockKind::Blockquote;
    let s = section(vec![block]);
    let p = Proposal::QuoteInline {
        body: vec![0],
        credit: "— Mira Vale".into(),
        alignment: Some(QuoteAlignment::Center),
    };
    assert!(valid(&p, &s));
    let mut blocks = s.blocks.clone();
    compose(&mut blocks, &annotation(&p, &s));
    let Block::Quote(q) = &blocks[0] else {
        panic!()
    };
    assert!(q.body[0].style.semantic_alignment.is_none());
    assert!(q.attribution.is_some());
}

#[test]
fn inline_credit_preserves_styles_links_and_explicit_break_offsets() {
    let Block::Text(mut t) = text("q", "Body\n— Mira Vale") else {
        panic!()
    };
    let Inline::Text(mut run) = t.content[0].clone() else {
        panic!()
    };
    run.text = "Body".into();
    let mut credit = run.clone();
    credit.text = "— Mira Vale".into();
    credit.style.italic = true;
    credit.link = Some(PublicationUrl::parse("credit.xhtml").unwrap());
    t.content = vec![
        Inline::Text(run),
        Inline::Break,
        Inline::Text(credit.clone()),
    ];
    let (body, attribution) = split_credit(&t, "— Mira Vale").unwrap();
    assert_eq!(body.source.unwrap().end.text_offset, 4);
    assert_eq!(attribution.source.unwrap().start.text_offset, 5);
    assert_eq!(attribution.content, vec![Inline::Text(credit)]);
}

#[test]
fn bad_credit_cannot_be_repaired_into_a_body_only_quote() {
    let s = section(vec![
        text("q", "Quoted-looking prose."),
        text("p", "Narrative."),
        text("a", "— Mira Vale"),
    ]);
    let bad = Proposal::Quote {
        body: vec![0],
        attribution: Some(2),
        alignment: None,
    };
    let safe = retain_valid_groups(&[bad], &s, &RecognitionRoles::default(), 0..3, 0..3);
    assert!(safe.groups.is_empty());
    assert_eq!(safe.skipped_groups, 1);
}
