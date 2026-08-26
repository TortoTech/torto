//! GPUI presentation adapter for immutable Torto layout frames.
//!
//! The layout crate owns text/flow/page decisions. This crate converts the
//! resulting backend-neutral paint descriptors into GPUI elements without
//! retaining GPUI objects in [`LayoutFrame`].

mod text_engine;

pub use text_engine::GpuiTextEngine;

use std::sync::Arc;

use gpui::{AnyElement, FontWeight, RenderImage, div, img, prelude::*, px, rgba};
use image::{Frame, RgbaImage};
use rebook_layout::{
    ImagePlacement, LayoutFrame, PageItem, QuotePlacement, RasterImage, TablePlacement,
    TextPlacement, text::TextPaintItem,
};
use rebook_publication::{FixedPageTextRect, Rgba, SourceRange};

#[derive(Default)]
pub struct GpuiFramePresenter {
    images: Vec<CachedImage>,
}

/// One GPUI-only source overlay. Durable ranges stay owned by the application;
/// this adapter resolves their fresh geometry from the immutable frame.
#[derive(Clone, Copy)]
pub struct GpuiSourceOverlay<'a> {
    pub ranges: &'a [SourceRange],
    pub color: u32,
}

impl<'a> GpuiSourceOverlay<'a> {
    #[must_use]
    pub const fn new(ranges: &'a [SourceRange], color: u32) -> Self {
        Self { ranges, color }
    }
}

struct CachedImage {
    width: u32,
    height: u32,
    pixels: Arc<[u8]>,
    rendered: Arc<RenderImage>,
}

impl GpuiFramePresenter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops backend image handles after a document/frame generation changes.
    pub fn clear(&mut self) {
        self.images.clear();
    }

    /// Converts every page item plus durable source selections into GPUI
    /// elements. Callers remain responsible for the page container and input
    /// event routing.
    pub fn elements(&mut self, frame: &LayoutFrame, selections: &[SourceRange]) -> Vec<AnyElement> {
        self.elements_with_overlays(frame, &[GpuiSourceOverlay::new(selections, 0xB8DC_C866)])
    }

    /// Converts page items plus independently styled durable source overlays.
    /// Overlay order is paint order, so an active selection may be placed after
    /// persisted highlights without changing layout or source mapping.
    pub fn elements_with_overlays(
        &mut self,
        frame: &LayoutFrame,
        overlays: &[GpuiSourceOverlay<'_>],
    ) -> Vec<AnyElement> {
        let mut elements = Vec::new();
        for item in &frame.items {
            match item {
                PageItem::Text(text) => elements.extend(self.text_elements(text)),
                PageItem::Quote(quote) => elements.extend(quote_elements(quote)),
                PageItem::Table(table) => elements.extend(self.table_elements(table)),
                PageItem::Image(image) => elements.extend(self.image_elements(image)),
                PageItem::Separator(separator) => elements.push(
                    div()
                        .absolute()
                        .left(px(separator.x))
                        .top(px(separator.y))
                        .w(px(separator.width))
                        .h(px(1.0))
                        .bg(rgba(0x7874_68A0))
                        .into_any_element(),
                ),
            }
        }
        for overlay in overlays {
            elements.extend(
                frame
                    .interaction()
                    .source_rects(overlay.ranges)
                    .into_iter()
                    .map(|rect| {
                        div()
                            .absolute()
                            .left(px(rect.x0))
                            .top(px(rect.y0))
                            .w(px((rect.x1 - rect.x0).max(1.0)))
                            .h(px((rect.y1 - rect.y0).max(1.0)))
                            .bg(rgba(overlay.color))
                            .into_any_element()
                    }),
            );
        }
        elements
    }

    fn table_elements(&mut self, table: &TablePlacement) -> Vec<AnyElement> {
        let mut elements = Vec::new();
        for cell in &table.cells {
            if cell.header && table.header_fill.alpha > 0 {
                elements.push(
                    div()
                        .absolute()
                        .left(px(cell.x))
                        .top(px(cell.y))
                        .w(px(cell.width))
                        .h(px(cell.height))
                        .bg(rgba(rgba32(table.header_fill)))
                        .into_any_element(),
                );
            }
        }
        for text in table.cells.iter().filter_map(|cell| cell.text.as_ref()) {
            elements.extend(self.text_elements(text));
        }
        for cell in &table.cells {
            elements.push(
                div()
                    .absolute()
                    .left(px(cell.x))
                    .top(px(cell.y))
                    .w(px(cell.width))
                    .h(px(cell.height))
                    .border_1()
                    .border_color(rgba(rgba32(table.border)))
                    .into_any_element(),
            );
        }
        elements
    }

    fn image_elements(&mut self, image: &ImagePlacement) -> Vec<AnyElement> {
        let rendered = self.render_image(&image.image);
        let mut elements = vec![
            img(rendered)
                .absolute()
                .left(px(image.x))
                .top(px(image.y))
                .w(px(image.width))
                .h(px(image.height))
                .into_any_element(),
        ];
        if let Some(replacement) = &image.replacement {
            for segment in &replacement.segments {
                elements.push(
                    div()
                        .absolute()
                        .left(px(segment.rect.x))
                        .top(px(segment.rect.y))
                        .w(px(segment.rect.width))
                        .h(px(segment.rect.height))
                        .bg(rgba(rgba32(fixed_page_mask_color(image, segment.rect))))
                        .into_any_element(),
                );
                elements.extend(self.text_elements(&segment.text));
            }
        }
        elements
    }

    fn text_elements(&mut self, text: &TextPlacement) -> Vec<AnyElement> {
        let mut elements = Vec::new();
        for line_id in text.lines.iter() {
            let Some(line) = text.layout.line(line_id) else {
                continue;
            };
            let has_native_runs = line
                .items
                .iter()
                .any(|item| matches!(item, TextPaintItem::NativeRun(_)));
            if has_native_runs {
                for run in line.items.iter().filter_map(|item| match item {
                    TextPaintItem::NativeRun(run) => Some(run),
                    _ => None,
                }) {
                    let Some(content) = text.text.get(run.text_range.clone()) else {
                        continue;
                    };
                    let content = content.trim_end();
                    if content.is_empty() {
                        continue;
                    }
                    let mut element = div()
                        .absolute()
                        .left(px(text.origin_x + run.x))
                        .top(px(text.origin_y + line.metrics.block_min))
                        .h(px(line.metrics.line_height))
                        .whitespace_nowrap()
                        .text_size(px(run.font_size))
                        .font_weight(FontWeight(run.font_weight.clamp(100.0, 900.0)))
                        .line_height(px(line.metrics.line_height))
                        .text_color(rgba(rgba32(run.color)))
                        .child(content.to_owned());
                    if let Some(family) = &run.font_family {
                        element = element.font_family(family.to_string());
                    }
                    if run.italic {
                        element = element.italic();
                    }
                    if run.underline {
                        element = element.underline();
                    }
                    elements.push(element.into_any_element());
                }
            } else if let Some(fallback) = fallback_line_element(text, line_id) {
                elements.push(fallback);
            }

            for item in line.items.iter() {
                match item {
                    TextPaintItem::InlineBox(inline_box) => {
                        let Some(inline_image) = text
                            .inline_images
                            .iter()
                            .find(|image| image.id == inline_box.id)
                        else {
                            continue;
                        };
                        let rendered = self.render_image(&inline_image.image);
                        elements.push(
                            img(rendered)
                                .absolute()
                                .left(px(text.origin_x + inline_box.x))
                                .top(px(text.origin_y + inline_box.y))
                                .w(px(inline_box.width))
                                .h(px(inline_box.height))
                                .into_any_element(),
                        );
                    }
                    TextPaintItem::Rule(rule) => elements.push(
                        div()
                            .absolute()
                            .left(px(text.origin_x + rule.x))
                            .top(px(text.origin_y + rule.y))
                            .w(px(rule.width))
                            .h(px(rule.thickness.max(1.0)))
                            .bg(rgba(rgba32(rule.color)))
                            .into_any_element(),
                    ),
                    TextPaintItem::FootnoteReference(reference) => {
                        let diameter = (reference.font_size * 0.72).max(9.0);
                        elements.push(
                            div()
                                .absolute()
                                .left(px(text.origin_x + reference.center_x - diameter / 2.0))
                                .top(px(text.origin_y + reference.baseline - diameter * 0.82))
                                .w(px(diameter))
                                .h(px(diameter))
                                .rounded_full()
                                .border_1()
                                .border_color(rgba(0x5D8F_D0FF))
                                .text_size(px((diameter * 0.72).max(8.0)))
                                .text_color(rgba(0x5D8F_D0FF))
                                .child("i")
                                .into_any_element(),
                        );
                    }
                    TextPaintItem::GlyphRun(_) | TextPaintItem::NativeRun(_) => {}
                }
            }
        }
        elements
    }

    fn render_image(&mut self, raster: &RasterImage) -> Arc<RenderImage> {
        if let Some(cached) = self.images.iter().find(|cached| {
            cached.width == raster.width
                && cached.height == raster.height
                && Arc::ptr_eq(&cached.pixels, &raster.pixels)
        }) {
            return Arc::clone(&cached.rendered);
        }
        let rendered = raster_to_render_image(raster);
        self.images.push(CachedImage {
            width: raster.width,
            height: raster.height,
            pixels: Arc::clone(&raster.pixels),
            rendered: Arc::clone(&rendered),
        });
        rendered
    }
}

fn fallback_line_element(
    text: &TextPlacement,
    line_id: rebook_layout::text::TextLineId,
) -> Option<AnyElement> {
    let line = text.layout.line(line_id)?;
    let content = text.text.get(line.text_range.clone())?.trim_end();
    if content.is_empty() {
        return None;
    }
    let (font_size, color) = line
        .items
        .iter()
        .find_map(|item| match item {
            TextPaintItem::GlyphRun(run) => Some((run.font_size, run.color)),
            TextPaintItem::NativeRun(run) => Some((run.font_size, run.color)),
            TextPaintItem::InlineBox(_)
            | TextPaintItem::Rule(_)
            | TextPaintItem::FootnoteReference(_) => None,
        })
        .unwrap_or((20.0, Rgba::BLACK));
    Some(
        div()
            .absolute()
            .left(px(text.origin_x
                + line.metrics.offset
                + line.metrics.inline_min))
            .top(px(text.origin_y + line.metrics.block_min))
            .h(px(line.metrics.line_height))
            .whitespace_nowrap()
            .text_size(px(font_size))
            .line_height(px(line.metrics.line_height))
            .text_color(rgba(rgba32(color)))
            .child(content.to_owned())
            .into_any_element(),
    )
}

fn quote_elements(quote: &QuotePlacement) -> Vec<AnyElement> {
    let mut elements = Vec::new();
    if quote.fill.alpha > 0 {
        elements.push(
            div()
                .absolute()
                .left(px(quote.x))
                .top(px(quote.y))
                .w(px(quote.width))
                .h(px(quote.height))
                .rounded_md()
                .bg(rgba(rgba32(quote.fill)))
                .into_any_element(),
        );
    }
    let inset = 8.0_f32.min(quote.height * 0.2);
    let top = quote.y + if quote.continued_before { 0.0 } else { inset };
    let bottom = quote.y + quote.height - if quote.continued_after { 0.0 } else { inset };
    elements.push(
        div()
            .absolute()
            .left(px(quote.x + 6.0))
            .top(px(top))
            .w(px(4.0))
            .h(px((bottom - top).max(1.0)))
            .rounded_full()
            .bg(rgba(rgba32(quote.accent)))
            .into_any_element(),
    );
    elements
}

fn raster_to_render_image(raster: &RasterImage) -> Arc<RenderImage> {
    let expected_len = usize::try_from(raster.width)
        .ok()
        .and_then(|width| {
            usize::try_from(raster.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4));
    let (width, height, mut bgra) = if expected_len == Some(raster.pixels.len()) {
        (
            raster.width.max(1),
            raster.height.max(1),
            raster.pixels.to_vec(),
        )
    } else {
        (1, 1, vec![0, 0, 0, 0])
    };
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(width, height, bgra)
        .expect("validated GPUI raster dimensions must match their byte buffer");
    Arc::new(RenderImage::new(vec![Frame::new(buffer)]))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "fixed-page raster coordinates are clamped to bounded image dimensions"
)]
fn fixed_page_mask_color(image: &ImagePlacement, rect: FixedPageTextRect) -> Rgba {
    let raster_width = image.image.width as usize;
    let raster_height = image.image.height as usize;
    if raster_width == 0
        || raster_height == 0
        || image.width <= 0.0
        || image.height <= 0.0
        || image.image.pixels.len() < raster_width.saturating_mul(raster_height).saturating_mul(4)
    {
        return Rgba {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        };
    }
    let to_x = |x: f32| {
        (((x - image.x) / image.width) * image.image.width as f32)
            .floor()
            .clamp(0.0, (raster_width - 1) as f32) as usize
    };
    let to_y = |y: f32| {
        (((y - image.y) / image.height) * image.image.height as f32)
            .floor()
            .clamp(0.0, (raster_height - 1) as f32) as usize
    };
    let x0 = to_x(rect.x);
    let x1 = to_x(rect.x + rect.width).max(x0);
    let y0 = to_y(rect.y);
    let y1 = to_y(rect.y + rect.height).max(y0);
    let mut samples = Vec::<[u8; 4]>::new();
    let sample = |samples: &mut Vec<[u8; 4]>, x: usize, y: usize| {
        let offset = (y * raster_width + x) * 4;
        samples.push([
            image.image.pixels[offset],
            image.image.pixels[offset + 1],
            image.image.pixels[offset + 2],
            image.image.pixels[offset + 3],
        ]);
    };
    let steps = 12_usize;
    for step in 0..=steps {
        let x = x0 + (x1 - x0) * step / steps;
        let y = y0 + (y1 - y0) * step / steps;
        sample(&mut samples, x, y0);
        sample(&mut samples, x, y1);
        sample(&mut samples, x0, y);
        sample(&mut samples, x1, y);
    }
    let median = |channel: usize| {
        let mut values = samples
            .iter()
            .map(|sample| sample[channel])
            .collect::<Vec<_>>();
        values.sort_unstable();
        values[values.len() / 2]
    };
    Rgba {
        red: median(0),
        green: median(1),
        blue: median(2),
        alpha: median(3),
    }
}

const fn rgba32(color: Rgba) -> u32 {
    (color.red as u32) << 24
        | (color.green as u32) << 16
        | (color.blue as u32) << 8
        | color.alpha as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::{
        LayoutViewport, PageLayout, QuotePlacement, SeparatorPlacement, frame::LayoutFrame,
    };

    #[test]
    fn presenter_covers_quote_separator_and_selection_free_frames() {
        let frame = LayoutFrame::freeze(PageLayout {
            viewport: LayoutViewport::new(320, 240).unwrap(),
            background: Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            leading_gap: 0.0,
            items: vec![
                PageItem::Quote(QuotePlacement {
                    x: 20.0,
                    y: 20.0,
                    width: 280.0,
                    height: 80.0,
                    continued_before: false,
                    continued_after: false,
                    fill: Rgba {
                        red: 240,
                        green: 240,
                        blue: 240,
                        alpha: 255,
                    },
                    accent: Rgba::BLACK,
                    sources: Vec::new(),
                }),
                PageItem::Separator(SeparatorPlacement {
                    x: 40.0,
                    y: 120.0,
                    width: 240.0,
                }),
            ],
        });

        let elements = GpuiFramePresenter::new().elements(&frame, &[]);
        assert_eq!(elements.len(), 3);
    }

    #[test]
    fn image_cache_reuses_the_same_raster_allocation() {
        let raster = RasterImage {
            width: 1,
            height: 1,
            pixels: Arc::from([255, 0, 0, 255]),
        };
        let mut presenter = GpuiFramePresenter::new();
        let first = presenter.render_image(&raster);
        let second = presenter.render_image(&raster);

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(presenter.images.len(), 1);
    }
}
