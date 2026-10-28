//! Actual scrolling composition, including nested figures and quote accents.
use super::*;
use anyrender::ImageRenderer;
use rebook_layout::{LayoutEngine, PageItem, ReaderTypesetting, SectionLayout};
use rebook_renderer::DisplayListCompiler;

fn layout(
    source: &dyn BookSource,
    body: &str,
    depth: usize,
    width: u32,
    height: u32,
    dark: bool,
) -> (SectionLayout, Section, ReaderStyle) {
    let mut body = body.to_owned();
    for level in 0..depth {
        body = format!(
            "<figure><div>{body}</div><figcaption>Figure {} Example</figcaption></figure>",
            level + 1
        );
    }
    let section = rebook_html::parse_section(
        &format!("<html><body>{body}</body></html>"),
        &source.book().sections[0],
        |_| None,
    )
    .unwrap();
    let mut style = ReaderStyle {
        spread: SpreadMode::Single,
        typesetting: ReaderTypesetting::unified(),
        ..Default::default()
    };
    if dark {
        style.background = Rgba::BLACK;
        style.foreground = Rgba {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        };
    }
    let layout = LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned())
        .layout_blocks(
            source,
            &section.blocks,
            LayoutViewport::new(width, height).unwrap(),
            &style,
        )
        .unwrap();
    (layout, section, style)
}

fn scroll(layout: &SectionLayout) -> ScrollSectionLayout {
    let pages = layout
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| ReaderSectionPage {
            position: ReaderPosition {
                section_index: 0,
                segment_index: 0,
                page_index: index,
            },
            page: Arc::new(DisplayListCompiler.compile(page)),
            placeholder: false,
            visible_top: None,
            visible_bottom: None,
        })
        .collect();
    ScrollSectionLayout::new(0, 0, pages, false)
}

#[test]
fn continued_figure_text_lists_and_code_share_the_normal_line_spacing() {
    let (reader, _, _) = semantic_layout::tests::fixture();
    let words =
        "Content continues across the page boundary without changing its source. ".repeat(65);
    let code = (0..70)
        .map(|i| format!("line {i}: some preformatted content\n"))
        .collect::<String>();
    let cjk = "跨页时应保留正文、来源定位和原有行距。".repeat(210);
    for body in [
        format!("<p>{words}</p>"),
        format!("<ol><li><p>{words}</p></li></ol>"),
        format!("<pre>{code}</pre>"),
        format!("<blockquote><p>{words}</p></blockquote>"),
        format!("<p>{cjk}</p>"),
    ] {
        for (depth, width, height, dark) in [
            (0, 900, 500, false),
            (1, 900, 500, false),
            (1, 700, 350, true),
            (2, 900, 500, false),
        ] {
            let (layout, section, _) =
                layout(reader.source.as_ref(), &body, depth, width, height, dark);
            let scroll = scroll(&layout);
            let mut previous: Option<(SourceRange, usize, usize, f32)> = None;
            let mut joins = 0;
            for (page_index, page) in layout.pages.iter().enumerate() {
                for item in &page.items {
                    let PageItem::Text(text) = item else { continue };
                    let Some(source) = &text.source else { continue };
                    if text.text.len() < 100 {
                        continue;
                    }
                    assert!(
                        section
                            .blocks
                            .iter()
                            .flat_map(Block::text_blocks)
                            .any(|t| t.source.as_ref() == Some(source))
                    );
                    let top = text.origin_y
                        + text
                            .layout
                            .get(text.lines.start)
                            .unwrap()
                            .metrics()
                            .block_min_coord;
                    let last = text.layout.get(text.lines.end - 1).unwrap().metrics();
                    let bottom = text.origin_y
                        + last
                            .block_max_coord
                            .max(last.block_min_coord + last.line_height);
                    if let Some((prior_source, prior_end, prior_page, prior_bottom)) = &previous
                        && prior_source == source
                        && *prior_page != page_index
                    {
                        assert_eq!(*prior_end, text.lines.start, "no lost or duplicated lines");
                        let gap = scroll.content_y(page_index, top) - *prior_bottom;
                        assert!(
                            gap.abs() < 0.1,
                            "depth={depth}, {width}x{height}, gap={gap}; {}",
                            &body[..body.len().min(30)]
                        );
                        joins += 1;
                    }
                    previous = Some((
                        source.clone(),
                        text.lines.end,
                        page_index,
                        scroll.content_y(page_index, bottom),
                    ));
                }
            }
            assert!(joins > 0, "exercise a continuation");
        }
    }
}

#[test]
fn continued_figure_quotes_keep_their_accent_connected() {
    let (reader, _, _) = semantic_layout::tests::fixture();
    let words = "Quoted content continues with a continuous accent. ".repeat(100);
    let paragraphs = (0..45)
        .map(|i| format!("<p>Paragraph {i} in the same quotation.</p>"))
        .collect::<String>();
    for (depth, content) in [
        (1, format!("<p>{words}</p>")),
        (2, format!("<p>{words}</p>")),
        (1, paragraphs),
    ] {
        let (layout, _, _) = layout(
            reader.source.as_ref(),
            &format!("<blockquote>{content}</blockquote>"),
            depth,
            800,
            400,
            false,
        );
        let scroll = scroll(&layout);
        let mut prior: Option<(usize, f32, Vec<SourceRange>)> = None;
        let mut joins = 0;
        for (index, page) in layout.pages.iter().enumerate() {
            for item in &page.items {
                let PageItem::Quote(quote) = item else {
                    continue;
                };
                if quote.continued_before {
                    let (old_index, old_bottom, old_sources) = prior.as_ref().unwrap();
                    assert_eq!(old_sources, &quote.sources);
                    assert!((scroll.content_y(index, quote.y) - old_bottom).abs() < 0.1);
                    assert!(
                        scroll.pages[*old_index]
                            .page
                            .quote_bridge_to(&scroll.pages[index].page)
                            .is_some()
                    );
                    joins += 1;
                }
                prior = Some((
                    index,
                    scroll.content_y(index, quote.y + quote.height),
                    quote.sources.clone(),
                ));
            }
        }
        assert!(joins > 0);
    }
}

#[test]
fn figure_paragraph_spacing_survives_page_boundaries() {
    let (reader, _, _) = semantic_layout::tests::fixture();
    let body = (0..45)
        .map(|i| format!("<p>Paragraph {i} keeps its normal spacing.</p>"))
        .collect::<String>();
    for depth in [1, 2] {
        let (layout, _, style) = layout(reader.source.as_ref(), &body, depth, 800, 350, false);
        let scroll = scroll(&layout);
        let mut previous: Option<(usize, f32)> = None;
        let mut joins = 0;
        for (index, page) in layout.pages.iter().enumerate() {
            for item in &page.items {
                let PageItem::Text(text) = item else { continue };
                if !text.text.starts_with("Paragraph ") {
                    continue;
                }
                let top = text.origin_y
                    + text
                        .layout
                        .get(text.lines.start)
                        .unwrap()
                        .metrics()
                        .block_min_coord;
                let last = text.layout.get(text.lines.end - 1).unwrap().metrics();
                let bottom = text.origin_y
                    + last
                        .block_max_coord
                        .max(last.block_min_coord + last.line_height);
                if let Some((old_index, old_bottom)) = previous {
                    let gap = scroll.content_y(index, top) - old_bottom;
                    let expected = style.typography.font_size * style.typesetting.paragraph_gap_em;
                    assert!(
                        (gap - expected).abs() < 0.1,
                        "expected paragraph gap {expected}, got {gap}"
                    );
                    joins += usize::from(old_index != index);
                }
                previous = Some((index, scroll.content_y(index, bottom)));
            }
        }
        assert!(joins > 0);
    }
}

#[test]
fn quotes_keep_their_inset_and_theme_inside_figures_with_zero_paragraph_indent() {
    let (reader, _, _) = semantic_layout::tests::fixture();
    let words = "Quoted text has its own space after the accent. ".repeat(75);
    for depth in [0, 1, 2] {
        for dark in [false, true] {
            let (_, section, mut style) = layout(
                reader.source.as_ref(),
                &format!("<blockquote><p>{words}</p></blockquote>"),
                depth,
                800,
                400,
                dark,
            );
            style.typesetting.paragraph_indent_mode = rebook_layout::ParagraphIndentMode::Custom;
            style.typesetting.paragraph_indent_em = 0.0;
            style.background = if dark {
                Rgba::BLACK
            } else {
                Rgba {
                    red: 245,
                    green: 242,
                    blue: 235,
                    alpha: 255,
                }
            };
            let layout =
                LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned())
                    .layout_blocks(
                        reader.source.as_ref(),
                        &section.blocks,
                        LayoutViewport::new(800, 400).unwrap(),
                        &style,
                    )
                    .unwrap();
            let mut quotes = 0;
            for page in &layout.pages {
                let display = DisplayListCompiler.compile(page);
                for item in &page.items {
                    match item {
                        PageItem::Quote(quote) => {
                            quotes += 1;
                            // The rendered accent ends at x + 10; leave at least
                            // six pixels between that edge and selectable text.
                            for rect in display.source_rects(&quote.sources) {
                                assert!(
                                    rect.x0 >= f64::from(quote.x + 16.0) - 0.1,
                                    "depth={depth}: quote text overlaps its accent: {rect:?}"
                                );
                            }
                        }
                        PageItem::FigureBody(body) => assert!(
                            !body.paint_background,
                            "a semantic quote must retain the reading theme"
                        ),
                        _ => {}
                    }
                }
            }
            assert!(
                quotes > 1,
                "check every continuation as well as the first slice"
            );
            if depth > 0 {
                let page = &layout.pages[0];
                let body = page
                    .items
                    .iter()
                    .find_map(|item| match item {
                        PageItem::FigureBody(body) => Some(body),
                        _ => None,
                    })
                    .unwrap();
                let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(800, 400);
                let mut pixels = Vec::new();
                renderer.render_to_vec(
                    |scene| DisplayListCompiler.compile(page).paint(scene),
                    &mut pixels,
                );
                let at = (((body.y + 2.0) as usize) * 800 + (body.x + 2.0) as usize) * 4;
                assert_eq!(
                    &pixels[at..at + 4],
                    &[
                        style.background.red,
                        style.background.green,
                        style.background.blue,
                        style.background.alpha
                    ],
                    "no forced white panel"
                );
            }
        }
    }
}
