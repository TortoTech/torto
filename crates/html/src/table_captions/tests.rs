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

fn plain(text: &TextBlock) -> String {
    text.content
        .iter()
        .flat_map(|inline| inline.text_runs())
        .map(|run| run.text.as_str())
        .collect()
}

#[test]
fn figure_notes_are_not_stolen_by_the_next_table_title() {
    for notes in [
        "<p id='note'>数据说明：CEIC，安信证券。</p>",
        "<p id='source'>数据来源：国家统计局，安信证券。</p><p id='note'>数据说明：横轴为城市常住人口。</p>",
    ] {
        let section = parse(&format!(
            "<p><img src='figure.png'/></p><p>图2-3 重工业增加值占比</p>{notes}<p id='title'>表2-2 工业行业增加值变化</p>{GRID}"
        ));
        let [Block::Figure(figure), Block::Table(table)] = &section.blocks[..] else {
            panic!("{:?}", section.blocks);
        };
        assert_eq!(figure.caption_position, CaptionPosition::After);
        assert_eq!(plain(&figure.captions[0]), "图2-3 重工业增加值占比");
        assert!(plain(figure.captions.last().unwrap()).starts_with("数据说明："));
        assert_eq!(
            figure.captions.len(),
            if notes.contains("id='source'") { 3 } else { 2 }
        );
        assert_eq!(table.before.len(), 1);
        assert_eq!(plain(&table.before[0]), "表2-2 工业行业增加值变化");
        for anchor in &section.anchors {
            assert!(figure.captions.iter().chain(&table.before).any(|text| {
                text.source
                    .as_ref()
                    .is_some_and(|range| range.start == anchor.source)
            }));
        }
        assert_eq!(
            figure.source.as_ref().unwrap().end,
            figure.captions.last().unwrap().source.as_ref().unwrap().end
        );
    }
}

#[test]
fn notes_after_a_wrapped_table_stay_with_it_before_the_next_title() {
    let section = parse(&format!(
        "<div><p>Table 1. First results</p>{GRID}</div><p id='source'>Source: First author.</p><p id='note'>Note: Rounded values.</p><p>Table 2. Next results</p>{GRID}"
    ));
    let [Block::Table(first), Block::Table(second)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(
        first.after.iter().map(plain).collect::<Vec<_>>(),
        ["Source: First author.", "Note: Rounded values."]
    );
    assert_eq!(
        second.before.iter().map(plain).collect::<Vec<_>>(),
        ["Table 2. Next results"]
    );
    for (anchor, text) in section.anchors.iter().zip(&first.after) {
        assert_eq!(anchor.source, text.source.as_ref().unwrap().start);
    }
}

#[test]
fn authored_upper_sources_and_explicit_targets_override_previous_media() {
    for group in [
        format!("<p>Table 1. Individual differences</p><p id='source'>Source: Lewis (2002).</p>{GRID}"),
        format!("<div><p id='source'>Source: Lewis (2002).</p><p>Table 1. Individual differences</p>{GRID}</div>"),
        "<p id='source'>Source: <a href='#next'>Lewis (2002).</a></p><p>Table 1. Individual differences</p><table id='next'><tr><td>42</td></tr></table>".to_owned(),
    ] {
        let section = parse(&format!(
            "<p><img src='figure.png'/></p><p>Figure 1. Earlier results</p>{group}"
        ));
        let Some(Block::Table(table)) = section.blocks.last() else {
            panic!("{:?}", section.blocks);
        };
        assert_eq!(table.before.len(), 2);
        let source = table.before.iter().find(|text| plain(text).starts_with("Source:")).unwrap();
        assert_eq!(section.anchors.iter().find(|anchor| anchor.fragment == "source").unwrap().source,
            source.source.as_ref().unwrap().start);
    }
}

#[test]
fn ordinary_prose_and_headings_separate_upper_notes_from_previous_figures() {
    for barrier in ["<p>New discussion.</p>", "<h2>New chapter</h2>"] {
        let section = parse(&format!(
            "<p><img src='figure.png'/></p><p>Figure 1. Earlier results</p>{barrier}<p>Source: WHO, 2008.</p><p>Table 1. Results</p>{GRID}"
        ));
        let Some(Block::Table(table)) = section.blocks.last() else {
            panic!()
        };
        assert_eq!(table.before.len(), 2);
        assert_eq!(plain(&table.before[0]), "Source: WHO, 2008.");
    }
}

#[test]
fn two_image_table_keeps_upper_source_images_and_anchors_in_authored_order() {
    let section = parse(
        "<p id='title' class='tablecaption'>Stages of ISO Standardization Process</p><p id='source' class='tablesource'>Source: ISO.</p><div class='graphic'><img id='first' src='part1.jpg'/><img id='second' src='part2.jpg'/></div>",
    );
    let [Block::Figure(figure)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(figure.images.len(), 2);
    assert_eq!(figure.caption_position, CaptionPosition::Before);
    assert_eq!(
        figure.captions.iter().map(plain).collect::<Vec<_>>(),
        ["Stages of ISO Standardization Process", "Source: ISO."]
    );
    assert_eq!(
        figure.source.as_ref().unwrap().start,
        figure.captions[0].source.as_ref().unwrap().start
    );
    assert_eq!(
        figure.source.as_ref().unwrap().end,
        figure.images[1].source.as_ref().unwrap().end
    );
    for (index, fragment) in ["title", "source", "first", "second"].iter().enumerate() {
        assert_eq!(&section.anchors[index].fragment, fragment);
        let range = if index < 2 {
            &figure.captions[index].source
        } else {
            &figure.images[index - 2].source
        };
        assert_eq!(section.anchors[index].source, range.as_ref().unwrap().start);
    }
}

#[test]
fn image_bundle_carriers_are_bounded_and_do_not_consume_mixed_content() {
    for body in [
        "<div><img src='first.jpg'/><p>Ordinary prose.</p><img src='second.jpg'/></div>".to_owned(),
        format!("<div><img src='first.jpg'/>{GRID}</div>"),
        format!("<div>{}</div>", "<img src='part.jpg'/>".repeat(9)),
    ] {
        let document = roxmltree::Document::parse(&body).unwrap();
        assert!(!carrier(document.root_element()));
    }
    let document = roxmltree::Document::parse(
        "<div><p><img src='first.jpg'/></p><div><img src='second.jpg'/></div></div>",
    )
    .unwrap();
    assert!(carrier(document.root_element()));
}

#[test]
fn consecutive_numbered_tables_keep_their_titles_and_trailing_notes_without_classes() {
    for class in ["", "unexpected-publisher-name", "tablecaption"] {
        let section = parse(&format!(
            "<p class='{class}'>Table 1. First</p>{GRID}<p class='{class}'>Table 2. Objectives</p>{GRID}<p id='source'>Source: Chapanis (1995).</p><p>Following discussion.</p>"
        ));
        let [Block::Table(first), Block::Table(second), Block::Text(_)] = &section.blocks[..]
        else {
            panic!("{:?}", section.blocks);
        };
        assert_eq!(plain(&first.before[0]), "Table 1. First");
        assert!(first.after.is_empty());
        assert_eq!(plain(&second.before[0]), "Table 2. Objectives");
        assert_eq!(plain(&second.after[0]), "Source: Chapanis (1995).");
        assert_eq!(second.after[0].kind, TextBlockKind::Paragraph);
        assert_eq!(
            section.anchors[0].source,
            second.after[0].source.as_ref().unwrap().start
        );
    }
}

#[test]
fn sources_stay_with_previous_grid_when_another_numbered_grid_follows() {
    let section = parse(&format!(
        "<p>Table 1. First</p>{GRID}<p>Source: First author.</p><p>TABLE 2</p><p>Second results</p>{GRID}<p>Notes: Rounded values.</p>"
    ));
    let [Block::Table(first), Block::Table(second)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(plain(&first.after[0]), "Source: First author.");
    assert_eq!(plain(&second.before[0]), "TABLE 2 Second results");
    assert_eq!(plain(&second.after[0]), "Notes: Rounded values.");
}

#[test]
fn unscoped_sources_accept_textual_prefixes_and_stop_at_body_or_media() {
    for prefix in [
        "Source:",
        "Sources :",
        "NOTE:",
        "Notes：",
        "Abbreviations:",
        "来源：",
        "资料来源:",
        "注：",
        "注释：",
    ] {
        let section = parse(&format!(
            "{GRID}<p>{prefix} Values.</p><p>Ordinary discussion.</p>{GRID}"
        ));
        let [Block::Table(table), Block::Text(_), Block::Table(_)] = &section.blocks[..] else {
            panic!("{prefix}: {:?}", section.blocks);
        };
        assert_eq!(table.after.len(), 1);
        assert!(
            table.after[0]
                .content
                .iter()
                .flat_map(|inline| inline.text_runs())
                .all(|run| (run.style.size_scale - 1.0).abs() < 0.0001)
        );
    }
}

#[test]
fn independent_note_container_preserves_paragraphs_alignment_and_source_ranges() {
    let section = parse(&format!(
        "{GRID}<footer id='notes'><p id='source' style='text-align:right'>Source: First author.</p><p id='continuation'>Unmarked continuation.</p><p id='note'>Note: Rounded values.</p></footer><p>Ordinary discussion.</p>"
    ));
    let [Block::Table(table), Block::Text(_)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(table.after.len(), 3);
    assert_eq!(
        table.after[0].style.authored_alignment,
        Some(TextAlignment::End)
    );
    for (caption, expected) in table.after.iter().zip([
        "Source: First author.",
        "Unmarked continuation.",
        "Note: Rounded values.",
    ]) {
        assert_eq!(plain(caption), expected);
        assert!(caption.source.is_some());
        assert_eq!(caption.kind, TextBlockKind::Paragraph);
    }
    assert_ne!(table.after[0].source, table.after[1].source);
    assert_eq!(section.anchors.len(), 4);
}

#[test]
fn matching_table_superscripts_confirm_definitions_but_unknown_markers_stay_independent() {
    let grid = "<table><tr><td>Value<sup>a</sup></td><td>42</td></tr></table>";
    let section = parse(&format!(
        "{grid}<p><sup>a</sup>Measured value.</p><p><sup>b</sup>Ordinary prose.</p>"
    ));
    let [Block::Table(table), Block::Text(_)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(table.after.len(), 1);
    assert_eq!(plain(&table.after[0]), "aMeasured value.");
}

#[test]
fn explicit_target_resolves_middle_title_without_captioning_the_previous_grid() {
    let section = parse(
        "<table id='first'><tr><td>A</td></tr></table><p><a href='#second'>Table 2. Results</a></p><table id='second'><tr><td>B</td></tr></table>",
    );
    let [Block::Table(first), Block::Table(second)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert!(first.after.is_empty());
    assert_eq!(plain(&second.before[0]), "Table 2. Results");
}

#[test]
fn repeated_post_table_titles_and_explicit_previous_targets_keep_source_order() {
    let section = parse(&format!(
        "{GRID}<p>Table 1. First</p>{GRID}<p>Table 2. Second</p><p>Following discussion.</p>"
    ));
    let [Block::Table(first), Block::Table(second), Block::Text(_)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(plain(&first.after[0]), "Table 1. First");
    assert_eq!(plain(&second.after[0]), "Table 2. Second");
    let section = parse(
        "<table id='first'><tr><td>A</td></tr></table><p><a href='#first'>Table 1. Results</a></p><table id='second'><tr><td>B</td></tr></table>",
    );
    let [Block::Table(first), Block::Table(second)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(first.after.len(), 1);
    assert!(second.before.is_empty());
}

#[test]
fn image_table_keeps_leading_title_and_trailing_source_inside_one_group() {
    let section = parse(
        "<p>Table 4. Results</p><div><p><img src='table.png'/></p></div><p>Source: First author.</p><p>Following discussion.</p>",
    );
    let [Block::Figure(figure), Block::Text(_)] = &section.blocks[..] else {
        panic!("{:?}", section.blocks);
    };
    assert_eq!(figure.caption_position, CaptionPosition::Split(1));
    assert_eq!(plain(&figure.captions[0]), "Table 4. Results");
    assert_eq!(plain(&figure.captions[1]), "Source: First author.");
}

#[test]
fn unrelated_existing_semantic_blocks_stop_annotation_collection() {
    for barrier in [
        "<h2>Note: A new section</h2>",
        "<aside role='doc-footnote' id='n'><p>Note: Book footnote.</p></aside>",
        "<blockquote><p>Source: A quotation.</p></blockquote>",
        "<p><img src='figure.png'/></p>",
    ] {
        let section = parse(&format!(
            "{GRID}<p>Source: First author.</p>{barrier}<p>Ordinary discussion.</p>"
        ));
        let Block::Table(table) = &section.blocks[0] else {
            panic!("table");
        };
        assert_eq!(table.after.len(), 1, "{barrier}");
        assert!(section.blocks.len() >= 3, "{barrier}");
    }
}

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
