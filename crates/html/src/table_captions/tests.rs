use super::*;
use crate::{Inline, PublicationUrl, Section, SpineItem, SpineItemId, parse_section};

fn parse(body: &str) -> Section {
    let descriptor = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: Vec::new(),
    };
    parse_section(
        &format!("<html><body>{body}</body></html>"),
        &descriptor,
        |_| None,
    )
    .unwrap()
}

const GRID: &str = "<table><tr><td>Value</td><td>42</td></tr></table>";

#[test]
fn imported_nested_document_wrapper_keeps_grid_and_chinese_caption() {
    let section = parse(&format!(
        "<section><p>表 7-1 三个阶段的特点</p><html><head/><body>{GRID}</body></html><p>Following prose.</p></section>"
    ));
    let [Block::Table(table), Block::Text(_)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks)
    };
    assert_eq!(table.before.len(), 1);
    assert_eq!(table.rows.len(), 1);
}

#[test]
fn ambiguous_label_between_unscoped_grids_stays_independent() {
    let section = parse(&format!("{GRID}<p>Table 2. Results</p>{GRID}"));
    assert!(matches!(
        section.blocks.as_slice(),
        [Block::Table(_), Block::Text(_), Block::Table(_)]
    ));
}

#[test]
fn explicit_table_caption_class_works_for_grids_and_images() {
    for media in [GRID, "<img src='table.png'/>"] {
        let section = parse(&format!(
            "<p class='table-caption'>Measured results</p>{media}"
        ));
        assert!(matches!(
            section.blocks.as_slice(),
            [Block::Table(_) | Block::Figure(_)]
        ));
    }
}

#[test]
fn captions_on_either_side_keep_text_and_source() {
    for (label, before) in [
        ("Table 2.1 Results", true),
        ("表 7-1 三个阶段的特点", false),
    ] {
        let caption = format!("<p id='caption'>{label}</p>");
        let section = parse(&if before {
            format!("{caption}{GRID}")
        } else {
            format!("{GRID}{caption}")
        });
        let [Block::Table(table)] = &section.blocks[..] else {
            panic!("{:?}", section.blocks)
        };
        assert_eq!(table.before.len(), usize::from(before));
        assert_eq!(table.after.len(), usize::from(!before));
        assert_eq!(table.text_blocks().count(), 3);
        assert!(table.text_blocks().all(|text| text.source.is_some()));
        assert_eq!(section.anchors.len(), 1);
    }
}

#[test]
fn scoped_note_then_caption_retains_order_and_reference() {
    let section = parse(&format!(
        "<div class='table'>{GRID}<p class='note2'>NOTE: Rounded values.</p><p class='table'><b>TABLE 1.1</b> Preferences<a href='notes.xhtml#n1' role='doc-noteref'>43</a></p></div><p>Following prose.</p>"
    ));
    let Block::Table(table) = &section.blocks[0] else {
        panic!()
    };
    assert_eq!(table.after.len(), 2);
    assert_eq!(table.after[0].kind, TextBlockKind::Paragraph);
    assert_eq!(table.after[1].kind, TextBlockKind::Caption);
    assert!(
        table.after[1]
            .content
            .iter()
            .any(|inline| matches!(inline, Inline::Text(run) if run.link.is_some()))
    );
    assert_eq!(section.blocks.len(), 2);
}

#[test]
fn consecutive_wrapped_tables_do_not_steal_next_title() {
    let section = parse(&format!(
        "<div class='table'><p class='title'>Table 5-3. Joins</p><div class='table-contents'>{GRID}</div></div><div class='table'><p class='title'>Table 5-4. Dash patterns</p><div class='table-contents'>{GRID}</div></div>"
    ));
    assert_eq!(section.blocks.len(), 2);
    for block in section.blocks {
        let Block::Table(table) = block else { panic!() };
        assert_eq!(table.before.len(), 1);
        assert!(table.after.is_empty());
    }
}

#[test]
fn native_caption_bottom_preserves_inline_symbols_and_paragraph_breaks() {
    let section = parse(
        "<style>caption { caption-side: bottom; }</style><table><caption><p>Table 1. <em>Values</em></p><p>With <img src='symbol.png'/> symbols.</p></caption><tr><td>42</td></tr></table>",
    );
    let [Block::Table(table)] = &section.blocks[..] else {
        panic!()
    };
    assert!(table.before.is_empty());
    assert_eq!(table.after.len(), 1);
    assert!(
        table.after[0]
            .content
            .iter()
            .any(|inline| matches!(inline, Inline::Image(_)))
    );
    assert!(
        table.after[0]
            .content
            .iter()
            .any(|inline| matches!(inline, Inline::Break))
    );
}

#[test]
fn image_tables_accept_preceding_and_following_captions() {
    for before in [true, false] {
        let media = "<p><img src='table.png'/></p>";
        let caption = "<p>Table 4. Results</p>";
        let section = parse(&if before {
            format!("{caption}{media}")
        } else {
            format!("{media}{caption}")
        });
        let [Block::Figure(figure)] = &section.blocks[..] else {
            panic!("{:?}", section.blocks)
        };
        assert_eq!(figure.images.len(), 1);
        assert_eq!(figure.captions.len(), 1);
        assert_eq!(
            figure.caption_position,
            if before {
                CaptionPosition::Before
            } else {
                CaptionPosition::After
            }
        );
    }
}

#[test]
fn prose_and_cell_headers_remain_untouched() {
    let section = parse(&format!(
        "<p>Table 1 shows the results.</p>{GRID}<p>Following discussion.</p>"
    ));
    assert_eq!(section.blocks.len(), 3);
    let section = parse(
        "<div class='table'><p>TABLE 3.1</p><table><thead><tr><td colspan='3'><p class='table'>Benchmark phenomena</p></td></tr></thead><tbody><tr><td>Learning</td></tr></tbody></table></div>",
    );
    let [Block::Table(table)] = &section.blocks[..] else {
        panic!()
    };
    assert_eq!(table.before.len(), 1);
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.rows[0].cells[0].column_span, 3);
}
