use super::*;

fn parse(body: &str) -> Section {
    let descriptor = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    parse_section(
        &format!("<html><body>{body}</body></html>"),
        &descriptor,
        |_| None,
    )
    .unwrap()
}
fn content(text: &TextBlock) -> String {
    text.content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(run) => Some(run.text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn nested_link_wrappers_preserve_figure_caption_anchor_and_dimensions() {
    let section = parse(
        r##"<p id="before">Body.</p><p class="fig" id="outer"><span><a id="fig2.6"/></span><a href="#before"><div class="arbitrary"><div><p><img src="figure.png" width="297" height="125"/></p></div><div><p class="captions" id="caption"><b>Figure 2.6</b> Caption text.</p></div></div></a></p><p>After.</p>"##,
    );
    let [Block::Text(_), Block::Figure(figure), Block::Text(after)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks)
    };
    assert_eq!(figure.images.len(), 1);
    assert_eq!(figure.captions.len(), 1);
    assert_eq!(
        figure.images[0].style.width,
        Some(ImageLength::Pixels(297.0))
    );
    assert_eq!(
        figure.images[0].style.height,
        Some(ImageLength::Pixels(125.0))
    );
    assert_eq!(figure.caption_position, CaptionPosition::After);
    assert_eq!(content(&figure.captions[0]), "Figure 2.6 Caption text.");
    assert_eq!(content(after), "After.");
    assert!(figure.captions[0].content.iter().all(|i| matches!(i,Inline::Text(r) if r.link.as_ref().is_some_and(|l| l.fragment()==Some("before")))));
    for id in ["outer", "fig2.6"] {
        assert_eq!(
            &section
                .anchors
                .iter()
                .find(|a| a.fragment == id)
                .unwrap()
                .source,
            &figure.images[0].source.as_ref().unwrap().start
        );
    }
    assert_eq!(
        &section
            .anchors
            .iter()
            .find(|a| a.fragment == "caption")
            .unwrap()
            .source,
        &figure.captions[0].source.as_ref().unwrap().start
    );
}

#[test]
fn before_caption_multiple_images_and_surrounding_text_keep_order() {
    let section = parse(
        r#"<p>Leading <span>words.</span><span><div><p class="captions">Diagram description</p><div><p><img src="a.png"/></p><p><img src="b.png"/></p></div></div></span>Trailing words.</p>"#,
    );
    let [Block::Text(first), Block::Figure(figure), Block::Text(last)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks)
    };
    assert_eq!(content(first), "Leading words.");
    assert_eq!(content(last), "Trailing words.");
    assert_eq!(figure.caption_position, CaptionPosition::Before);
    assert_eq!(figure.images.len(), 2);
    assert_eq!(figure.captions[0].kind, TextBlockKind::Caption);
}

#[test]
fn ordinary_inline_images_and_unrelated_prose_are_not_absorbed() {
    let section = parse(
        r#"<p>Symbol <a href="notes.xhtml"><img src="symbol.png"/></a> in prose.</p><p><div><p><img src="photo.png"/></p><p>Ordinary prose that follows a picture.</p></div></p>"#,
    );
    let [Block::Text(inline), Block::Image(_), Block::Text(prose)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks)
    };
    assert!(inline.content.iter().any(|i| matches!(i, Inline::Image(_))));
    assert_eq!(prose.kind, TextBlockKind::Paragraph);
}
