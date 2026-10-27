use super::*;

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
