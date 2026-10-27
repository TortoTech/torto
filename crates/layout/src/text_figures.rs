//! Structured figure bodies retain live text inside a centered white surface.
use super::*;
use rebook_publication::FigureBlock;

fn caption(block: &Block) -> bool {
    matches!(block, Block::Text(t) if matches!(t.kind,
        TextBlockKind::Caption | TextBlockKind::QuoteAttribution))
}

impl LayoutEngine {
    pub(super) fn push_text_figure(
        &mut self,
        paginator: &mut Paginator,
        source: &dyn BookSource,
        figure: &FigureBlock,
        reader_style: &ReaderStyle,
    ) -> Result<(), LayoutError> {
        let Some(start) = figure.body.iter().position(|block| !caption(block)) else {
            let captions: Vec<_> = figure
                .body
                .iter()
                .filter_map(|block| match block {
                    Block::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .collect();
            for (text, block) in
                self.shape_figure_captions(source, &captions, reader_style, paginator.width, true)?
            {
                paginator.push_text(&text, &block)?;
            }
            return Ok(());
        };
        let end = figure
            .body
            .iter()
            .rposition(|block| !caption(block))
            .unwrap()
            + 1;
        let captions = |blocks: &[Block]| {
            blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let before = captions(&figure.body[..start]);
        let after = captions(&figure.body[end..]);
        let mut before =
            self.shape_figure_captions(source, &before, reader_style, paginator.width, true)?;
        let mut after =
            self.shape_figure_captions(source, &after, reader_style, paginator.width, true)?;
        let gap = reader_style.typography.font_size * reader_style.typesetting.caption_gap_em;
        let caption_height = |texts: &[(PreparedText, Cow<'_, TextBlock>)]| {
            texts
                .iter()
                .map(|(text, block)| {
                    prepared_flow_height(text)
                        + block.style.margin_before.max(0.0)
                        + block.style.margin_after.max(0.0)
                })
                .sum::<f32>()
        };
        let before_height = caption_height(&before) + if before.is_empty() { 0.0 } else { gap };
        let after_height = caption_height(&after) + if after.is_empty() { 0.0 } else { gap };
        let padding = (reader_style.typography.font_size * 0.8)
            .min(((paginator.width - 80.0) * 0.5).max(0.0));
        let max_width = (paginator.width - padding * 2.0).floor().max(80.0);
        let height =
            (paginator.bottom - paginator.top - padding * 2.0 - before_height - after_height)
                .floor()
                .max(40.0);
        let mut style = reader_style.clone();
        style.spread = SpreadMode::Single;
        style.horizontal_margin = 0.0;
        style.minimum_horizontal_margin = 0.0;
        style.minimum_content_width = 0.0;
        style.top_margin = 0.0;
        style.bottom_margin = 0.0;
        style.foreground = Rgba::BLACK;
        // Diagram paragraphs use container padding, not the running prose's
        // automatic first-line indent (which also displaces centered lines).
        style.typesetting.paragraph_indent_mode = ParagraphIndentMode::Custom;
        style.typesetting.paragraph_indent_em = 0.0;
        let body = &figure.body[start..end];
        let target = self.raster_target;
        // At most two shaping passes, and only for text figures. Measure the
        // natural width first, then retain authored alignment at the final width.
        // No image raster or second copy of the body is created here.
        let result = (|| {
            let mut viewport = paginator.viewport;
            viewport.width = max_width as u32;
            viewport.height = height as u32;
            let mut layout = self.layout_blocks(source, body, viewport, &style)?;
            let natural = layout
                .pages
                .iter()
                .flat_map(|page| &page.items)
                .map(natural_right)
                .fold(80.0, f32::max)
                .ceil()
                .min(max_width);
            let width = (natural + 1.0).min(max_width);
            if width < max_width {
                viewport.width = width as u32;
                layout = self.layout_blocks(source, body, viewport, &style)?;
            }
            Ok::<_, LayoutError>((layout, width + padding * 2.0))
        })();
        self.raster_target = target;
        let (layout, width) = result?;
        self.align_media_sources(source, &mut before, reader_style, paginator.width, width)?;
        self.align_media_sources(source, &mut after, reader_style, paginator.width, width)?;
        let outer_gap = reader_style.typography.font_size * reader_style.typesetting.media_gap_em;
        let pages: Vec<_> = layout
            .pages
            .into_iter()
            .filter(|page| !page.items.is_empty())
            .collect();
        let first_height = pages
            .first()
            .map_or(0.0, |page| page_bottom(page) + padding * 2.0);
        paginator.prepare_group(
            before_height + first_height + if pages.len() == 1 { after_height } else { 0.0 },
            outer_gap,
        );
        for (text, block) in &before {
            paginator.push_text(text, block)?;
        }
        if !before.is_empty() {
            paginator.add_semantic_spacing(gap);
        }
        let page_count = pages.len();
        for (index, mut page) in pages.into_iter().enumerate() {
            if index > 0 {
                paginator.force_page();
            }
            let height = page_bottom(&page) + padding * 2.0;
            paginator.keep_together_if_fits(
                height
                    + if index + 1 == page_count {
                        after_height
                    } else {
                        0.0
                    },
            );
            let x = paginator.column_left() + (paginator.width - width) * 0.5;
            let y = paginator.cursor_y;
            paginator
                .items
                .push(PageItem::FigureBody(FigureBodyPlacement {
                    x,
                    y,
                    width,
                    height,
                    sources: body_sources(&page),
                }));
            for item in &mut page.items {
                translate(item, x + padding, y + padding);
            }
            paginator.items.extend(page.items);
            paginator.cursor_y = y + height;
            paginator.column_has_content = true;
            paginator.previous_block_was_paragraph = false;
            paginator.pending_leading_gap = 0.0;
        }
        if !after.is_empty() {
            paginator.add_semantic_spacing(gap);
        }
        for (text, block) in &after {
            paginator.push_text(text, block)?;
        }
        if after.is_empty() {
            // The panel padding extends beyond its last child text placement.
            paginator.add_semantic_spacing(outer_gap);
        } else {
            paginator.ensure_minimum_spacing(outer_gap);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::*;

    struct Source(Book);
    impl BookSource for Source {
        fn book(&self) -> &Book {
            &self.0
        }
        fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
            unreachable!()
        }
        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }
    fn source() -> Source {
        Source(Book {
            id: PublicationId::new("text-figure").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        })
    }
    fn text(value: &str, align: TextAlignment, kind: TextBlockKind) -> Block {
        Block::Text(TextBlock {
            kind,
            style: BlockStyle {
                align,
                authored_alignment: Some(align),
                ..Default::default()
            },
            content: vec![Inline::Text(TextRun {
                text: value.into(),
                style: Default::default(),
                link: None,
            })],
            source: Some(SourceRange {
                start: SourceAnchor {
                    spine: SpineItemId::new("chapter").unwrap(),
                    node: value.into(),
                    text_offset: 0,
                },
                end: SourceAnchor {
                    spine: SpineItemId::new("chapter").unwrap(),
                    node: value.into(),
                    text_offset: value.chars().count() as u64,
                },
            }),
        })
    }
    fn figure(body: Vec<Block>) -> Block {
        Block::Figure(FigureBlock {
            body,
            images: vec![],
            captions: vec![],
            caption_position: CaptionPosition::After,
            style: Default::default(),
            source: None,
        })
    }
    fn engine() -> LayoutEngine {
        LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(
            include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf")
                .as_slice()
                .to_vec(),
        ))])
    }
    #[test]
    fn table_figure_uses_the_same_padding_on_all_sides() {
        let rows = [["Year", "Gold medallist", "Country", "Time"],
            ["2008", "Alain Bernard", "France", "47.21 seconds"]]
            .into_iter().enumerate().map(|(row, values)| TableRow {
                cells: values.into_iter().map(|value| {
                    let Block::Text(text) = text(value, TextAlignment::Start, TextBlockKind::Paragraph) else { unreachable!() };
                    TableCell { text, authored_alignment: None, column_span: 1, row_span: 1, header: row == 0 }
                }).collect(),
            }).collect();
        let blocks = [figure(vec![Block::Table(TableBlock {
            before: vec![], after: vec![], rows, source: None,
        })])];
        let style = ReaderStyle { spread: SpreadMode::Single,
            typesetting: ReaderTypesetting::unified(), ..Default::default() };
        for width in [600, 1000, 1400] {
            let layout = engine().layout_blocks(&source(), &blocks,
                LayoutViewport::new(width, 800).unwrap(), &style).unwrap();
            let page = &layout.pages[0];
            let panel = page.items.iter().find_map(|item| match item {
                PageItem::FigureBody(panel) => Some(panel), _ => None,
            }).unwrap();
            let table = page.items.iter().find_map(|item| match item {
                PageItem::Table(table) => Some(table), _ => None,
            }).unwrap();
            let left = table.cells.iter().map(|cell| cell.x).reduce(f32::min).unwrap();
            let right = table.cells.iter().map(|cell| cell.x + cell.width).reduce(f32::max).unwrap();
            let padding = style.typography.font_size * 0.8;
            for gap in [left - panel.x, panel.x + panel.width - right,
                table.y - panel.y, panel.y + panel.height - table.y - table.height] {
                assert!((gap - padding).abs() < 1.1, "width={width}, padding={padding}, actual={gap}");
            }
            assert!((panel.x + panel.width * 0.5 - width as f32 * 0.5).abs() < 0.1);
            assert_eq!(panel.sources.len(), 8);
        }
    }

    #[test]
    fn figure_surface_centers_the_body_preserving_internal_alignment_and_caption_ownership() {
        let blocks = [figure(vec![
            text(
                "A longer line of diagram text",
                TextAlignment::Start,
                TextBlockKind::Paragraph,
            ),
            text("Center", TextAlignment::Center, TextBlockKind::Paragraph),
            text("Right", TextAlignment::End, TextBlockKind::Paragraph),
            text(
                "Figure 1 A text diagram",
                TextAlignment::Start,
                TextBlockKind::Caption,
            ),
            text(
                "Source: Authors.",
                TextAlignment::End,
                TextBlockKind::QuoteAttribution,
            ),
        ])];
        let mut style = ReaderStyle {
            spread: SpreadMode::Single,
            typesetting: ReaderTypesetting::unified(),
            ..Default::default()
        };
        for width in [400, 1000] {
            let layout = engine()
                .layout_blocks(
                    &source(),
                    &blocks,
                    LayoutViewport::new(width, 800).unwrap(),
                    &style,
                )
                .unwrap();
            assert_eq!(layout.pages.len(), 1);
            let page = &layout.pages[0];
            let panel = page
                .items
                .iter()
                .find_map(|item| match item {
                    PageItem::FigureBody(p) => Some(p),
                    _ => None,
                })
                .unwrap();
            assert!((panel.x + panel.width / 2.0 - width as f32 / 2.0).abs() < 0.1);
            if width == 1000 {
                assert!(panel.width < reading_content_width(width as f32, &style) * 0.8);
            }
            let padding = style.typography.font_size * 0.8;
            let mut texts = page.items.iter().filter_map(|item| match item {
                PageItem::Text(t) => Some(t),
                _ => None,
            });
            for (index, label) in ["A longer line of diagram text", "Center", "Right"]
                .iter()
                .enumerate()
            {
                let placed = texts.next().unwrap();
                assert_eq!(placed.text.as_ref(), *label);
                assert_eq!(placed.source.as_ref().unwrap().start.node, *label);
                let line = placed.layout.get(0).unwrap();
                let left = placed.origin_x + line.metrics().offset;
                let right = placed.origin_x + positioned_line_content_end(line);
                assert!(left >= panel.x + padding - 0.1);
                assert!(
                    right <= panel.x + panel.width - padding + 0.1,
                    "{label}: {right}, panel right {}",
                    panel.x + panel.width - padding
                );
                if index == 1 {
                    assert!(((left + right) * 0.5 - (panel.x + panel.width * 0.5)).abs() < 1.0);
                }
                if index == 2 {
                    assert!((right - (panel.x + panel.width - padding)).abs() < 1.0);
                }
            }
            for caption in texts {
                let top =
                    caption.origin_y + caption.layout.get(0).unwrap().metrics().block_min_coord;
                assert!(top >= panel.y + panel.height);
            }
        }
        style.typesetting = ReaderTypesetting::default();
        let classic = engine()
            .layout_blocks(
                &source(),
                &blocks,
                LayoutViewport::new(1000, 800).unwrap(),
                &style,
            )
            .unwrap();
        assert!(
            !classic
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .any(|i| matches!(i, PageItem::FigureBody(_)))
        );
    }

    #[test]
    fn long_text_figures_paginate_without_losing_or_duplicating_children() {
        let mut children: Vec<_> = (0..35)
            .map(|i| {
                text(
                    &format!("Diagram line {i}"),
                    TextAlignment::Start,
                    TextBlockKind::Paragraph,
                )
            })
            .collect();
        children.push(text(
            "Figure 2 Long diagram",
            TextAlignment::Start,
            TextBlockKind::Caption,
        ));
        let style = ReaderStyle {
            spread: SpreadMode::Single,
            typesetting: ReaderTypesetting::unified(),
            ..Default::default()
        };
        let layout = engine()
            .layout_blocks(
                &source(),
                &[figure(children)],
                LayoutViewport::new(500, 350).unwrap(),
                &style,
            )
            .unwrap();
        assert!(layout.pages.len() > 1);
        let texts: Vec<_> = layout
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .filter_map(|i| match i {
                PageItem::Text(t) => Some(t.text.as_ref()),
                _ => None,
            })
            .collect();
        for i in 0..35 {
            assert_eq!(
                texts
                    .iter()
                    .filter(|t| **t == format!("Diagram line {i}"))
                    .count(),
                1
            );
        }
        assert_eq!(texts.last(), Some(&"Figure 2 Long diagram"));
        for panel in layout
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .filter_map(|i| match i {
                PageItem::FigureBody(p) => Some(p),
                _ => None,
            })
        {
            assert!(panel.y >= style.top_margin);
            assert!(panel.y + panel.height <= 350.0 - style.bottom_margin + 0.1);
        }
    }
}

fn natural_right(item: &PageItem) -> f32 {
    match item {
        PageItem::Text(text) => text.origin_x + text.layout.width(),
        PageItem::FigureBody(body) => body.x + body.width,
        PageItem::Quote(quote) => quote.x + quote.width,
        PageItem::Image(image) => image.x + image.width,
        // The grid is already centered in the measuring viewport. That offset
        // is not intrinsic width; carrying it forward adds padding a second time.
        PageItem::Table(table) => {
            let left = table.cells.iter().map(|c| c.x).reduce(f32::min).unwrap_or(0.0);
            let right = table.cells.iter().map(|c| c.x + c.width).fold(0.0, f32::max);
            (right - left).max(0.0)
        }
        PageItem::Separator(line) => line.x + line.width,
    }
}

fn body_sources(page: &PageLayout) -> Vec<SourceRange> {
    let mut sources = Vec::new();
    for item in &page.items {
        match item {
            PageItem::Text(text) => sources.extend(text.sources().cloned()),
            PageItem::Table(table) => sources.extend(table.cells.iter()
                .filter_map(|cell| cell.text.as_ref()).flat_map(TextPlacement::sources).cloned()),
            PageItem::FigureBody(body) => sources.extend(body.sources.iter().cloned()),
            PageItem::Quote(quote) => sources.extend(quote.sources.iter().cloned()),
            PageItem::Image(image) => sources.extend(image.source.iter().cloned()),
            PageItem::Separator(_) => {}
        }
    }
    sources
}

fn page_bottom(page: &PageLayout) -> f32 {
    page.items
        .iter()
        .filter_map(|item| match item {
            PageItem::Text(text) => text
                .lines
                .end
                .checked_sub(1)
                .and_then(|i| text.layout.get(i))
                .map(|line| {
                    text.origin_y
                        + line
                            .metrics()
                            .block_max_coord
                            .max(line.metrics().block_min_coord + line.metrics().line_height)
                }),
            PageItem::FigureBody(body) => Some(body.y + body.height),
            PageItem::Quote(quote) => Some(quote.y + quote.height),
            PageItem::Image(image) => Some(image.y + image.height),
            PageItem::Table(table) => Some(table.y + table.height),
            PageItem::Separator(line) => Some(line.y + 1.0),
        })
        .fold(0.0, f32::max)
}

fn translate(item: &mut PageItem, x: f32, y: f32) {
    fn text(text: &mut TextPlacement, x: f32, y: f32) {
        text.origin_x += x;
        text.origin_y += y;
    }
    match item {
        PageItem::Text(t) => text(t, x, y),
        PageItem::FigureBody(body) => {
            body.x += x;
            body.y += y;
        }
        PageItem::Quote(quote) => {
            quote.x += x;
            quote.y += y;
        }
        PageItem::Image(image) => {
            image.x += x;
            image.y += y;
            if let Some(replacement) = &mut image.replacement {
                for segment in &mut replacement.segments {
                    segment.rect.x += x;
                    segment.rect.y += y;
                    text(&mut segment.text, x, y);
                }
            }
        }
        PageItem::Table(table) => {
            table.y += y;
            for cell in &mut table.cells {
                cell.x += x;
                cell.y += y;
                if let Some(t) = &mut cell.text {
                    text(t, x, y);
                }
            }
        }
        PageItem::Separator(line) => {
            line.x += x;
            line.y += y;
        }
    }
}
