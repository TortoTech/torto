use super::*;
use rebook_layout::{LayoutViewport, ReaderStyle, ReaderTypesetting, SpreadMode};
use rebook_reader::ReaderSession;
use std::sync::Arc;

#[test]
#[ignore = "requires TORTO_BARRIERS_BOOK local EPUB"]
fn local_barriers_search_child_sources_and_offscreen_descriptions() {
    let opened = rebook_formats::open_file(std::env::var("TORTO_BARRIERS_BOOK").unwrap()).unwrap();
    let source = opened.source();
    let results = search_book(source.as_ref(), ":", 200).unwrap();
    let mut reader = ReaderSession::open_deferred_with_fonts(
        Arc::clone(&source),
        LayoutViewport::new(1200, 800).unwrap(),
        ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            spread: SpreadMode::Single,
            ..Default::default()
        },
        Arc::clone(&crate::fonts::embedded_reader_fonts()),
        None,
    )
    .unwrap();
    let mut checked = 0;
    for result in results.iter().filter(|r| {
        source.book().sections[r.section_index]
            .href
            .path()
            .ends_with("chapter-9.xhtml")
    }) {
        reader.go_to_source(&result.range.start).unwrap();
        assert!(
            reader
                .current_page()
                .contains_source_anchor(&result.range.start),
            "{}",
            result.excerpt
        );
        let content = reader.current_reading_unit_content().unwrap();
        assert!(
            content
                .iter()
                .flat_map(|(_, s)| &s.blocks)
                .flat_map(Block::text_blocks)
                .any(|t| t
                    .source
                    .as_ref()
                    .is_some_and(|r| r.start.node == result.range.start.node))
        );
        checked += 1;
    }
    assert_eq!(checked, 6);
    let chapter = |name| {
        let index = source
            .book()
            .sections
            .iter()
            .position(|s| s.href.path().ends_with(name))
            .unwrap();
        source.parse_section(index).unwrap()
    };
    let five = chapter("chapter-5.xhtml");
    let prose = five
        .blocks
        .iter()
        .flat_map(Block::text_blocks)
        .map(text_block_text)
        .collect::<String>();
    for description in [
        "A table with 4 columns and 4 rows.",
        "A table shows the results for 4 years and 5 distances",
        "The fastest 50-metre freestyle swimmer",
        "The fastest 100-metre freestyle swimmer",
        "The fastest 200-metre freestyle swimmer",
    ] {
        assert!(
            !prose.contains(description),
            "offscreen description remains: {description}"
        );
        assert!(
            search_book(source.as_ref(), description, 200)
                .unwrap()
                .is_empty()
        );
    }
    let eight = chapter("chapter-8.xhtml");
    assert!(eight.blocks.iter().flat_map(Block::text_blocks).any(|t| {
        text_block_text(t).contains("You do not need to describe a decorative border around text.")
    }));
    println!(
        "Verified {checked} chapter-9 search targets, 5 hidden descriptions and visible absolute-positioned text"
    );
}
