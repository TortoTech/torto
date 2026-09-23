use super::*;
use rebook_publication::{ImageFormula, InlineImageRun};
use resvg::tiny_skia::{Pixmap, Transform};

pub(super) fn only_formula(block: &TextBlock) -> Option<(&InlineImageRun, Option<String>)> {
    let mut image = None;
    let mut label = String::new();
    for inline in &block.content {
        match inline {
            Inline::Image(run) if run.image.formula.is_some() && image.is_none() => {
                image = Some(run.as_ref())
            }
            Inline::Text(run) => label.push_str(&run.text),
            Inline::Break => label.push(' '),
            _ => return None,
        }
    }
    let label = label.trim();
    if label.is_empty() {
        return Some((image?, None));
    }
    let inner = label
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .or_else(|| label.strip_prefix('（').and_then(|s| s.strip_suffix('）')))?;
    if inner.len() > 24
        || !inner
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | ' '))
    {
        return None;
    }
    Some((image?, Some(label.to_owned())))
}

pub(super) fn inline(
    formula: &ImageFormula,
    scale: f32,
    typography: &ReaderTypography,
    color: Rgba,
    width: f32,
    options: &resvg::usvg::Options<'_>,
    original: RasterImage,
    id: u64,
    index: usize,
) -> Result<PreparedInlineImage, String> {
    if formula.equation_number.is_some() {
        return Err("Numbered formula requires a display row".into());
    }
    let math = MathRun {
        latex: formula.latex.clone(),
        display: false,
        size_scale: scale,
    };
    let (image, w, h) = rasterize_formula(&math, typography, color, width, options)?;
    let (box_height, offset_y) = math_vertical_metrics(&math, typography, h)?;
    Ok(PreparedInlineImage {
        formula_presentation: Some(FormulaPresentation {
            original,
            latex: formula.latex.clone(),
        }),
        id,
        index,
        image,
        width: w,
        height: h,
        box_height,
        offset_y,
    })
}

pub(super) fn math_vertical_metrics(
    math: &MathRun,
    typography: &ReaderTypography,
    h: f32,
) -> Result<(f32, f32), String> {
    let size = (typography.font_size
        * math.size_scale.clamp(0.5, 3.0)
        * if math.display { 1.12 } else { 1.0 })
    .max(typography.minimum_font_size);
    let metrics = rebook_math::math::render_math(&math.latex, size, "#000000", math.display)?;
    // rasterize_formula uses a 1.5-pixel padding on each side.
    let ratio = h / (metrics.ascent + metrics.descent + 3.0).max(1.0);
    let below = (metrics.descent + 1.5) * ratio;
    let above = (h - below).max(size * 0.8);
    let box_height = (above + below.max(size * 0.2)).max(h);
    Ok((box_height, box_height - h + below))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{
        Book, Metadata, PublicationId, RasterResource, Resource, SourceAnchor, SpineItemId,
    };
    struct Source(Book);
    impl BookSource for Source {
        fn book(&self) -> &Book {
            &self.0
        }
        fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
            unreachable!()
        }
        fn resource(&self, _: &PublicationUrl) -> Result<Resource, PublicationError> {
            unreachable!()
        }
        fn raster_resource(
            &self,
            _: &PublicationUrl,
        ) -> Result<Option<RasterResource>, PublicationError> {
            Ok(Some(RasterResource {
                width: 60,
                height: 24,
                pixels: vec![255; 60 * 24 * 4].into(),
            }))
        }
    }

    #[test]
    fn image_gaps_use_the_paragraph_end_after_discretionary_hyphens() {
        let source = Source(Book {
            id: PublicationId::new("image-gap-test").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let paragraph = |value: &str| {
            Block::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                content: vec![Inline::Text(TextRun {
                    text: value.into(),
                    style: TextStyle {
                        language: rebook_publication::TextLanguage::EnglishUs,
                        ..Default::default()
                    },
                    link: None,
                })],
                style: Default::default(),
                source: None,
            })
        };
        let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(include_bytes!(
            "../../../assets/fonts/Literata-opsz-wght.ttf"
        )))]);
        for formula in [false, true] {
            let image = Block::Image(ImageBlock {
                formula_image: formula,
                formula: formula.then(|| ImageFormula {
                    latex: "x=1".into(),
                    equation_number: None,
                }),
                href: PublicationUrl::parse("math.png").unwrap(),
                alt: String::new(),
                style: Default::default(),
                source: None,
                text_layer: None,
            });
            let blocks = [
                paragraph(
                    "Extraordinary typographical considerations improve international readability and representation. These productions identify configurations of letters:",
                ),
                image,
                paragraph("The following paragraph explains the result."),
            ];
            let style = ReaderStyle {
                typesetting: ReaderTypesetting::unified(),
                spread: SpreadMode::Single,
                horizontal_margin: 0.0,
                ..ReaderStyle::default()
            };
            let mut covered = false;
            for width in (120..=260).step_by(10) {
                let layout = engine
                    .layout_blocks(
                        &source,
                        &blocks,
                        LayoutViewport::new(width, 1600).unwrap(),
                        &style,
                    )
                    .unwrap();
                let items = &layout.pages[0].items;
                let image_index = items
                    .iter()
                    .position(|i| matches!(i, PageItem::Image(_)))
                    .unwrap();
                if !items[..image_index].iter().any(|i|matches!(i,PageItem::Text(t) if t.source.is_none() && t.text.as_ref()=="\u{2010}")) {continue;}
                let PageItem::Text(before) = &items[0] else {
                    panic!()
                };
                let PageItem::Image(image) = &items[image_index] else {
                    panic!()
                };
                let PageItem::Text(after) = &items[image_index + 1] else {
                    panic!()
                };
                let line = before.layout.get(before.lines.end - 1).unwrap();
                let last = line.metrics();
                let bottom = before.origin_y
                    + last
                        .block_max_coord
                        .max(last.block_min_coord + last.line_height);
                let top = after.origin_y
                    + after
                        .layout
                        .get(after.lines.start)
                        .unwrap()
                        .metrics()
                        .block_min_coord;
                let gap_before = image.y - bottom;
                let gap_after = top - image.y - image.height;
                assert!(
                    (gap_before - gap_after).abs() < 0.01,
                    "formula={formula}, width={width}: before={gap_before}, after={gap_after}"
                );
                assert!(
                    gap_before + 0.01
                        >= style.typography.font_size * style.typesetting.media_gap_em
                );
                covered = true;
                break;
            }
            assert!(
                covered,
                "fixture must exercise hyphenated prose before the image"
            );
        }
    }

    #[test]
    fn formulas_preserve_originals_and_numbers_and_fall_back_when_invalid() {
        let source = Source(Book {
            id: PublicationId::new("formula-test").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "formula".into(),
            text_offset: 0,
        };
        let range = SourceRange {
            start: anchor.clone(),
            end: anchor,
        };
        let image = ImageBlock {
            formula_image: true,
            formula: Some(ImageFormula {
                latex: r"P_i=\frac{e^{U_i/t}}{\sum_j e^{U_j/t}}".into(),
                equation_number: None,
            }),
            href: PublicationUrl::parse("math.png").unwrap(),
            alt: "original".into(),
            style: Default::default(),
            source: Some(range.clone()),
            text_layer: None,
        };
        let mut engine = LayoutEngine::with_fonts([ReaderFontBlob::new(Arc::new(include_bytes!(
            "../../../assets/fonts/Literata-opsz-wght.ttf"
        )))]);
        for unified in [false, true] {
            let mut style = ReaderStyle {
                spread: SpreadMode::Single,
                horizontal_margin: 12.0,
                ..ReaderStyle::default()
            };
            if unified {
                style.typesetting = ReaderTypesetting::unified();
            }
            let result = engine
                .layout_blocks(
                    &source,
                    &[Block::Image(image.clone())],
                    LayoutViewport::new(600, 600).unwrap(),
                    &style,
                )
                .unwrap();
            let placed = result
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .find_map(|i| match i {
                    PageItem::Image(i) => Some(i),
                    _ => None,
                })
                .unwrap();
            assert_eq!(placed.source, Some(range.clone()));
            assert_eq!(placed.formula_presentation.is_some(), unified);
            if let Some(f) = &placed.formula_presentation {
                assert_eq!((f.original.width, f.original.height), (60, 24));
                assert_eq!(f.latex, image.formula.as_ref().unwrap().latex);
            }
        }
        let run = InlineImageRun {
            image: image.clone(),
            size_scale: 1.0,
            intrinsic_sizing: true,
            vertical_align: InlineImageAlignment::Baseline,
            presentation: false,
        };
        let numbered = TextBlock {
            kind: TextBlockKind::Paragraph,
            style: Default::default(),
            source: Some(range.clone()),
            content: vec![
                Inline::Image(Box::new(run.clone())),
                Inline::Text(TextRun {
                    text: " (A.2)".into(),
                    style: Default::default(),
                    link: None,
                }),
            ],
        };
        assert!(is_display_formula(&numbered));
        let style = ReaderStyle {
            typesetting: ReaderTypesetting::unified(),
            spread: SpreadMode::Single,
            horizontal_margin: 12.0,
            ..ReaderStyle::default()
        };
        let result = engine
            .layout_blocks(
                &source,
                &[Block::Text(numbered)],
                LayoutViewport::new(600, 600).unwrap(),
                &style,
            )
            .unwrap();
        let placed = result
            .pages
            .iter()
            .flat_map(|p| &p.items)
            .find_map(|i| match i {
                PageItem::Image(i) => Some(i),
                _ => None,
            })
            .unwrap();
        assert!(
            placed.width > 500.0,
            "equation label uses the right edge of the display row"
        );
        let mut paragraph = TextBlock {
            kind: TextBlockKind::Paragraph,
            style: Default::default(),
            source: Some(range),
            content: vec![
                Inline::Text(TextRun {
                    text: "Before ".into(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Image(Box::new(run)),
                Inline::Text(TextRun {
                    text: " after.".into(),
                    style: Default::default(),
                    link: None,
                }),
            ],
        };
        assert!(!is_display_formula(&paragraph));
        for valid in [true, false] {
            if !valid {
                let Inline::Image(i) = &mut paragraph.content[1] else {
                    panic!()
                };
                i.image.formula.as_mut().unwrap().latex = r"\frac{".into();
            }
            let result = engine
                .layout_blocks(
                    &source,
                    &[Block::Text(paragraph.clone())],
                    LayoutViewport::new(600, 600).unwrap(),
                    &style,
                )
                .unwrap();
            let inline = result
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .filter_map(|i| match i {
                    PageItem::Text(t) => Some(t),
                    _ => None,
                })
                .flat_map(|t| t.inline_images.iter())
                .next()
                .unwrap();
            assert_eq!(inline.formula_presentation.is_some(), valid);
            if !valid {
                assert_eq!((inline.image.width, inline.image.height), (60, 24));
            }
        }
    }
}

impl LayoutEngine {
    pub(super) fn push_formula_image(
        &mut self,
        paginator: &mut Paginator,
        source: &dyn BookSource,
        image: &ImageBlock,
        owner: Option<SourceRange>,
        number: Option<&str>,
        style: &ReaderStyle,
        width: f32,
    ) -> Result<bool, LayoutError> {
        let Some(formula) = &image.formula else {
            return Ok(false);
        };
        let original = load_raster_image(source, image)?;
        if let (Some(authored), Some(recognized)) = (number, formula.equation_number.as_deref()) {
            let normalize = |s: &str| {
                s.chars()
                    .filter(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '（' | '）'))
                    .collect::<String>()
            };
            if normalize(authored) != normalize(recognized) {
                return Ok(false);
            }
        }
        let number = number.or(formula.equation_number.as_deref());
        let number_image = number.and_then(|number| {
            let label = if number.starts_with(['(', '（']) {
                number.to_owned()
            } else {
                format!("({number})")
            };
            rasterize_formula(
                &MathRun {
                    latex: format!(r"\text{{{label}}}"),
                    display: false,
                    size_scale: 0.85,
                },
                &style.typography,
                style.foreground,
                width,
                &self.svg_options,
            )
            .ok()
        });
        // A number must never disappear when its renderer rejects the label.
        if number.is_some() && number_image.is_none() {
            return Ok(false);
        }
        let usable = number_image
            .as_ref()
            .map_or(width, |(_, w, _)| (width - 2.0 * (w + 12.0)).max(40.0));
        let Ok((mut raster, mut w, mut h)) = rasterize_formula(
            &MathRun {
                latex: formula.latex.clone(),
                display: true,
                size_scale: 1.0,
            },
            &style.typography,
            style.foreground,
            usable,
            &self.svg_options,
        ) else {
            return Ok(false);
        };
        if let Some((label, lw, lh)) = number_image {
            let height = h.max(lh);
            let scale = 2.0;
            let Some(mut canvas) = Pixmap::new(
                (width * scale).ceil().max(1.0) as u32,
                (height * scale).ceil().max(1.0) as u32,
            ) else {
                return Ok(false);
            };
            for (part, x, y, target_w, target_h) in [
                (&raster, (width - w) * 0.5, (height - h) * 0.5, w, h),
                (&label, width - lw, (height - lh) * 0.5, lw, lh),
            ] {
                let Some(size) = resvg::tiny_skia::IntSize::from_wh(part.width, part.height) else {
                    return Ok(false);
                };
                let Some(pixmap) = Pixmap::from_vec(part.pixels.to_vec(), size) else {
                    return Ok(false);
                };
                canvas.draw_pixmap(
                    (x * scale).round() as i32,
                    (y * scale).round() as i32,
                    pixmap.as_ref(),
                    &resvg::tiny_skia::PixmapPaint::default(),
                    Transform::from_scale(
                        target_w * scale / part.width as f32,
                        target_h * scale / part.height as f32,
                    ),
                    None,
                );
            }
            raster = RasterImage {
                width: canvas.width(),
                height: canvas.height(),
                pixels: canvas.take().into(),
            };
            w = width;
            h = height;
        }
        let gap = style.typography.font_size * style.typesetting.media_gap_em;
        paginator.push_image(
            raster,
            ImageStyle {
                width: Some(ImageLength::Pixels(w)),
                height: Some(ImageLength::Pixels(h)),
                margin_before: gap,
                margin_after: gap,
                ..ImageStyle::default()
            },
            owner.or_else(|| image.source.clone()),
            None,
        );
        if let Some(PageItem::Image(placed)) = paginator.items.last_mut() {
            placed.formula_presentation = Some(FormulaPresentation {
                original,
                latex: formula.latex.clone(),
            });
        }
        Ok(true)
    }
}
