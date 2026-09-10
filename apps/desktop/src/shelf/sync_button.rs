use egui::{Response, Sense, Ui, Vec2};
use std::time::{Duration, Instant};

use crate::preferences::AppLanguage;
use crate::sync::SyncStage;
use crate::ui::{Icon, icon_button, palette};

#[derive(Default)]
pub(super) enum SyncButtonState {
    #[default]
    Idle,
    Running {
        stage: SyncStage,
        completed: u64,
        total: u64,
    },
    Complete {
        expires_at: Instant,
    },
    Failed,
}

impl SyncButtonState {
    pub(super) fn completed(now: Instant) -> Self {
        Self::Complete {
            expires_at: now + Duration::from_secs(3),
        }
    }

    pub(super) fn expire(&mut self, now: Instant) {
        if matches!(self, Self::Complete { expires_at } if now >= *expires_at) {
            *self = Self::Idle;
        }
    }

    pub(super) fn hover_text(&self, language: AppLanguage) -> String {
        match self {
            Self::Idle => language.text("云同步", "Cloud sync").into(),
            Self::Running {
                stage,
                completed,
                total,
            } => sync_progress_text(language, *stage, *completed, *total),
            Self::Complete { expires_at } if Instant::now() < *expires_at => {
                language.text("同步完成", "Sync complete").into()
            }
            Self::Complete { .. } => language.text("云同步", "Cloud sync").into(),
            Self::Failed => language
                .text("同步失败，点击重试", "Sync failed. Click to retry")
                .into(),
        }
    }

    pub(super) fn show(&self, ui: &mut Ui, blocked: bool, language: AppLanguage) -> Response {
        if let Self::Complete { expires_at } = self
            && *expires_at > Instant::now()
        {
            ui.ctx()
                .request_repaint_after(expires_at.saturating_duration_since(Instant::now()));
        }
        let pending = matches!(self, Self::Running { .. });
        let response = ui
            .add_enabled_ui(!pending && !blocked, |ui| {
                if pending {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                    egui::Spinner::new()
                        .color(palette().accent)
                        .paint_at(ui, rect.shrink(7.0));
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::ProgressIndicator,
                            false,
                            language.text("云同步", "Cloud sync"),
                        )
                    });
                    response
                } else {
                    icon_button(ui, Icon::Cloud)
                }
            })
            .inner;
        if blocked {
            response
        } else if pending {
            // Match the reader's OCR control: disabled during work, with live hover progress.
            response.on_disabled_hover_text(self.hover_text(language))
        } else {
            response.on_hover_text(self.hover_text(language))
        }
    }
}

pub(super) fn sync_progress_text(
    language: AppLanguage,
    stage: SyncStage,
    completed: u64,
    total: u64,
) -> String {
    let direction = match stage {
        SyncStage::Uploading => language.text("上传", "Uploading"),
        SyncStage::Downloading => language.text("下载", "Downloading"),
        _ => return language.text("正在同步…", "Syncing…").into(),
    };
    if total == 0 {
        return language.text("正在同步…", "Syncing…").into();
    }
    let percent = (u128::from(completed) * 100 / u128::from(total)).min(100);
    format!(
        "{} · {direction} {percent}%",
        language.text("正在同步", "Syncing")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_completion_expires_without_resetting_a_new_sync_or_error() {
        let now = Instant::now();
        let mut state = SyncButtonState::completed(now);
        state.expire(now + Duration::from_secs(2));
        assert!(matches!(state, SyncButtonState::Complete { .. }));
        state.expire(now + Duration::from_secs(3));
        assert!(matches!(state, SyncButtonState::Idle));
        assert_eq!(state.hover_text(AppLanguage::English), "Cloud sync");
        state = SyncButtonState::Running {
            stage: SyncStage::Uploading,
            completed: 2,
            total: 10,
        };
        state.expire(now + Duration::from_secs(10));
        assert!(matches!(state, SyncButtonState::Running { .. }));
        state = SyncButtonState::Failed;
        state.expire(now + Duration::from_secs(10));
        assert!(matches!(state, SyncButtonState::Failed));
    }

    fn frame(
        ctx: &egui::Context,
        state: &SyncButtonState,
        time: f64,
        events: Vec<egui::Event>,
    ) -> (Vec<String>, egui::Rect, bool) {
        let mut rect = egui::Rect::NOTHING;
        let mut enabled = true;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(640.0, 480.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                let response = state.show(ui, false, AppLanguage::English);
                rect = response.rect;
                enabled = response.enabled();
                if matches!(state, SyncButtonState::Running { .. }) {
                    assert!(!response.clicked());
                }
            },
        );
        output.textures_delta.clear();
        fn collect(shape: &egui::Shape, text: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(shape) => text.push(shape.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect(shape, text)),
                _ => {}
            }
        }
        let mut text = Vec::new();
        for shape in output.shapes {
            collect(&shape.shape, &mut text);
        }
        (text, rect, enabled)
    }

    #[test]
    fn sync_loading_button_only_shows_progress_on_hover() {
        let ctx = egui::Context::default();
        let mut state = SyncButtonState::Running {
            stage: SyncStage::Uploading,
            completed: 35,
            total: 100,
        };
        let (text, rect, enabled) = frame(&ctx, &state, 0.0, Vec::new());
        assert!(
            text.is_empty(),
            "there must be no standalone sync notification"
        );
        assert_eq!(rect.size(), Vec2::splat(32.0));
        assert!(!enabled);
        let position = rect.center();
        frame(&ctx, &state, 1.0, vec![egui::Event::PointerMoved(position)]);
        frame(&ctx, &state, 2.0, Vec::new());
        let (text, _, _) = frame(&ctx, &state, 3.0, Vec::new());
        assert!(text.iter().any(|text| text == "Syncing · Uploading 35%"));
        state = SyncButtonState::Running {
            stage: SyncStage::Downloading,
            completed: 62,
            total: 100,
        };
        let (text, _, _) = frame(&ctx, &state, 4.0, Vec::new());
        assert!(text.iter().any(|text| text == "Syncing · Downloading 62%"));
        for (index, pressed) in [true, false].into_iter().enumerate() {
            frame(
                &ctx,
                &state,
                5.0 + index as f64,
                vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        let (_, idle_rect, enabled) = frame(
            &ctx,
            &SyncButtonState::completed(Instant::now()),
            7.0,
            Vec::new(),
        );
        assert!(enabled);
        assert_eq!(idle_rect.size(), rect.size());
        let (text, _, _) = frame(
            &ctx,
            &state,
            8.0,
            vec![egui::Event::PointerMoved(egui::pos2(400.0, 400.0))],
        );
        assert!(text.is_empty());
    }
}
