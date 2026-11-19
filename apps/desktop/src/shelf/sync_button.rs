use egui::{Response, Sense, Ui, Vec2};
use std::time::{Duration, Instant};

use crate::preferences::AppLanguage;
use crate::sync::SyncStage;
use crate::ui::{Icon, LoadingSpinner, icon_button, palette};

#[derive(Default)]
pub(super) enum SyncButtonState {
    #[default]
    Idle,
    Running {
        stage: SyncStage,
        completed: u64,
        total: u64,
        ready_books: (u64, u64),
    },
    Complete {
        expires_at: Instant,
    },
    Failed {
        detail: String,
        retry_at: Option<Instant>,
    },
}

impl SyncButtonState {
    pub(super) fn starting() -> Self {
        Self::Running {
            stage: SyncStage::Preparing,
            completed: 0,
            total: 0,
            ready_books: (0, 0),
        }
    }

    pub(super) fn update_stage(&mut self, stage: SyncStage, completed: u64, total: u64) {
        let Self::Running { ready_books, .. } = self else {
            return;
        };
        if matches!(stage, SyncStage::ReadingApplied | SyncStage::Uploading) {
            return;
        }
        let ready_books = if stage == SyncStage::Books {
            (completed, total)
        } else {
            *ready_books
        };
        *self = Self::Running {
            stage,
            completed,
            total,
            ready_books,
        };
    }

    pub(super) fn book_ready(&mut self, total: u64) {
        if let Self::Running { ready_books, .. } = self {
            *ready_books = (ready_books.0.saturating_add(1).min(total), total);
        }
    }

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
                ready_books,
            } => sync_progress_text(language, *stage, *completed, *total, *ready_books),
            Self::Complete { expires_at } if Instant::now() < *expires_at => {
                language.text("同步完成", "Sync complete").into()
            }
            Self::Complete { .. } => language.text("云同步", "Cloud sync").into(),
            Self::Failed {
                retry_at: Some(deadline),
                ..
            } if *deadline > Instant::now() => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let now = chrono::Local::now();
                let retry = chrono::Duration::from_std(remaining)
                    .ok()
                    .and_then(|duration| now.checked_add_signed(duration))
                    .unwrap_or(now);
                let title =
                    language.text("同步暂缓 · 最早可重试时间", "Sync paused · Earliest retry");
                format!("{title} {}", retry.format("%H:%M"))
            }
            Self::Failed { detail, .. } => {
                format!("{}\n{detail}", language.text("同步失败", "Sync failed"))
            }
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
                    LoadingSpinner::new()
                        .color(palette().accent)
                        .paint_at(ui, rect.shrink(7.0));
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::ProgressIndicator,
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

fn sync_progress_text(
    language: AppLanguage,
    stage: SyncStage,
    completed: u64,
    total: u64,
    (ready, books_total): (u64, u64),
) -> String {
    let chinese = language.resolved() == AppLanguage::SimplifiedChinese;
    let phase = match stage {
        SyncStage::UploadingBooks if total > 0 => {
            return if chinese {
                format!("已上传 {completed}/{total} 本")
            } else {
                format!("{completed}/{total} books uploaded")
            };
        }
        SyncStage::Uploading | SyncStage::UploadingBooks => {
            language.text("上传书籍", "Uploading books")
        }
        SyncStage::PreparingDownload | SyncStage::Downloading | SyncStage::Books => {
            language.text("下载书籍", "Downloading books")
        }
        SyncStage::DerivedUploading
        | SyncStage::DerivedDownloading
        | SyncStage::CheckingDerivedData => {
            language.text("同步 PDF 处理数据", "Syncing PDF processing data")
        }
        SyncStage::Statistics => language.text("同步阅读统计", "Syncing reading statistics"),
        SyncStage::ReadingData => {
            language.text("同步进度与批注", "Syncing progress and annotations")
        }
        SyncStage::Checking | SyncStage::Discovering => {
            language.text("检查云端变更", "Checking cloud changes")
        }
        SyncStage::Preparing => language.text("准备同步", "Preparing sync"),
        SyncStage::ReadingApplied => {
            language.text("同步进度与批注", "Syncing progress and annotations")
        }
    };
    if books_total == 0 {
        return phase.into();
    }
    let readiness = if chinese {
        format!("已就绪 {ready}/{books_total} 本")
    } else {
        format!("{ready}/{books_total} books ready")
    };
    format!("{readiness}\n{phase}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_counts_survive_transfer_and_later_sync_phases() {
        let mut state = SyncButtonState::starting();
        state.update_stage(SyncStage::UploadingBooks, 1, 3);
        state.update_stage(SyncStage::Uploading, 40, 100);
        assert_eq!(state.hover_text(AppLanguage::English), "1/3 books uploaded");

        state.update_stage(SyncStage::Books, 8, 11);
        assert_eq!(
            state.hover_text(AppLanguage::English),
            "8/11 books ready\nDownloading books"
        );
        state.update_stage(SyncStage::Downloading, 100, 500);
        assert_eq!(
            state.hover_text(AppLanguage::English),
            "8/11 books ready\nDownloading books"
        );
        state.book_ready(11);
        for stage in [
            SyncStage::DerivedUploading,
            SyncStage::DerivedDownloading,
            SyncStage::CheckingDerivedData,
        ] {
            state.update_stage(stage, 300, 400);
            assert_eq!(
                state.hover_text(AppLanguage::English),
                "9/11 books ready\nSyncing PDF processing data"
            );
        }
        state.update_stage(SyncStage::Statistics, 0, 0);
        assert_eq!(
            state.hover_text(AppLanguage::English),
            "9/11 books ready\nSyncing reading statistics"
        );
    }

    #[test]
    fn running_sync_schedules_a_timer_after_layout_settles() {
        let ctx = egui::Context::default();
        let state = SyncButtonState::starting();
        let mut repaint_delay = Duration::ZERO;
        for pass in 0..5 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(640.0, 480.0),
                    )),
                    time: Some(f64::from(pass) * 0.1),
                    ..Default::default()
                },
                |ui| {
                    let _ = state.show(ui, false, AppLanguage::English);
                },
            );
            repaint_delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.textures_delta.clear();
        }
        assert!(repaint_delay > Duration::ZERO);
        assert!(repaint_delay <= Duration::from_millis(100));
    }

    #[test]
    fn sync_completion_expires_without_resetting_a_new_sync_or_error() {
        let now = Instant::now();
        let mut state = SyncButtonState::completed(now);
        assert_eq!(state.hover_text(AppLanguage::SimplifiedChinese), "同步完成");
        state.expire(now + Duration::from_secs(2));
        assert!(matches!(state, SyncButtonState::Complete { .. }));
        state.expire(now + Duration::from_secs(3));
        assert!(matches!(state, SyncButtonState::Idle));
        assert_eq!(state.hover_text(AppLanguage::English), "Cloud sync");
        state = SyncButtonState::starting();
        state.update_stage(SyncStage::UploadingBooks, 2, 10);
        state.expire(now + Duration::from_secs(10));
        assert!(matches!(state, SyncButtonState::Running { .. }));
        state = SyncButtonState::Failed {
            detail: "offline".into(),
            retry_at: None,
        };
        state.expire(now + Duration::from_secs(10));
        assert!(matches!(state, SyncButtonState::Failed { .. }));
    }

    #[test]
    fn failure_hover_explains_cooldown_and_keeps_settled_frames_idle() {
        let ctx = egui::Context::default();
        let state = SyncButtonState::Failed {
            detail: "HTTP 429".into(),
            retry_at: Some(Instant::now() + Duration::from_secs(300)),
        };
        assert!(
            state
                .hover_text(AppLanguage::English)
                .contains("Sync paused · Earliest retry")
        );
        assert!(
            state
                .hover_text(AppLanguage::SimplifiedChinese)
                .contains("同步暂缓 · 最早可重试时间")
        );
        let mut repaint_delay = Duration::ZERO;
        for pass in 0..6 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(640.0, 480.0),
                    )),
                    time: Some(f64::from(pass)),
                    ..Default::default()
                },
                |ui| {
                    state.show(ui, false, AppLanguage::English);
                },
            );
            repaint_delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.textures_delta.clear();
        }
        assert!(repaint_delay > Duration::from_secs(60));
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
        let mut state = SyncButtonState::starting();
        state.update_stage(SyncStage::UploadingBooks, 2, 5);
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
        assert!(text.iter().any(|text| text == "2/5 books uploaded"));
        state.update_stage(SyncStage::Books, 3, 6);
        state.update_stage(SyncStage::Downloading, 62, 100);
        let (text, _, _) = frame(&ctx, &state, 4.0, Vec::new());
        assert!(
            text.iter()
                .any(|text| text.contains("3/6 books ready") && text.contains("Downloading books"))
        );
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
