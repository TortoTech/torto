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
    fn display_formula_padding_survives_narrow_and_numbered_rows() {
        let source = Source(Book {
            id: PublicationId::new("formula-padding").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: Vec::new(),
            table_of_contents: Vec::new(),
        });
        let formulas = [
            r"S_i(p^*,t)=\max\left[\sum_b\mathrm{difference}_{cbi}(p^*,t)\right]",
            r"\frac{\sum_{i=1}^{n}x_i^2}{1+\sqrt{x}}",
            r"\begin{pmatrix}a&b\\c&d\\e&f\end{pmatrix}",
        ];
        let mut engine = LayoutEngine::new();
        for width in [200, 480] {
            for font_size in [18.0, 32.0] {
                for numbered in [false, true] {
                    for (index, latex) in formulas.iter().enumerate() {
                        let block = Block::Image(ImageBlock {
                            href: PublicationUrl::parse("formula.png").unwrap(),
                            alt: String::new(),
                            style: Default::default(),
                            source: None,
                            text_layer: None,
                            formula_image: true,
                            formula: Some(ImageFormula {
                                latex: (*latex).into(),
                                equation_number: numbered.then(|| "3.1".into()),
                            }),
                        });
                        let mut style = ReaderStyle {
                            typesetting: ReaderTypesetting::unified(),
                            spread: SpreadMode::Single,
                            horizontal_margin: 12.0,
                            ..Default::default()
                        };
                        style.typography.font_size = font_size;
                        let layout = engine
                            .layout_blocks(
                                &source,
                                &[block],
                                LayoutViewport::new(width, 240).unwrap(),
                                &style,
                            )
                            .unwrap();
                        let placed = layout
                            .pages
                            .iter()
                            .flat_map(|page| &page.items)
                            .find_map(|item| {
                                if let PageItem::Image(image) = item {
                                    Some(image)
                                } else {
                                    None
                                }
                            })
                            .unwrap();
                        assert!(placed.formula_presentation.is_some(), "{latex}");
                        let raster = &placed.image;
                        let ink = raster
                            .pixels
                            .chunks_exact(4)
                            .enumerate()
                            .filter(|(_, pixel)| pixel[3] > 16)
                            .map(|(i, _)| {
                                (
                                    (i % raster.width as usize) as u32,
                                    (i / raster.width as usize) as u32,
                                )
                            })
                            .collect::<Vec<_>>();
                        assert!(!ink.is_empty());
                        let left = ink.iter().map(|p| p.0).min().unwrap() as f32 * placed.width
                            / raster.width as f32;
                        let right = (raster.width - 1 - ink.iter().map(|p| p.0).max().unwrap())
                            as f32
                            * placed.width
                            / raster.width as f32;
                        let top = ink.iter().map(|p| p.1).min().unwrap() as f32 * placed.height
                            / raster.height as f32;
                        let bottom = (raster.height - 1 - ink.iter().map(|p| p.1).max().unwrap())
                            as f32
                            * placed.height
                            / raster.height as f32;
                        assert!(
                            left >= 5.5 && right >= 5.5 && top >= 3.5 && bottom >= 3.5,
                            "width={width} font={font_size} number={numbered}: [{left}, {right}, {top}, {bottom}]"
                        );
                        assert!(placed.x >= 0.0 && placed.x + placed.width <= width as f32 + 0.1);
                        if width == 480
                            && font_size == 18.0
                            && !numbered
                            && index == 0
                            && let Some(path) = std::env::var_os("TORTO_FORMULA_PADDING_PREVIEW")
                        {
                            let pixels = Pixmap::from_vec(
                                raster.pixels.to_vec(),
                                resvg::tiny_skia::IntSize::from_wh(raster.width, raster.height)
                                    .unwrap(),
                            )
                            .unwrap();
                            let mut preview = Pixmap::new(raster.width, raster.height).unwrap();
                            preview.fill(resvg::tiny_skia::Color::from_rgba8(250, 249, 246, 255));
                            preview.draw_pixmap(
                                0,
                                0,
                                pixels.as_ref(),
                                &resvg::tiny_skia::PixmapPaint::default(),
                                Transform::identity(),
                                None,
                            );
                            let border = resvg::tiny_skia::PathBuilder::from_rect(
                                resvg::tiny_skia::Rect::from_xywh(
                                    2.0,
                                    2.0,
                                    raster.width as f32 - 4.0,
                                    raster.height as f32 - 4.0,
                                )
                                .unwrap(),
                            );
                            let mut paint = resvg::tiny_skia::Paint::default();
                            paint.set_color_rgba8(66, 139, 103, 255);
                            preview.stroke_path(
                                &border,
                                &paint,
                                &resvg::tiny_skia::Stroke {
                                    width: 4.0,
                                    ..Default::default()
                                },
                                Transform::identity(),
                                None,
                            );
                            preview.save_png(path).unwrap();
                        }
                    }
                }
            }
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
            assert!(
                placed.formula_presentation.is_some(),
                "known formulas keep copyable text in either presentation mode"
            );
            if !unified {
                assert_eq!((placed.image.width, placed.image.height), (60, 24));
            }
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

fn padded_display_row(
    body: (RasterImage, f32, f32),
    number: Option<(RasterImage, f32, f32)>,
    inner_width: f32,
    page_height: f32,
    pad_x: f32,
    pad_y: f32,
    stacked: bool,
) -> Option<(RasterImage, f32, f32)> {
    let (body, mut bw, mut bh) = body;
    let available_height = (page_height - pad_y * 2.0).max(1.0);
    let number = number.map(|(image, w, h)| {
        let scale = (available_height * if stacked { 0.3 } else { 1.0 } / h).min(1.0);
        (image, w * scale, h * scale)
    });
    let stack_gap = if stacked {
        (available_height * 0.1).min(6.0)
    } else {
        0.0
    };
    let label_height = number.as_ref().map_or(0.0, |(_, _, height)| *height);
    let body_height = if stacked {
        (available_height - label_height - stack_gap).max(1.0)
    } else {
        available_height
    };
    let scale = (body_height / bh).min(1.0);
    bw *= scale;
    bh *= scale;
    let content_height = if stacked {
        bh + stack_gap + label_height
    } else {
        bh.max(label_height)
    };
    let content_width = if number.is_some() { inner_width } else { bw };
    let width = content_width + pad_x * 2.0;
    let height = content_height + pad_y * 2.0;
    let scale = 2.0;
    let mut canvas = Pixmap::new(
        (width * scale).ceil().max(1.0) as u32,
        (height * scale).ceil().max(1.0) as u32,
    )?;
    let mut draw = |image: &RasterImage, x: f32, y: f32, w: f32, h: f32| -> Option<()> {
        let size = resvg::tiny_skia::IntSize::from_wh(image.width, image.height)?;
        let pixels = Pixmap::from_vec(image.pixels.to_vec(), size)?;
        canvas.draw_pixmap(
            0,
            0,
            pixels.as_ref(),
            &resvg::tiny_skia::PixmapPaint::default(),
            Transform::from_scale(
                w * scale / image.width as f32,
                h * scale / image.height as f32,
            )
            .post_translate(x * scale, y * scale),
            None,
        );
        Some(())
    };
    draw(
        &body,
        pad_x + (content_width - bw) * 0.5,
        pad_y
            + if stacked {
                0.0
            } else {
                (content_height - bh) * 0.5
            },
        bw,
        bh,
    )?;
    if let Some((number, w, h)) = number {
        draw(
            &number,
            pad_x + content_width - w,
            pad_y
                + if stacked {
                    bh + stack_gap
                } else {
                    (content_height - h) * 0.5
                },
            w,
            h,
        )?;
    }
    Some((
        RasterImage {
            width: canvas.width(),
            height: canvas.height(),
            pixels: canvas.take().into(),
        },
        width,
        height,
    ))
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
        let em = (style.typography.font_size * 1.12).max(style.typography.minimum_font_size);
        let pad_x = (em * 0.4).max(6.0).min((width - 1.0).max(0.0) * 0.25);
        let page_height = (paginator.bottom - paginator.top).max(1.0);
        let pad_y = (em * 0.25)
            .max(4.0)
            .min((page_height - 1.0).max(0.0) * 0.25);
        let inner_width = (width - pad_x * 2.0).max(1.0);
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
                inner_width,
                &self.svg_options,
            )
            .ok()
        });
        // A number must never disappear when its renderer rejects the label.
        if number.is_some() && number_image.is_none() {
            return Ok(false);
        }
        let number_gap = 12.0;
        let stacked = number_image
            .as_ref()
            .is_some_and(|(_, w, _)| inner_width - 2.0 * (w + number_gap) < em * 2.0);
        let usable = if stacked {
            inner_width
        } else {
            number_image.as_ref().map_or(inner_width, |(_, w, _)| {
                (inner_width - 2.0 * (w + number_gap)).max(1.0)
            })
        };
        let Ok((raster, w, h)) = rasterize_formula(
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
        let Some((raster, w, h)) = padded_display_row(
            (raster, w, h),
            number_image,
            inner_width,
            page_height,
            pad_x,
            pad_y,
            stacked,
        ) else {
            return Ok(false);
        };
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
