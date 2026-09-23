//! Optional regression using a local book; no copyrighted fixtures are stored.
use super::*;
use rebook_layout::{LayoutEngine, PageItem, ReaderFontBlob};
use rebook_renderer::DisplayListCompiler;

fn text_blocks(blocks: &[Block], out: &mut Vec<TextBlock>) {
    for block in blocks {
        match block {
            Block::Text(t) => out.push(t.clone()),
            Block::Quote(q) => {
                out.extend(q.body.clone());
                out.extend(q.attribution.clone());
            }
            Block::Figure(f) => out.extend(f.captions.clone()),
            Block::Note(n) => text_blocks(&n.blocks, out),
            _ => {}
        }
    }
}

#[test]
#[ignore = "requires TORTO_BLANK_LINE_BOOK pointing to a local EPUB with paragraph-internal blank lines"]
fn local_book_blank_lines_do_not_paint_highlight_bars() {
    let opened =
        rebook_formats::open_file(std::env::var("TORTO_BLANK_LINE_BOOK").unwrap()).unwrap();
    let source = opened.source();
    const LATIN: &[u8] = include_bytes!("../../../../assets/fonts/Literata-opsz-wght.ttf");
    let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(LATIN))]);
    let mut candidates = 0;
    let mut checked_blank_lines = 0;
    for index in 0..source.book().sections.len() {
        let section = source.parse_section(index).unwrap();
        let mut texts = Vec::new();
        text_blocks(&section.blocks, &mut texts);
        for t in texts {
            let text = crate::plugins::text_block_text(&t);
            let lines: Vec<_> = text.split('\n').collect();
            if text.chars().count() < 180
                || lines.len() < 3
                || !lines[1..lines.len() - 1]
                    .iter()
                    .any(|l| l.trim().is_empty())
            {
                continue;
            }
            let Some(range) = &t.source else {
                continue;
            };
            candidates += 1;
            println!(
                "{} | {} | node {} | {} characters | {:?}",
                source.book().metadata.title,
                section.href.path(),
                range.start.node,
                text.chars().count(),
                t.style.align
            );
            for unified in [false, true] {
                let mut style = ReaderStyle {
                    spread: SpreadMode::Single,
                    ..ReaderStyle::default()
                };
                if unified {
                    style.typesetting = ReaderTypesetting::unified();
                }
                let layout = engine
                    .layout_blocks(
                        source.as_ref(),
                        &[Block::Text(t.clone())],
                        LayoutViewport::new(900, 300).unwrap(),
                        &style,
                    )
                    .unwrap();
                let mut upstream_bars = 0;
                for page in &layout.pages {
                    let display = DisplayListCompiler.compile(page);
                    let rects = display.source_rects(std::slice::from_ref(range));
                    for item in &page.items {
                        let PageItem::Text(placed) = item else {
                            continue;
                        };
                        if placed.source.is_none() {
                            continue;
                        }
                        let selection = parley::editing::Selection::new(
                            parley::editing::Cursor::from_byte_index(
                                &placed.layout,
                                0,
                                parley::layout::Affinity::Downstream,
                            ),
                            parley::editing::Cursor::from_byte_index(
                                &placed.layout,
                                placed.text.len(),
                                parley::layout::Affinity::Upstream,
                            ),
                        );
                        for i in placed.lines.clone() {
                            let line = placed.layout.get(i).unwrap();
                            if !placed.text[line.text_range()]
                                .chars()
                                .all(char::is_whitespace)
                            {
                                continue;
                            }
                            checked_blank_lines += 1;
                            let y = f64::from(
                                placed.origin_y
                                    + (line.metrics().block_min_coord
                                        + line.metrics().block_max_coord)
                                        * 0.5,
                            );
                            for (rect, _) in selection
                                .geometry(&placed.layout)
                                .iter()
                                .filter(|(_, ix)| *ix == i)
                            {
                                upstream_bars += 1;
                                println!(
                                    "  blank line {i}: upstream bar x={:.1}, width={:.1}",
                                    rect.x0,
                                    rect.x1 - rect.x0
                                );
                            }
                            assert!(
                                !rects.iter().any(|r| r.y0 < y && r.y1 > y),
                                "blank-line highlight in {}",
                                section.href.path()
                            );
                        }
                    }
                }
                println!("  unified={unified}: filtered {upstream_bars} upstream blank-line bars");
            }
        }
    }
    assert!(candidates > 0 && checked_blank_lines > 0);
    println!(
        "Checked {candidates} candidate paragraphs, {checked_blank_lines} blank lines across both styles"
    );
}
