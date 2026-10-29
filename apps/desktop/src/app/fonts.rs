use super::DesktopApp;

impl DesktopApp {
    pub(super) fn font_notice(&mut self, ctx: &egui::Context) {
        if self.settings.is_open()
            || self.feedback.is_open()
            || self.shelf.opening_visible()
            || self.font_notice_dismissed == Some(self.fonts.revision)
        {
            return;
        }
        let Some(reader) = self.reader.as_ref() else {
            return;
        };
        if reader.has_status_notice() {
            return;
        }
        let ready = self.fonts.settled() && self.settings.font_apply_available;
        let missing = reader.missing_font_resources();
        if !ready && !missing {
            return;
        }
        let language = self.settings.applied().language;
        let mut apply = false;
        let mut dismiss = false;
        let mut retry = false;
        let message = if ready {
            language.text(
                "新字体已下载完成",
                "New fonts downloaded",
            )
        } else {
            language.text("此 PDF 缺少可用字体，部分文字可能显示不全。默认字体就绪后可应用修复", "This PDF needs a missing font. Apply the default fonts when ready to restore its text")
        };
        dismiss |= crate::ui::show_toast_with_actions(
            ctx,
            "reader-font-notice",
            message,
            if ready {
                crate::ui::ToastKind::Success
            } else {
                crate::ui::ToastKind::Error
            },
            egui::vec2(-18.0, 62.0),
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ready {
                        apply = crate::ui::dialog_action_button(
                            ui,
                            language.text("应用", "Apply"),
                            true,
                        )
                        .clicked();
                    }
                    if self
                        .fonts
                        .statuses
                        .iter()
                        .any(|status| matches!(status, crate::fonts::Status::Failed(_)))
                    {
                        retry = crate::ui::dialog_action_button(
                            ui,
                            language.text("重试下载", "Retry download"),
                            true,
                        )
                        .clicked();
                    }
                    dismiss =
                        crate::ui::dialog_action_button(ui, language.text("稍后", "Later"), false)
                            .clicked();
                });
            },
        );
        if retry {
            self.fonts.retry();
        }
        if dismiss {
            self.font_notice_dismissed = Some(self.fonts.revision);
        }
        if apply && let Some(reader) = self.reader.as_mut() {
            reader.apply_downloaded_fonts();
        }
    }
}
