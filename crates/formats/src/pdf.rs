mod catalog;

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use hayro::hayro_interpret::font::{FontData, FontQuery, Glyph};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt as _;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache,
    InterpreterSettings, Paint, PathDrawMode, SoftMask, interpret_page,
};
use hayro::hayro_syntax::{Pdf, PdfData};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};
use kurbo::{Affine, BezPath, Point, Rect, Shape};
use rebook_publication::{
    Block, Book, BookSource, FixedPageDimensions, FixedPageTextLayer, FixedPageTextRect,
    FixedPageTextSpan, Metadata, PublicationError, PublicationUrl, RasterResource, RenditionLayout,
    Resource, Section, SourceAnchor, SourceRange, TableOfContentsOrigin,
};
use sha2::{Digest, Sha256};

use crate::source::{DirectBookSource, SectionContent, SourceBook, SourceSection};
use crate::{BookFormat, FormatError, conversion_error};

const COVER_PATH: &str = "Cover/thumbnail.png";
const PAGE_PATH_PREFIX: &str = "Pages/page-";
const PAGE_CACHE_CAPACITY: usize = 6;
const PAGE_MAX_DIMENSION: f32 = 2_048.0;
const COVER_MAX_DIMENSION: f32 = 384.0;
const MAX_RENDER_SCALE: f32 = 2.0;
static CJK_FALLBACK_FONT: &[u8] = include_bytes!("../../../assets/fonts/LXGWWenKaiGBScreen.ttf");

pub fn cjk_fallback_font_bytes() -> &'static [u8] {
    CJK_FALLBACK_FONT
}

pub(crate) struct PdfPublication {
    descriptor: DirectBookSource,
    pdf: Arc<Pdf>,
    page_count: usize,
    cache: Mutex<PdfResourceCache>,
}

#[derive(Default)]
struct PdfResourceCache {
    cover: Option<Arc<[u8]>>,
    pages: HashMap<usize, Arc<[u8]>>,
    page_lru: VecDeque<usize>,
    rasters: HashMap<usize, RasterResource>,
    raster_lru: VecDeque<usize>,
    text_layers: HashMap<usize, FixedPageTextLayer>,
}

pub(crate) fn open(bytes: Vec<u8>, file_name: &str) -> Result<PdfPublication, FormatError> {
    let digest = format!("{:x}", Sha256::digest(&bytes));
    open_data(PdfData::from(bytes), digest, file_name)
}

pub(crate) fn open_with_id(
    bytes: Vec<u8>,
    file_name: &str,
    publication_id: &str,
) -> Result<PdfPublication, FormatError> {
    open_data(PdfData::from(bytes), publication_id.to_owned(), file_name)
}

pub(crate) fn open_shared(
    bytes: Arc<[u8]>,
    file_name: &str,
) -> Result<PdfPublication, FormatError> {
    let digest = format!("{:x}", Sha256::digest(bytes.as_ref()));
    open_data(
        PdfData::from(Arc::new(SharedPdfBytes(bytes))),
        digest,
        file_name,
    )
}

struct SharedPdfBytes(Arc<[u8]>);

impl AsRef<[u8]> for SharedPdfBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

fn open_data(
    bytes: PdfData,
    digest: String,
    file_name: &str,
) -> Result<PdfPublication, FormatError> {
    let pdf = Arc::new(
        Pdf::new(bytes)
            .map_err(|error| conversion_error(BookFormat::Pdf, format_args!("{error:?}")))?,
    );
    let page_count = pdf.pages().len();
    if page_count == 0 {
        return Err(conversion_error(
            BookFormat::Pdf,
            "PDF does not contain any pages",
        ));
    }

    let catalog = catalog::read(&pdf);
    let title = catalog
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| title_from_file_name(file_name));
    let authors = catalog
        .author
        .filter(|author| !author.trim().is_empty())
        .into_iter()
        .collect();
    let table_of_contents = catalog.table_of_contents;
    let sections = (0..page_count)
        .map(|index| SourceSection {
            title: format!("Page {}", index + 1),
            content: SectionContent::Image {
                resource_path: page_path(index),
                alt: format!("PDF page {}", index + 1),
            },
            linear: true,
        })
        .collect();
    let descriptor = DirectBookSource::open(
        SourceBook {
            id: digest,
            metadata: Metadata {
                title,
                authors,
                languages: Vec::new(),
                layout: RenditionLayout::PrePaginated,
            },
            sections,
            table_of_contents,
            resources: Vec::new(),
            cover_path: Some(COVER_PATH.into()),
        },
        BookFormat::Pdf,
    )?;

    Ok(PdfPublication {
        descriptor,
        pdf,
        page_count,
        cache: Mutex::new(PdfResourceCache::default()),
    })
}

impl BookSource for PdfPublication {
    fn book(&self) -> &Book {
        self.descriptor.book()
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.descriptor.table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let mut section = self.descriptor.parse_section(index)?;
        let text_layer = self.page_text_layer(index)?;
        let Some(Block::Image(image)) = section.blocks.first_mut() else {
            return Err(PublicationError::InvalidPublication(format!(
                "PDF page {} did not produce a fixed page image",
                index + 1
            )));
        };
        let source = page_text_source(&section.id, text_layer.text.chars().count());
        image.source = Some(source);
        image.text_layer = Some(text_layer);
        Ok(section)
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        let path = href.resource_url();
        let bytes = if path.path() == COVER_PATH {
            self.cover_resource()?
        } else {
            let page_index = page_index_from_path(path.path())
                .filter(|index| *index < self.page_count)
                .ok_or_else(|| PublicationError::ResourceNotFound(href.to_string()))?;
            self.page_resource(page_index)?
        };
        Ok(Resource {
            href: path,
            media_type: "image/png".into(),
            bytes,
        })
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        let path = href.resource_url();
        if path.path() == COVER_PATH {
            return Ok(None);
        }
        let page_index = page_index_from_path(path.path())
            .filter(|index| *index < self.page_count)
            .ok_or_else(|| PublicationError::ResourceNotFound(href.to_string()))?;
        self.page_raster(page_index).map(Some)
    }

    fn fixed_page_dimensions(
        &self,
        section_index: usize,
    ) -> Result<Option<FixedPageDimensions>, PublicationError> {
        self.page_raster_dimensions(section_index).map(Some)
    }
}

impl PdfPublication {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "hayro uses the same bounded f32-to-u16 conversion when allocating the pixmap"
    )]
    fn page_raster_dimensions(
        &self,
        page_index: usize,
    ) -> Result<FixedPageDimensions, PublicationError> {
        let page = self.pdf.pages().get(page_index).ok_or_else(|| {
            PublicationError::ResourceNotFound(format!("PDF page {}", page_index + 1))
        })?;
        let (width, height) = page.render_dimensions();
        let scale = (PAGE_MAX_DIMENSION / width.max(height).max(1.0)).min(MAX_RENDER_SCALE);
        Ok(FixedPageDimensions {
            width: u32::from((width * scale).floor() as u16),
            height: u32::from((height * scale).floor() as u16),
        })
    }

    fn page_text_layer(&self, page_index: usize) -> Result<FixedPageTextLayer, PublicationError> {
        if let Some(layer) = self.lock_cache()?.text_layers.get(&page_index).cloned() {
            return Ok(layer);
        }

        let layer = self.extract_page_text(page_index)?;
        self.lock_cache()?
            .text_layers
            .insert(page_index, layer.clone());
        Ok(layer)
    }

    fn extract_page_text(&self, page_index: usize) -> Result<FixedPageTextLayer, PublicationError> {
        let page = self.pdf.pages().get(page_index).ok_or_else(|| {
            PublicationError::ResourceNotFound(format!("PDF page {}", page_index + 1))
        })?;
        let (width, height) = page.render_dimensions();
        let cache = InterpreterCache::new();
        let mut context = Context::new(
            page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            &cache,
            page.xref(),
            interpreter_settings(),
        );
        let mut extractor = PdfTextExtractor::default();
        interpret_page(page, &mut context, &mut extractor);
        Ok(extractor.finish(width, height))
    }

    fn cover_resource(&self) -> Result<Arc<[u8]>, PublicationError> {
        let cached = { self.lock_cache()?.cover.clone() };
        if let Some(cover) = cached {
            return Ok(cover);
        }
        let cover: Arc<[u8]> = self.render_page_png(0, COVER_MAX_DIMENSION)?.into();
        let mut cache = self.lock_cache()?;
        if let Some(cached) = &cache.cover {
            return Ok(Arc::clone(cached));
        }
        cache.cover = Some(Arc::clone(&cover));
        Ok(cover)
    }

    fn page_resource(&self, page_index: usize) -> Result<Arc<[u8]>, PublicationError> {
        let cached = { self.lock_cache()?.pages.get(&page_index).cloned() };
        if let Some(page) = cached {
            let mut cache = self.lock_cache()?;
            touch_page(&mut cache.page_lru, page_index);
            return Ok(page);
        }

        let page: Arc<[u8]> = self.render_page_png(page_index, PAGE_MAX_DIMENSION)?.into();
        let mut cache = self.lock_cache()?;
        if let Some(cached) = cache.pages.get(&page_index).cloned() {
            touch_page(&mut cache.page_lru, page_index);
            return Ok(cached);
        }
        cache.pages.insert(page_index, Arc::clone(&page));
        touch_page(&mut cache.page_lru, page_index);
        while cache.pages.len() > PAGE_CACHE_CAPACITY {
            let Some(evicted) = cache.page_lru.pop_front() else {
                break;
            };
            cache.pages.remove(&evicted);
        }
        Ok(page)
    }

    fn page_raster(&self, page_index: usize) -> Result<RasterResource, PublicationError> {
        let cached = { self.lock_cache()?.rasters.get(&page_index).cloned() };
        if let Some(raster) = cached {
            let mut cache = self.lock_cache()?;
            touch_page(&mut cache.raster_lru, page_index);
            return Ok(raster);
        }

        let pixmap = self.render_page_pixmap(page_index, PAGE_MAX_DIMENSION)?;
        let raster = RasterResource {
            width: u32::from(pixmap.width()),
            height: u32::from(pixmap.height()),
            pixels: pixmap.data_as_u8_slice().to_vec().into(),
        };
        let mut cache = self.lock_cache()?;
        if let Some(cached) = cache.rasters.get(&page_index).cloned() {
            touch_page(&mut cache.raster_lru, page_index);
            return Ok(cached);
        }
        cache.rasters.insert(page_index, raster.clone());
        touch_page(&mut cache.raster_lru, page_index);
        while cache.rasters.len() > PAGE_CACHE_CAPACITY {
            let Some(evicted) = cache.raster_lru.pop_front() else {
                break;
            };
            cache.rasters.remove(&evicted);
        }
        Ok(raster)
    }

    fn lock_cache(&self) -> Result<std::sync::MutexGuard<'_, PdfResourceCache>, PublicationError> {
        self.cache.lock().map_err(|_| {
            PublicationError::InvalidPublication("PDF raster cache is unavailable".into())
        })
    }

    fn render_page_pixmap(
        &self,
        page_index: usize,
        max_dimension: f32,
    ) -> Result<hayro::vello_cpu::Pixmap, PublicationError> {
        let page = self.pdf.pages().get(page_index).ok_or_else(|| {
            PublicationError::ResourceNotFound(format!("PDF page {}", page_index + 1))
        })?;
        let (width, height) = page.render_dimensions();
        let scale = (max_dimension / width.max(height).max(1.0)).min(MAX_RENDER_SCALE);
        let cache = RenderCache::new();
        let interpreter_settings = interpreter_settings();
        Ok(hayro::render(
            page,
            &cache,
            &interpreter_settings,
            &RenderSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
                ..RenderSettings::default()
            },
        ))
    }

    fn render_page_png(
        &self,
        page_index: usize,
        max_dimension: f32,
    ) -> Result<Vec<u8>, PublicationError> {
        self.render_page_pixmap(page_index, max_dimension)?
            .into_png()
            .map_err(pdf_render_error)
    }
}

fn page_text_source(spine: &rebook_publication::SpineItemId, char_count: usize) -> SourceRange {
    let end = u64::try_from(char_count).unwrap_or(u64::MAX);
    SourceRange {
        start: SourceAnchor {
            spine: spine.clone(),
            node: "pdf-page-text".into(),
            text_offset: 0,
        },
        end: SourceAnchor {
            spine: spine.clone(),
            node: "pdf-page-text".into(),
            text_offset: end,
        },
    }
}

#[derive(Default)]
struct PdfTextExtractor {
    glyphs: Vec<ExtractedGlyph>,
}

struct ExtractedGlyph {
    text: String,
    rect: Rect,
    baseline: Point,
    advance_end: Point,
}

impl Device<'_> for PdfTextExtractor {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'_>>) {}

    fn set_blend_mode(&mut self, _: BlendMode) {}

    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'_>, _: &PathDrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'_>>, _: BlendMode) {}

    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'_>,
        transform: Affine,
        glyph_transform: Affine,
        _: &Paint<'_>,
        _: &GlyphDrawMode,
    ) {
        let Some(unicode) = glyph.as_unicode() else {
            return;
        };
        let text = match unicode {
            BfString::Char(character) => character.to_string(),
            BfString::String(value) => value,
        }
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\t' | '\n' | '\r'))
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect::<String>();
        if text.is_empty() {
            return;
        }

        let combined = transform * glyph_transform;
        let baseline = combined * Point::ORIGIN;
        let advance_end = combined * Point::new(glyph_advance(glyph), 0.0);
        let fallback = fallback_glyph_rect(glyph, combined);
        let rect =
            glyph_outline_rect(glyph, combined).map_or(fallback, |outline| outline.union(fallback));
        if !rect_is_finite(rect) {
            return;
        }
        if self.glyphs.last().is_some_and(|previous| {
            previous.text == text
                && (previous.rect.x0 - rect.x0).abs() < 0.01
                && (previous.rect.y0 - rect.y0).abs() < 0.01
                && (previous.rect.x1 - rect.x1).abs() < 0.01
                && (previous.rect.y1 - rect.y1).abs() < 0.01
        }) {
            return;
        }
        self.glyphs.push(ExtractedGlyph {
            text,
            rect,
            baseline,
            advance_end,
        });
    }

    fn draw_image(&mut self, _: Image<'_, '_>, _: Affine) {}

    fn pop_clip_path(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

impl PdfTextExtractor {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "PDF page coordinates are already represented as bounded f32 dimensions"
    )]
    fn finish(self, width: f32, height: f32) -> FixedPageTextLayer {
        let mut text = String::new();
        let mut char_len = 0_u64;
        let mut spans = Vec::new();
        let mut previous: Option<&ExtractedGlyph> = None;

        for glyph in &self.glyphs {
            if let Some(previous) = previous {
                if glyph_starts_new_line(previous, glyph) {
                    if !text.ends_with('\n') {
                        text.push('\n');
                        char_len = char_len.saturating_add(1);
                    }
                } else if glyph_needs_space(previous, glyph)
                    && !text.ends_with(char::is_whitespace)
                    && !glyph.text.starts_with(char::is_whitespace)
                {
                    text.push(' ');
                    char_len = char_len.saturating_add(1);
                }
            }

            let glyph_char_count = u64::try_from(glyph.text.chars().count()).unwrap_or(u64::MAX);
            let start = char_len;
            text.push_str(&glyph.text);
            char_len = char_len.saturating_add(glyph_char_count);
            append_glyph_spans(&mut spans, glyph.rect, start, glyph_char_count);
            previous = Some(glyph);
        }

        FixedPageTextLayer {
            width,
            height,
            text,
            spans,
            replacement: None,
        }
    }
}

fn glyph_outline_rect(glyph: &Glyph<'_>, transform: Affine) -> Option<Rect> {
    let Glyph::Outline(glyph) = glyph else {
        return None;
    };
    let mut outline = glyph.outline();
    if outline.elements().is_empty() {
        return None;
    }
    outline.apply_affine(transform);
    let rect = outline.bounding_box();
    (rect.width().abs() > 0.01 && rect.height().abs() > 0.01).then_some(rect)
}

fn fallback_glyph_rect(glyph: &Glyph<'_>, transform: Affine) -> Rect {
    let advance = glyph_advance(glyph).max(100.0);
    transformed_bounds(
        transform,
        [
            Point::new(0.0, -200.0),
            Point::new(advance, -200.0),
            Point::new(0.0, 800.0),
            Point::new(advance, 800.0),
        ],
    )
}

fn glyph_advance(glyph: &Glyph<'_>) -> f64 {
    match glyph {
        Glyph::Outline(glyph) => f64::from(glyph.advance_width().unwrap_or(500.0)),
        Glyph::Type3(_) => 500.0,
    }
}

fn transformed_bounds(transform: Affine, points: [Point; 4]) -> Rect {
    let points = points.map(|point| transform * point);
    let min_x = points
        .iter()
        .map(|point| point.x)
        .fold(f64::INFINITY, f64::min);
    let min_y = points
        .iter()
        .map(|point| point.y)
        .fold(f64::INFINITY, f64::min);
    let max_x = points
        .iter()
        .map(|point| point.x)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = points
        .iter()
        .map(|point| point.y)
        .fold(f64::NEG_INFINITY, f64::max);
    Rect::new(min_x, min_y, max_x, max_y)
}

fn rect_is_finite(rect: Rect) -> bool {
    [rect.x0, rect.y0, rect.x1, rect.y1]
        .into_iter()
        .all(f64::is_finite)
}

fn glyph_starts_new_line(previous: &ExtractedGlyph, current: &ExtractedGlyph) -> bool {
    let tolerance = previous
        .rect
        .height()
        .abs()
        .max(current.rect.height().abs())
        .max(1.0)
        * 0.55;
    (current.baseline.y - previous.baseline.y).abs() > tolerance
}

fn glyph_needs_space(previous: &ExtractedGlyph, current: &ExtractedGlyph) -> bool {
    let gap = current.baseline.x - previous.advance_end.x;
    let line_height = previous
        .rect
        .height()
        .abs()
        .max(current.rect.height().abs());
    gap > (line_height * 0.12).max(0.75)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "PDF page coordinates are already represented as bounded f32 dimensions"
)]
fn append_glyph_spans(spans: &mut Vec<FixedPageTextSpan>, rect: Rect, start: u64, char_count: u64) {
    if char_count == 0 {
        return;
    }
    let bounded_count = u32::try_from(char_count).unwrap_or(u32::MAX);
    let vertical = rect.height().abs() > rect.width().abs() * 1.5 && bounded_count > 1;
    for index in 0..bounded_count {
        let first = f64::from(index) / f64::from(bounded_count);
        let second = f64::from(index + 1) / f64::from(bounded_count);
        let fragment = if vertical {
            Rect::new(
                rect.x0,
                rect.y0 + rect.height() * first,
                rect.x1,
                rect.y0 + rect.height() * second,
            )
        } else {
            Rect::new(
                rect.x0 + rect.width() * first,
                rect.y0,
                rect.x0 + rect.width() * second,
                rect.y1,
            )
        };
        let source_index = u64::from(index);
        spans.push(FixedPageTextSpan {
            char_range: start.saturating_add(source_index)..start.saturating_add(source_index + 1),
            rect: FixedPageTextRect {
                x: fragment.x0 as f32,
                y: fragment.y0 as f32,
                width: fragment.width().abs() as f32,
                height: fragment.height().abs() as f32,
            },
        });
    }
}

fn interpreter_settings() -> InterpreterSettings {
    let mut settings = InterpreterSettings::default();
    let default_resolver = Arc::clone(&settings.font_resolver);
    let cjk_fallback: FontData = Arc::new(cjk_fallback_font_bytes());
    settings.font_resolver = Arc::new(move |query| match query {
        FontQuery::Fallback(fallback) if fallback.character_collection.is_some() => {
            Some((Arc::clone(&cjk_fallback), 0))
        }
        FontQuery::Fallback(_) | FontQuery::Standard(_) => default_resolver(query),
    });
    settings
}

#[cfg(test)]
mod spacing_tests {
    use super::*;

    fn extracted_glyph(x: f64, advance_end_x: f64) -> ExtractedGlyph {
        ExtractedGlyph {
            text: "x".into(),
            rect: Rect::new(x, 0.0, x + 5.0, 10.0),
            baseline: Point::new(x, 10.0),
            advance_end: Point::new(advance_end_x, 10.0),
        }
    }

    #[test]
    fn pdf_word_spacing_accepts_compressed_justified_spaces() {
        let previous = extracted_glyph(0.0, 5.0);
        let compressed_word_start = extracted_glyph(6.3, 11.3);
        let normal_letter = extracted_glyph(5.7, 10.7);

        assert!(glyph_needs_space(&previous, &compressed_word_start));
        assert!(!glyph_needs_space(&previous, &normal_letter));
    }
}

fn pdf_render_error(error: impl std::fmt::Debug) -> PublicationError {
    PublicationError::InvalidPublication(format!("PDF rendering failed: {error:?}"))
}

fn page_path(index: usize) -> String {
    format!("{PAGE_PATH_PREFIX}{:05}.png", index + 1)
}

fn page_index_from_path(path: &str) -> Option<usize> {
    path.strip_prefix(PAGE_PATH_PREFIX)?
        .strip_suffix(".png")?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)
}

fn touch_page(lru: &mut VecDeque<usize>, page_index: usize) {
    lru.retain(|cached| *cached != page_index);
    lru.push_back(page_index);
}

fn title_from_file_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("Untitled PDF")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use rebook_publication::{Block, BookSource};

    use super::*;

    #[test]
    fn opens_and_renders_a_pdf_as_lazy_fixed_pages() {
        let bytes = minimal_pdf();
        let publication = open(bytes, "fallback.pdf").unwrap();
        assert_eq!(publication.book().metadata.title, "Test PDF");
        assert_eq!(publication.book().metadata.authors, ["Rebook"]);
        assert_eq!(
            publication.book().metadata.layout,
            RenditionLayout::PrePaginated
        );
        assert_eq!(publication.book().sections.len(), 1);
        assert_eq!(publication.book().table_of_contents[0].label, "Page 1");

        let section = publication.parse_section(0).unwrap();
        let Some(Block::Image(image)) = section.blocks.first() else {
            panic!("expected a fixed PDF page image");
        };
        assert_eq!(image.href.path(), "Pages/page-00001.png");
        let text_layer = image
            .text_layer
            .as_ref()
            .expect("PDF page should expose extracted text geometry");
        assert!(text_layer.text.contains("Hello PDF"));
        assert_eq!(text_layer.spans.len(), "Hello PDF".chars().count());
        assert_eq!(image.source.as_ref().unwrap().end.text_offset, 9);
        let first_span = &text_layer.spans[0];
        let baseline_y = 80.0;
        assert!(first_span.rect.y < baseline_y);
        assert!(first_span.rect.y + first_span.rect.height > baseline_y);
        assert!(first_span.rect.y + first_span.rect.height / 2.0 < baseline_y);

        let page = publication.resource(&image.href).unwrap();
        assert!(page.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let cached = publication.resource(&image.href).unwrap();
        assert!(Arc::ptr_eq(&page.bytes, &cached.bytes));
        let dimensions = publication.fixed_page_dimensions(0).unwrap().unwrap();
        assert!(publication.lock_cache().unwrap().rasters.is_empty());
        let raster = publication.raster_resource(&image.href).unwrap().unwrap();
        assert_eq!(
            (dimensions.width, dimensions.height),
            (raster.width, raster.height)
        );
        let cached_raster = publication.raster_resource(&image.href).unwrap().unwrap();
        assert!(Arc::ptr_eq(&raster.pixels, &cached_raster.pixels));
        assert_eq!(
            raster.pixels.len(),
            raster.width as usize * raster.height as usize * 4
        );
        let cover = publication
            .resource(publication.book().cover.as_ref().unwrap())
            .unwrap();
        assert!(cover.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let page_dimensions = png_dimensions(&page.bytes);
        let cover_dimensions = png_dimensions(&cover.bytes);
        assert!(cover_dimensions.0 <= page_dimensions.0);
        assert!(cover_dimensions.1 <= page_dimensions.1);
        assert_ne!(cover_dimensions, (0, 0));
    }

    fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
        (
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
        )
    }

    fn minimal_pdf() -> Vec<u8> {
        let content = b"BT /F1 12 Tf 20 80 Td (Hello PDF) Tj ET";
        let objects = [
            b"<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 160] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_vec(),
            format!("<< /Length {} >>\nstream\n", content.len())
                .into_bytes()
                .into_iter()
                .chain(content.iter().copied())
                .chain(b"\nendstream".iter().copied())
                .collect(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            b"<< /Title (Test PDF) /Author (Rebook) >>".to_vec(),
            b"<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 1 >>".to_vec(),
            b"<< /Title (Page 1) /Parent 7 0 R /Dest [3 0 R /Fit] >>".to_vec(),
        ];
        let mut output = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            writeln!(&mut output, "{} 0 obj", index + 1).unwrap();
            output.write_all(object).unwrap();
            output.extend_from_slice(b"\nendobj\n");
        }
        let xref = output.len();
        write!(&mut output, "xref\n0 {}\n", objects.len() + 1).unwrap();
        output.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            writeln!(&mut output, "{offset:010} 00000 n ").unwrap();
        }
        write!(
            &mut output,
            "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .unwrap();
        output
    }
}
