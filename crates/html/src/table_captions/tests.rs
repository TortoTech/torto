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
fn split_table_number_and_title_in_pagination_wrapper_are_captions() {
    let title =
        "Relationship between random and control variables and internal and external validity.";
    let section = parse(&format!(
        "<div class='pageavoid' id='cetable1'><p class='tnum'>TABLE 4.1</p><p class='ttitle'><a id='cecap14'></a><a id='spara14'></a>{title}</p>{GRID}</div><p>Following discussion.</p>"
    ));
    let [Block::Table(table), Block::Text(_)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks)
    };
    assert_eq!(table.before.len(), 1);
    for (caption, expected) in table.before.iter().zip([format!("TABLE 4.1 {title}")]) {
        assert_eq!(caption.kind, TextBlockKind::Caption);
        assert!(caption.source.is_some());
        let text: String = caption
            .content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) => Some(run.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, expected);
    }
    assert!(table.after.is_empty());
    assert_eq!(section.anchors.len(), 3);
    let source = table.before[0].source.as_ref().unwrap();
    assert!(
        section
            .anchors
            .iter()
            .all(|anchor| anchor.source == source.start)
    );
    assert_eq!(
        source.end.text_offset,
        format!("TABLE 4.1 {title}").chars().count() as u64
    );
    assert!(
        !table.before[0]
            .content
            .iter()
            .any(|inline| matches!(inline, Inline::Break))
    );
}

#[test]
fn pagination_wrapper_does_not_make_prose_a_table_caption() {
    for class in ["", "title", "ttitle-extra"] {
        let section = parse(&format!(
            "<div class='pageavoid'><p>TABLE 4.1</p><p class='{class}'>Ordinary discussion.</p><p>Another paragraph.</p>{GRID}</div>"
        ));
        let [
            Block::Text(_),
            Block::Text(_),
            Block::Text(_),
            Block::Table(table),
        ] = &section.blocks[..]
        else {
            panic!("{:?}", section.blocks)
        };
        assert!(table.before.is_empty());
        assert!(table.after.is_empty());
    }
}

#[test]
fn split_caption_structure_needs_no_publisher_classes() {
    for label in ["TABLE 4.1", "表 4.1"] {
        for media in [GRID, "<p><img src='table.png'/></p>"] {
            let section = parse(&format!(
                "<p>{label}</p><!-- spacer --><a id='title'/><p><b>Relationship between variables.</b></p>{media}"
            ));
            let captions = match &section.blocks[..] {
                [Block::Table(table)] => &table.before,
                [Block::Figure(figure)] => &figure.captions,
                _ => panic!("{:?}", section.blocks),
            };
            assert_eq!(captions.len(), 1);
            assert!(captions[0].content.iter().any(|inline| matches!(inline, Inline::Text(run) if run.style.bold && run.text.contains("Relationship"))));
            assert_eq!(
                section.anchors[0].source,
                captions[0].source.as_ref().unwrap().start
            );
        }
    }
    let section = parse(&format!(
        "<p>Table 4.1 shows the results.</p><p>Discussion.</p>{GRID}"
    ));
    assert!(matches!(
        section.blocks.as_slice(),
        [Block::Text(_), Block::Text(_), Block::Table(_)]
    ));
}

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
    for class in ["table-caption", "table-title"] {
        for media in [GRID, "<img src='table.png'/>"] {
            let section = parse(&format!("<p class='{class}'>Measured results</p>{media}"));
            assert!(matches!(
                section.blocks.as_slice(),
                [Block::Table(_) | Block::Figure(_)]
            ));
        }
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

#[test]
fn spanning_title_is_marked_without_moving_cells_or_sources() {
    let section = parse(
        "<div class='table'><p>TABLE 3.1</p><table><thead><tr><td colspan='3' id='title'><p class='table'>Benchmark <em>phenomena</em><a href='notes.xhtml#n1' role='doc-noteref'>1</a></p></td></tr><tr><td>Category</td><td>Description</td></tr></thead><tbody><tr><td>Learning</td><td>Effects</td></tr></tbody></table></div>",
    );
    let [Block::Table(table)] = &section.blocks[..] else {
        panic!()
    };
    assert_eq!(table.rows.len(), 3);
    assert_eq!(table.before.len(), 1);
    let title = &table.rows[0].cells[0];
    assert_eq!(title.column_span, 3);
    assert_eq!(title.text.kind, TextBlockKind::Caption);
    assert_eq!(
        section.anchors[0].source,
        title.text.source.as_ref().unwrap().start
    );
    assert!(
        title
            .text
            .content
            .iter()
            .any(|inline| matches!(inline, Inline::Text(run) if run.style.italic))
    );
    assert!(
        title
            .text
            .content
            .iter()
            .any(|inline| matches!(inline, Inline::Text(run) if run.link.is_some()))
    );
    assert_eq!(table.text_blocks().count(), 6);
}

#[test]
fn distinct_centered_title_over_column_labels_needs_no_publisher_classes() {
    let section = parse(
        "<style>.title {text-align:center} .label {text-align:justify}</style><table><tr><td colspan='5'><p class='title'>九型人格</p></td></tr><tr><td><p class='label'>类型</p></td><td><p class='label'>理想</p></td><td><p class='label'>恐惧</p></td><td><p class='label'>愿望</p></td><td><p class='label'>缺陷</p></td></tr></table>",
    );
    let [Block::Table(table)] = &section.blocks[..] else {
        panic!()
    };
    assert!(table.before.is_empty());
    assert_eq!(table.rows[0].cells[0].text.kind, TextBlockKind::Caption);
    assert_eq!(
        table.rows[0].cells[0].authored_alignment,
        Some(TextAlignment::Center)
    );
}

#[test]
fn title_paragraph_alignment_overrides_inherited_cell_alignment_for_identification_only() {
    let section = parse(
        "<style>body {text-align:justify} td {text-align:inherit} .title {text-align:center} .label {text-align:justify}</style><table><tr><td colspan='2'><span>&#160; </span><p class='title'>&#160; 九型人格</p></td></tr><tr><td><span>&#160;</span><p class='label'>类型</p></td><td><p class='label'>理想</p></td></tr></table>",
    );
    let [Block::Table(table)] = &section.blocks[..] else {
        panic!()
    };
    assert_eq!(table.rows[0].cells[0].text.kind, TextBlockKind::Caption);
    // Keep the existing cell style for original-book rendering.
    assert_eq!(
        table.rows[0].cells[0].authored_alignment,
        Some(TextAlignment::Justify)
    );
}

#[test]
fn data_groups_notes_partial_spans_and_numeric_rows_are_not_titles() {
    for first in [
        "<td colspan='2'>Group A</td>",
        "<td colspan='2' style='text-align:center'>Note: Rounded results</td>",
        "<td colspan='2' style='text-align:center'>1234</td>",
        "<td colspan='1' style='text-align:center'>Title</td>",
        "<td colspan='2' rowspan='2' style='text-align:center'>Title</td>",
    ] {
        let section = parse(&format!(
            "<table><tr>{first}</tr><tr><td>Category</td><td>Description</td></tr></table>"
        ));
        let Block::Table(table) = &section.blocks[0] else {
            panic!()
        };
        assert_eq!(
            table.rows[0].cells[0].text.kind,
            TextBlockKind::Paragraph,
            "{first}"
        );
    }
    let section = parse(
        "<table><thead><tr><th colspan='2'>Group A</th></tr><tr><th>Category</th><th>Description</th></tr></thead><tr><td colspan='2'>Group B</td></tr></table>",
    );
    let Block::Table(table) = &section.blocks[0] else {
        panic!()
    };
    assert_eq!(table.rows[0].cells[0].text.kind, TextBlockKind::Paragraph);
}
