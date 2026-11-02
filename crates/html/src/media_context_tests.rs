use super::*;

fn parse(body: &str) -> Section {
    let item = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    parse_section(&format!("<html><body>{body}</body></html>"), &item, |_| {
        None
    })
    .unwrap()
}

#[test]
fn numbered_quote_caption_groups_with_preceding_image_without_renumbering_sources() {
    let body = "<p><img id='diagram' src='diagram.gif'/></p><div height='3%'/><p height='2%' width='0%'/><blockquote id='caption'><b>Figure 1.1: Animation of insertion sort</b></blockquote><p><img id='code' src='code.gif'/></p><p id='after'>After.</p>";
    let section = parse(body);
    let baseline = parse(&body.replace("Figure 1.1:", "Quoted 1.1:"));
    let [
        Block::Figure(figure),
        Block::Image(code),
        Block::Text(after),
    ] = section.blocks.as_slice()
    else {
        panic!("unexpected structure: {:?}", section.blocks);
    };
    assert_eq!(figure.images.len(), 1);
    assert!(figure.images[0].href.path().ends_with("diagram.gif"));
    assert_eq!(figure.captions.len(), 1);
    assert_eq!(figure.captions[0].kind, TextBlockKind::Caption);
    assert_eq!(figure.caption_position, CaptionPosition::After);
    assert!(code.href.path().ends_with("code.gif"));
    let Block::Quote(original) = &baseline.blocks[1] else {
        panic!()
    };
    assert_eq!(figure.captions[0].source, original.body[0].source);
    assert_eq!(figure.captions[0].style, original.body[0].style);
    for (id, source) in [
        ("diagram", figure.images[0].source.as_ref().unwrap()),
        ("caption", figure.captions[0].source.as_ref().unwrap()),
        ("code", code.source.as_ref().unwrap()),
        ("after", after.source.as_ref().unwrap()),
    ] {
        let anchor = section.anchors.iter().find(|a| a.fragment == id).unwrap();
        assert_eq!(anchor.source, source.start);
        assert_eq!(
            anchor,
            baseline.anchors.iter().find(|a| a.fragment == id).unwrap()
        );
    }
}

#[test]
fn numbered_quote_caption_before_an_image_and_inside_a_figure_uses_existing_grouping() {
    for caption in [
        "Fig. A: A diagram",
        "Figure IV: A diagram",
        "图一：示意图",
        "图 2-1：示意图",
        "Figure 2.1: <i>A diagram</i>",
    ] {
        let section = parse(&format!(
            "<blockquote id='caption'>{caption}</blockquote><img id='diagram' src='diagram.png'/>"
        ));
        assert!(
            matches!(&section.blocks[..], [Block::Figure(f)] if f.images.len()==1 && f.captions.len()==1 && f.caption_position==CaptionPosition::Before)
        );
    }
    let section = parse(
        "<figure><img src='diagram.png'/><blockquote>Figure 2.1: A diagram</blockquote></figure><p>After.</p>",
    );
    let Block::Figure(outer) = &section.blocks[0] else {
        panic!()
    };
    assert!(
        matches!(outer.body.as_slice(), [Block::Image(_), Block::Text(t)] if t.kind==TextBlockKind::Caption)
    );
    assert!(!outer.body.iter().any(|b| matches!(b, Block::Quote(_))));
}

#[test]
fn weak_or_credited_quotes_and_intervening_content_never_become_captions() {
    for body in [
        "<img src='a.png'/><blockquote>Figure 1.1 shows the result.</blockquote>",
        "<img src='a.png'/><blockquote>Figure 1.1 shows: the result.</blockquote>",
        "<img src='a.png'/><blockquote>Figure 1.1<br/>shows: the result.</blockquote>",
        "<img src='a.png'/><blockquote>Figure Out: How things work.</blockquote>",
        "<img src='a.png'/><blockquote><p>Figure 1.1: A quoted discussion.</p><cite>An author</cite></blockquote>",
        "<img src='a.png'/><blockquote><p>Figure 1.1: A quoted discussion.</p><p>Another paragraph.</p></blockquote>",
        "<img src='a.png'/><p>Intervening prose.</p><blockquote>Figure 1.1: A separate quote.</blockquote>",
        "<img src='a.png'/><hr/><blockquote>Figure 1.1: A separate quote.</blockquote>",
        "<img src='a.png'/><div style='height:24px'/><blockquote>Figure 1.1: A separate quote.</blockquote>",
        "<blockquote>Figure 1.1: A standalone quote.</blockquote>",
    ] {
        let section = parse(body);
        assert!(
            section.blocks.iter().any(|b| matches!(b, Block::Quote(_))),
            "quote lost: {body}"
        );
        assert!(
            !section.blocks.iter().any(|b| matches!(b, Block::Figure(_))),
            "unexpected caption: {body}"
        );
    }
}

#[test]
fn toc_destinations_in_quoted_captions_stay_navigation_boundaries() {
    let item = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    let target = item.href.resolve("#chapter-title").unwrap();
    let section = parse_section_with_hints_and_image_classifier("<html><body><img src='a.png'/><blockquote id='chapter-title'>Figure 1.1: A chapter title</blockquote></body></html>", &item, |_| None, |_| false, SectionParseHints { heading_targets: &[target], ..Default::default() }).unwrap();
    assert!(matches!(
        &section.blocks[..],
        [Block::Image(_), Block::Quote(_)]
    ));
}

#[test]
fn image_width_context_composes_percentages_absolute_caps_and_sibling_branches() {
    let section = parse(
        r#"
        <div style="width:60%;max-width:400px"><div style="width:50%">
            <img src="a.jpg" style="width:100%"/>
        </div><img src="b.jpg" style="width:100%"/></div>
        <div style="width:300px"><div style="width:50%;max-width:80%"><img src="c.jpg"/></div></div>
        <p><span style="width:5%"><img src="d.jpg"/></span></p>
    "#,
    );
    let images = section
        .blocks
        .iter()
        .filter_map(|b| {
            if let Block::Image(i) = b {
                Some(i)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 4);
    let widths = images[..3]
        .iter()
        .map(|image| image.style.container_width.unwrap())
        .collect::<Vec<_>>();
    assert_close(widths[0].resolve(1000.0), 200.0);
    assert_close(widths[0].resolve(400.0), 120.0);
    assert_close(widths[1].resolve(1000.0), 400.0);
    assert_close(widths[1].resolve(400.0), 240.0);
    assert_close(widths[2].resolve(1000.0), 150.0);
    assert!(
        images[3].style.container_width.is_none(),
        "inline spans do not establish a CSS width"
    );
}

#[test]
fn compound_caption_preserves_titles_body_order_and_individual_anchors() {
    let section = parse(
        r#"
        <style>.Caption_Head{font-size:1.1em}.Caption{font-size:.9em}</style>
        <div style="break-inside:avoid"><div style="width:60%"><img id="picture" src="chart.jpg" style="width:100%"/></div>
        <p class="Caption_Head" id="title">What am I paying for?</p>
        <p class="Caption" id="description">Manufacturing and development.</p>
        <p class="Caption">Profits and operating costs.</p></div>
        <p id="after">Unrelated prose.</p>
    "#,
    );
    let [Block::Figure(figure), Block::Text(after)] = section.blocks.as_slice() else {
        panic!("expected a compound figure and prose: {:?}", section.blocks)
    };
    assert_eq!(figure.images.len(), 1);
    assert_eq!(figure.captions.len(), 3);
    assert!(
        figure.text_blocks()
            .all(|c| c.kind == TextBlockKind::Caption)
    );
    assert_close(
        figure.images[0]
            .style
            .container_width
            .unwrap()
            .resolve(1000.0),
        600.0,
    );
    for (id, source) in [
        ("picture", figure.images[0].source.as_ref().unwrap()),
        ("title", figure.captions[0].source.as_ref().unwrap()),
        ("description", figure.captions[1].source.as_ref().unwrap()),
        ("after", after.source.as_ref().unwrap()),
    ] {
        assert_eq!(
            &section
                .anchors
                .iter()
                .find(|a| a.fragment == id)
                .unwrap()
                .source,
            &source.start
        );
    }
}

#[test]
fn caption_titles_without_adjacent_definite_caption_do_not_absorb_prose() {
    for body in [
        "<img src='chart.jpg'/><p class='Caption_Head'>Info title</p><p>Ordinary body.</p>",
        "<p class='Caption_Head'>Standalone title</p><p class='Caption'>Info body.</p>",
        "<img src='chart.jpg'/><p class='Caption_Head'>Info title</p><h3>New section</h3><p class='Caption'>Description.</p>",
    ] {
        let section = parse(body);
        assert!(
            section
                .blocks
                .iter()
                .all(|b| !matches!(b, Block::Figure(_)))
        );
    }
}

#[test]
fn navigation_only_breadcrumbs_are_removed_but_prose_and_note_links_survive() {
    let section = parse_navigation(
        r#"
        <style>.GeneralSymbols-4{font-family:Symbols}</style>
        <div class="backlink_box"><p class="backlink_text"><span class="GeneralSymbols-4">g</span>
        <a href="basics.xhtml">Beauty basics</a><span class="GeneralSymbols-4">g</span><a href="toc.xhtml">Contents</a></p></div>
        <h2 id="packaging">Does packaging matter?</h2>
        <p class="breadcrumbs">Discuss <a href="a.xhtml">one</a> and <a href="b.xhtml">two</a> studies.</p>
        <p class="backlink_text"><a href="a.xhtml" role="doc-backlink">1</a><a href="b.xhtml" role="doc-backlink">2</a></p>
    "#,
    );
    assert_eq!(section.blocks.len(), 3);
    assert!(matches!(&section.blocks[0],Block::Text(t) if t.kind==TextBlockKind::Heading(2)));
    assert!(
        matches!(&section.blocks[0],Block::Text(t) if t.source.as_ref().unwrap().start.node=="n1"),
        "filtering navigation must not renumber previously saved content sources"
    );
    assert!(section.anchors.iter().any(|a| a.fragment == "packaging"));
}

fn parse_navigation(body: &str) -> Section {
    let item = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    parse_section_with_hints_and_image_classifier(
        &format!("<html><body>{body}</body></html>"),
        &item,
        |_| None,
        |_| false,
        SectionParseHints {
            navigation_documents: &[PublicationUrl::parse("OPS/toc.xhtml").unwrap()],
            ancestor_targets: &[PublicationUrl::parse("OPS/basics.xhtml").unwrap()],
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn opaque_class_renaming_preserves_media_relationships_and_navigation() {
    let html = r#"<style>
        .pagebreakinside{page-break-inside:avoid}.width60{width:60%}
        .Caption_Head{font-size:1.1em}.Caption{font-size:.9em}
        .GeneralSymbols-4{font-family:Symbols}
        </style>
        <div class="backlink_box"><p class="backlink_text"><span class="GeneralSymbols-4">g</span>
        <a href="basics.xhtml">Basics</a><span class="GeneralSymbols-4">g</span><a href="toc.xhtml">Contents</a></p></div>
        <h2 id="heading">Chapter</h2>
        <div class="pagebreakinside"><div class="width60"><img src="chart.jpg"/></div>
        <p class="Caption_Head">Costs</p><p class="Caption">Manufacturing costs.</p></div>
        <p>Independent body.</p>"#;
    let mut renamed = html.to_owned();
    for (index, class) in [
        "pagebreakinside",
        "width60",
        "Caption_Head",
        "Caption",
        "backlink_box",
        "backlink_text",
        "GeneralSymbols-4",
    ]
    .into_iter()
    .enumerate()
    {
        renamed = renamed.replace(class, &format!("x{index}"));
    }
    let original = parse_navigation(html);
    assert_eq!(original, parse_navigation(&renamed));
    assert_eq!(original.blocks.len(), 3);
    assert!(matches!(&original.blocks[1], Block::Figure(f) if f.captions.len() == 2));
}

#[test]
fn media_inference_requires_typography_and_respects_toc_destinations() {
    for body in [
        "<div style='break-inside:avoid'><img src='a.jpg'/><p><b>Ordinary paragraph lead.</b> Body text.</p></div>",
        "<div><img src='a.jpg'/><p style='font-size:.9em'>Small but unrelated paragraph.</p></div>",
        "<div style='break-inside:avoid'><img src='a.jpg'/><h3>New heading</h3><p style='font-size:.9em'>Body.</p></div>",
    ] {
        assert!(
            parse(body)
                .blocks
                .iter()
                .all(|b| !matches!(b, Block::Figure(_)))
        );
    }
    let descriptor = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    let section = parse_section_with_hints_and_image_classifier(
        "<html><body><div style='break-inside:avoid'><img src='a.jpg'/><p id='new-section' style='font-size:.9em'>Actual section heading</p></div></body></html>",
        &descriptor, |_| None, |_| false,
        SectionParseHints { heading_targets: &[PublicationUrl::parse("chapter.xhtml#new-section").unwrap()], ..Default::default() },
    ).unwrap();
    assert!(matches!(&section.blocks[1], Block::Text(t) if t.kind == TextBlockKind::Paragraph));
}

#[test]
fn known_navigation_destinations_do_not_hide_visible_toc_or_body_references() {
    for body in [
        "<nav epub:type='toc' xmlns:epub='http://www.idpf.org/2007/ops'><a href='basics.xhtml'>Basics</a><a href='toc.xhtml'>Contents</a></nav>",
        "<p>See <a href='basics.xhtml'>Basics</a> and <a href='toc.xhtml'>Contents</a> for details.</p>",
        "<h2>Chapter</h2><div><a href='basics.xhtml'>Basics</a><a href='toc.xhtml'>Contents</a></div>",
        "<div><a href='basics.xhtml' role='doc-backlink'>1</a><a href='toc.xhtml' role='doc-backlink'>2</a></div>",
    ] {
        assert!(!parse_navigation(body).blocks.is_empty());
    }
}

#[test]
fn captions_can_follow_introductory_text_or_use_explicit_image_links_and_widths() {
    let section = parse(
        r##"
        <div style="break-inside:avoid"><p>Introductory text.</p><img src="a.jpg"/>
            <p style="font-size:1.1em">Title A</p><p style="font-size:.9em">Description A.</p>
            <p>Normal body after the image.</p></div>
        <div><img id="image-b" src="b.jpg"/>
            <p style="font-size:1.1em"><a href="#image-b">Title B</a></p>
            <p style="font-size:.9em">Description B.</p></div>
        <div><div style="width:70%;text-align:center"><img src="c.jpg"/></div>
            <p style="font-size:1.1em">Title C</p><p style="font-size:.9em">Description C.</p>
            <p>Normal body after the image.</p></div>
    "##,
    );
    let figures = section
        .blocks
        .iter()
        .filter_map(|block| {
            if let Block::Figure(figure) = block {
                Some(figure)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(figures.len(), 3);
    assert!(figures.iter().all(|figure| figure.captions.len() == 2));
    assert_eq!(
        section
            .blocks
            .iter()
            .filter(|block| matches!(block, Block::Text(t) if t.kind == TextBlockKind::Paragraph))
            .count(),
        3
    );
}

#[test]
fn breadcrumb_at_a_later_unit_boundary_is_removed_with_sources_preserved() {
    let section = parse_navigation(
        "<p>Earlier subsection.</p><div><a href='basics.xhtml'>Basics</a><a href='toc.xhtml'>Contents</a></div><div><h2 id='second'>Next subsection</h2><p>Body.</p></div>",
    );
    assert_eq!(section.blocks.len(), 3);
    assert!(
        matches!(&section.blocks[1], Block::Text(t) if t.kind == TextBlockKind::Heading(2) && t.source.as_ref().unwrap().start.node == "n2")
    );
}

#[test]
fn explicit_pullquote_semantics_do_not_require_author_styles() {
    for body in [
        "<p role='doc-pullquote'>Extracted quotation.</p>",
        "<aside xmlns:epub='http://www.idpf.org/2007/ops' epub:type='pullquote'><p>Extracted quotation.</p></aside>",
    ] {
        assert!(matches!(parse(body).blocks.as_slice(), [Block::Quote(_)]));
    }
}

fn assert_close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
}

#[test]
fn legacy_dimensions_apply_only_to_supported_elements_and_css_keeps_its_priority() {
    let section = parse("<p width='0%' height='4%'><img src='a.png'/></p><div width='10%'><img src='b.png'/></div><p style='width:0%'><img src='c.png'/></p><img src='d.png' width='600' height='400' style='width:auto'/><img src='e.png' width='600' style='width:40%;width:-1px;width:garbage'/>");
    let images: Vec<_> = section.blocks.iter().filter_map(|b| if let Block::Image(i)=b {Some(i)} else {None}).collect();
    assert_eq!(images.len(),5);
    assert!(images[0].style.container_width.is_none());
    assert!(images[1].style.container_width.is_none());
    assert_eq!(images[2].style.container_width.unwrap().resolve(800.0),0.0);
    assert_eq!(images[3].style.width,None);
    assert_eq!(images[3].style.height,Some(ImageLength::Pixels(400.0)));
    assert_eq!(images[4].style.width,Some(ImageLength::Fraction(0.4)));
}

#[test]
fn containing_heights_and_overflow_are_separate_from_image_sizing() {
    let section = parse("<div style='width:0'><img src='visible.png'/></div><div style='width:0;overflow:hidden'><img src='clipped.png'/></div><div style='height:200px'><img src='half.png' style='height:50%'/><p><img src='auto.png' style='height:50%'/></p></div><div style='height:50%'><img src='indefinite.png' style='height:100%'/></div>");
    let images: Vec<_> = section.blocks.iter().filter_map(|b| if let Block::Image(i)=b {Some(i)} else {None}).collect();
    assert_eq!(images.len(),5);
    assert!(images[0].style.clip_width.is_none());
    assert_eq!(images[1].style.clip_width.unwrap().resolve(800.0),0.0);
    assert_eq!(images[2].style.container_height.unwrap().resolve(700.0),200.0);
    assert!(images[3].style.container_height.is_none());
    assert!(images[4].style.container_height.is_none());
    assert_eq!(image_context::html_dimension("0%",true),None);
    assert_eq!(image_context::html_dimension("0%",false),Some(ImageLength::Fraction(0.0)));
    assert_eq!(image_context::html_dimension("2em",false),Some(ImageLength::Pixels(2.0)));
}

#[test]
fn inline_wrappers_do_not_establish_width_and_overflow_axes_compute_together() {
    let section=parse("<p style='display:inline;width:0'><img src='a.png' style='width:100%'/></p><div style='display:contents;width:0'><img src='b.png'/></div><div style='width:100px;height:50px;overflow-x:hidden;overflow-y:visible'><img src='c.png'/></div>");
    let images:Vec<_>=section.blocks.iter().filter_map(|b|if let Block::Image(i)=b{Some(i)}else{None}).collect();
    assert_eq!(images.len(),3);
    assert!(images[0].style.container_width.is_none());
    assert!(images[1].style.container_width.is_none());
    assert_eq!(images[2].style.clip_width.unwrap().resolve(800.0),100.0);
    assert_eq!(images[2].style.clip_height.unwrap().resolve(700.0),50.0);
}
