/*!
A crate for rendering PDF files.

This crate allows you to render pages of a PDF file into bitmaps. It is supposed to be relatively
lightweight, since we do not have any dependencies on the GPU. All the rendering happens on the CPU.

The ultimate goal of this crate is to be a *feature-complete* and *performant* PDF rasterizer.
With that said, we are currently still very far away from reaching that goal: So far, no effort
has been put into performance optimizations, as we are still working on implementing missing features.
However, this crate is currently the most comprehensive and feature-complete
implementation of a PDF rasterizer in pure Rust. This claim is supported by the fact that we currently
include over 1000 PDF files in our regression test suite. The majority of those have been scraped
from the `pdf.js` and `PDFBOX` test suites and therefore represent a very large and diverse sample
of PDF files.

As mentioned, there are still some serious limitations, including lack of support for
encrypted/password-protected PDF files, blending and isolation, knockout groups as well as a range
of smaller features such as color key masking. But you should be able to render the vast majority
of PDF files without too many issues.

## Safety
This crate forbids unsafe code via a crate-level attribute.

## Examples
For usage examples, see the [example](https://github.com/LaurenzV/hayro/tree/master/hayro/examples) in
the GitHub repository.

## Cargo features
This crate has one optional feature:
- `embed-fonts`: See the description of [`hayro-interpret`](https://docs.rs/hayro-interpret/latest/hayro_interpret/#cargo-features) for more information.
*/

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use crate::renderer::Renderer;
use hayro_interpret::Device;
use hayro_interpret::FillRule;
use hayro_interpret::InterpreterCache;
use hayro_interpret::InterpreterSettings;
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::hayro_syntax::page::Page;
use hayro_interpret::util::{RectExt, TransformExt};
use hayro_interpret::{BlendMode, Context};
use hayro_interpret::{ClipPath, interpret_page};
use kurbo::{Affine, Rect, Shape};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::rc::Rc;

pub use hayro_interpret;
pub use hayro_interpret::hayro_syntax;
pub use vello_cpu;

use vello_cpu::color::AlphaColor;
use vello_cpu::color::Srgb;
use vello_cpu::color::palette::css::TRANSPARENT;
use vello_cpu::color::palette::css::WHITE;
use vello_cpu::{Level, Pixmap};

mod renderer;

/// A cache used by the renderer.
///
/// Ideally, such a cache should be constructed once per PDF and then reused across
/// multiple render invocations on the same document.
#[derive(Clone, Default)]
pub struct RenderCache<'a> {
    pub(crate) interpreter_cache: InterpreterCache<'a>,
    pub(crate) outline_cache: Rc<RefCell<OutlineCache>>,
}

impl<'a> RenderCache<'a> {
    /// Create a new render cache.
    pub fn new() -> Self {
        Self {
            interpreter_cache: InterpreterCache::new(),
            outline_cache: Rc::new(RefCell::new(OutlineCache::default())),
        }
    }
}

// Only outlines are shared across pages in bounded conversion caches. Decoded
// fonts, images and color objects are reset before the next physical page.
#[derive(Default)]
pub(crate) struct OutlineCache {
    paths: HashMap<u128, Rc<kurbo::BezPath>>,
    retained_bytes: usize,
    limit: Option<usize>,
}

impl OutlineCache {
    pub(crate) fn get(&self, id: &u128) -> Option<&Rc<kurbo::BezPath>> {
        self.paths.get(id)
    }

    pub(crate) fn insert(&mut self, id: u128, path: Rc<kurbo::BezPath>) {
        // Account for Vec spare capacity and hash-table overhead conservatively.
        let bytes = path
            .elements()
            .len()
            .saturating_mul(2 * size_of::<kurbo::PathEl>())
            .saturating_add(256);
        if let Some(limit) = self.limit {
            if bytes > limit {
                return;
            }
            if self.retained_bytes.saturating_add(bytes) > limit {
                self.paths = HashMap::new();
                self.retained_bytes = 0;
            }
        }
        if self.paths.insert(id, path).is_none() {
            self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        }
    }
}

impl<'a> RenderCache<'a> {
    /// Create a worker-local cache with bounded retained outline memory.
    pub fn with_outline_budget(bytes: usize) -> Self {
        Self {
            interpreter_cache: InterpreterCache::new(),
            outline_cache: Rc::new(RefCell::new(OutlineCache {
                limit: Some(bytes),
                ..OutlineCache::default()
            })),
        }
    }

    /// Accounted retained outline bytes, excluding transient render assets.
    pub fn retained_outline_bytes(&self) -> usize {
        self.outline_cache.borrow().retained_bytes
    }

    /// Release decoded page assets while retaining bounded reusable outlines.
    /// Call only after all render invocations for the previous page have ended.
    pub fn begin_page(&mut self) {
        self.interpreter_cache = InterpreterCache::new();
    }
}

/// Settings to apply during rendering.
#[derive(Clone, Copy)]
pub struct RenderSettings {
    /// How much the contents should be scaled into the x direction.
    pub x_scale: f32,
    /// How much the contents should be scaled into the y direction.
    pub y_scale: f32,
    /// The width of the viewport. If this is set to `None`, the width will be chosen
    /// automatically based on the scale factor and the dimensions of the PDF.
    pub width: Option<u16>,
    /// The height of the viewport. If this is set to `None`, the height will be chosen
    /// automatically based on the scale factor and the dimensions of the PDF.
    pub height: Option<u16>,
    /// The background color. Determines the color of the base
    /// rectangle during rendering to a pixmap.
    pub bg_color: AlphaColor<Srgb>,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            x_scale: 1.0,
            y_scale: 1.0,
            width: None,
            height: None,
            bg_color: TRANSPARENT,
        }
    }
}

/// Render the page with the given settings to a pixmap.
pub fn render<'a>(
    page: &'a Page<'a>,
    cache: &RenderCache<'a>,
    interpreter_settings: &InterpreterSettings,
    render_settings: &RenderSettings,
) -> Pixmap {
    render_region(page, cache, interpreter_settings, render_settings, (0, 0))
}

/// Render a viewport beginning at an integer pixel offset in the scaled page.
/// Width and height in `render_settings` specify the viewport dimensions.
/// The scale and page transform are identical to full-page rendering.
pub fn render_region<'a>(
    page: &'a Page<'a>,
    cache: &RenderCache<'a>,
    interpreter_settings: &InterpreterSettings,
    render_settings: &RenderSettings,
    origin: (u16, u16),
) -> Pixmap {
    let (x_scale, y_scale) = (render_settings.x_scale, render_settings.y_scale);
    let (width, height) = page.render_dimensions();
    let (scaled_width, scaled_height) = ((width * x_scale) as f64, (height * y_scale) as f64);
    let initial_transform = Affine::translate((-(origin.0 as f64), -(origin.1 as f64)))
        * Affine::scale_non_uniform(x_scale as f64, y_scale as f64)
        * page.initial_transform(true).to_kurbo();

    let (pix_width, pix_height) = (
        render_settings.width.unwrap_or(scaled_width.floor() as u16),
        render_settings
            .height
            .unwrap_or(scaled_height.floor() as u16),
    );
    let mut state = Context::new(
        initial_transform,
        Rect::new(0.0, 0.0, pix_width as f64, pix_height as f64),
        &cache.interpreter_cache,
        page.xref(),
        interpreter_settings.clone(),
    );

    let vc_settings = vello_cpu::RenderSettings {
        level: Level::new(),
        num_threads: 0,
    };

    let mut device = Renderer::new(pix_width, pix_height, vc_settings, cache);

    device.ctx.set_paint(render_settings.bg_color);
    device
        .ctx
        .fill_rect(&Rect::new(0.0, 0.0, pix_width as f64, pix_height as f64));
    let mut clip_path = page.intersected_crop_box().to_kurbo().to_path(0.1);
    clip_path.apply_affine(initial_transform);
    device.push_clip_path(&ClipPath {
        path: clip_path,
        fill: FillRule::NonZero,
    });

    device.push_transparency_group(1.0, None, BlendMode::Normal);
    interpret_page(page, &mut state, &mut device);

    device.pop_transparency_group();

    device.pop_clip_path();

    let mut pixmap = Pixmap::new(pix_width, pix_height);
    let mut resources = vello_cpu::Resources::default();
    device.ctx.flush();
    device.ctx.render(&mut pixmap, &mut resources);

    pixmap
}

/// Render a proven isolated, opaque decoded raster using the normal PDF image
/// sampling path, without interpreting the rest of its page. `transform` maps
/// source image pixels into the output viewport, including its crop offset.
pub fn render_embedded_image(
    image: hayro_interpret::ImageData,
    transform: Affine,
    width: u16,
    height: u16,
    bg_color: AlphaColor<Srgb>,
) -> Pixmap {
    let cache = RenderCache::new();
    let settings = vello_cpu::RenderSettings {
        level: Level::new(),
        num_threads: 0,
    };
    let mut device = Renderer::new(width, height, settings, &cache);
    device.ctx.set_paint(bg_color);
    device
        .ctx
        .fill_rect(&Rect::new(0.0, 0.0, width as f64, height as f64));
    device.push_transparency_group(1.0, None, BlendMode::Normal);
    device.ctx.set_aliasing_threshold(Some(1));
    device.ctx.set_transform(transform);
    device.draw_image(image, None);
    device.pop_transparency_group();
    let mut pixmap = Pixmap::new(width, height);
    device.ctx.flush();
    device
        .ctx
        .render(&mut pixmap, &mut vello_cpu::Resources::default());
    pixmap
}

// Just a convenience method for testing.
#[doc(hidden)]
pub fn render_pdf(
    pdf: &Pdf,
    scale: f32,
    settings: InterpreterSettings,
    range: Option<RangeInclusive<usize>>,
) -> Option<Vec<Pixmap>> {
    let cache = RenderCache::new();
    let rendered = pdf
        .pages()
        .iter()
        .enumerate()
        .flat_map(|(idx, page)| {
            if range.clone().is_some_and(|range| !range.contains(&idx)) {
                return None;
            }

            let pixmap = render(
                page,
                &cache,
                &settings,
                &RenderSettings {
                    x_scale: scale,
                    y_scale: scale,
                    bg_color: WHITE,
                    ..Default::default()
                },
            );

            Some(pixmap)
        })
        .collect();

    Some(rendered)
}

pub(crate) fn derive_settings(settings: &vello_cpu::RenderSettings) -> vello_cpu::RenderSettings {
    vello_cpu::RenderSettings {
        num_threads: 0,
        ..*settings
    }
}
