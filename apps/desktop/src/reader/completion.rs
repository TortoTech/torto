use super::{DesktopReader, ReaderFramePlan};

pub(super) struct CompletionPage {
    elapsed_ms: u64,
    finished: Option<String>,
    error: Option<String>,
    focus_pending: bool,
    content_height: f32,
}

impl DesktopReader {
    pub(super) fn open_completion_page(&mut self) {
        if self.completion.is_some() {
            return;
        }
        self.statistics
            .tick(false, false, self.snapshot.total_progression);
        let summary = self.statistics.completion_summary();
        let (elapsed_ms, finished, error) = match summary {
            Ok((elapsed, finished)) => (elapsed, finished, None),
            Err(error) => (0, None, Some(error.to_string())),
        };
        self.cancel_text_selection();
        self.ui.focus_actions_visible = false;
        self.ui.focus_footnotes_visible = false;
        self.ui.focus_scroll_motion = None;
        self.pending_focus_wheel_turn = None;
        self.selected_image = None;
        self.completion = Some(CompletionPage {
            elapsed_ms,
            finished,
            error,
            focus_pending: true,
            content_height: 170.0,
        });
    }

    pub(super) fn completion_page_ui(
        &mut self,
        root: &mut egui::Ui,
        blocked: bool,
    ) -> ReaderFramePlan {
        let ctx = root.ctx().clone();
        if !blocked && ctx.input_mut(|i| i.consume_shortcut(&self.shortcuts.return_to_shelf)) {
            self.request_exit();
        }
        let backwards = !blocked
            && ctx.input_mut(|i| {
                [
                    egui::Key::ArrowUp,
                    egui::Key::ArrowLeft,
                    egui::Key::PageUp,
                    egui::Key::Escape,
                ]
                .into_iter()
                .any(|key| i.consume_key(egui::Modifiers::NONE, key))
            });
        let colors = crate::ui::palette();
        let mut rect = egui::Rect::NOTHING;
        let mut action = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(colors.background))
            .show(root, |ui| {
                rect = ui.available_rect_before_wrap();
                let page = self.completion.as_mut().expect("completion page is open");
                let height = page.content_height;
                let bounds = egui::Rect::from_center_size(
                    rect.center(),
                    egui::vec2(rect.width().min(420.0), height),
                );
                let measured = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(bounds), |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new(self.language.text("全书完", "The End"))
                                    .size(30.0)
                                    .strong()
                                    .color(colors.text),
                            );
                            if page.elapsed_ms > 0 {
                                ui.add_space(12.0);
                                let minutes = page.elapsed_ms / 60_000;
                                let label = if minutes == 0 {
                                    self.language
                                        .text("已阅读不足1分钟", "Read for less than a minute")
                                        .to_owned()
                                } else if self.language.resolved()
                                    == crate::preferences::AppLanguage::SimplifiedChinese
                                {
                                    format!("已阅读 {minutes} 分钟")
                                } else {
                                    format!("Read for {minutes} minutes")
                                };
                                ui.label(egui::RichText::new(label).color(colors.muted));
                            }
                            if let Some(date) = &page.finished {
                                ui.add_space(12.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {date}",
                                        self.language.text("已读完", "Finished")
                                    ))
                                    .color(colors.muted),
                                );
                            }
                            ui.add_space(32.0);
                            let response = ui
                                .add_enabled_ui(!blocked, |ui| {
                                    crate::ui::dialog_action_button(
                                        ui,
                                        if page.finished.is_some() {
                                            self.language.text("返回书架", "Back to library")
                                        } else {
                                            self.language.text("标记读完", "Mark finished")
                                        },
                                        true,
                                    )
                                })
                                .inner;
                            if page.focus_pending && !blocked {
                                response.request_focus();
                                page.focus_pending = false;
                            }
                            action = response.clicked();
                            if let Some(error) = &page.error {
                                ui.colored_label(colors.error_text, error);
                            }
                        });
                    })
                    .response
                    .rect
                    .height();
                if (measured - page.content_height).abs() > 0.5 {
                    page.content_height = measured;
                    ctx.request_repaint();
                }
            });
        if action {
            if self
                .completion
                .as_ref()
                .is_some_and(|p| p.finished.is_some())
            {
                self.request_exit();
            } else {
                self.statistics.mark_finished();
                let summary = self.statistics.completion_summary();
                if let Some(page) = &mut self.completion {
                    match summary {
                        Ok((time, finished)) => {
                            page.elapsed_ms = time;
                            page.finished = finished;
                            page.error = None;
                            page.focus_pending = true;
                        }
                        Err(error) => page.error = Some(error.to_string()),
                    }
                }
            }
        }
        let wheel_back = !blocked
            && ctx.input(|i| {
                i.pointer.hover_pos().is_some_and(|p| rect.contains(p))
                    && i.events
                        .iter()
                        .any(|e| matches!(e,egui::Event::MouseWheel {delta,..} if delta.y>0.0))
            });
        if backwards || wheel_back {
            self.completion = None;
            self.scroll_target_position = None;
            self.bump_scene_revision();
        }
        ctx.set_cursor_icon(egui::CursorIcon::Default);
        ReaderFramePlan {
            rect,
            scene_id: self.scene_id,
            scene_revision: self.scene_revision,
            background: peniko::Color::from_rgba8(
                colors.background.r(),
                colors.background.g(),
                colors.background.b(),
                255,
            ),
        }
    }
}
