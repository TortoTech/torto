//! Adapter from the renderer-neutral retained display list to Vello.

use std::sync::Arc;

use anyrender::{Filter, NormalizedCoord, Paint, PaintRef, PaintScene, RenderContext};
use kurbo::{Affine, Diagonal2, Rect, Shape, Stroke, Vec2};
use peniko::{BlendMode, Color, Fill, FontData, StyleRef};
use vello::{FontEmbolden, Glyph, Scene};

pub struct VelloScene<'a> {
    scene: &'a mut Scene,
}

impl<'a> VelloScene<'a> {
    pub fn new(scene: &'a mut Scene) -> Self {
        Self { scene }
    }
}

impl RenderContext for VelloScene<'_> {}

impl PaintScene for VelloScene<'_> {
    fn reset(&mut self) {
        self.scene.reset();
    }

    fn push_layer(
        &mut self,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
        _filter: Option<Arc<Filter>>,
        _backdrop_filter: Option<Arc<Filter>>,
    ) {
        self.scene
            .push_layer(Fill::NonZero, blend, alpha, transform, clip);
    }

    fn push_clip_layer(&mut self, transform: Affine, clip: &impl Shape) {
        self.scene.push_clip_layer(Fill::NonZero, transform, clip);
    }

    fn pop_layer(&mut self) {
        self.scene.pop_layer();
    }

    fn stroke<'a>(
        &mut self,
        style: &Stroke,
        transform: Affine,
        paint: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        source_shape: &impl Shape,
    ) {
        self.scene.stroke(
            style,
            transform,
            solid_brush(&paint.into()),
            brush_transform,
            source_shape,
        );
    }

    fn fill<'a>(
        &mut self,
        fill: Fill,
        transform: Affine,
        paint: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        source_shape: &impl Shape,
    ) {
        match paint.into() {
            Paint::Solid(value) => {
                self.scene
                    .fill(fill, transform, value, brush_transform, source_shape);
            }
            Paint::Gradient(value) => {
                self.scene
                    .fill(fill, transform, value, brush_transform, source_shape);
            }
            Paint::Image(value) => {
                self.scene
                    .fill(fill, transform, value, brush_transform, source_shape);
            }
            Paint::Resource(_) | Paint::Custom(_) => {
                self.scene.fill(
                    fill,
                    transform,
                    Color::TRANSPARENT,
                    brush_transform,
                    source_shape,
                );
            }
        }
    }

    fn draw_glyphs<'a, 's: 'a>(
        &'s mut self,
        font: &'a FontData,
        font_size: f32,
        hint: bool,
        normalized_coords: &'a [NormalizedCoord],
        embolden: Vec2,
        style: impl Into<StyleRef<'a>>,
        paint: impl Into<PaintRef<'a>>,
        brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = anyrender::Glyph>,
    ) {
        let paint = paint.into();
        let builder = self
            .scene
            .draw_glyphs(font)
            .font_size(font_size)
            .hint(hint)
            .normalized_coords(normalized_coords)
            .font_embolden(FontEmbolden::new(Diagonal2::new(embolden.x, embolden.y)))
            .brush(solid_brush(&paint))
            .brush_alpha(brush_alpha)
            .transform(transform)
            .glyph_transform(glyph_transform);
        let glyphs = glyphs.map(|glyph| Glyph {
            id: glyph.id,
            x: glyph.x,
            y: glyph.y,
        });
        match style.into() {
            StyleRef::Fill(fill) => builder.draw(fill, glyphs),
            StyleRef::Stroke(stroke) => builder.draw(stroke, glyphs),
        }
    }

    fn draw_box_shadow(
        &mut self,
        transform: Affine,
        rect: Rect,
        brush: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.scene
            .draw_blurred_rounded_rect(transform, rect, brush, radius, std_dev);
    }
}

fn solid_brush(paint: &PaintRef<'_>) -> Color {
    match paint {
        Paint::Solid(value) => *value,
        Paint::Gradient(_) | Paint::Image(_) | Paint::Resource(_) | Paint::Custom(_) => {
            Color::TRANSPARENT
        }
    }
}
