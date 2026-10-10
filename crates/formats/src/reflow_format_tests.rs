//! Exercise conversions and their common IR, rather than only EPUB fixtures.
use std::io::{Cursor, Write};

use rebook_publication::{
    Block, BookSource, Inline, LinkRole, Metadata, TextAlignment, TextBlockKind,
};

use crate::BookFormat;
use crate::source::{
    DirectBookSource, SectionContent, SourceBook, SourceResource, SourceSection, SourceTocEntry,
};

#[test]
#[ignore = "requires local Kindle fixtures in test-data"]
fn local_kindle_books_parse_every_section() {
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-data");
    let mut books = 0;
    for path in std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
    {
        if !path
            .extension()
            .is_some_and(|extension| matches!(extension.to_str(), Some("mobi" | "azw" | "azw3")))
        {
            continue;
        }
        let opened =
            crate::open_file(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let source = opened.source();
        assert!(!source.book().sections.is_empty());
        let mut blocks = 0;
        for index in 0..source.book().sections.len() {
            blocks += source
                .parse_section(index)
                .unwrap_or_else(|error| panic!("{} section {index}: {error}", path.display()))
                .blocks
                .len();
        }
        assert!(blocks > 0);
        println!(
            "{}: {} sections, {blocks} blocks",
            path.display(),
            source.book().sections.len()
        );
        books += 1;
    }
    assert!(books > 0);
}

pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([35, 120, 70, 255]));
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    output.into_inner()
}

fn direct(
    sections: Vec<SourceSection>,
    toc: Vec<SourceTocEntry>,
    resources: Vec<SourceResource>,
    format: BookFormat,
) -> DirectBookSource {
    DirectBookSource::open(
        SourceBook {
            id: "cross-format-reflow".into(),
            metadata: Metadata::default(),
            sections,
            table_of_contents: toc,
            resources,
            cover_path: None,
        },
        format,
    )
    .unwrap()
}

fn section(content: &str) -> SourceSection {
    SourceSection {
        title: "Chapter".into(),
        content: SectionContent::Html(content.into()),
        linear: true,
        properties: Vec::new(),
    }
}

#[test]
fn kindle_empty_tail_fragments_keep_markup_without_failing_recovery() {
    let normalized = crate::mobi::normalize_chapter(
        "<a id=\"terminal\"></a></body></html>",
        &Default::default(),
        BookFormat::Mobi,
    )
    .unwrap();
    let source = direct(
        vec![section(&normalized)],
        Vec::new(),
        Vec::new(),
        BookFormat::Mobi,
    );
    let parsed = source.parse_section(0).unwrap();
    assert!(normalized.contains("id=\"terminal\""));
    assert!(parsed.blocks.is_empty());
}

#[test]
#[ignore = "requires TORTO_PERF_BOOK pointing to The Science of Beauty"]
fn local_science_captions_preserve_author_alignment() {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    let opened = crate::open_file(std::path::PathBuf::from(
        std::env::var_os("TORTO_PERF_BOOK").unwrap(),
    ))
    .unwrap();
    let source = opened.source();
    let style = ReaderStyle {
        spread: SpreadMode::Single,
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let mut engine = LayoutEngine::new();
    let mut centered = 0;
    let mut left = 0;
    let mut compound = 0;
    fn collect(blocks: &[Block], groups: &mut Vec<Vec<rebook_publication::TextBlock>>) {
        let mut pending = Vec::new();
        for block in blocks {
            if let Block::Text(text) = block
                && text.kind == TextBlockKind::Caption
            {
                pending.push(text.clone());
                continue;
            }
            if !pending.is_empty() {
                groups.push(std::mem::take(&mut pending));
            }
            match block {
                Block::Figure(figure) if !figure.captions.is_empty() => {
                    groups.push(figure.captions.clone())
                }
                Block::Note(note) => collect(&note.blocks, groups),
                Block::Table(table) => {
                    let captions = table
                        .text_blocks()
                        .filter(|text| text.kind == TextBlockKind::Caption)
                        .cloned()
                        .collect::<Vec<_>>();
                    if !captions.is_empty() {
                        groups.push(captions);
                    }
                }
                _ => {}
            }
        }
        if !pending.is_empty() {
            groups.push(pending);
        }
    }
    for index in 0..source.book().sections.len() {
        let parsed = source.parse_section(index).unwrap();
        let mut groups = Vec::new();
        collect(&parsed.blocks, &mut groups);
        for captions in groups {
            compound += usize::from(captions.len() > 1);
            let blocks = captions
                .iter()
                .cloned()
                .map(Block::Text)
                .collect::<Vec<_>>();
            for width in [400, 1000] {
                let layout = engine
                    .layout_blocks(
                        source.as_ref(),
                        &blocks,
                        LayoutViewport::new(width, 1800).unwrap(),
                        &style,
                    )
                    .unwrap();
                for caption in &captions {
                    if captions.len() == 1
                        && !caption.content.iter().any(|inline| {
                            matches!(inline, Inline::Break)
                                || inline
                                    .text_runs()
                                    .iter()
                                    .any(|run| run.text.contains(['\n', '\r']))
                        })
                    {
                        // Ordinary captions now use automatic alignment even
                        // when the source CSS declares an alignment.
                        continue;
                    }
                    let Some(alignment) = caption.style.authored_alignment else {
                        continue;
                    };
                    let Some(range) = caption.source.as_ref() else {
                        continue;
                    };
                    let text = layout
                        .pages
                        .iter()
                        .flat_map(|page| &page.items)
                        .find_map(|item| match item {
                            PageItem::Text(text) if text.source.as_ref() == Some(range) => {
                                Some(text)
                            }
                            _ => None,
                        })
                        .expect("authored caption must remain source-backed in layout");
                    let first = text.layout.get(text.lines.start).unwrap();
                    match alignment {
                        TextAlignment::Start => {
                            left += 1;
                            assert!(first.metrics().offset.abs() < 0.01);
                        }
                        TextAlignment::Center => {
                            centered += 1;
                            let metrics = first.metrics();
                            let expected = (text.available_width - metrics.advance
                                + metrics.trailing_whitespace)
                                .max(0.0)
                                * 0.5;
                            assert!(
                                (metrics.offset - expected).abs() < 0.1,
                                "{} caption must stay centered at {width}",
                                parsed.href
                            );
                        }
                        TextAlignment::End | TextAlignment::Justify => {}
                    }
                }
            }
        }
    }
    assert!(
        left > 0 && compound > 0,
        "centered {centered}, left {left}, compound {compound}"
    );
    println!(
        "Science of Beauty: {centered} centered and {left} left-aligned placements, {compound} compound caption groups"
    );
}

#[test]
#[ignore = "requires TORTO_PERF_BOOK pointing to The Science of Beauty"]
fn local_science_complete_caption_structures() {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    let opened = crate::open_file(std::path::PathBuf::from(
        std::env::var_os("TORTO_PERF_BOOK").unwrap(),
    ))
    .unwrap();
    let source = opened.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|section| section.href.path().contains("062-063_Skin_Types"))
        .unwrap();
    let section = source.parse_section(index).unwrap();
    let captions = section
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Figure(figure)
                if figure.captions.first().is_some_and(|text| {
                    matches!(
                        crate::reflow::text(text).as_str(),
                        "Normal skin" | "Dry skin" | "Oily skin" | "Combination skin"
                    )
                }) =>
            {
                Some(&figure.captions)
            }
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        captions.len(),
        8,
        "all four images must retain their two-part captions"
    );
    let style = ReaderStyle {
        spread: SpreadMode::Single,
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let mut engine = LayoutEngine::new();
    for width in [400, 1000] {
        let layout = engine
            .layout_section(
                source.as_ref(),
                &section,
                LayoutViewport::new(width, 1800).unwrap(),
                &style,
            )
            .unwrap();
        for caption in &captions {
            assert_eq!(caption.kind, TextBlockKind::Caption);
            assert_eq!(caption.style.authored_alignment, Some(TextAlignment::Start));
            let range = caption.source.as_ref().unwrap();
            let items = layout
                .pages
                .iter()
                .flat_map(|page| &page.items)
                .filter_map(|item| match item {
                    PageItem::Text(text) if text.source.as_ref() == Some(range) => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(
                !items.is_empty(),
                "caption must survive complete-section pagination"
            );
            for item in items {
                for index in item.lines.clone() {
                    let line = item.layout.get(index).unwrap();
                    assert!(
                        line.metrics().offset.abs() < 0.01,
                        "{} must preserve author left alignment",
                        crate::reflow::text(caption)
                    );
                }
            }
        }
    }
    println!(
        "Skin types: all 8 caption paragraphs preserve author left alignment in complete chapters at widths 400 and 1000"
    );
}

#[test]
#[ignore = "requires TORTO_CAPTION_BOOK pointing to Thinking in Systems"]
fn local_thinking_systems_ordinary_captions_use_automatic_alignment() {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    let opened = crate::open_file(std::env::var_os("TORTO_CAPTION_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let style = ReaderStyle {
        spread: SpreadMode::Single,
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    let mut engine = LayoutEngine::new();
    let mut verified = 0;
    for index in 0..source.book().sections.len() {
        let section = source.parse_section(index).unwrap();
        let captions = section
            .blocks
            .iter()
            .flat_map(|block| match block {
                Block::Figure(figure) => figure.captions.iter().collect::<Vec<_>>(),
                Block::Text(text) if text.kind == TextBlockKind::Caption => vec![text],
                _ => vec![],
            })
            .filter(|caption| {
                let text = crate::reflow::text(caption);
                ["Figure 15.", "Figure 16.", "Figure 17."]
                    .iter()
                    .any(|label| text.starts_with(label))
            })
            .collect::<Vec<_>>();
        if captions.is_empty() {
            continue;
        }
        assert_eq!(captions.len(), 3);
        for caption in &captions {
            assert_eq!(
                caption.style.authored_alignment,
                Some(TextAlignment::Justify)
            );
            assert!(
                !caption
                    .content
                    .iter()
                    .any(|inline| matches!(inline, Inline::Break))
            );
        }
        for width in [400, 1200] {
            let layout = engine
                .layout_section(
                    source.as_ref(),
                    &section,
                    LayoutViewport::new(width, 1800).unwrap(),
                    &style,
                )
                .unwrap();
            for caption in &captions {
                let range = caption.source.as_ref().unwrap();
                let items = layout
                    .pages
                    .iter()
                    .flat_map(|page| &page.items)
                    .filter_map(|item| match item {
                        PageItem::Text(text) if text.source.as_ref() == Some(range) => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert!(!items.is_empty());
                let line_count = items.iter().map(|item| item.lines.len()).sum::<usize>();
                assert_eq!(line_count == 1, width == 1200);
                for item in items {
                    for index in item.lines.clone() {
                        let line = item.layout.get(index).unwrap();
                        let metrics = line.metrics();
                        if width == 1200 {
                            let expected = (item.available_width - metrics.advance
                                + metrics.trailing_whitespace)
                                .max(0.0)
                                * 0.5;
                            assert!(expected > 0.0 && (metrics.offset - expected).abs() < 0.1);
                        } else {
                            assert!(metrics.offset.abs() < 0.01);
                        }
                    }
                }
                println!(
                    "{} width={width}: {line_count} lines, {}",
                    crate::reflow::text(caption),
                    if width == 1200 {
                        "centered"
                    } else {
                        "start aligned"
                    }
                );
                verified += 1;
            }
        }
    }
    assert_eq!(verified, 6);
}

#[test]
fn caption_alignment_uses_css_values_and_inheritance_with_arbitrary_class_names() {
    use rebook_layout::{
        LayoutEngine, LayoutViewport, PageItem, ReaderStyle, ReaderTypesetting, SpreadMode,
    };
    for class_name in ["q17", "different-token"] {
        for format in [
            BookFormat::Mobi,
            BookFormat::Azw,
            BookFormat::Azw3,
            BookFormat::Fb2,
            BookFormat::Fbz,
        ] {
            let markup = format!(
                r#"<html><head><link rel="stylesheet" href="../Styles/layout.css"/></head><body>
                <figure><img src="../Images/photo.png" width="40"/>
                <figcaption style="text-align:right">
                  <p class="{class_name}">First line<br/>A much longer line describing this picture and its appearance in considerable detail.</p>
                  <p style="text-align:left">Left label.</p>
                  <p>Inherited right.</p>
                </figcaption></figure></body></html>"#
            );
            let source = direct(
                vec![section(&markup)],
                Vec::new(),
                vec![
                    SourceResource {
                        path: "Styles/layout.css".into(),
                        media_type: "text/css".into(),
                        bytes: format!(".{class_name} {{text-align:center !important}}")
                            .into_bytes(),
                    },
                    SourceResource {
                        path: "Images/photo.png".into(),
                        media_type: "image/png".into(),
                        bytes: png(40, 50),
                    },
                ],
                format,
            );
            let parsed = source.parse_section(0).unwrap();
            let figure = parsed
                .blocks
                .iter()
                .find_map(|block| match block {
                    Block::Figure(figure) => Some(figure),
                    _ => None,
                })
                .unwrap();
            assert!(
                figure
                    .captions
                    .iter()
                    .any(|caption| caption.style.authored_alignment == Some(TextAlignment::Center))
            );
            assert!(
                figure
                    .captions
                    .iter()
                    .any(|caption| caption.style.authored_alignment == Some(TextAlignment::Start))
            );
            assert!(
                figure
                    .captions
                    .iter()
                    .any(|caption| caption.style.authored_alignment == Some(TextAlignment::End))
            );
            let style = ReaderStyle {
                spread: SpreadMode::Single,
                typesetting: ReaderTypesetting::unified(),
                ..Default::default()
            };
            for width in [320, 1200] {
                let layout = LayoutEngine::new()
                    .layout_section(
                        &source,
                        &parsed,
                        LayoutViewport::new(width, 1800).unwrap(),
                        &style,
                    )
                    .unwrap();
                for caption in &figure.captions {
                    let source_range = caption.source.as_ref().unwrap();
                    let text = layout
                        .pages
                        .iter()
                        .flat_map(|page| &page.items)
                        .find_map(|item| match item {
                            PageItem::Text(text) if text.source.as_ref() == Some(source_range) => {
                                Some(text)
                            }
                            _ => None,
                        })
                        .expect("caption must keep its source range after layout");
                    let offset = text.layout.get(text.lines.start).unwrap().metrics().offset;
                    match caption.style.authored_alignment.unwrap() {
                        TextAlignment::Start => {
                            assert!(offset.abs() < 0.01, "{format} at {width}: left caption")
                        }
                        TextAlignment::Center | TextAlignment::End => {
                            assert!(offset > 0.01, "{format} at {width}: aligned caption")
                        }
                        TextAlignment::Justify => unreachable!(),
                    }
                }
            }
        }
    }
}

#[test]
fn kindle_conversion_retains_css_tables_figures_inline_order_and_links() {
    let html = r##"<html><head><link rel="stylesheet" href="../Styles/book.css"/><style>.accent {color:#123456}</style></head><body>
        <p>Before <em>middle</em> after <a href="https://example.com">site</a>.</p>
        <figure><img src="../Images/photo.png" width="40"/><figcaption>Line one<br/>Line two</figcaption></figure>
        <table><tr><td colspan="2" rowspan="2" class="cell"><img src="../Images/photo.png" style="display:block;width:100%"/></td><td>Text</td></tr><tr><td>More</td></tr></table>
        <p class="accent">Colored text.</p>
        </body></html>"##;
    for format in [BookFormat::Mobi, BookFormat::Azw, BookFormat::Azw3] {
        let normalized = crate::mobi::normalize_chapter(html, &Default::default(), format).unwrap();
        let source = direct(
            vec![section(&normalized)],
            Vec::new(),
            vec![
                SourceResource {
                    path: "Styles/book.css".into(),
                    media_type: "text/css".into(),
                    bytes: b".cell {text-align:right}".to_vec(),
                },
                SourceResource {
                    path: "Images/photo.png".into(),
                    media_type: "image/png".into(),
                    bytes: png(40, 50),
                },
            ],
            format,
        );
        let parsed = source.parse_section(0).unwrap();
        let Block::Text(first) = &parsed.blocks[0] else {
            panic!("{format}: missing paragraph");
        };
        assert_eq!(
            crate::reflow::text(first),
            "Before middle after site.",
            "{format}"
        );
        assert!(
            first
                .content
                .iter()
                .flat_map(Inline::text_runs)
                .any(|run| run.style.emphasis)
        );
        assert!(first.content.iter().flat_map(Inline::text_runs).any(|run| {
            run.link
                .as_ref()
                .is_some_and(|link| link.website_url().is_some())
        }));
        let figure = parsed
            .blocks
            .iter()
            .find_map(|block| {
                if let Block::Figure(figure) = block {
                    Some(figure)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(
            figure
                .captions
                .iter()
                .map(crate::reflow::text)
                .collect::<Vec<_>>(),
            ["Line one", "Line two"]
        );
        assert_eq!(
            figure.images[0].style.width,
            Some(rebook_publication::ImageLength::Pixels(40.0))
        );
        let table = parsed
            .blocks
            .iter()
            .find_map(|block| {
                if let Block::Table(table) = block {
                    Some(table)
                } else {
                    None
                }
            })
            .unwrap();
        let cell = &table.rows[0].cells[0];
        assert_eq!((cell.column_span, cell.row_span), (2, 2));
        assert_eq!(cell.authored_alignment, Some(TextAlignment::End));
        assert!(
            cell.text
                .content
                .iter()
                .any(|inline| matches!(inline, Inline::Image(_)))
        );
        let style = rebook_layout::ReaderStyle {
            spread: rebook_layout::SpreadMode::Single,
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            ..Default::default()
        };
        let layout = rebook_layout::LayoutEngine::new()
            .layout_section(
                &source,
                &parsed,
                rebook_layout::LayoutViewport::new(1000, 1600).unwrap(),
                &style,
            )
            .unwrap();
        assert_eq!(
            layout
                .pages
                .iter()
                .map(|page| rebook_renderer::DisplayListCompiler
                    .compile(page)
                    .image_data()
                    .count())
                .sum::<usize>(),
            2,
            "{format}: figure and table images must both render"
        );
        assert!(
            parsed
                .blocks
                .iter()
                .filter_map(|block| if let Block::Text(text) = block {
                    Some(text)
                } else {
                    None
                })
                .flat_map(|text| text.content.iter().flat_map(Inline::text_runs))
                .any(|run| (
                    run.style.color.red,
                    run.style.color.green,
                    run.style.color.blue
                ) == (0x12, 0x34, 0x56))
        );
    }
}

#[test]
fn fb2_and_fbz_keep_merged_picture_cells_and_cross_section_notes() {
    let xml = r##"<FictionBook xmlns:l="http://www.w3.org/1999/xlink"><description><title-info><book-title>FB2 semantics</book-title></title-info></description>
        <body><section><title><p>Chapter</p></title><p id="ref">Before <a type="note" l:href="#n1">1</a> after.</p>
        <table><tr><td colspan="2" rowspan="2" align="right"><image l:href="#photo"/></td><td>A</td></tr><tr><td>B</td></tr></table></section></body>
        <body name="notes"><section id="n1"><title><p>1</p></title><p>The note continues here.</p><p>Another note paragraph.</p><p><a l:href="#ref">Back</a></p></section></body>
        <binary id="photo" content-type="image/png">iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=</binary></FictionBook>"##;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("fixture.fb2", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    let zipped = zip.finish().unwrap().into_inner();
    for (bytes, name) in [
        (xml.as_bytes(), "fixture.fb2"),
        (zipped.as_slice(), "fixture.fbz"),
    ] {
        let opened = crate::open_bytes(bytes.to_vec(), name).unwrap();
        let source = opened.source();
        assert!(source.book().sections[1].is_note_section());
        let chapter = source.parse_section(0).unwrap();
        let reference = chapter
            .blocks
            .iter()
            .filter_map(|block| {
                if let Block::Text(text) = block {
                    Some(text)
                } else {
                    None
                }
            })
            .flat_map(|text| text.content.iter().flat_map(Inline::text_runs))
            .find(|run| run.style.link_role == LinkRole::FootnoteReference)
            .unwrap();
        assert_eq!(
            reference.link.as_ref().unwrap().to_string(),
            "Text/section-2.xhtml#n1"
        );
        let table = chapter
            .blocks
            .iter()
            .find_map(|block| {
                if let Block::Table(table) = block {
                    Some(table)
                } else {
                    None
                }
            })
            .unwrap();
        let cell = &table.rows[0].cells[0];
        assert_eq!((cell.column_span, cell.row_span), (2, 2));
        assert_eq!(cell.authored_alignment, Some(TextAlignment::End));
        assert!(
            cell.text
                .content
                .iter()
                .any(|inline| matches!(inline, Inline::Image(_)))
        );
        let notes = source.parse_section(1).unwrap();
        assert!(
            notes
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Note(_)))
        );
        assert!(notes.anchors.iter().any(|anchor| anchor.fragment == "n1"));
    }
}

#[test]
fn direct_context_removes_breadcrumbs_and_protects_toc_headings_from_captions() {
    let parent = "<html><head><guide><reference type='toc' href='section-3.xhtml'/></guide></head><body><h1 id='parent'>Parent</h1></body></html>";
    let child = "<div><a href='section-3.xhtml'>Contents</a> / <a href='section-1.xhtml#parent'>Parent</a></div><img src='../Images/photo.png'/><p id='child' style='text-align:center'>Child</p><p>Ordinary prose.</p>";
    let source = direct(
        vec![
            section(parent),
            section(child),
            section("<nav role='doc-toc'><a href='section-2.xhtml#child'>Child</a></nav>"),
        ],
        vec![
            SourceTocEntry {
                label: "Parent".into(),
                href: "Text/section-1.xhtml#parent".into(),
                children: vec![SourceTocEntry {
                    label: "Child".into(),
                    href: "Text/section-2.xhtml#child".into(),
                    children: Vec::new(),
                }],
            },
            SourceTocEntry {
                label: "Contents".into(),
                href: "Text/section-3.xhtml".into(),
                children: Vec::new(),
            },
        ],
        vec![SourceResource {
            path: "Images/photo.png".into(),
            media_type: "image/png".into(),
            bytes: png(40, 50),
        }],
        BookFormat::Azw3,
    );
    let parsed = source.parse_section(1).unwrap();
    assert!(parsed.blocks.iter().any(|block| matches!(block, Block::Text(text) if matches!(text.kind, TextBlockKind::Heading(_)) && crate::reflow::text(text) == "Child")), "{:#?}", parsed.blocks);
    assert!(!parsed.blocks.iter().any(
        |block| matches!(block, Block::Text(text) if crate::reflow::text(text).contains("Contents"))
    ));
    assert!(
        parsed
            .anchors
            .iter()
            .any(|anchor| anchor.fragment == "child")
    );
}

#[test]
fn direct_context_classifies_intrinsic_separators_and_unlisted_quote_continuations() {
    let source = direct(
        vec![
            section(
                "<h1>Chapter</h1><p>A repeated quotation.</p><img src='../Images/rule.png'/><p>Independent prose.</p>",
            ),
            section("<blockquote role='doc-pullquote'>A repeated quotation.</blockquote>"),
            section("<blockquote role='doc-epigraph'>A repeated quotation.</blockquote>"),
        ],
        vec![SourceTocEntry {
            label: "Chapter".into(),
            href: "Text/section-1.xhtml".into(),
            children: Vec::new(),
        }],
        vec![SourceResource {
            path: "Images/rule.png".into(),
            media_type: "image/png".into(),
            bytes: png(96, 3),
        }],
        BookFormat::Azw3,
    );
    assert!(
        source.book().sections[1]
            .properties
            .iter()
            .any(|property| property == rebook_publication::CONTINUATION_SECTION_PROPERTY)
    );
    assert!(
        !source.book().sections[2]
            .properties
            .iter()
            .any(|property| property == rebook_publication::CONTINUATION_SECTION_PROPERTY)
    );
    assert!(
        source
            .parse_section(0)
            .unwrap()
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Separator(_)))
    );
}

#[test]
fn direct_cover_requires_authored_semantics_and_renders_the_composition() {
    for (body, expected) in [
        (
            "<section role='doc-cover'><h1>Book title</h1><img src='../Images/photo.png'/><p>Author</p></section>",
            true,
        ),
        (
            "<h1>Opening chapter</h1><img src='../Images/photo.png'/>",
            false,
        ),
    ] {
        let source = direct(
            vec![section(body)],
            Vec::new(),
            vec![SourceResource {
                path: "Images/photo.png".into(),
                media_type: "image/png".into(),
                bytes: png(40, 50),
            }],
            BookFormat::Azw3,
        );
        assert_eq!(source.cover_section().unwrap().is_some(), expected);
        assert_eq!(crate::cover::page_thumbnail(&source).is_some(), expected);
    }
}

#[test]
fn fragment_resolution_keeps_local_and_ambiguous_targets_and_preserves_backlinks() {
    let source = direct(
        vec![
            section(
                "<p id='same'>Local</p><p><a href='#same'>Local link</a> <a href='#note'>Note</a></p><p id='ref'>Reference</p>",
            ),
            section("<p id='same'>Duplicate</p><p id='note'>Note <a href='#ref'>Back</a></p>"),
        ],
        Vec::new(),
        Vec::new(),
        BookFormat::Fb2,
    );
    let first = source.parse_section(0).unwrap();
    let links: Vec<_> = first
        .blocks
        .iter()
        .filter_map(|block| {
            if let Block::Text(text) = block {
                Some(text)
            } else {
                None
            }
        })
        .flat_map(|text| text.content.iter().flat_map(Inline::text_runs))
        .filter_map(|run| run.link.as_ref().map(ToString::to_string))
        .collect();
    assert!(links.contains(&"Text/section-1.xhtml#same".into()));
    assert!(links.contains(&"Text/section-2.xhtml#note".into()));
    let second = source.parse_section(1).unwrap();
    assert!(
        second
            .blocks
            .iter()
            .filter_map(|block| if let Block::Text(text) = block {
                Some(text)
            } else {
                None
            })
            .flat_map(|text| text.content.iter().flat_map(Inline::text_runs))
            .any(|run| run
                .link
                .as_ref()
                .is_some_and(|link| link.to_string() == "Text/section-1.xhtml#ref"))
    );
}

#[test]
fn kindle_uppercase_markup_keeps_styles_dimensions_and_foreign_names() {
    let html = "<HTML><HEAD><STYLE>.cell {text-align:right}</STYLE></HEAD><BODY><TABLE><TR><TD CLASS='cell' COLSPAN='2'><IMG SRC='../Images/photo.png' WIDTH='40'/></TD><TD>Text</TD></TR></TABLE><svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 10 20'><linearGradient id='paint'/></svg></BODY></HTML>";
    let normalized =
        crate::mobi::normalize_chapter(html, &Default::default(), BookFormat::Mobi).unwrap();
    assert!(normalized.contains("<linearGradient"));
    assert!(normalized.contains("viewBox="));
    let source = direct(
        vec![section(&normalized)],
        Vec::new(),
        vec![SourceResource {
            path: "Images/photo.png".into(),
            media_type: "image/png".into(),
            bytes: png(40, 50),
        }],
        BookFormat::Mobi,
    );
    let parsed = source.parse_section(0).unwrap();
    let table = parsed
        .blocks
        .iter()
        .find_map(|block| {
            if let Block::Table(table) = block {
                Some(table)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(table.rows[0].cells[0].column_span, 2);
    assert_eq!(
        table.rows[0].cells[0].authored_alignment,
        Some(TextAlignment::End)
    );
}

#[test]
fn fb2_composed_cover_retains_every_picture_and_keeps_body_linear() {
    let xml = r##"<FictionBook xmlns:l="http://www.w3.org/1999/xlink"><description><title-info><book-title>Composite</book-title><coverpage><image l:href="#cover"/><image l:href="#cover"/></coverpage></title-info></description><body><section><p>Actual chapter.</p></section></body><binary id="cover" content-type="image/png">iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=</binary></FictionBook>"##;
    let opened = crate::open_bytes(xml.as_bytes().to_vec(), "cover.fb2").unwrap();
    assert!(opened.cover_bytes().is_some());
    let source = opened.source();
    assert!(!source.book().sections[0].linear);
    assert!(source.book().sections[1].linear);
    assert!(source.book().cover.is_none());
    let cover = source.cover_section().unwrap().unwrap();
    let images = cover
        .blocks
        .iter()
        .map(|block| match block {
            Block::Image(_) => 1,
            Block::Figure(figure) => figure.images.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert_eq!(images, 2);
}
