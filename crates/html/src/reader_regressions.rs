use super::*;

#[test]
fn empty_page_marker_wrappers_do_not_add_lines_to_list_items_or_table_cells() {
    let section = parse("<ol><li><p>First item.</p><div><span role='doc-pagebreak' id='page48' title='48'/></div></li><li><p>Second item.</p></li></ol><table><tr><td><p>Cell.</p><div><span epub:type='pagebreak' id='page49' title='49'/></div></td></tr></table>");
    let first = section.blocks[0].text_blocks().next().unwrap();
    assert!(!first.content.iter().any(|inline| matches!(inline, Inline::Break)), "{:?}", first.content);
    assert_eq!(section.anchors.iter().find(|a| a.fragment == "page48").unwrap().source.text_offset, 11);
    let Block::Table(table) = &section.blocks[2] else { panic!() };
    assert!(!table.rows[0].cells[0].text.content.iter().any(|inline| matches!(inline, Inline::Break)));
}

#[test]
fn marker_wrappers_are_transparent_but_authored_content_and_breaks_remain() {
    let section = parse("<p>Before.</p><div style='margin:0;padding:0;width:0;height:0'><span><span role='doc-pagebreak' id='page2' title='2'/></span></div><p id='after'>After.</p>");
    assert_eq!(section.blocks.len(), 2);
    assert_eq!(section.anchors.iter().find(|a| a.fragment == "page2").unwrap().source,
        section.anchors.iter().find(|a| a.fragment == "after").unwrap().source);
    let list = parse("<ol><li><p>First paragraph.</p><div><span role='doc-pagebreak' id='p3' title='3'/></div><p>Second paragraph.</p><div><br/></div></li></ol>");
    let text = list.blocks[0].text_blocks().next().unwrap();
    assert_eq!(text.content.iter().filter(|inline| matches!(inline, Inline::Break)).count(), 3);
    let visible = parse("<div><span role='doc-pagebreak'>42</span></div><p>Body.</p>");
    assert!(visible.blocks.iter().flat_map(Block::text_blocks).any(|t| text_block_text(t).contains("42")));
    let spaced = parse("<div style='height:24px'><span role='doc-pagebreak' title='4'/></div><p>Body.</p>");
    assert!(spaced.blocks.iter().any(|block| matches!(block, Block::Separator(s) if s.kind == rebook_publication::SeparatorKind::AuthoredSpacing)));
}

#[test]
fn printed_pages_are_semantic_locations_without_artificial_line_breaks() {
    let section = parse("<p id='start'>Before <span epub:type='pagebreak' role='doc-pagebreak' id='p42' title='42'/>after.</p><div role='doc-pagebreak' id='p43' aria-label='43'/><p id='next'>Next.</p>");
    assert_eq!(section.blocks.len(), 2);
    assert_eq!(section.blocks.iter().flat_map(Block::text_blocks).map(text_block_text).collect::<String>(), "Before after.Next.");
    let marker = section.anchors.iter().find(|a| a.fragment == "p42").unwrap();
    assert_eq!(marker.page_label.as_deref(), Some("42"));
    assert_eq!(marker.source.text_offset, 7);
    assert_eq!(section.anchors.iter().find(|a| a.fragment == "p43").unwrap().source,
        section.anchors.iter().find(|a| a.fragment == "next").unwrap().source);
    let visible = parse("<div role='doc-pagebreak' id='page'>44</div><p>Body.</p>");
    assert_eq!(visible.anchors.iter().find(|a| a.fragment == "page").unwrap().page_label.as_deref(), Some("44"));
    assert!(matches!(visible.blocks[0], Block::Separator(_)));
    let Block::Text(body) = &visible.blocks[1] else { panic!() };
    assert_eq!(visible.anchors.iter().find(|a| a.fragment == "page").unwrap().source, body.source.as_ref().unwrap().start);
}

#[test]
fn empty_zero_boxes_disappear_but_explicit_spacing_and_breaks_remain() {
    let section = parse("<p>Before.</p><p style='margin:0;padding:0;width:0;height:0'> </p><div style='margin:0;padding:0;width:0;height:0'><span role='doc-pagebreak' id='page' title='2'/></div><p id='next'>After.</p>");
    assert_eq!(section.blocks.len(), 2);
    let spaced = parse("<p>Before.</p><div style='height:24px;margin:12px 0'/><p><br/></p><p>After.</p>");
    assert!(spaced.blocks.iter().any(|block| matches!(block, Block::Separator(s) if s.kind == rebook_publication::SeparatorKind::AuthoredSpacing)));
    assert!(spaced.blocks.iter().any(|block| matches!(block, Block::Separator(s) if s.kind == rebook_publication::SeparatorKind::Spacing)));
    let padded = parse("<div style='padding:12px 0'/><p>Body.</p>");
    assert!(matches!(&padded.blocks[0], Block::Separator(s) if s.style.margin_before == 12.0 && s.style.margin_after == 12.0));
}

#[test]
fn transparent_content_uses_css_cascade_and_preserves_following_source_ids() {
    for hidden in [
        "<p style='opacity:0'>Hidden</p>",
        "<div style='opacity:0'><p style='opacity:1'>Hidden</p></div>",
        "<style>.transparent {opacity:0 !important}</style><p class='transparent' style='opacity:1'>Hidden</p>",
    ] {
        let body = format!("{hidden}<p id='after'>Visible</p>");
        let section = parse(&body);
        assert_eq!(
            section
                .blocks
                .iter()
                .flat_map(Block::text_blocks)
                .map(text_block_text)
                .collect::<String>(),
            "Visible"
        );
        let visible = parse(&body.replace("opacity:0", "opacity:1"));
        assert_eq!(
            section.anchors.iter().find(|a| a.fragment == "after"),
            visible.anchors.iter().find(|a| a.fragment == "after")
        );
    }
    let notes = parse(
        "<p>Body<a epub:type='noteref' href='#note'>1</a></p><aside id='note' epub:type='footnote' style='opacity:0'><p>Note content.</p></aside>",
    );
    assert!(
        notes
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Note(_)))
    );
}

#[test]
fn aria_hidden_ornaments_preserve_authored_text_as_semantic_separators() {
    let section = parse(
        "<p>Before.</p><div aria-hidden='true'>—</div><p>Between.</p><p aria-hidden='true'>• • •</p><div aria-hidden='true' style='border-top:1px solid black'/><p>After.</p>",
    );
    let separators: Vec<_> = section
        .blocks
        .iter()
        .filter_map(|block| {
            if let Block::Separator(s) = block {
                Some(s)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(separators.len(), 3);
    assert_eq!(text_block_text(separators[0].text.as_ref().unwrap()), "—");
    let source = parse(
        "<p aria-hidden='true'>Meaningful alternative text.</p><p id='release_identifier_line'>pid_example_123_c0_r0</p>",
    );
    assert_eq!(source.blocks.iter().flat_map(Block::text_blocks).count(), 2);
}

#[test]
fn linked_stylesheet_can_hide_a_publisher_identifier() {
    let descriptor = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    let section = parse_section("<html><head><link rel='stylesheet' href='style.css'/></head><body><p class='identifier'>Hidden identifier</p><p>Visible.</p></body></html>", &descriptor,
        |href| (href.path()=="style.css").then(|| ".identifier {opacity:0}".into())).unwrap();
    assert_eq!(
        section
            .blocks
            .iter()
            .flat_map(Block::text_blocks)
            .map(text_block_text)
            .collect::<String>(),
        "Visible."
    );
}

#[test]
fn promoting_a_caption_raster_preserves_following_source_anchors() {
    let body = "<p><img src='diagram.png' alt='Figure'/>Fig 4.14: Example</p><p id='next'>Following prose.</p>";
    let promoted = parse(body);
    let original = parse(&body.replace("alt='Figure'", "alt='Ordinary image'"));
    assert_eq!(
        promoted
            .anchors
            .iter()
            .find(|anchor| anchor.fragment == "next"),
        original
            .anchors
            .iter()
            .find(|anchor| anchor.fragment == "next")
    );
}

fn text_block_text(text: &TextBlock) -> String {
    text.content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|run| run.text.as_str())
        .collect()
}

fn parse(body: &str) -> Section {
    parse_section(
        &format!("<html xmlns:epub='http://www.idpf.org/2007/ops'><body>{body}</body></html>"),
        &SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        },
        |_| None,
    )
    .unwrap()
}

#[test]
fn preceding_numbered_caption_attaches_to_its_figure() {
    let section = parse(
        "<p id='caption'><b>Figure 11.12</b> A diagram</p><figure><div><img src='diagram.jpg'/></div></figure><p>Following prose.</p>",
    );
    let [Block::Figure(figure), Block::Text(_)] = section.blocks.as_slice() else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(figure.caption_position, CaptionPosition::Before);
    assert_eq!(figure.caption_blocks().count(), 1);
    assert!(
        figure
            .text_blocks()
            .any(|text| text_block_text(text).contains("11.12"))
    );
    assert!(
        section
            .anchors
            .iter()
            .any(|anchor| anchor.fragment == "caption")
    );
}

#[test]
fn note_definitions_do_not_break_image_caption_association() {
    let section = parse(
        "<p><img src='chart.jpg'/></p><aside epub:type='footnote' id='n1'><p>Note.</p></aside><p>图一 示例图注</p><p>Following prose.</p>",
    );
    let [Block::Figure(figure), Block::Note(_), Block::Text(_)] = section.blocks.as_slice() else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(figure.images.len(), 1);
    assert_eq!(figure.captions.len(), 1);
    assert_eq!(text_block_text(&figure.captions[0]), "图一 示例图注");
    let unrelated =
        parse("<img src='chart.jpg'/><p>Ordinary prose.</p><p>Figure 1 A different diagram</p>");
    assert!(matches!(unrelated.blocks[0], Block::Image(_)));
}

#[test]
fn figure_raster_inside_its_numbered_caption_is_a_block_but_symbols_stay_inline() {
    let section = parse(
        "<div><p><span><img src='diagram.png' alt='Figure'/></span> Fig 4.14: Example <img src='symbol.png' style='height:1em' alt='Figure'/></p></div>",
    );
    assert!(
        section.blocks.iter().any(
            |block| matches!(block, Block::Figure(figure) if figure.images.iter().any(|image| image.href.path() == "diagram.png"))
        ),
        "{:?}",
        section.blocks
    );
    assert_eq!(
        section
            .blocks
            .iter()
            .flat_map(Block::text_blocks)
            .flat_map(|text| &text.content)
            .filter(|inline| matches!(inline, Inline::Image(_)))
            .count(),
        1
    );
}

#[test]
fn skip_notes_navigation_is_hidden_without_hiding_note_content_or_prose() {
    let section = parse(
        "<p>Body.</p><p><a href='next.xhtml'>Skip Notes</a></p><aside epub:type='footnote' id='n1'><p>Note content.</p></aside>",
    );
    assert!(
        !section
            .blocks
            .iter()
            .flat_map(Block::text_blocks)
            .any(|text| text_block_text(text) == "Skip Notes")
    );
    assert!(
        section
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Note(_)))
    );
    let ordinary =
        parse("<p><a href='next.xhtml'>Skip Notes</a></p><p>Discussion of navigation.</p>");
    assert!(
        ordinary
            .blocks
            .iter()
            .flat_map(Block::text_blocks)
            .any(|text| text_block_text(text) == "Skip Notes")
    );
}

#[test]
fn question_title_row_is_recognized_despite_a_trailing_source_row() {
    let section = parse(
        "<table><tr><td colspan='2'>Which item do you prefer?</td></tr><tr><td>Group</td><td>Result</td></tr><tr><td>A</td><td>15</td></tr><tr><td colspan='2'>Source: Example survey.</td></tr></table>",
    );
    let Block::Table(table) = &section.blocks[0] else {
        panic!();
    };
    assert_eq!(table.rows[0].cells[0].text.kind, TextBlockKind::Caption);
    assert_eq!(
        table.rows.len(),
        4,
        "Authored grid and source IDs must remain intact"
    );
}
