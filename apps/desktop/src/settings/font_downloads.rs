use super::SettingsFeature;
use crate::{fonts::Status, ui::palette};

pub(super) fn show(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let fonts = &crate::fonts::catalog::catalog().fonts;
    let mut shown_any = false;
    for (index, asset) in fonts.iter().enumerate() {
        let Some(status) = state.font_statuses.get(index) else {
            continue;
        };
        if *status == Status::Ready || (!asset.preload && *status == Status::Pending) {
            continue;
        }
        if !shown_any {
            ui.add_space(10.0);
            shown_any = true;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(asset.label());
            let response = ui.colored_label(palette().muted, status.label(language));
            if let Status::Failed(error) = status {
                response.on_hover_text(error);
                if ui.small_button(language.text("重试", "Retry")).clicked() {
                    state.font_retry_requested = true;
                }
            }
        });
    }
    if state.font_apply_available {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(language.text("默认字体下载完成", "Default fonts downloaded"));
            if ui.small_button(language.text("应用", "Apply")).clicked() {
                state.font_apply_requested = true;
            }
        });
    }
}
