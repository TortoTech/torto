use super::*;

fn descriptor() -> SpineItem {
    SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    }
}

fn parse(body: &str) -> Section {
    parse_section(
        &format!("<html><body>{body}</body></html>"),
        &descriptor(),
        |_| None,
    )
    .unwrap()
}

fn plain(text: &TextBlock) -> String {
    text.content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|r| r.text.as_str())
        .collect()
}

#[test]
fn front_caption_keeps_authored_order_styles_links_and_source_ids() {
    let body = "<style>.x7{font-size:small;color:#708090;page-break-after:avoid}.x8 img{max-width:100%}</style><p id='lead'><a id='ref' href='#fig'>See Figure 3</a></p><div class='x8'><p class='x7'><a id='fig' href='#ref'><b>Figure 3</b></a> Evolution in development of HFE discipline (after Karwowski, 2005)</p><span id='page'/><img id='picture' src='images/c01f003.jpg' alt='1.3' width='640' height='320'/></div><p id='after'>Following prose.</p>";
    // Same allocation path without a caption label: source node numbering and
    // anchors must not change when only the relationship is recovered.
    let baseline = parse(&body.replace("Figure 3</b>", "F_gure 3</b>"));
    let section = parse(body);
    let [Block::Text(_), Block::Figure(f), Block::Text(after)] = section.blocks.as_slice() else {
        panic!("{:?}", section.blocks)
    };
    assert_eq!(section.anchors, baseline.anchors);
    assert_eq!(f.caption_position, CaptionPosition::Before);
    assert_eq!(f.images.len(), 1);
    assert_eq!(f.captions.len(), 1);
    assert!(f.body.is_empty());
    let caption = &f.captions[0];
    assert_eq!(caption.kind, TextBlockKind::Caption);
    assert_eq!(plain(caption).chars().count(), 75);
    let Block::Text(original) = &baseline.blocks[1] else {
        panic!()
    };
    let Block::Image(image) = &baseline.blocks[2] else {
        panic!()
    };
    assert_eq!(caption.source, original.source);
    assert_eq!(f.images[0].source, image.source);
    assert_eq!(f.images[0].style, image.style);
    assert_eq!(
        f.source.as_ref().unwrap().start,
        caption.source.as_ref().unwrap().start
    );
    assert_eq!(
        f.source.as_ref().unwrap().end,
        image.source.as_ref().unwrap().end
    );
    let mut original_content = original.content.clone();
    for inline in &mut original_content {
        if let Inline::Text(run) = inline {
            run.text = run.text.replace("F_gure 3", "Figure 3");
        }
    }
    assert_eq!(caption.content, original_content);
    assert!(
        caption
            .content
            .iter()
            .flat_map(Inline::text_runs)
            .any(|r| r.text == "Figure 3"
                && r.style.bold
                && r.link.as_ref().is_some_and(|l| l.fragment() == Some("ref")))
    );
    assert_eq!(plain(after), "Following prose.");
}

#[test]
fn front_caption_accepts_bare_and_wrapped_images_without_publisher_classes() {
    for image in [
        "<img src='a.jpg'/>",
        "<p><img src='a.jpg'/></p>",
        "<div><p><a href='https://example.org'><img src='a.jpg'/></a></p></div>",
    ] {
        for container in [false, true] {
            let body = format!("<p>Figure 1 Diagram</p>{image}");
            let section = parse(&if container {
                format!("<div>{body}</div>")
            } else {
                body
            });
            assert!(
                matches!(section.blocks.as_slice(), [Block::Figure(f)] if f.images.len()==1 && f.captions.len()==1 && f.caption_position==CaptionPosition::Before),
                "{:?}",
                section.blocks
            );
        }
    }
}

#[test]
fn consecutive_front_captions_do_not_attach_to_the_previous_image() {
    for body in [
        "<p>Figure 1 First</p><img src='a.jpg'/><p>Figure 2 Second</p><p><img src='b.jpg'/></p>",
        "<div><p>Figure 1 First</p><img src='a.jpg'/></div><div><p>Figure 2 Second</p><img src='b.jpg'/></div>",
    ] {
        let section = parse(body);
        assert_eq!(section.blocks.len(), 2);
        for (block, (image, caption)) in section
            .blocks
            .iter()
            .zip([("a.jpg", "Figure 1 First"), ("b.jpg", "Figure 2 Second")])
        {
            let Block::Figure(f) = block else {
                panic!("{block:?}")
            };
            assert_eq!(f.caption_position, CaptionPosition::Before);
            assert_eq!(f.images.len(), 1);
            assert_eq!(f.images[0].href.path(), image);
            assert_eq!(f.captions.iter().map(plain).collect::<Vec<_>>(), [caption]);
        }
    }
    let scoped =
        parse("<img src='unrelated.jpg'/><div><p>Figure 2 Second</p><img src='b.jpg'/></div>");
    assert!(
        matches!(scoped.blocks.as_slice(), [Block::Image(_), Block::Figure(f)] if f.images[0].href.path()=="b.jpg")
    );
}

#[test]
fn front_caption_groups_image_run_continuations_and_following_sources() {
    let section = parse(
        "<div><p>Figure 1 Panels</p><p role='doc-caption' id='detail'>Caption continuation.</p><a id='page'/><img src='a.jpg'/><p><img src='b.jpg'/></p><p>Source: The authors.</p></div>",
    );
    let [Block::Figure(f)] = section.blocks.as_slice() else {
        panic!("{:?}", section.blocks)
    };
    assert_eq!(f.images.len(), 2);
    assert_eq!(
        f.captions.iter().map(plain).collect::<Vec<_>>(),
        [
            "Figure 1 Panels",
            "Caption continuation.",
            "Source: The authors."
        ]
    );
    assert_eq!(f.captions[2].kind, TextBlockKind::QuoteAttribution);
    assert_eq!(f.caption_position, CaptionPosition::Split(2));
    assert!(section.anchors.iter().any(|a| a.fragment=="page" && a.source==f.images[0].source.as_ref().unwrap().start));
}

#[test]
fn front_caption_inference_respects_prose_headings_and_container_boundaries() {
    for body in [
        "<div><p>Figure 3 illustrates a prose reference.</p><img src='a.jpg'/></div>",
        "<div><p>Figure 3 A title</p><p>Intervening prose.</p><img src='a.jpg'/></div>",
        "<div><p>Figure 3 A title</p><h2>Another section</h2><img src='a.jpg'/></div>",
        "<div><p>Figure 3 A title</p></div><div><img src='unrelated.jpg'/></div>",
        "<div><blockquote><p>Figure 3 A quotation</p></blockquote><img src='a.jpg'/></div>",
    ] {
        let section = parse(body);
        assert!(
            section
                .blocks
                .iter()
                .all(|b| !matches!(b, Block::Figure(_))),
            "{body}: {:?}",
            section.blocks
        );
    }
    let section = parse_section_with_hints_and_image_classifier(
        "<html><body><div><p><a id='destination'/>Figure 3 A navigation heading</p><img src='a.jpg'/></div></body></html>",
        &descriptor(), |_| None, |_| false,
        SectionParseHints {heading_targets:&[PublicationUrl::parse("chapter.xhtml#destination").unwrap()], ..Default::default()},
    ).unwrap();
    assert!(
        section
            .blocks
            .iter()
            .all(|b| !matches!(b, Block::Figure(_)))
    );
}

#[test]
fn after_captions_remain_attached_to_their_original_images() {
    let section = parse(
        "<div><img src='a.jpg'/><p>Figure 1 First</p><img src='b.jpg'/><p>Figure 2 Second</p></div>",
    );
    let captions: Vec<_> = section
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .map(plain)
        .collect();
    assert_eq!(captions, ["Figure 1 First", "Figure 2 Second"]);
    assert!(
        matches!(section.blocks.as_slice(), [Block::Image(a),Block::Text(c1),Block::Image(b),Block::Text(c2)]
        if a.href.path()=="a.jpg" && b.href.path()=="b.jpg" && c1.kind==TextBlockKind::Caption && c2.kind==TextBlockKind::Caption),
        "{:?}",
        section.blocks
    );
}

#[test]
fn hidden_media_does_not_discard_or_reclassify_its_preceding_text() {
    let section = parse(
        "<div><p>Figure 1 Preserved text</p><img style='display:none' src='hidden.jpg'/></div><p>After.</p>",
    );
    assert!(
        section
            .blocks
            .iter()
            .all(|b| !matches!(b, Block::Figure(_)))
    );
    assert_eq!(
        section
            .blocks
            .iter()
            .flat_map(Block::text_blocks)
            .map(plain)
            .collect::<Vec<_>>(),
        ["Figure 1 Preserved text", "After."]
    );
}

#[test]
fn keep_with_next_disambiguates_long_captions_without_a_publisher_class_name() {
    let body = "<style>.x23{page-break-after:avoid;page-break-inside:avoid}</style><div><p class='x23'>Figure 1 A model shows the relationship. The lower panel depicts the result.</p><img src='a.jpg'/></div>";
    let section = parse(body);
    assert_eq!(section, parse(&body.replace("x23", "arbitrary")));
    assert!(
        matches!(section.blocks.as_slice(), [Block::Figure(f)] if f.captions.len()==1 && f.images.len()==1)
    );
    let prose = parse(&body.replace("page-break-after:avoid;", ""));
    assert!(prose.blocks.iter().all(|b| !matches!(b, Block::Figure(_))));
}

#[test]
fn front_caption_preserves_inline_symbols_and_footnote_references() {
    let body = "<div><p id='caption'><b>Figure 1</b> Signal <img src='pi.gif' style='height:1em'/> + <em>x</em><a role='doc-noteref' href='#note'>*</a></p><img id='diagram' src='a.jpg'/></div><aside role='doc-footnote' id='note'><p>Note text.</p></aside>";
    let section = parse(body);
    let baseline = parse(&body.replace("Figure 1", "F_gure 1"));
    let Block::Figure(f) = &section.blocks[0] else {
        panic!("{:?}", section.blocks)
    };
    let Block::Text(original) = &baseline.blocks[0] else {
        panic!()
    };
    assert_eq!(section.anchors, baseline.anchors);
    let mut expected = original.content.clone();
    for inline in &mut expected {
        if let Inline::Text(run) = inline {
            run.text = run.text.replace("F_gure 1", "Figure 1");
        }
    }
    assert_eq!(f.captions[0].content, expected);
    assert!(
        f.captions[0]
            .content
            .iter()
            .any(|i| matches!(i, Inline::Image(_)))
    );
    assert!(
        f.captions[0]
            .content
            .iter()
            .flat_map(Inline::text_runs)
            .any(|r| r.style.link_role == LinkRole::FootnoteReference)
    );
    assert_eq!(f.images[0].href.path(), "a.jpg");
    assert!(matches!(section.blocks[1], Block::Note(_)));
}
