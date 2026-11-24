//! Presentation keeps the author's stable label and body anchors separate.
use super::*;

pub(super) fn join_label(blocks: &[&Block]) -> Option<(TextBlock, Vec<TextSourceSpan>)> {
    let [Block::Text(label), Block::Text(body), ..] = blocks else {
        return None;
    };
    let count = label.style.floating_label?;
    if body.kind != TextBlockKind::Paragraph || body.style.floating_label.is_some() {
        return None;
    }
    let label_source = label.source.as_ref()?;
    let body_source = body.source.as_ref()?;
    let mut combined = body.clone();
    combined.content = label.content.clone();
    combined.content.push(Inline::Text(TextRun {
        text: "\u{2002}".into(),
        style: Default::default(),
        link: None,
    }));
    let body_start = combined.content.len();
    combined.content.extend(body.content.iter().cloned());
    combined.style.floating_label = Some(count + 1);
    combined.style.margin_before = label.style.margin_before.max(body.style.margin_before);
    let spans = vec![
        TextSourceSpan {
            range: 0..label.content.len(),
            source: label_source.clone(),
        },
        TextSourceSpan {
            range: body_start..combined.content.len(),
            source: body_source.clone(),
        },
    ];
    Some((combined, spans))
}

pub(super) struct Band {
    right: bool,
    width: f32,
    bottom: f32,
    page: usize,
}

impl Band {
    pub(super) fn active(&self, paginator: &Paginator) -> bool {
        self.page == paginator.pages.len() && self.bottom > paginator.cursor_y
    }
    pub(super) fn settle(&self, paginator: &mut Paginator) {
        if self.active(paginator) {
            paginator.cursor_y = self.bottom;
        }
    }
    pub(super) fn accepts(&self, block: &Block) -> bool {
        matches!(block, Block::Text(t) if matches!(t.kind, TextBlockKind::Paragraph | TextBlockKind::ListItem { .. })
            && t.style.clear_floats & (if self.right { 2 } else { 1 }) == 0
            && t.style.floating_label.is_none()
            && t.content.iter().all(|item| matches!(item, Inline::Text(r) if r.style.drop_cap.is_none()) || matches!(item, Inline::Break | Inline::Image(_))))
    }
    pub(super) fn wrap(
        &self,
        prepared: &mut PreparedText,
        paginator: &Paginator,
        block: &TextBlock,
    ) {
        let occupied = if self.right {
            self.width
        } else {
            (self.width - prepared.start_offset).max(0.0)
        };
        if !self.active(paginator)
            || occupied <= 0.0
            || prepared.available_width - occupied < 52.0
            || prepared.layout.has_floating_initial()
            || !prepared.ruby.is_empty()
        {
            return;
        }
        let top = paginator
            .cursor_y
            .max(paginator.current_content_bottom().map_or(0.0, |b| {
                b + if paginator.previous_block_was_paragraph {
                    paginator.minimum_paragraph_gap
                } else {
                    0.0
                }
            }));
        let depth = self.bottom - top - block.style.margin_before.max(0.0);
        if depth <= 0.0 {
            return;
        }
        let layout = Arc::make_mut(&mut prepared.layout);
        layout.break_side_float(prepared.available_width, occupied, depth, self.right);
        let align = table_shaping::resolved_text_alignment(layout, block, false);
        layout.align(align, AlignmentOptions::default());
        prepared.lines = 0..layout.len();
    }
}

impl LayoutEngine {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_authored_float(
        &mut self,
        paginator: &mut Paginator,
        source: &dyn BookSource,
        block: &Block,
        next: &Block,
        style: &ReaderStyle,
        content_width: f32,
    ) -> Result<Option<Band>, LayoutError> {
        let (image, captions, position, outer) = match block {
            Block::Image(image) => (
                image,
                &[][..],
                CaptionPosition::After,
                BlockStyle::default(),
            ),
            Block::Figure(f) if f.body.is_empty() && f.images.len() == 1 => (
                &f.images[0],
                f.captions.as_slice(),
                f.caption_position,
                f.style,
            ),
            _ => return Ok(None),
        };
        let Some(right) = image.style.float_right else {
            return Ok(None);
        };
        if image.formula.is_some() || image.text_layer.is_some() {
            return Ok(None);
        }
        let mut band = Band {
            right,
            width: 0.0,
            bottom: 0.0,
            page: 0,
        };
        if !band.accepts(next) {
            return Ok(None);
        }
        let raster = self.load_raster(source, image)?;
        let fit = style
            .typesetting
            .fit_block_images
            .then_some((content_width, paginator.bottom - paginator.top));
        let image_style = normalized_image_style(image, &raster, false, fit);
        let (width, height) = paginator.image_display_size(&raster, image_style);
        let (visible_width, visible_height) = paginator.image_visible_size(&raster, image_style);
        let gap = image.style.float_gap;
        let remainder = content_width - width - gap;
        if width <= 0.0
            || height <= 0.0
            || visible_width <= 0.0
            || visible_height <= 0.0
            || width > content_width * 0.57
            || remainder < (style.typography.font_size * 7.0).max(72.0)
        {
            return Ok(None);
        }
        let prepared = self.shape_figure_captions(source, captions, style, width, false)?;
        let split = match position {
            CaptionPosition::Before => prepared.len(),
            CaptionPosition::After => 0,
            CaptionPosition::Split(n) => n.min(prepared.len()),
        };
        let caption_height = |text: &PreparedText, block: &TextBlock| {
            prepared_flow_height(text)
                + block.style.margin_before.max(0.0)
                + block.style.margin_after.max(0.0)
        };
        let above = image_style
            .margin_before
            .max(if captions.is_empty() {
                0.0
            } else {
                outer.margin_before
            })
            .max(0.0);
        let below = image_style
            .margin_after
            .max(if captions.is_empty() {
                0.0
            } else {
                outer.margin_after
            })
            .max(0.0);
        let height_with_captions = above
            + visible_height
            + below
            + prepared
                .iter()
                .map(|(text, block)| caption_height(text, block))
                .sum::<f32>();
        if height_with_captions > (paginator.bottom - paginator.top) * 0.8 {
            return Ok(None);
        }
        paginator.keep_together_if_fits(height_with_captions + style.typography.font_size);
        let x = paginator.column_left() + if right { content_width - width } else { 0.0 };
        let at = paginator.items.len();
        let item_start = paginator.floating_items.len();
        let image_y = paginator.cursor_y
            + above
            + prepared
                .iter()
                .take(split)
                .map(|(text, block)| caption_height(text, block))
                .sum::<f32>();
        let mut y = paginator.cursor_y + above;
        for (index, (text, block)) in prepared.into_iter().enumerate() {
            if index == split {
                y += visible_height;
            }
            if let Some(first) = text.layout.get(text.lines.start) {
                paginator.floating_items.push((
                    at,
                    PageItem::Text(TextPlacement {
                        source_spans: text.source_spans.clone(),
                        ruby: text.ruby.clone(),
                        citations: text.citations.clone(),
                        layout: text.layout.clone(),
                        text: text.text.clone(),
                        source_text_start: text.source_text_start,
                        lines: text.lines.clone(),
                        origin_x: x + text.start_offset,
                        origin_y: y + block.style.margin_before.max(0.0)
                            - first.metrics().block_min_coord,
                        available_width: text.available_width,
                        source: block.source.clone(),
                        inline_images: text.inline_images.clone(),
                    }),
                ));
            }
            y += caption_height(&text, &block);
        }
        paginator.floating_items.push((
            at,
            PageItem::Image(ImagePlacement {
                clip: (visible_width < width || visible_height < height).then_some(
                    FixedPageTextRect {
                        x,
                        y: image_y,
                        width: visible_width,
                        height: visible_height,
                    },
                ),
                formula_presentation: None,
                image: raster,
                x,
                y: image_y,
                width,
                height,
                source: image.source.clone(),
                text_layer: None,
                replacement: None,
            }),
        ));
        paginator.floating_items[item_start..]
            .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| top(&a.1).total_cmp(&top(&b.1))));
        band.width = width + gap;
        band.bottom = paginator.cursor_y + height_with_captions;
        band.page = paginator.pages.len();
        Ok(Some(band))
    }
}

fn top(item: &PageItem) -> f32 {
    match item {
        PageItem::Text(text) => text.origin_y,
        PageItem::Image(image) => image.y,
        _ => 0.0,
    }
}
