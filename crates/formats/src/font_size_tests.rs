//! Font sizes across the actual EPUB parser and unified shaping pipeline.
use super::*;
use rebook_layout::{
    LayoutEngine, LayoutViewport, PageItem, ReaderFontBlob, ReaderStyle, ReaderTypesetting,
    SpreadMode,
};
use rebook_publication::{Block, TextBlock, TextBlockKind};

fn style() -> ReaderStyle {
    let mut style = ReaderStyle {
        typesetting: ReaderTypesetting::unified(),
        spread: SpreadMode::Single,
        ..Default::default()
    };
    style.typography.font_size = 20.0;
    style.typography.minimum_font_size = 8.0;
    style.typesetting.table_font_scale = 0.8;
    style
}

fn engine() -> LayoutEngine {
    LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
        include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf")
            .as_slice()
            .to_vec(),
    ))])
}

fn sizes(
    source: &dyn BookSource,
    block: &TextBlock,
    kind: TextBlockKind,
    unified: bool,
) -> Vec<f32> {
    let mut block = block.clone();
    block.kind = kind;
    let mut style = style();
    if !unified {
        style.typesetting = ReaderTypesetting::default();
    }
    let result = engine()
        .layout_blocks(
            source,
            &[Block::Text(block)],
            LayoutViewport::new(900, 800).unwrap(),
            &style,
        )
        .unwrap();
    result
        .pages
        .iter()
        .flat_map(|p| &p.items)
        .filter_map(|item| {
            if let PageItem::Text(text) = item {
                Some(text)
            } else {
                None
            }
        })
        .flat_map(|text| {
            text.layout.lines().flat_map(|line| {
                line.runs()
                    .filter(|run| !text.text[run.text_range()].trim().is_empty())
                    .map(|run| run.font_size())
            })
        })
        .collect()
}

fn assert_size(sizes: &[f32], expected: f32) {
    assert!(!sizes.is_empty());
    assert!(
        sizes.iter().all(|size| (size - expected).abs() < 0.01),
        "expected {expected}, got {sizes:?}"
    );
}

#[test]
fn epub_block_keywords_use_semantic_sizes_and_inline_changes_remain_local() {
    let mut entries = epub::tests::minimal_entries();
    entries[5] = ("OPS/Text/chapter.xhtml", br#"<html><head><style>body{font-size:large} p{font-size:small}</style></head><body>
        <p>Ordinary body.</p>
        <p>Before <span style="font-size:smaller">local</span> after.</p>
        <p>Before <sup>raised</sup> after.</p>
        <table><tr><td style="font-size:x-small"><p style="font-size:small">Cell body.</p><p>Cell <span style="font-size:larger">larger</span>.</p></td></tr></table>
    </body></html>"#, zip::CompressionMethod::Deflated);
    let source = epub::EpubPublication::open_bytes(epub::tests::zip_entries(&entries)).unwrap();
    let section = source.parse_section(0).unwrap();
    let body: Vec<_> = section
        .blocks
        .iter()
        .filter_map(|b| {
            if let Block::Text(t) = b {
                Some(t)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(body.len(), 3);
    for (kind, expected) in [
        (TextBlockKind::Paragraph, 20.0),
        (TextBlockKind::Heading(2), 28.64),
        (TextBlockKind::Blockquote, 19.0),
        (TextBlockKind::QuoteAttribution, 17.6),
        (TextBlockKind::Caption, 17.6),
        (TextBlockKind::Preformatted, 18.0),
        (TextBlockKind::FootnoteDefinition, 20.0),
        (TextBlockKind::DefinitionTerm { depth: 0 }, 20.0),
        (
            TextBlockKind::ListItem {
                ordered: false,
                ordinal: 1,
                depth: 0,
                marker_visible: false,
            },
            20.0,
        ),
    ] {
        assert_size(&sizes(&source, body[0], kind, true), expected);
    }
    assert_size(
        &sizes(&source, body[0], TextBlockKind::Paragraph, false),
        20.0 * 8.0 / 9.0,
    );
    let local = sizes(&source, body[1], TextBlockKind::Paragraph, true);
    assert!(local.iter().any(|s| (*s - 20.0 / 1.2).abs() < 0.01));
    assert!(local.iter().any(|s| (*s - 20.0).abs() < 0.01));
    let raised = sizes(&source, body[2], TextBlockKind::Paragraph, true);
    assert!(raised.iter().any(|s| (*s - 14.0).abs() < 0.01));
    assert!(raised.iter().any(|s| (*s - 20.0).abs() < 0.01));
    let table = section
        .blocks
        .iter()
        .find(|b| matches!(b, Block::Table(_)))
        .unwrap();
    let layout = engine()
        .layout_blocks(
            &source,
            &[table.clone()],
            LayoutViewport::new(900, 800).unwrap(),
            &style(),
        )
        .unwrap();
    let cell_sizes: Vec<_> = layout
        .pages
        .iter()
        .flat_map(|p| &p.items)
        .filter_map(|item| {
            if let PageItem::Table(table) = item {
                Some(table)
            } else {
                None
            }
        })
        .flat_map(|table| &table.cells)
        .filter_map(|cell| cell.text.as_ref())
        .flat_map(|text| {
            text.layout.lines().flat_map(|line| {
                line.runs()
                    .filter(|run| !text.text[run.text_range()].trim().is_empty())
                    .map(|run| run.font_size())
            })
        })
        .collect();
    assert!(
        cell_sizes
            .iter()
            .all(|s| (*s - 16.0).abs() < 0.01 || (*s - 19.2).abs() < 0.01),
        "{cell_sizes:?}"
    );
    assert!(cell_sizes.iter().any(|s| (*s - 19.2).abs() < 0.01));
}

#[test]
#[ignore = "requires TORTO_FONT_AUDIT_BOOK; reads local EPUB without changing it"]
fn local_handbook_body_uses_configured_unified_size() {
    let path = std::env::var_os("TORTO_FONT_AUDIT_BOOK").expect("local EPUB path");
    let source = epub::EpubPublication::open_bytes(fs::read(path).unwrap()).unwrap();
    let mut checked = 0;
    for (index, descriptor) in source.book().sections.iter().enumerate() {
        if !["c01.xhtml", "c02.xhtml"]
            .iter()
            .any(|suffix| descriptor.href.path().ends_with(suffix))
        {
            continue;
        }
        let section = source.parse_section(index).unwrap();
        let mut chapter_checked = 0;
        for block in &section.blocks {
            let Block::Text(text) = block else { continue };
            if text.kind != TextBlockKind::Paragraph {
                continue;
            }
            let runs: Vec<_> = text
                .content
                .iter()
                .flat_map(|i| i.text_runs())
                .filter(|r| !r.text.trim().is_empty())
                .collect();
            if runs.is_empty()
                || !runs.iter().all(|r| {
                    r.style
                        .keyword_size_scale
                        .is_some_and(|s| (s - 8.0 / 9.0).abs() < 0.0001)
                        && r.style.inline_size_scale.is_none()
                        && r.style.baseline == rebook_publication::TextBaseline::Normal
                        && r.link.is_none()
                        && r.style.inline_citation == 0
                })
            {
                continue;
            }
            assert_size(&sizes(&source, text, TextBlockKind::Paragraph, true), 20.0);
            assert_size(
                &sizes(&source, text, TextBlockKind::Paragraph, false),
                20.0 * 8.0 / 9.0,
            );
            checked += 1;
            chapter_checked += 1;
            if chapter_checked == 4 {
                break;
            }
        }
        assert_eq!(chapter_checked, 4, "{}", descriptor.href.path());
    }
    assert_eq!(checked, 8);
    println!("HANDBOOK_FONT_AUDIT paragraphs={checked} authored=17.7778 unified=20");
}

fn check_floating_initial(
    source: &dyn BookSource,
    block: &TextBlock,
    initial: &str,
    right: bool,
    expected_wrap_lines: Option<usize>,
) {
    use rebook_layout::text_layout::{Affinity, Cluster, Cursor, PositionedLayoutItem, Selection};

    let cap = block.content[0].text_runs().first().unwrap();
    assert_eq!(cap.text, initial);
    assert_eq!(cap.style.drop_cap.unwrap().right, right);
    let source_range = block.source.as_ref().unwrap();
    let expected_text: String = block
        .content
        .iter()
        .flat_map(|i| i.text_runs())
        .map(|r| r.text.as_str())
        .collect();
    for (unified, width) in [(false, 390), (false, 780), (true, 390)] {
        let mut config = style();
        if !unified {
            config.typesetting = ReaderTypesetting::default();
        }
        let pages = engine()
            .layout_blocks(
                source,
                &[Block::Text(block.clone())],
                LayoutViewport::new(width, 1000).unwrap(),
                &config,
            )
            .unwrap();
        let placement = pages
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .find_map(|item| {
                if let PageItem::Text(text) = item {
                    Some(text)
                } else {
                    None
                }
            })
            .unwrap();

        assert_eq!(placement.source.as_ref(), Some(source_range));
        assert_eq!(placement.text.as_ref(), expected_text);
        let layout = &placement.layout;
        let line = layout.get(0).unwrap();
        assert_eq!(line.text_range().start, 0);
        let run = line.runs().next().unwrap();
        if unified {
            assert_eq!(run.font_size(), 20.0);
            assert_eq!(line.metrics().inline_min_coord, 0.0);
            continue;
        }
        assert_eq!(run.text_range(), 0..initial.len());
        assert!((run.font_size() - 20.0 * cap.style.size_scale).abs() < 0.01);
        let first = line.metrics();
        if let Some(expected) = expected_wrap_lines {
            let full = layout.lines().last().unwrap().metrics();
            let wrapped = layout
                .lines()
                .take_while(|line| {
                    let m = line.metrics();
                    if right {
                        m.inline_max_coord < full.inline_max_coord
                    } else {
                        m.inline_min_coord > 0.0
                    }
                })
                .count();
            assert_eq!(wrapped, expected);
        }
        let second = layout.get(1).unwrap().metrics();
        let last = layout.lines().last().unwrap().metrics();
        if right {
            assert!(
                second.inline_max_coord < last.inline_max_coord,
                "{second:?} vs {last:?}"
            );
            assert_eq!(second.inline_min_coord, 0.0);
        } else {
            assert!(second.inline_min_coord > 0.0, "{second:?}");
            assert_eq!(last.inline_min_coord, 0.0);
        }
        assert!((first.line_height - second.line_height).abs() < 0.01);
        let (cap_x, cap_width) = line
            .items()
            .find_map(|item| {
                if let PositionedLayoutItem::GlyphRun(glyph) = item {
                    (glyph.run().text_range() == (0..initial.len()))
                        .then(|| (glyph.offset(), glyph.advance()))
                } else {
                    None
                }
            })
            .unwrap();
        let selection = Selection::new(
            Cursor::from_byte_index(layout, 0, Affinity::Downstream),
            Cursor::from_byte_index(layout, initial.len(), Affinity::Upstream),
        );
        let [(rect, 0)] = selection.geometry(layout)[..] else {
            panic!("initial selection not unique")
        };
        assert!(rect.y1 > f64::from(first.block_max_coord));
        let (hit, _) =
            Cluster::from_point_exact(layout, cap_x + cap_width * 0.5, rect.y1 as f32 - 0.01)
                .unwrap();
        assert!((0..initial.len()).contains(&hit.text_range().start));
        assert_eq!(selection.text_range(), 0..initial.len());
    }
}

#[test]
fn floating_initials_use_browser_style_geometry_without_changing_text() {
    let mut entries = epub::tests::minimal_entries();
    let body = "The following words wrap around the initial across multiple lines before returning to the whole width of the page. A short fixture could end while the letter is still in view, so this paragraph deliberately contains multiple sentences. ".repeat(4);
    let html = format!(
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><style>
        p {{ line-height: 1.4; }}
        .first {{ float: left; font-size: 340%; margin: -0.1em 0.15em -0.2em 0; line-height: 0.9; }}
        p.pseudo::first-letter {{ float: right; font-size: 340%; line-height: 0.9; }}
        .number {{ float: left; font-size: 2em; }}
        </style></head><body>
        <p><span class="first">J</span>ust {body}</p>
        <p class="pseudo">Élan. {body}</p>
        <p><span class="number">(1)</span>Institute of Educational Technology.</p>
        <p><span style="font-size:1.8em">A</span> non-floating large letter.</p>
        </body></html>"#
    );
    entries[5] = (
        "OPS/Text/chapter.xhtml",
        html.as_bytes(),
        zip::CompressionMethod::Deflated,
    );
    let source = epub::EpubPublication::open_bytes(epub::tests::zip_entries(&entries)).unwrap();
    let section = source.parse_section(0).unwrap();
    let blocks = section
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
    assert_eq!(blocks.len(), 4);
    check_floating_initial(&source, blocks[0], "J", false, Some(2));
    check_floating_initial(&source, blocks[1], "É", true, Some(3));
    for block in &blocks[2..] {
        assert!(
            block
                .content
                .iter()
                .flat_map(|i| i.text_runs())
                .all(|r| r.style.drop_cap.is_none())
        );
    }
    assert!(
        sizes(&source, blocks[3], TextBlockKind::Paragraph, true)
            .iter()
            .any(|&s| (s - 36.0).abs() < 0.01)
    );
}

#[test]
#[ignore = "requires TORTO_DROP_CAP_ZH_EPUB and TORTO_DROP_CAP_EN_EPUB; reads local books"]
fn local_floating_initials_match_authored_and_unified_modes() {
    for (env, suffix, letter) in [
        ("TORTO_DROP_CAP_ZH_EPUB", "/chapter1.xhtml", "首"),
        ("TORTO_DROP_CAP_EN_EPUB", "/Chapter04.xhtml", "J"),
    ] {
        let path = std::env::var_os(env).expect("local EPUB path");
        let source = epub::EpubPublication::open_bytes(fs::read(path).unwrap()).unwrap();
        let index = source
            .book()
            .sections
            .iter()
            .position(|d| d.href.path().ends_with(suffix))
            .unwrap();
        let section = source.parse_section(index).unwrap();
        let block = section
            .blocks
            .iter()
            .find_map(|b| {
                let Block::Text(t) = b else {
                    return None;
                };
                t.content
                    .first()?
                    .text_runs()
                    .first()
                    .is_some_and(|run| run.text == letter)
                    .then_some(t)
            })
            .unwrap();
        check_floating_initial(&source, block, letter, false, None);
        println!("Checked {suffix}, initial {letter}");
    }
}

#[test]
fn authored_floats_keep_label_sources_and_clear_figure_bands() {
    let mut entries = epub::tests::minimal_entries();
    let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><style>
    .number{float:left;padding-right:.5em}.body{margin-left:2em}
    figure{float:right;padding-left:1em} .clear{clear:both}
    </style></head><body><section>
    <div><span class="number">(1)</span><div class="body">Institute of Educational Technology, The Open University, Milton Keynes, Buckinghamshire, UK.</div></div>
    <figure><img src="float.svg" width="140" height="180" alt="An example"/><figcaption>Figure 1. An example.</figcaption></figure>
    <p>Text beside the image should start on the same row as the artwork and wrap naturally.</p>
    <div class="clear"></div>
    <p>Text after the clear returns to the original measure.</p>
    </section></body></html>"#;
    entries[5] = (
        "OPS/Text/chapter.xhtml",
        xml.as_bytes(),
        zip::CompressionMethod::Deflated,
    );
    entries.push(("OPS/Text/float.svg", br#"<svg xmlns="http://www.w3.org/2000/svg" width="140" height="180"><rect width="140" height="180" fill="green"/></svg>"#, zip::CompressionMethod::Deflated));
    let source = epub::EpubPublication::open_bytes(epub::tests::zip_entries(&entries)).unwrap();
    let section = source.parse_section(0).unwrap();
    let Block::Text(label) = &section.blocks[0] else {
        panic!("missing authored label")
    };
    let Block::Text(body) = &section.blocks[1] else {
        panic!("missing label body")
    };
    assert_eq!(label.style.floating_label, Some(3));

    for (unified, width) in [(false, 660), (false, 240), (true, 660)] {
        let mut config = style();
        if !unified {
            config.typesetting = ReaderTypesetting::default();
        }
        let result = engine()
            .layout_blocks(
                &source,
                &section.blocks,
                LayoutViewport::new(width, 1500).unwrap(),
                &config,
            )
            .unwrap();
        let items = result
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .collect::<Vec<_>>();
        let text = |prefix: &str| {
            items
                .iter()
                .find_map(|i| match i {
                    PageItem::Text(t) if t.text.starts_with(prefix) => Some(t),
                    _ => None,
                })
                .unwrap()
        };
        let affiliation = text("(1)");
        assert_eq!(
            affiliation.sources().collect::<Vec<_>>(),
            [
                label.source.as_ref().unwrap(),
                body.source.as_ref().unwrap()
            ]
        );
        assert!(affiliation.text.starts_with("(1)\u{2002}Institute"));
        if let Some(next) = affiliation.layout.get(1) {
            assert!(next.metrics().indent > 0.0);
        }
        let beside = text("Text beside");
        let after = text("Text after");
        assert_eq!(beside.layout.has_flow_exclusion(), !unified && width == 660);
        assert!(!after.layout.has_flow_exclusion());
        let image = items
            .iter()
            .find_map(|item| match item {
                PageItem::Image(image) => Some(image),
                _ => None,
            })
            .unwrap();
        if beside.layout.has_flow_exclusion() {
            assert!(beside.origin_y < image.y + image.height);
            assert!(
                beside.layout.get(0).unwrap().metrics().inline_max_coord < beside.available_width
            );
        }
        assert!(after.origin_y >= image.y + image.height);
    }
}

#[test]
#[ignore = "requires TORTO_FLOAT_SPRINGER_EPUB and TORTO_FLOAT_LAUNDRY_EPUB"]
fn local_authored_floats_keep_affiliation_and_wrap_original_figure() {
    for (env, matching, required) in [
        (
            "TORTO_FLOAT_SPRINGER_EPUB",
            "/516022_1_En_5_Chapter.xhtml",
            "affiliation",
        ),
        ("TORTO_FLOAT_LAUNDRY_EPUB", "/chapter1.xhtml", "figure"),
    ] {
        let source =
            epub::EpubPublication::open_bytes(fs::read(std::env::var_os(env).unwrap()).unwrap())
                .unwrap();
        let i = source
            .book()
            .sections
            .iter()
            .position(|d| d.href.path().ends_with(matching))
            .unwrap();
        let section = source.parse_section(i).unwrap();
        if required == "affiliation" {
            assert!(
                section
                    .blocks
                    .iter()
                    .any(|b| matches!(b, Block::Text(t) if t.style.floating_label == Some(3)))
            );
        } else {
            assert!(section.blocks.iter().any(|b| matches!(b, Block::Figure(f) if f.images.iter().any(|img| img.style.float_right == Some(true)))));
        }
        let result = engine()
            .layout_blocks(
                &source,
                &section.blocks,
                LayoutViewport::new(900, 1100).unwrap(),
                &ReaderStyle::default(),
            )
            .unwrap();
        let texts = result
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .filter_map(|i| match i {
                PageItem::Text(t) => Some(t),
                _ => None,
            });
        if required == "affiliation" {
            assert!(texts.into_iter().any(|t| {
                t.text
                    .starts_with("(1)\u{2002}Institute of Educational Technology")
            }));
        } else {
            assert!(texts.into_iter().any(|t| t.layout.has_flow_exclusion()));
        }
        println!("checked {required}: {matching}");
    }
}
