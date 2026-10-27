//! Shared reader and attachment image preview. Pixels are prepared by callers off the UI thread.
use egui::{Color32, Pos2, Rect, TextureId, Vec2};

pub(crate) struct ImagePreview {
    pub(crate) motion: PreviewMotion,
    /// Optional text copied in place of pixels (for example a reader formula).
    pub(crate) copy_text: Option<String>,
    pub(crate) texture: egui::TextureHandle,
    pub(crate) image: egui::ColorImage,
    pub(crate) source_size: Vec2,
    pub(crate) zoom: f32,
    pub(crate) pan: Vec2,
    id: egui::Id,
}

impl ImagePreview {
    pub(crate) fn new(ctx: &egui::Context, image: egui::ColorImage, namespace: &str) -> Self {
        Self {
            motion: PreviewMotion::new(ctx),
            copy_text: None,
            source_size: Vec2::new(image.size[0] as f32, image.size[1] as f32),
            texture: ctx.load_texture(namespace, image.clone(), egui::TextureOptions::LINEAR),
            image,
            zoom: 1.0,
            pan: Vec2::ZERO,
            id: egui::Id::new(("image-preview", namespace)),
        }
    }

    /// Returns true only after the closing fade has finished.
    pub(crate) fn show(&mut self, ctx: &egui::Context) -> bool {
        let opacity = self.motion.opacity(ctx);
        if self.motion.is_closing() && opacity <= f32::EPSILON {
            return true;
        }
        let screen = crate::ui::overlay_rect(ctx);
        let available =
            (screen.size() - Vec2::splat(IMAGE_PREVIEW_MARGIN * 2.0)).max(Vec2::splat(1.0));
        let fit_scale = preview_fit_scale(self.source_size, available);
        let wheel_delta = ctx.input(preview_wheel_delta);
        if wheel_delta != 0.0 && !self.motion.is_closing() {
            let factor = zoom_from_wheel(self.zoom, wheel_delta) / self.zoom;
            let pointer = ctx
                .input(|i| i.pointer.hover_pos())
                .unwrap_or_else(|| screen.center());
            update_preview_zoom(self, factor, pointer, screen.center());
            ctx.request_repaint();
        }
        let display_size = self.source_size * fit_scale * self.zoom;
        self.pan = clamp_preview_pan(self.pan, display_size, available);
        let image_rect = Rect::from_center_size(screen.center() + self.pan, display_size);
        let interaction = show_image_preview_area(
            ctx,
            self.id,
            screen,
            image_rect,
            self.texture.id(),
            fit_scale * self.zoom * 100.0,
            opacity,
            !self.motion.is_closing(),
        );
        if interaction.reset {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
            ctx.request_repaint();
        } else if interaction.drag_delta != Vec2::ZERO {
            self.pan =
                clamp_preview_pan(self.pan + interaction.drag_delta, display_size, available);
            ctx.request_repaint();
        }
        if interaction.close {
            self.motion.close(ctx);
        }
        false
    }

    /// Standalone overlays can use the reader's standard Escape and +/- controls.
    pub(crate) fn keyboard(&mut self, ctx: &egui::Context) {
        if self.motion.is_closing() {
            return;
        }
        let (close, step, copy) = ctx.input_mut(|input| {
            let close = input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            let step = if input.consume_key(egui::Modifiers::NONE, egui::Key::Minus) {
                -1.0
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Equals) {
                1.0
            } else {
                0.0
            };
            let copy = input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy));
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Copy));
            (close, step, copy)
        });
        if close {
            self.motion.close(ctx);
        }
        if step != 0.0 {
            let center = crate::ui::overlay_rect(ctx).center();
            update_preview_zoom(self, 1.25_f32.powf(step), center, center);
            ctx.request_repaint();
        }
        if copy {
            ctx.copy_image(self.image.clone());
        }
    }
}

pub(crate) const IMAGE_PREVIEW_MARGIN: f32 = 48.0;
pub(crate) const IMAGE_PREVIEW_MIN_ZOOM: f32 = 0.25;
pub(crate) const IMAGE_PREVIEW_MAX_ZOOM: f32 = 8.0;
pub(crate) const IMAGE_PREVIEW_WHEEL_SPEED: f32 = 0.0025;

/// Match the settings dialog's 180 ms fade, retaining pixels until dismissal ends.
/// Use an absolute deadline so returning to an unfocused window finishes the fade
/// without leaving a paused animation requesting frames indefinitely.
pub(crate) struct PreviewMotion {
    pub(crate) opened_at: f64,
    pub(crate) closing: Option<(f64, f32)>,
}

impl PreviewMotion {
    const DURATION: f64 = 0.18;

    pub(crate) fn new(ctx: &egui::Context) -> Self {
        Self {
            opened_at: ctx.input(|i| i.time),
            closing: None,
        }
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    fn opacity_at(&self, now: f64) -> f32 {
        match self.closing {
            Some((started, from)) => {
                from * (1.0 - ((now - started) / Self::DURATION).clamp(0.0, 1.0) as f32)
            }
            None => ((now - self.opened_at) / Self::DURATION).clamp(0.0, 1.0) as f32,
        }
    }

    pub(crate) fn close(&mut self, ctx: &egui::Context) {
        if !self.is_closing() {
            let now = ctx.input(|i| i.time);
            self.closing = Some((now, self.opacity_at(now)));
            ctx.request_repaint();
        }
    }

    pub(crate) fn opacity(&self, ctx: &egui::Context) -> f32 {
        let now = ctx.input(|i| i.time);
        let started = self.closing.map_or(self.opened_at, |(time, _)| time);
        if now < started + Self::DURATION && ctx.input(|i| i.focused) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        self.opacity_at(now)
    }
}

fn preview_wheel_delta(input: &egui::InputState) -> f32 {
    input
        .raw
        .events
        .iter()
        .filter_map(|event| match event {
            egui::Event::MouseWheel { unit, delta, .. } => Some(
                delta.y
                    * match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => 240.0,
                    },
            ),
            _ => None,
        })
        .sum()
}

pub(crate) struct ImagePreviewInteraction {
    pub(crate) close: bool,
    pub(crate) reset: bool,
    pub(crate) drag_delta: Vec2,
}

pub(crate) fn show_image_preview_area(
    ctx: &egui::Context,
    id: egui::Id,
    screen: Rect,
    image_rect: Rect,
    texture_id: TextureId,
    zoom_percent: f32,
    opacity: f32,
    interactive: bool,
) -> ImagePreviewInteraction {
    let backdrop_screen = ctx.content_rect();
    #[cfg(target_os = "windows")]
    crate::app::window_chrome::dim_controls(ctx, 190.0 / 255.0 * opacity);
    let mut interaction = ImagePreviewInteraction {
        close: false,
        reset: false,
        drag_delta: Vec2::ZERO,
    };
    egui::Area::new(id)
        .order(egui::Order::Tooltip)
        .fade_in(false)
        .fixed_pos(backdrop_screen.min)
        .constrain(false)
        .default_size(backdrop_screen.size())
        .show(ctx, |ui| {
            if !interactive {
                ui.disable();
            }
            // Disabling interaction must not add egui's disabled-widget dimming.
            ui.set_opacity(opacity);
            ui.set_clip_rect(backdrop_screen);
            // The backdrop only owns clicks used to close the preview. Registering it for
            // dragging as well competes with the image's drag response on the same layer.
            let (backdrop_rect, backdrop) =
                ui.allocate_exact_size(backdrop_screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(backdrop_rect, 0.0, Color32::from_black_alpha(190));
            // Transparent book illustrations often contain black formulas or line art.
            // Composite them over opaque paper, so the dimmed page cannot show through
            // and the original ink remains readable in either application theme.
            ui.painter().rect_filled(image_rect, 0.0, Color32::WHITE);
            ui.painter().image(
                texture_id,
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            ui.painter().rect_stroke(
                image_rect,
                2.0,
                egui::Stroke::new(1.0, Color32::from_white_alpha(48)),
                egui::StrokeKind::Inside,
            );

            let image_response = ui
                .interact(
                    image_rect,
                    ui.id().with("preview-image"),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab);
            if image_response.dragged() {
                interaction.drag_delta = ctx.input(|input| input.pointer.delta());
            }
            interaction.reset = image_response.double_clicked();

            ui.painter().text(
                Pos2::new(screen.center().x.round(), (screen.bottom() - 24.0).round()),
                egui::Align2::CENTER_BOTTOM,
                format!("{zoom_percent:.0}%"),
                egui::FontId::monospace(crate::ui::scaled_font_size(12.0)),
                Color32::WHITE,
            );
            if backdrop.clicked()
                && backdrop
                    .interact_pointer_pos()
                    .is_some_and(|position| !image_rect.contains(position))
            {
                interaction.close = true;
            }
        });
    interaction
}

pub(crate) fn preview_fit_scale(source: Vec2, available: Vec2) -> f32 {
    // Bound the initial preview on large monitors; manual zoom is unaffected.
    let available = available.min(Vec2::new(1200.0, 900.0));
    (available.x / source.x.max(1.0)).min(available.y / source.y.max(1.0))
}

pub(crate) fn update_preview_zoom(
    preview: &mut ImagePreview,
    factor: f32,
    anchor: Pos2,
    center: Pos2,
) {
    let old_zoom = preview.zoom;
    preview.zoom = (old_zoom * factor).clamp(IMAGE_PREVIEW_MIN_ZOOM, IMAGE_PREVIEW_MAX_ZOOM);
    let ratio = preview.zoom / old_zoom;
    preview.pan -= (anchor - (center + preview.pan)) * (ratio - 1.0);
}

pub(crate) fn zoom_from_wheel(zoom: f32, wheel_delta: f32) -> f32 {
    (zoom * (wheel_delta * IMAGE_PREVIEW_WHEEL_SPEED).exp())
        .clamp(IMAGE_PREVIEW_MIN_ZOOM, IMAGE_PREVIEW_MAX_ZOOM)
}

pub(crate) fn clamp_preview_pan(pan: Vec2, display_size: Vec2, available: Vec2) -> Vec2 {
    let limit = Vec2::new(
        (display_size.x - available.x).abs() / 2.0,
        (display_size.y - available.y).abs() / 2.0,
    );
    Vec2::new(
        pan.x.clamp(-limit.x, limit.x),
        pan.y.clamp(-limit.y, limit.y),
    )
}
