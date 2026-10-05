use std::time::Duration;

use egui::{Color32, Rect, Response, Sense, Ui, Widget, WidgetInfo, WidgetType};

/// Schedule a timer without egui shortening it by the predicted frame time.
#[track_caller]
pub(crate) fn request_repaint_in(ctx: &egui::Context, interval: Duration) {
    let predicted = ctx.input(|input| input.predicted_dt);
    let predicted = Duration::try_from_secs_f32(predicted).unwrap_or_default();
    ctx.request_repaint_after(interval.saturating_add(predicted));
}

/// Loading animation paced at 30 FPS, or 10 FPS when the window is unfocused.
#[derive(Default)]
#[must_use = "Add this widget to a ui or call paint_at"]
pub(crate) struct LoadingSpinner {
    size: Option<f32>,
    color: Option<Color32>,
}

impl LoadingSpinner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    pub(crate) fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    pub(crate) fn paint_at(&self, ui: &Ui, rect: Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let (time, focused) = ui.input(|input| (input.time, input.focused));
        request_repaint_in(
            ui.ctx(),
            Duration::from_millis(if focused { 33 } else { 100 }),
        );
        // Match egui's spinner shape and theme colors.
        let color = self
            .color
            .unwrap_or_else(|| ui.visuals().strong_text_color());
        let radius = rect.width().min(rect.height()) / 2.0 - 2.0;
        let count = (radius.round() as u32).clamp(8, 128);
        let start = time * std::f64::consts::TAU;
        let end = start + 240_f64.to_radians() * time.sin();
        let points = (0..count)
            .map(|index| {
                let angle = egui::lerp(start..=end, f64::from(index) / f64::from(count));
                let (sin, cos) = angle.sin_cos();
                rect.center() + radius * egui::vec2(cos as f32, sin as f32)
            })
            .collect();
        ui.painter()
            .add(egui::Shape::line(points, egui::Stroke::new(3.0, color)));
    }
}

impl Widget for LoadingSpinner {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = self.size.unwrap_or(ui.spacing().interact_size.y);
        let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(size), Sense::hover());
        response.widget_info(|| WidgetInfo::new(WidgetType::ProgressIndicator));
        self.paint_at(ui, rect);
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_uses_a_timer_and_stops_when_hidden() {
        for focused in [true, false] {
            let ctx = egui::Context::default();
            for pass in 0..8 {
                let visible = pass < 5;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(f64::from(pass) * 0.1),
                        focused,
                        ..Default::default()
                    },
                    |ui| {
                        if visible {
                            ui.add(LoadingSpinner::new());
                        }
                    },
                );
                let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
                output.textures_delta.clear();
                if pass == 4 {
                    let expected = Duration::from_millis(if focused { 33 } else { 100 });
                    assert_eq!(delay, expected);
                } else if pass == 7 {
                    assert_eq!(delay, Duration::MAX);
                }
            }
        }
    }
}
