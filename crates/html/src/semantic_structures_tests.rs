use super::*;

fn parse(body: &str) -> Section {
    let descriptor = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    parse_section_with_image_metadata(
        &format!("<html><body>{body}</body></html>"),
        &descriptor,
        |_| None,
        |_| false,
        |href| {
            Some(if href.path().contains("large") {
                [300, 200]
            } else {
                [16, 16]
            })
        },
        SectionParseHints::default(),
    )
    .unwrap()
}
fn text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .map(|b| match b {
            Block::Text(t) => t
                .content
                .iter()
                .filter_map(|i| {
                    if let Inline::Text(r) = i {
                        Some(r.text.as_str())
                    } else {
                        None
                    }
                })
                .collect(),
            Block::Note(n) => text(&n.blocks),
            _ => String::new(),
        })
        .collect()
}

#[test]
fn hidden_format_fallbacks_keep_only_visible_images_and_stable_following_sources() {
    let hidden = parse(
        "<style>img.alt{display:none}p.fallback{display:none}</style><figure><img src='one.jpg'/></figure><p class='fallback'><img src='one.jpg'/></p><p><img class='alt' src='two.jpg'/></p><p id='after'>After.</p>",
    );
    let visible = parse(
        "<style>img.alt{display:inline}p.fallback{display:block}</style><figure><img src='one.jpg'/></figure><p class='fallback'><img src='one.jpg'/></p><p><img class='alt' src='two.jpg'/></p><p id='after'>After.</p>",
    );
    let count = hidden
        .blocks
        .iter()
        .map(|b| match b {
            Block::Image(_) => 1,
            Block::Figure(f) => f.images.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert_eq!(count, 1);
    assert_eq!(
        hidden.anchors.iter().find(|a| a.fragment == "after"),
        visible.anchors.iter().find(|a| a.fragment == "after")
    );
    assert!(text(&hidden.blocks).contains("After."));
}

#[test]
fn css_auto_clears_html_image_dimensions_with_cascade_priority() {
    let section = parse("<style>.responsive {height: auto !important} .wide {width:auto} .fixed{height:240px}</style><img class='responsive' src='a.png' width='800' height='600' style='height:500px;display:block'/><img class='wide' src='b.png' width='800' height='600' style='display:block'/><img class='fixed' src='c.png' width='800' height='600' style='display:block'/><img src='d.png' width='800' height='600' style='width: AUTO; height:auto;display:block'/>");
    let images: Vec<_> = section.blocks.iter().filter_map(|b| match b {Block::Image(i)=>Some(i),_=>None}).collect();
    assert_eq!(images.len(),4);
    assert_eq!(images[0].style.width,Some(ImageLength::Pixels(800.0)));
    assert_eq!(images[0].style.height,None);
    assert_eq!(images[1].style.width,None);
    assert_eq!(images[1].style.height,Some(ImageLength::Pixels(600.0)));
    assert_eq!(images[2].style.height,Some(ImageLength::Pixels(240.0)));
    assert_eq!(images[3].style.width,None);
    assert_eq!(images[3].style.height,None);
}

#[test]
fn hidden_footnote_definitions_remain_available_to_the_popup() {
    let section = parse(
        "<p>Body<a href='#n1' role='doc-noteref'>1</a></p><aside hidden='hidden' id='n1' role='doc-footnote'><p>Hidden note body.</p></aside>",
    );
    assert!(text(&section.blocks).contains("Hidden note body."));
    assert!(section.blocks.iter().any(|b| matches!(b, Block::Note(_))));
}

#[test]
fn offscreen_alternatives_are_filtered_without_losing_sources_or_visible_positioned_text() {
    let body = "<style>.alternative{position:absolute;left:-9999em;width:1px;height:1px;overflow:hidden}.frame p{position:absolute;top:50%;left:50%;width:70%;transform:translate(-50%,-50%)}</style><div class='alternative' id='table-desc'><p>Screen reader table description.</p></div><div class='frame'><p>Visible text in decorative border.</p></div><p id='after'>After.</p>";
    let hidden = parse(body);
    let visible = parse(&body.replace("left:-9999em", "left:0"));
    assert!(!text(&hidden.blocks).contains("Screen reader"));
    assert!(text(&hidden.blocks).contains("Visible text in decorative border."));
    assert_eq!(hidden.anchors.iter().find(|a| a.fragment == "after"),
        visible.anchors.iter().find(|a| a.fragment == "after"));
    let clipped = parse("<p style='position:absolute;width:1px;height:1px;clip:rect(0,0,0,0)'>Hidden</p><p style='position:absolute;width:1px;height:1px'>Visible tiny box</p><p style='position:relative;left:-2000px;width:1px'>Visible relative text</p>");
    assert!(!text(&clipped.blocks).contains("Hidden"));
    assert!(text(&clipped.blocks).contains("Visible tiny box"));
    assert!(text(&clipped.blocks).contains("Visible relative text"));
    let shifted = parse("<p style='position:absolute;left:-2000px;width:1px;height:1px;overflow:hidden;transform:translateX(2000px)'>Visible transformed text</p><p style='position:absolute;left:-2000px;width:1px'>Potentially overflowing text</p>");
    assert!(text(&shifted.blocks).contains("Visible transformed text"));
    assert!(text(&shifted.blocks).contains("Potentially overflowing text"));
}

#[test]
fn hidden_inline_text_keeps_later_source_offsets_without_displaying_the_text() {
    let section = parse("<p>A<span hidden='hidden'>Hidden</span>B</p>");
    let visible = parse("<p>A<span>Hidden</span>B</p>");
    let Block::Text(actual) = &section.blocks[0] else {
        panic!()
    };
    let Block::Text(before) = &visible.blocks[0] else {
        panic!()
    };
    assert_eq!(actual.source, before.source);
    assert_eq!(text(&section.blocks).replace('\u{2060}', ""), "AB");
}

#[test]
fn native_marker_images_get_metadata_but_content_images_do_not() {
    let section = parse(
        "<ul><li><img src='bullet.jpg'/> First.</li><li><img src='bullet.jpg'/> Second.</li><li><img src='large.jpg'/> Picture.</li><li>Glyph <img src='glyph.jpg'/> example.</li></ul><ul><li><img src='bullet.jpg'/> Standalone item.</li></ul>",
    );
    let items = section
        .blocks
        .iter()
        .filter_map(|b| {
            if let Block::Text(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(items[0].style.authored_list_marker.is_some());
    assert!(items[1].style.authored_list_marker.is_some());
    assert!(items[2].style.authored_list_marker.is_none());
    assert!(items[3].style.authored_list_marker.is_none());
    assert!(items[4].style.authored_list_marker.is_some());
}

#[test]
fn definition_wrappers_and_nested_lists_share_the_outer_container() {
    let section = parse(
        "<dl><div><dt>One</dt><dd><p>First paragraph.</p><p>Second paragraph.</p><ul><li>Nested.</li></ul></dd></div><dt>Two</dt><dd>Last.</dd></dl>",
    );
    let texts = section
        .blocks
        .iter()
        .filter_map(|b| {
            if let Block::Text(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        texts
            .iter()
            .filter(|t| matches!(t.kind, TextBlockKind::DefinitionTerm { .. }))
            .count(),
        2
    );
    assert_eq!(
        texts
            .iter()
            .filter(|t| matches!(t.kind, TextBlockKind::DefinitionDescription { .. }))
            .count(),
        2
    );
    assert!(texts[0].style.list_group.is_some());
    assert!(
        texts
            .iter()
            .all(|t| t.style.list_group == texts[0].style.list_group)
    );
    assert_eq!(text(&section.blocks).matches("Nested.").count(), 1);
}

#[test]
fn glossary_entries_use_structure_not_class_names_and_keep_source_offsets() {
    let section = parse(
        "<section role='doc-glossary'><p><b>acetate:</b> A textile.</p><p>经线：纵向的线。</p></section><p>Ordinary: prose.</p>",
    );
    assert_eq!(section.blocks.len(), 5);
    let Block::Text(first) = &section.blocks[0] else {
        panic!()
    };
    let Block::Text(description) = &section.blocks[1] else {
        panic!()
    };
    assert!(matches!(first.kind, TextBlockKind::DefinitionTerm { .. }));
    assert_eq!(
        first.source.as_ref().unwrap().end,
        description.source.as_ref().unwrap().start
    );
    assert_eq!(
        text(&section.blocks),
        "acetate: A textile.经线：纵向的线。Ordinary: prose."
    );
    assert!(matches!(&section.blocks[4],Block::Text(t) if t.kind==TextBlockKind::Paragraph));
}
