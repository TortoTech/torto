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
    #[cfg(test)]
    line_starts: Vec<f32>,
    #[cfg(test)]
    font_sizes: Vec<f32>,
    #[cfg(test)]
    text_bounds: Vec<egui::Rect>,
    pub height: f32,
    pub width: f32,
    pub content_width: f32,
    pub wrapped: bool,
    svg: Arc<[u8]>,
    uri: String,
    fallback: Option<Arc<egui::Galley>>,
    websites: Vec<(egui::Rect, String)>,
}

impl FootnoteLayout {
    pub fn fallback_marked(
        ctx: &egui::Context,
        text: &str,
        marker: &str,
        font: &egui::FontId,
        color: egui::Color32,
        marker_color: egui::Color32,
        width: f32,
        marker_slot: f32,
    ) -> Arc<Self> {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = width;
        let marker_width = ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(marker.to_owned(), font.clone(), marker_color)
                .size()
                .x
        });
        job.append(
            marker,
            (marker_slot - marker_width).max(0.0) * 0.5,
            egui::TextFormat {
                font_id: font.clone(),
                color: marker_color,
                ..Default::default()
            },
        );
        job.append(
            text,
            font.size * 0.3 + (marker_slot - marker_width).max(0.0) * 0.5,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
        Self::fallback_job(ctx, job, font.size, width)
    }

    fn fallback_job(
        ctx: &egui::Context,
        job: egui::text::LayoutJob,
        _size: f32,
        width: f32,
    ) -> Arc<Self> {
        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(job));
        Arc::new(Self {
            #[cfg(test)]
            line_starts: galley.rows.iter().map(|row| row.pos.x).collect(),
            #[cfg(test)]
            font_sizes: vec![_size],
            #[cfg(test)]
            text_bounds: Vec::new(),
            height: galley.size().y,
            width,
            content_width: galley.size().x,
            wrapped: galley.rows.len() > galley.job.text.lines().count().max(1),
            svg: Arc::from([]),
            uri: String::new(),
            fallback: Some(galley),
            websites: Vec::new(),
        })
    }
    pub fn paint(&self, ui: &mut egui::Ui) {
        if let Some(galley) = &self.fallback {
            ui.label(galley.clone());
            return;
        }
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(self.width, self.height), egui::Sense::hover());
        // paint_at snaps the destination to physical pixels and rasterizes at
        // that exact size. Fractional Image widget positions otherwise filter
        // the text texture again, making some rows appear lighter than others.
        egui::Image::from_bytes(self.uri.clone(), self.svg.clone())
            .maintain_aspect_ratio(false)
            .paint_at(ui, rect);
        for (index, (bounds, url)) in self.websites.iter().enumerate() {
            let response = ui
                .interact(
                    bounds.translate(rect.min.to_vec2()),
                    ui.id().with(("website", index)),
                    egui::Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            website_tooltip(&response, response.rect, url);
            if response.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
    }
}

pub(super) fn website_tooltip(response: &egui::Response, anchor: egui::Rect, url: &str) {
    let mut tooltip = egui::Tooltip::for_enabled(response);
    tooltip.popup = tooltip
        .popup
        .anchor(anchor)
        .align(egui::emath::RectAlign::TOP)
        .align_alternatives(&[egui::emath::RectAlign::TOP])
        .gap(6.0);
    tooltip.show(|ui| {
        ui.set_max_width(
            420.0_f32
                .min(ui.ctx().content_rect().width() - 24.0)
                .max(1.0),
        );
        ui.add(egui::Label::new(url).wrap());
    });
}

#[derive(Default)]
pub(super) struct FootnoteRenderer {
    engine: Option<LayoutEngine>,
    cache: Vec<(
        String,
        ReaderStyle,
        f32,
        f32,
        Option<(usize, egui::Color32, f32)>,
        Arc<FootnoteLayout>,
    )>,
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
    #[cfg(test)]
    pub fn layout(
        &mut self,
        source: &dyn BookSource,
        text: &str,
        reader_style: &ReaderStyle,
        size: f32,
        color: egui::Color32,
        width: f32,
    ) -> Result<Arc<FootnoteLayout>, String> {
        self.layout_indented(source, text, reader_style, size, color, width, 0.0)
    }

    #[cfg(test)]
    pub fn layout_indented(
        &mut self,
        source: &dyn BookSource,
        text: &str,
        reader_style: &ReaderStyle,
        size: f32,
        color: egui::Color32,
        width: f32,
        first_indent: f32,
    ) -> Result<Arc<FootnoteLayout>, String> {
        self.layout_marked_internal(
            source,
            text,
            reader_style,
            size,
            color,
            width,
            first_indent,
            None,
        )
    }

    #[cfg(test)]
    pub fn layout_marked(
        &mut self,
        source: &dyn BookSource,
        text: &str,
        marker: &str,
        reader_style: &ReaderStyle,
        size: f32,
        color: egui::Color32,
        marker_color: egui::Color32,
        width: f32,
    ) -> Result<Arc<FootnoteLayout>, String> {
        let joined = format!("{marker} {text}");
        self.layout_marked_internal(
            source,
            &joined,
            reader_style,
            size,
            color,
            width,
            0.0,
            Some((marker.len(), marker_color, 0.0)),
        )
    }

    pub fn marker_width(
        &mut self,
        source: &dyn BookSource,
        marker: &str,
        style: &ReaderStyle,
        size: f32,
    ) -> Result<f32, String> {
        let layout = self.layout_marked_internal(
            source,
            marker,
            style,
            size,
            egui::Color32::BLACK,
            480.0,
            0.0,
            None,
        )?;
        Ok((layout.content_width - 4.0).max(0.0))
    }

    pub fn layout_aligned_marked(
        &mut self,
        source: &dyn BookSource,
        text: &str,
        marker: &str,
        reader_style: &ReaderStyle,
        size: f32,
        color: egui::Color32,
        marker_color: egui::Color32,
        width: f32,
        slot: f32,
    ) -> Result<Arc<FootnoteLayout>, String> {
        let marker_width = self.marker_width(source, marker, reader_style, size)?;
        let prefix = format!("{marker}\u{00a0}");
        let prefix_width = self.marker_width(source, &prefix, reader_style, size)?;
        // Body starts at the shared slot edge; center the marker within that
        // slot independently so bare numbers align with bracketed numbers.
        let shift =
            size * 0.3 - (prefix_width - marker_width) + (slot - marker_width).max(0.0) * 0.5;
        self.layout_marked_internal(
            source,
            &format!("{prefix}{text}"),
            reader_style,
            size,
            color,
            width,
            slot.max(marker_width) + size * 0.3 - prefix_width,
            Some((marker.len(), marker_color, shift)),
        )
    }

    fn layout_marked_internal(
        &mut self,
        source: &dyn BookSource,
        text: &str,
        reader_style: &ReaderStyle,
        size: f32,
        color: egui::Color32,
        width: f32,
        first_indent: f32,
        marker: Option<(usize, egui::Color32, f32)>,
    ) -> Result<Arc<FootnoteLayout>, String> {
        // The native reader has an 80px minimum text column. Use the popup's
        // egui fallback below that width instead of drawing outside its bounds.
        if width < 84.0 {
            return Err("popup column is narrower than the reader minimum".into());
        }
        let mut style = reader_style.clone();
        style.typography.font_size = size;
        style.typography.minimum_font_size = size;
        // Leave room for glyph side bearings and antialiasing at the SVG edge.
        style.horizontal_margin = 2.0;
        style.top_margin = 0.0;
        style.bottom_margin = 0.0;
        style.spread = SpreadMode::Single;
        style.website_icons = reader_style.typesetting.mode == TypesettingMode::Unified;
        style.typesetting.mode = TypesettingMode::Book;
        style.typesetting.line_break_strategy = rebook_layout::LineBreakStrategy::Optimized;
        style.foreground = rebook_publication::Rgba {
            red: color.r(),
            green: color.g(),
            blue: color.b(),
            alpha: color.a(),
        };
        if let Some((_, _, _, _, _, layout)) = self.cache.iter().find(|(t, s, w, indent, m, _)| {
            t == text && s == &style && *w == width && *indent == first_indent && *m == marker
        }) {
            return Ok(layout.clone());
        }
        let english = text.chars().filter(|c| c.is_ascii_alphabetic()).count()
            > text
                .chars()
                .filter(|c| !c.is_ascii() && c.is_alphabetic())
                .count();
        let display_text = if style.website_icons {
            text.to_owned()
        } else {
            url_line_breaks(text)
        };
        let mut blocks: Vec<_> = display_text
            .lines()
            .enumerate()
            .map(|(index, line)| {
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
                        indent: if index == 0 { first_indent } else { 0.0 },
                        align: TextAlignment::Justify,
                        margin_after: 0.0,
                        line_height: 1.45,
                        ..Default::default()
                    },
                    source: None,
                })
            })
            .collect();
        if let Some((length, marker_color, _)) = marker
            && let Some(Block::Text(block)) = blocks.first_mut()
            && let Some(Inline::Text(first)) = block.content.first_mut()
        {
            let mut prefix = first.clone();
            prefix.text = first.text[..length].to_owned();
            prefix.style.color = rebook_publication::Rgba {
                red: marker_color.r(),
                green: marker_color.g(),
                blue: marker_color.b(),
                alpha: marker_color.a(),
            };
            first.text = first.text[length..].to_owned();
            block.content.insert(0, Inline::Text(prefix));
        }
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
        let mut websites = Vec::new();
        let mut content_width = 0.0_f32;
        let mut wrapped = false;
        let mut height = size * 1.45;
        let mut page_y = 0.0;
        #[cfg(test)]
        let mut line_starts = Vec::new();
        #[cfg(test)]
        let mut font_sizes = Vec::new();
        #[cfg(test)]
        let mut text_bounds = Vec::new();
        for page in &layout.pages {
            let mut bottom = 0.0_f32;
            for item in &page.items {
                let PageItem::Text(text) = item else {
                    continue;
                };
                let mut painted_websites = std::collections::HashSet::new();
                wrapped |= text.layout.len() > 1;
                for line in text
                    .layout
                    .lines()
                    .skip(text.lines.start)
                    .take(text.lines.len())
                {
                    #[cfg(test)]
                    if let Some(run) = line.items().find_map(|item| match item {
                        PositionedLayoutItem::GlyphRun(run) if run.advance() > 0.0 => Some(run),
                        _ => None,
                    }) {
                        line_starts.push(text.origin_x + run.offset());
                    }
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
                                    content_width = content_width.max(x + image.width + 2.0);
                                    let y = page_y + text.origin_y + inline_box.y + image.offset_y;
                                    bottom = bottom.max(y - page_y + image.height);
                                    paths.push_str(&format!(r#"<image x="{x}" y="{y}" width="{}" height="{}" href="data:image/png;base64,{data}"/>"#,image.width,image.height));
                                }
                            }
                            continue;
                        }
                        let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                            continue;
                        };
                        content_width = content_width
                            .max(text.origin_x + glyph_run.offset() + glyph_run.advance() + 2.0);
                        let run = glyph_run.run();
                        if let Some(url) = text
                            .citations
                            .iter()
                            .find(|c| c.number == glyph_run.style().brush.footnote_reference_group)
                            .and_then(|c| c.website.as_ref())
                        {
                            if !painted_websites
                                .insert(glyph_run.style().brush.footnote_reference_group)
                            {
                                continue;
                            }
                            let diameter = (run.font_size() * 0.78).clamp(8.0, 12.0);
                            let radius = diameter / 2.0;
                            let x = text.origin_x + glyph_run.offset() + glyph_run.advance() / 2.0;
                            let metrics = run.metrics();
                            let y = page_y + text.origin_y + glyph_run.baseline()
                                - (metrics.ascent - metrics.descent) * 0.5;
                            // Website links keep their link color independently
                            // of the active footnote number.
                            let icon_color = crate::ui::footnote_link_color();
                            paths.push_str(&format!(r##"<g fill="none" stroke="#{:02x}{:02x}{:02x}" stroke-width="1"><circle cx="{x}" cy="{y}" r="{radius}"/><ellipse cx="{x}" cy="{y}" rx="{}" ry="{radius}"/><path d="M{} {y}H{}"/></g>"##, icon_color.r(), icon_color.g(), icon_color.b(), radius * 0.45, x-radius, x+radius));
                            websites.push((
                                egui::Rect::from_center_size(
                                    egui::pos2(x, y),
                                    egui::vec2(diameter, diameter),
                                ),
                                url.clone(),
                            ));
                            continue;
                        }
                        #[cfg(test)]
                        font_sizes.push(run.font_size());
                        #[cfg(test)]
                        if glyph_run.advance() > 0.0 {
                            let baseline = page_y + text.origin_y + glyph_run.baseline();
                            text_bounds.push(egui::Rect::from_min_max(
                                egui::pos2(
                                    text.origin_x + glyph_run.offset(),
                                    baseline - run.metrics().ascent,
                                ),
                                egui::pos2(
                                    text.origin_x + glyph_run.offset() + glyph_run.advance(),
                                    baseline + run.metrics().descent,
                                ),
                            ));
                        }
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
                                "<path fill=\"#{:02x}{:02x}{:02x}\" transform=\"translate({} {}) scale(1 -1)\" d=\"{}\"/>",
                                glyph_run.style().brush.color.red, glyph_run.style().brush.color.green, glyph_run.style().brush.color.blue,
                                text.origin_x + glyph.x - marker.filter(|(_, c, _)| glyph_run.style().brush.color == rebook_publication::Rgba { red: c.r(), green: c.g(), blue: c.b(), alpha: c.a() }).map_or(0.0, |(_, _, shift)| shift),
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
        let svg = make_svg(width);
        // Content-addressed image URI prevents stale textures across books/themes.
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        svg.hash(&mut hash);
        let result = Arc::new(FootnoteLayout {
            #[cfg(test)]
            line_starts,
            #[cfg(test)]
            font_sizes,
            #[cfg(test)]
            text_bounds,
            width,
            content_width,
            wrapped,
            height,
            fallback: None,
            websites,
            svg: Arc::from(svg.into_bytes()),
            uri: format!("bytes://footnote-{:x}.svg", hash.finish()),
        });
        if self.cache.len() >= 64 {
            self.cache.remove(0);
        }
        self.cache.push((
            text.into(),
            style,
            width,
            first_indent,
            marker,
            result.clone(),
        ));
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
    #[test]
    fn aligned_markers_share_first_body_edge_and_full_width_continuations() {
        let source = Source(Book {
            id: PublicationId::new("aligned-popup").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let style = ReaderStyle::default();
        let mut renderer = FootnoteRenderer::default();
        let markers = ["1", "12", "[1]", "[12]"];
        let slot = markers
            .iter()
            .map(|marker| {
                renderer
                    .marker_width(&source, marker, &style, 14.0)
                    .unwrap()
            })
            .fold(0.0_f32, f32::max);
        let mut edges = Vec::new();
        let mut digit_edges = Vec::new();
        for marker in markers {
            let layout = renderer.layout_aligned_marked(&source, "According to the author this explanatory note continues across several lines of text.", marker, &style, 14.0, egui::Color32::BLACK, egui::Color32::BLUE, 180.0, slot).unwrap();
            let svg = std::str::from_utf8(&layout.svg).unwrap();
            let digit = svg
                .split("<path ")
                .filter(|path| path.starts_with("fill=\"#0000ff\"") && !path.contains("d=\"\""))
                .nth(usize::from(marker.starts_with('[')))
                .unwrap();
            digit_edges.push(
                digit
                    .split("translate(")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse::<f32>()
                    .unwrap(),
            );
            let body = svg
                .split("<path ")
                .find(|path| path.starts_with("fill=\"#000000\"") && !path.contains("d=\"\""))
                .unwrap();
            let x: f32 = body
                .split("translate(")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            edges.push(x);
            assert!(layout.line_starts.len() > 1);
            assert!(
                layout.line_starts[1..3]
                    .iter()
                    .all(|x| (*x - 2.0).abs() < 0.1),
                "{marker}: {:?}",
                layout.line_starts
            );
        }
        assert!(
            edges.iter().all(|x| (*x - edges[0]).abs() < 0.1),
            "{edges:?}"
        );
        assert!(
            (digit_edges[0] - digit_edges[2]).abs() < 0.2,
            "{digit_edges:?}"
        );
        assert!(
            (digit_edges[1] - digit_edges[3]).abs() < 0.2,
            "{digit_edges:?}"
        );
    }
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
    fn popup_marker_shares_body_size_and_continuation_width() {
        let source = Source(Book {
            id: PublicationId::new("popup-marker-test").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let mut renderer = FootnoteRenderer::default();
        for marker in ["1", "12", "[1]", "[12]"] {
            let layout = renderer
                .layout_marked(
                    &source,
                    "Churchland & Sejnowsky, 1992; Eliasmith, 2013",
                    marker,
                    &ReaderStyle::default(),
                    14.0,
                    egui::Color32::BLACK,
                    egui::Color32::from_rgb(30, 80, 210),
                    180.0,
                )
                .unwrap();
            assert!(layout.font_sizes.len() > 1);
            assert!(
                layout
                    .font_sizes
                    .iter()
                    .all(|size| (*size - 14.0).abs() < 0.001)
            );
            assert!(layout.line_starts.len() > 1);
            assert!((layout.line_starts[0] - layout.line_starts[1]).abs() < 0.1);
            let svg = std::str::from_utf8(&layout.svg).unwrap();
            assert!(svg.contains("fill=\"#1e50d2\""));
            assert!(svg.contains("fill=\"#000000\""));
        }
    }

    #[test]
    fn popup_websites_are_compact_clickable_icons_only_in_unified_mode() {
        let source = Source(Book {
            id: PublicationId::new("popup-websites").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let mut renderer = FootnoteRenderer::default();
        let mut style = ReaderStyle::default();
        let text = "网址 https://example.com/long/path?q=1，以及 example.org。";
        style.typesetting.mode = TypesettingMode::Unified;
        let unified = renderer
            .layout_marked(
                &source,
                text,
                "1",
                &style,
                14.0,
                egui::Color32::BLACK,
                egui::Color32::from_rgb(250, 204, 21),
                460.0,
            )
            .unwrap();
        assert_eq!(unified.websites.len(), 2);
        assert_eq!(unified.websites[0].1, "https://example.com/long/path?q=1");
        assert_eq!(unified.websites[1].1, "https://example.org/");
        let svg = std::str::from_utf8(&unified.svg).unwrap();
        let link_color = crate::ui::footnote_link_color();
        assert!(svg.contains(&format!(
            "stroke=\"#{:02x}{:02x}{:02x}\"",
            link_color.r(),
            link_color.g(),
            link_color.b(),
        )));
        assert!(
            !svg.contains("stroke=\"#facc15\""),
            "website icons must not inherit active yellow"
        );
        assert!(
            svg.contains("fill=\"#facc15\""),
            "the active note number stays yellow"
        );
        assert!(unified.websites.iter().all(|(rect, _)| rect.min.x >= 0.0
            && rect.max.x <= unified.width
            && rect.max.y <= unified.height));
        style.typesetting.mode = TypesettingMode::Book;
        let original = renderer
            .layout_marked(
                &source,
                text,
                "1",
                &style,
                14.0,
                egui::Color32::BLACK,
                egui::Color32::from_rgb(30, 80, 210),
                460.0,
            )
            .unwrap();
        assert!(original.websites.is_empty());
        assert!(unified.content_width < original.content_width);
    }

    #[test]
    fn popup_globe_reserves_space_before_punctuation_and_centers_with_body_text() {
        let source = Source(Book {
            id: PublicationId::new("popup-globe-spacing").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![],
            table_of_contents: vec![],
        });
        let mut renderer = FootnoteRenderer::default();
        let mut style = ReaderStyle::default();
        style.typesetting.mode = TypesettingMode::Unified;
        for size in [10.0, 14.0, 20.0] {
            for width in [180.0, 460.0] {
                for text in [
                    "Museum of Printing https://example.com/long/path?q=1, email from Bruce Rosenblum to the author, March 26, 2017.",
                    "参见 https://example.com/long/path?q=1，作者邮件说明。",
                ] {
                    let layout = renderer
                        .layout_marked(
                            &source,
                            text,
                            "2",
                            &style,
                            size,
                            egui::Color32::BLACK,
                            egui::Color32::BLUE,
                            width,
                        )
                        .unwrap();
                    assert_eq!(layout.websites.len(), 1);
                    let icon = layout.websites[0].0;
                    let same_line = layout
                        .text_bounds
                        .iter()
                        .filter(|bounds| {
                            bounds.min.y < icon.center().y && bounds.max.y > icon.center().y
                        })
                        .collect::<Vec<_>>();
                    assert!(!same_line.is_empty());
                    for bounds in same_line {
                        assert!(
                            bounds.max.x <= icon.min.x + 0.1 || bounds.min.x >= icon.max.x - 0.1,
                            "icon={icon:?}, text={bounds:?}, size={size}, width={width}"
                        );
                        // The marker font can differ; compare nearby body text.
                        if (bounds.min.x - icon.max.x).abs() < size * 2.0 {
                            assert!(
                                (bounds.center().y - icon.center().y).abs() < size * 0.15,
                                "icon and following body should share a vertical center"
                            );
                        }
                    }
                }
            }
        }
        let multiline = renderer
            .layout_marked(
                &source,
                "First https://example.com/.\nSecond https://example.org/.",
                "2",
                &style,
                14.0,
                egui::Color32::BLACK,
                egui::Color32::BLUE,
                460.0,
            )
            .unwrap();
        assert_eq!(
            multiline.websites.len(),
            2,
            "website IDs are paragraph-local"
        );
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
