use super::SettingsFeature;
use crate::{fonts::Status, ui::palette};

pub(super) fn show(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    ui.add_space(10.0);
    for (name, status) in ["霞鹜文楷 GB Screen", "Literata", "Literata Italic"]
        .iter()
        .zip(&state.font_statuses)
    {
        if *status == Status::Ready {
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(*name);
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
        ui.horizontal_wrapped(|ui| {
            ui.label(language.text(
                "新字体已下载完成",
                "New fonts downloaded",
            ));
            if ui.small_button(language.text("应用", "Apply")).clicked() {
                state.font_apply_requested = true;
            }
        });
    }
}
