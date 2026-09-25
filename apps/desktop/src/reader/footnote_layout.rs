//! Popup text uses the reader's shaping, hyphenation and justification pipeline.
use base64::Engine as _;
use parley::PositionedLayoutItem;
use rebook_layout::{
    LayoutEngine, LayoutViewport, PageItem, ReaderStyle, SpreadMode, TypesettingMode,
};
use rebook_publication::{
    Block, BlockStyle, BookSource, Inline, TextAlignment, TextBlock, TextBlockKind, TextLanguage,
    TextRun, TextStyle,
};
use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{LocationRef, Size},
    outline::{DrawSettings, OutlinePen},
};
use std::sync::Arc;

fn popup_inlines(text: &str, style: TextStyle) -> Vec<Inline> {
    let mut result = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(r"\(") {
        let Some(end) = rest[start + 2..].find(r"\)").map(|end| start + 2 + end) else {
            break;
        };
        let latex = &rest[start + 2..end];
        if rebook_math::math::render_math(latex, 14.0, "#000000", false).is_err() {
            break;
        }
        if start > 0 {
            result.push(Inline::Text(TextRun {
                text: rest[..start].into(),
                style,
                link: None,
            }));
        }
        result.push(Inline::Math(rebook_publication::MathRun {
            original: None,
            latex: latex.into(),
            display: false,
            size_scale: 1.0,
        }));
        rest = &rest[end + 2..];
    }
    if !rest.is_empty() {
        result.push(Inline::Text(TextRun {
            text: rest.into(),
            style,
            link: None,
        }));
    }
    result
}

pub(super) struct FootnoteLayout {
    pub height: f32,
    pub width: f32,
    pub content_width: f32,
    pub first_baseline: f32,
    svg: Arc<[u8]>,
    uri: String,
    compact_svg: Arc<[u8]>,
    compact_uri: String,
    fallback: Option<Arc<egui::Galley>>,
}

impl FootnoteLayout {
    pub fn fallback(
        ctx: &egui::Context,
        text: &str,
        font: &egui::FontId,
        color: egui::Color32,
        width: f32,
    ) -> Arc<Self> {
        let galley = ctx.fonts_mut(|fonts| fonts.layout(text.into(), font.clone(), color, width));
        Arc::new(Self {
            height: galley.size().y,
            width,
            content_width: galley.size().x.min(width),
            first_baseline: galley
                .rows
                .first()
                .and_then(|r| r.glyphs.first())
                .map_or(font.size, |g| g.pos.y),
            svg: Arc::from([]),
            uri: String::new(),
            compact_svg: Arc::from([]),
            compact_uri: String::new(),
            fallback: Some(galley),
        })
    }
    pub fn paint(&self, ui: &mut egui::Ui) {
        if let Some(galley) = &self.fallback {
            ui.label(galley.clone());
            return;
        }
        ui.add(
            egui::Image::from_bytes(self.uri.clone(), self.svg.clone())
                .fit_to_exact_size(egui::vec2(self.width, self.height)),
        );
    }

    fn paint_at(&self, ui: &egui::Ui, rect: egui::Rect, compact: bool) {
        if let Some(galley) = &self.fallback {
            ui.painter()
                .galley(rect.min, galley.clone(), egui::Color32::WHITE);
            return;
        }
        let (uri, svg) = if compact {
            (&self.compact_uri, &self.compact_svg)
        } else {
            (&self.uri, &self.svg)
        };
        egui::Image::from_bytes(uri.clone(), svg.clone())
            .maintain_aspect_ratio(false)
            .paint_at(ui, rect);
    }

    pub fn citation_row_height(&self, marker: &Self) -> f32 {
        let baseline = self.first_baseline.max(marker.first_baseline);
        (baseline - self.first_baseline + self.height)
            .max(baseline - marker.first_baseline + marker.height)
    }

    /// Use actual shaping baselines and explicit image rectangles. Cropping the
    /// marker's whitespace must not shrink its glyphs to the image aspect ratio.
    pub fn paint_with_marker(&self, ui: &mut egui::Ui, marker: &Self, gap: f32) -> egui::Response {
        let baseline = self.first_baseline.max(marker.first_baseline);
        let (row, response) = ui.allocate_exact_size(
            egui::vec2(
                marker.content_width + gap + self.width,
                self.citation_row_height(marker),
            ),
            egui::Sense::hover(),
        );
        let marker_rect = egui::Rect::from_min_size(
            row.min + egui::vec2(0.0, baseline - marker.first_baseline),
            egui::vec2(marker.content_width, marker.height),
        );
        let body_rect = egui::Rect::from_min_size(
            row.min + egui::vec2(marker.content_width + gap, baseline - self.first_baseline),
            egui::vec2(self.width, self.height),
        );
        marker.paint_at(ui, marker_rect, true);
        self.paint_at(ui, body_rect, false);
        response
    }
}

#[derive(Default)]
pub(super) struct FootnoteRenderer {
    engine: Option<LayoutEngine>,
    cache: Vec<(String, ReaderStyle, f32, Arc<FootnoteLayout>)>,
}

#[derive(Default)]
struct SvgPath(String);
impl OutlinePen for SvgPath {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push_str(&format!("M{x},{y}"));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push_str(&format!("L{x},{y}"));
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push_str(&format!("Q{cx},{cy} {x},{y}"));
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        self.0.push_str(&format!("C{ax},{ay} {bx},{by} {x},{y}"));
    }
    fn close(&mut self) {
        self.0.push('Z');
    }
}

impl FootnoteRenderer {
    pub fn layout(
        &mut self,
        source: &dyn BookSource,
        text: &str,
        reader_style: &ReaderStyle,
        size: f32,
        color: egui::Color32,
        width: f32,
    ) -> Result<Arc<FootnoteLayout>, String> {
        let mut style = reader_style.clone();
        style.typography.font_size = size;
        style.typography.minimum_font_size = size;
        // Leave room for glyph side bearings and antialiasing at the SVG edge.
        style.horizontal_margin = 2.0;
        style.top_margin = 0.0;
        style.bottom_margin = 0.0;
        style.spread = SpreadMode::Single;
        style.typesetting.mode = TypesettingMode::Book;
        style.typesetting.line_break_strategy = rebook_layout::LineBreakStrategy::Optimized;
        style.foreground = rebook_publication::Rgba {
            red: color.r(),
            green: color.g(),
            blue: color.b(),
            alpha: color.a(),
        };
        if let Some((_, _, _, layout)) = self
            .cache
            .iter()
            .find(|(t, s, w, _)| t == text && s == &style && *w == width)
        {
            return Ok(layout.clone());
        }
        let english = text.chars().filter(|c| c.is_ascii_alphabetic()).count()
            > text
                .chars()
                .filter(|c| !c.is_ascii() && c.is_alphabetic())
                .count();
        let display_text = url_line_breaks(text);
        let blocks: Vec<_> = display_text
            .lines()
            .map(|line| {
                Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: popup_inlines(
                        line,
                        TextStyle {
                            language: if english {
                                TextLanguage::EnglishUs
                            } else {
                                TextLanguage::default()
                            },
                            ..Default::default()
                        },
                    ),
                    style: BlockStyle {
                        align: TextAlignment::Justify,
                        margin_after: 0.0,
                        line_height: 1.45,
                        ..Default::default()
                    },
                    source: None,
                })
            })
            .collect();
        let engine = self.engine.get_or_insert_with(|| {
            LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned())
        });
        let layout = engine
            .layout_blocks(
                source,
                &blocks,
                LayoutViewport::new(width.floor().max(40.0) as u32, 100_000)
                    .map_err(|e| e.to_string())?,
                &style,
            )
            .map_err(|e| e.to_string())?;
        let mut paths = String::new();
        let mut content_width = 0.0_f32;
        let mut first_baseline = None;
        let mut height = size * 1.45;
        let mut page_y = 0.0;
        for page in &layout.pages {
            let mut bottom = 0.0_f32;
            for item in &page.items {
                let PageItem::Text(text) = item else {
                    continue;
                };
                for line in text
                    .layout
                    .lines()
                    .skip(text.lines.start)
                    .take(text.lines.len())
                {
                    first_baseline.get_or_insert(page_y + text.origin_y + line.metrics().baseline);
                    bottom = bottom
                        .max(text.origin_y + line.metrics().baseline + line.metrics().descent);
                    for item in line.items() {
                        if let PositionedLayoutItem::InlineBox(inline_box) = &item {
                            if let Some(image) = text
                                .inline_images
                                .iter()
                                .find(|image| image.id == inline_box.id)
                            {
                                let mut rgba = image.image.pixels.to_vec();
                                for pixel in rgba.chunks_exact_mut(4) {
                                    let alpha = u32::from(pixel[3]);
                                    if alpha > 0 && alpha < 255 {
                                        for c in &mut pixel[..3] {
                                            *c = (u32::from(*c) * 255 / alpha).min(255) as u8;
                                        }
                                    }
                                }
                                if let Some(rgba) = image::RgbaImage::from_raw(
                                    image.image.width,
                                    image.image.height,
                                    rgba,
                                ) {
                                    let mut png = std::io::Cursor::new(Vec::new());
                                    image::DynamicImage::ImageRgba8(rgba)
                                        .write_to(&mut png, image::ImageFormat::Png)
                                        .map_err(|e| e.to_string())?;
                                    let data = base64::engine::general_purpose::STANDARD
                                        .encode(png.into_inner());
                                    let x = text.origin_x + inline_box.x;
                                    let y = page_y + text.origin_y + inline_box.y + image.offset_y;
                                    content_width = content_width.max(x + image.width + 2.0);
                                    bottom = bottom.max(y - page_y + image.height);
                                    paths.push_str(&format!(r#"<image x="{x}" y="{y}" width="{}" height="{}" href="data:image/png;base64,{data}"/>"#,image.width,image.height));
                                }
                            }
                            continue;
                        }
                        let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                            continue;
                        };
                        let run = glyph_run.run();
                        content_width = content_width
                            .max(text.origin_x + glyph_run.offset() + glyph_run.advance() + 2.0);
                        let font = FontRef::from_index(run.font().data.as_ref(), run.font().index)
                            .map_err(|e| e.to_string())?;
                        let outlines = font.outline_glyphs();
                        let coords: Vec<_> = run
                            .normalized_coords()
                            .iter()
                            .map(|c| skrifa::instance::NormalizedCoord::from_bits(*c))
                            .collect();
                        for glyph in glyph_run.positioned_glyphs() {
                            let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
                                continue;
                            };
                            let mut path = SvgPath::default();
                            outline
                                .draw(
                                    DrawSettings::unhinted(
                                        Size::new(run.font_size()),
                                        LocationRef::new(&coords),
                                    ),
                                    &mut path,
                                )
                                .map_err(|e| e.to_string())?;
                            paths.push_str(&format!(
                                "<path transform=\"translate({} {}) scale(1 -1)\" d=\"{}\"/>",
                                text.origin_x + glyph.x,
                                page_y + text.origin_y + glyph.y,
                                path.0
                            ));
                        }
                    }
                }
            }
            height = height.max(page_y + bottom + 2.0);
            page_y += bottom;
        }
        let make_svg = |width: f32| {
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><g fill=\"#{:02x}{:02x}{:02x}\" fill-opacity=\"{}\">{paths}</g></svg>",
                color.r(),
                color.g(),
                color.b(),
                f32::from(color.a()) / 255.0
            )
        };
        let content_width = content_width.clamp(1.0, width.max(1.0));
        let svg = make_svg(width);
        // Crop the SVG viewport itself. UV cropping a narrow raster would
        // downsample then stretch the glyphs, even with a fixed widget size.
        let compact_svg = make_svg(content_width);
        // Content-addressed image URI prevents stale textures across books/themes.
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        svg.hash(&mut hash);
        let result = Arc::new(FootnoteLayout {
            width,
            content_width,
            first_baseline: first_baseline.unwrap_or(size),
            height,
            fallback: None,
            svg: Arc::from(svg.into_bytes()),
            uri: format!("bytes://footnote-{:x}.svg", hash.finish()),
            compact_svg: Arc::from(compact_svg.into_bytes()),
            compact_uri: format!("bytes://footnote-{:x}-compact.svg", hash.finish()),
        });
        if self.cache.len() >= 16 {
            self.cache.remove(0);
        }
        self.cache.push((text.into(), style, width, result.clone()));
        Ok(result)
    }
}

// Discretionary display-only breaks keep long URL/query components within a
// narrow popup. Preserve the source verbatim (including authored spaces).
fn url_line_breaks(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for token in text.split_inclusive(char::is_whitespace) {
        let url_like = token.contains("://") || (token.contains('&') && token.contains('='));
        for c in token.chars() {
            result.push(c);
            if url_like && matches!(c, '/' | '?' | '&' | '=' | '#' | '_' | '-') {
                result.push('\u{200b}');
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{
        Book, Metadata, PublicationError, PublicationId, PublicationUrl, Resource, Section,
    };
    struct Source(Book);
    impl BookSource for Source {
        fn book(&self) -> &Book {
            &self.0
        }
        fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
            unreachable!()
        }
        fn resource(&self, url: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(url.to_string()))
        }
    }
    #[test]
    fn citation_marker_is_cropped_without_shrinking_and_shares_first_baseline() {
        let source = Source(Book {
            id: PublicationId::new("citation-popup-test").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let mut renderer = FootnoteRenderer::default();
        let ctx = egui::Context::default();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        crate::ui::configure(
            &ctx,
            &crate::preferences::InterfaceTypography::default(),
            crate::preferences::AppLanguage::English,
            runtime.handle(),
        );
        for number in [1, 12] {
            let marker = renderer
                .layout(
                    &source,
                    &format!("[{number}]"),
                    &ReaderStyle::default(),
                    14.0,
                    egui::Color32::GRAY,
                    56.0,
                )
                .unwrap();
            let body = renderer
                .layout(
                    &source,
                    "Churchland & Sejnowsky, 1992; Eliasmith, 2013",
                    &ReaderStyle::default(),
                    14.0,
                    egui::Color32::BLACK,
                    220.0,
                )
                .unwrap();
            assert!(marker.content_width < marker.width * 0.7);
            assert!(body.height > marker.height);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                body.paint_with_marker(ui, &marker, 4.0);
            });
            output.textures_delta.clear();
            let image_rects: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.brush.is_some() => {
                        assert_eq!(rect.brush.as_ref().unwrap().uv.max, egui::pos2(1.0, 1.0));
                        Some(rect.rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(image_rects.len(), 2, "both SVG text images must render");
            assert!(
                (image_rects[0].height() - marker.height).abs() < 1.0,
                "cropping must preserve glyph height"
            );
            assert!((image_rects[0].width() - marker.content_width).abs() < 1.0);
            assert!(
                (image_rects[0].top() + marker.first_baseline
                    - image_rects[1].top()
                    - body.first_baseline)
                    .abs()
                    < 1.0
            );
            assert!((image_rects[1].left() - image_rects[0].right() - 4.0).abs() < 1.0);
            output.textures_delta.clear();
        }
    }

    #[test]
    fn footnote_formula_text_is_rendered_as_math() {
        let source = Source(Book {
            id: PublicationId::new("formula-note").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let mut renderer = FootnoteRenderer::default();
        let text = r"The variance is \(\sigma=\sqrt{k\theta^2}\).";
        assert!(
            popup_inlines(text, TextStyle::default())
                .iter()
                .any(|i| matches!(i, Inline::Math(_)))
        );
        let layout = renderer
            .layout(
                &source,
                text,
                &ReaderStyle::default(),
                14.0,
                egui::Color32::BLACK,
                260.0,
            )
            .unwrap();
        let svg = std::str::from_utf8(&layout.svg).unwrap();
        assert!(svg.contains("data:image/png;base64,"));
        assert!(
            resvg::usvg::Tree::from_data(&layout.svg, &resvg::usvg::Options::default()).is_ok()
        );
    }

    #[test]
    fn language_game_footnote_uses_reader_glyphs_at_narrow_widths() {
        let source = Source(Book {
            id: PublicationId::new("footnote-test").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: Vec::new(),
            table_of_contents: Vec::new(),
        });
        let text = "C. Darwin, The Autobiography of Charles Darwin 1809-1882. With the Original Omissions Restored. Edited and with Appendix and Notes by His Grand-Daughter Nora Barlow(London:Collins,1958),http://darwin-online.org.uk/content/frameset?pa geseq=1&itemID=F1497&viewtype=text（2004年由John van Wyhe扫描；2005年12月由AEL Data进行光学字符识别并由Sue Asscher校正）。请参阅第120页。";
        let mut renderer = FootnoteRenderer::default();
        for width in [200.0, 260.0, 320.0] {
            let layout = renderer
                .layout(
                    &source,
                    text,
                    &ReaderStyle::default(),
                    14.0,
                    egui::Color32::BLACK,
                    width,
                )
                .unwrap();
            assert!(layout.fallback.is_none());
            assert!(layout.height > 50.0 && layout.height < 1600.0);
            let tree = resvg::usvg::Tree::from_data(&layout.svg, &resvg::usvg::Options::default())
                .unwrap();
            let mut pixmap =
                resvg::tiny_skia::Pixmap::new(width as u32, layout.height.ceil() as u32).unwrap();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            if width == 260.0
                && let Ok(path) = std::env::var("TORTO_FOOTNOTE_TEST_PNG")
            {
                let mut preview =
                    resvg::tiny_skia::Pixmap::new(pixmap.width(), pixmap.height()).unwrap();
                preview.fill(resvg::tiny_skia::Color::WHITE);
                preview.draw_pixmap(
                    0,
                    0,
                    pixmap.as_ref(),
                    &resvg::tiny_skia::PixmapPaint::default(),
                    resvg::tiny_skia::Transform::identity(),
                    None,
                );
                preview.save_png(path).unwrap();
            }
            assert!(pixmap.data().chunks_exact(4).filter(|p| p[3] > 0).count() > 1000);
            let cached = renderer
                .layout(
                    &source,
                    text,
                    &ReaderStyle::default(),
                    14.0,
                    egui::Color32::BLACK,
                    width,
                )
                .unwrap();
            assert!(Arc::ptr_eq(&layout, &cached));
        }
    }
}
