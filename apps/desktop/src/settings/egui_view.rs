use egui::{Align2, Color32, Response, RichText, Vec2};
use rebook_layout::{ReaderDefaultFont, SpreadMode};

use super::{SettingsFeature, SettingsTab};
use crate::plugins::{
    AiModelConfig, AiModelKind, AiProviderKind, CHAT_HISTORY_TURNS_MAX, CHAT_HISTORY_TURNS_MIN,
    CHAT_TOOL_STEPS_MAX, CHAT_TOOL_STEPS_MIN, PdfOcrProviderKind, PluginSettings,
    TARGET_LANGUAGE_ENGLISH, TARGET_LANGUAGE_INTERFACE, TARGET_LANGUAGE_SIMPLIFIED_CHINESE,
    TranslationMode,
};
use crate::preferences::{AppLanguage, AppTheme, ReadingMode};
use crate::sync::CloudProviderKind;
use crate::ui::{Icon, dialog_action_button, icon_button, navigation_button, palette};

const SETTINGS_SELECT_WIDTH: f32 = 156.0;
const SETTINGS_MODEL_KIND_WIDTH: f32 = 92.0;
const SETTINGS_MODEL_SELECT_WIDTH: f32 = 280.0;
const SETTINGS_FONT_SELECT_WIDTH: f32 = 260.0;
const SETTINGS_SCROLLBAR_GUTTER: f32 = 14.0;

struct ConfiguredModel {
    provider_id: String,
    provider_name: String,
    model: String,
}

pub(crate) fn settings_overlay(ctx: &egui::Context, state: &mut SettingsFeature) {
    let visible = ctx.animate_bool_with_time(
        egui::Id::new("settings-overlay-motion"),
        state.is_open(),
        0.18,
    );
    if visible <= f32::EPSILON {
        return;
    }
    ctx.request_repaint();

    let screen = ctx.content_rect();
    let modal_size = Vec2::new(
        (screen.width() - 40.0)
            .clamp(420.0, 720.0)
            .min(screen.width()),
        (screen.height() - 40.0)
            .clamp(380.0, 540.0)
            .min(screen.height()),
    );
    let offset = Vec2::new(0.0, (1.0 - visible) * 12.0);
    let modal_id = egui::Id::new("settings-modal");
    let modal_area = egui::Modal::default_area(modal_id).anchor(Align2::CENTER_CENTER, offset);
    let response = egui::Modal::new(modal_id)
        .area(modal_area)
        .backdrop_color(Color32::BLACK.gamma_multiply(0.46 * visible))
        .frame(
            egui::Frame::new()
                .fill(palette().surface)
                .stroke(egui::Stroke::NONE)
                .corner_radius(12)
                .inner_margin(0),
        )
        .show(ctx, |ui| {
            ui.set_width(modal_size.x);
            ui.set_height(modal_size.y);
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.style_mut().visuals.widgets.hovered.expansion = 0.0;
            ui.style_mut().visuals.widgets.active.expansion = 0.0;
            ui.style_mut().visuals.widgets.open.expansion = 0.0;
            let sidebar_width = 144.0_f32.min(modal_size.x * 0.32);
            let content_width = (modal_size.x - sidebar_width).max(1.0);
            ui.horizontal(|ui| {
                egui::Frame::new()
                    .fill(palette().background)
                    .corner_radius(egui::CornerRadius {
                        nw: 12,
                        ne: 0,
                        sw: 12,
                        se: 0,
                    })
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                        ui.set_width((sidebar_width - 28.0).max(1.0));
                        ui.set_height((modal_size.y - 28.0).max(1.0));
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            settings_sidebar(ui, state);
                        });
                    });
                egui::Frame::new()
                    .fill(palette().surface)
                    .corner_radius(egui::CornerRadius {
                        nw: 0,
                        ne: 12,
                        sw: 0,
                        se: 12,
                    })
                    .inner_margin(egui::Margin::symmetric(20, 16))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                        ui.set_width((content_width - 40.0).max(1.0));
                        ui.set_height((modal_size.y - 32.0).max(1.0));
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            settings_content(ui, state);
                        });
                    });
            });
        });
    ctx.layer_painter(response.response.layer_id).rect_stroke(
        response.response.rect,
        12,
        egui::Stroke::new(1.0, palette().border),
        egui::StrokeKind::Inside,
    );
    if response.should_close() {
        state.close_overlay();
    }
}

fn settings_sidebar(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    ui.heading(
        RichText::new(state.draft_language.text("设置", "Settings"))
            .size(crate::ui::scaled_font_size(19.0))
            .color(palette().text),
    );
    ui.add_space(18.0);
    for (tab, glyph, zh, en) in [
        (SettingsTab::Reading, Icon::BookOpen, "阅读", "Reading"),
        (SettingsTab::Font, Icon::Type, "字体", "Font"),
        (SettingsTab::Ai, Icon::Server, "AI 提供商", "AI providers"),
        (SettingsTab::AiChat, Icon::Bot, "AI 对话", "AI chat"),
        (SettingsTab::Ocr, Icon::ScanText, "OCR", "OCR"),
        (
            SettingsTab::Semantic,
            Icon::Search,
            "语义搜索",
            "Semantic search",
        ),
        (
            SettingsTab::Translation,
            Icon::Languages,
            "翻译",
            "Translation",
        ),
        (SettingsTab::Cloud, Icon::Cloud, "云盘", "Cloud"),
        (SettingsTab::About, Icon::Info, "关于", "About"),
    ] {
        let selected = state.settings_tab == tab;
        if navigation_button(ui, glyph, state.draft_language.text(zh, en), selected).clicked() {
            state.settings_tab = tab;
        }
        ui.add_space(3.0);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the settings shell keeps its tab routing and footer actions together"
)]
fn settings_content(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    const HEADER_HEIGHT: f32 = 42.0;
    const FOOTER_HEIGHT: f32 = 48.0;
    const SEPARATOR_HEIGHT: f32 = 1.0;

    let available = ui.available_size();
    let body_height =
        (available.y - HEADER_HEIGHT - FOOTER_HEIGHT - SEPARATOR_HEIGHT * 2.0).max(120.0);
    ui.spacing_mut().item_spacing.y = 0.0;

    ui.allocate_ui_with_layout(
        Vec2::new(available.x, HEADER_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let title = match state.settings_tab {
                SettingsTab::Reading => state.draft_language.text("阅读设置", "Reading"),
                SettingsTab::Font => state.draft_language.text("字体", "Font"),
                SettingsTab::Ai => state.draft_language.text("AI 提供商", "AI providers"),
                SettingsTab::AiChat => state.draft_language.text("AI 对话", "AI chat"),
                SettingsTab::Ocr => "OCR",
                SettingsTab::Semantic => state.draft_language.text("语义搜索", "Semantic search"),
                SettingsTab::Translation => state.draft_language.text("翻译", "Translation"),
                SettingsTab::Cloud => state.draft_language.text("云盘同步", "Cloud sync"),
                SettingsTab::About => state.draft_language.text("关于", "About"),
            };
            ui.heading(
                RichText::new(title)
                    .size(crate::ui::scaled_font_size(18.0))
                    .color(palette().text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::X)
                    .on_hover_text(state.draft_language.text("关闭", "Close"))
                    .clicked()
                {
                    state.close_overlay();
                }
            });
        },
    );
    ui.separator();
    configure_settings_scrollbar(ui);
    egui::ScrollArea::vertical()
        .max_height(body_height)
        .min_scrolled_height(body_height)
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
        .show(ui, |ui| {
            let content_width = (ui.available_width() - SETTINGS_SCROLLBAR_GUTTER).max(1.0);
            ui.set_width(content_width);
            ui.add_space(12.0);
            ui.vertical(|ui| match state.settings_tab {
                SettingsTab::Reading => reading_settings(ui, state),
                SettingsTab::Font => font_settings(ui, state),
                SettingsTab::Ai => ai_provider_settings(ui, state),
                SettingsTab::AiChat => ai_chat_settings(ui, state),
                SettingsTab::Ocr => ocr_settings(ui, state),
                SettingsTab::Semantic => semantic_settings(ui, state),
                SettingsTab::Translation => translation_settings(ui, state),
                SettingsTab::Cloud => cloud_settings(ui, state),
                SettingsTab::About => about_settings(ui, state),
            });
            if let Some(error) = &state.error {
                ui.add_space(8.0);
                ui.colored_label(palette().error, error);
            }
            ui.add_space(12.0);
        });
    ui.separator();
    ui.allocate_ui_with_layout(
        Vec2::new(available.x, FOOTER_HEIGHT),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            if state.settings_tab == SettingsTab::About {
                if dialog_action_button(ui, state.draft_language.text("关闭", "Close"), false)
                    .clicked()
                {
                    state.close_overlay();
                }
            } else {
                if dialog_action_button(ui, state.draft_language.text("保存", "Save"), true)
                    .clicked()
                {
                    state.apply_settings();
                }
                if dialog_action_button(ui, state.draft_language.text("取消", "Cancel"), false)
                    .clicked()
                {
                    state.close_overlay();
                }
            }
        },
    );
}

fn about_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    settings_card(ui, |ui| {
        egui::Grid::new("about-settings-grid")
            .num_columns(2)
            .spacing([28.0, 12.0])
            .show(ui, |ui| {
                field_label(ui, state.draft_language.text("应用", "Application"));
                ui.label(RichText::new("Torto · 小龟阅读").color(palette().text));
                ui.end_row();
                field_label(ui, state.draft_language.text("版本", "Version"));
                ui.label(RichText::new(env!("CARGO_PKG_VERSION")).color(palette().text));
                ui.end_row();
            });

        ui.add_space(14.0);
        #[cfg(target_os = "windows")]
        ui.horizontal(|ui| {
            let checking = matches!(
                state.update_check_status,
                super::UpdateCheckStatus::Checking
            );
            let check = ui.add_enabled_ui(!checking, |ui| {
                secondary_button_sized(
                    ui,
                    if checking {
                        state.draft_language.text("检查中...", "Checking...")
                    } else {
                        state.draft_language.text("检查", "Check")
                    },
                    72.0,
                )
            });
            if check.inner.clicked() {
                state.update_check_requested = true;
                state.update_check_status = super::UpdateCheckStatus::Checking;
            }
            let update_available = matches!(
                state.update_check_status,
                super::UpdateCheckStatus::Available(_)
            );
            let update = ui.add_enabled_ui(update_available, |ui| {
                secondary_button_sized(ui, state.draft_language.text("更新", "Update"), 72.0)
            });
            if update.inner.clicked() {
                state.update_requested = true;
            }
        });

        #[cfg(target_os = "windows")]
        {
            ui.add_space(8.0);
            match &state.update_check_status {
                super::UpdateCheckStatus::Idle | super::UpdateCheckStatus::Checking => {}
                super::UpdateCheckStatus::UpToDate => {
                    ui.colored_label(
                        palette().accent,
                        state
                            .draft_language
                            .text("当前已是最新版本。", "Torto is up to date."),
                    );
                }
                super::UpdateCheckStatus::Available(version) => {
                    ui.colored_label(
                        palette().accent,
                        format!(
                            "{} {version}",
                            state.draft_language.text("发现新版本", "Update available")
                        ),
                    );
                }
                super::UpdateCheckStatus::Failed(error) => {
                    ui.colored_label(
                        palette().error,
                        format!(
                            "{}: {error}",
                            state
                                .draft_language
                                .text("检查更新失败", "Update check failed")
                        ),
                    );
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        ui.label(
            RichText::new(state.draft_language.text(
                "当前平台暂不支持应用内自动更新。",
                "In-app updates are not available on this platform yet.",
            ))
            .color(palette().muted),
        );
    });
}

#[allow(
    clippy::too_many_lines,
    reason = "reader preferences are rendered as one cohesive settings grid"
)]
fn reading_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    settings_card(ui, |ui| {
        egui::Grid::new("reading-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_row_label(ui, state.draft_language.text("阅读模式", "Reading mode"));
                settings_row_control_sized(ui, 250.0, |ui| {
                    let classic = state.draft_reading_mode == ReadingMode::Classic;
                    if choice_button(
                        ui,
                        state.draft_language.text("经典", "Classic"),
                        classic,
                        72.0,
                    )
                    .clicked()
                    {
                        state.draft_reading_mode = ReadingMode::Classic;
                    }
                    let focus = state.draft_reading_mode == ReadingMode::Focus;
                    if choice_button(ui, state.draft_language.text("专注", "Focus"), focus, 72.0)
                        .clicked()
                    {
                        state.draft_reading_mode = ReadingMode::Focus;
                    }
                });
                ui.end_row();

                if state.draft_reading_mode == ReadingMode::Classic {
                    settings_row_label(ui, state.draft_language.text("分页模式", "Page layout"));
                    settings_row_control_sized(ui, 250.0, |ui| {
                        let single = state.draft_spread == SpreadMode::Single;
                        if choice_button(
                            ui,
                            state.draft_language.text("单页", "Single"),
                            single,
                            72.0,
                        )
                        .clicked()
                        {
                            state.draft_spread = SpreadMode::Single;
                        }
                        let double = state.draft_spread == SpreadMode::Double;
                        if choice_button(
                            ui,
                            state.draft_language.text("双页", "Double"),
                            double,
                            72.0,
                        )
                        .clicked()
                        {
                            state.draft_spread = SpreadMode::Double;
                        }
                        let scroll = state.draft_spread == SpreadMode::Scroll;
                        if choice_button(
                            ui,
                            state.draft_language.text("滑动", "Scroll"),
                            scroll,
                            72.0,
                        )
                        .clicked()
                        {
                            state.draft_spread = SpreadMode::Scroll;
                        }
                    });
                    ui.end_row();
                }

                settings_row_label(ui, state.draft_language.text("主题", "Theme"));
                settings_row_control_sized(ui, 250.0, |ui| {
                    let light = state.draft_theme == AppTheme::Light;
                    if choice_button(ui, state.draft_language.text("浅色", "Light"), light, 72.0)
                        .clicked()
                    {
                        state.draft_theme = AppTheme::Light;
                    }
                    let dark = state.draft_theme == AppTheme::Dark;
                    if choice_button(ui, state.draft_language.text("深色", "Dark"), dark, 72.0)
                        .clicked()
                    {
                        state.draft_theme = AppTheme::Dark;
                    }
                });
                ui.end_row();

                settings_row_label(ui, state.draft_language.text("界面语言", "Language"));
                settings_row_control_sized(ui, SETTINGS_SELECT_WIDTH, |ui| {
                    egui::ComboBox::from_id_salt("settings-language")
                        .width(SETTINGS_SELECT_WIDTH)
                        .selected_text(match state.draft_language {
                            AppLanguage::SimplifiedChinese => "简体中文",
                            AppLanguage::English => "English",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.draft_language,
                                AppLanguage::SimplifiedChinese,
                                "简体中文",
                            );
                            ui.selectable_value(
                                &mut state.draft_language,
                                AppLanguage::English,
                                "English",
                            );
                        });
                });
                ui.end_row();
            });
    });
}

fn font_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    interface_font_settings(
        ui,
        language,
        &mut state.draft_interface_typography,
        &state.available_interface_font_families,
    );
    ui.add_space(12.0);
    reader_font_settings(
        ui,
        language,
        &mut state.draft_typography,
        &state.available_font_families,
    );
}

fn interface_font_settings(
    ui: &mut egui::Ui,
    language: AppLanguage,
    interface_typography: &mut crate::preferences::InterfaceTypography,
    interface_font_families: &[String],
) {
    settings_card(ui, |ui| {
        ui.label(
            RichText::new(language.text("界面与 AI 对话", "Interface and AI chat"))
                .strong()
                .color(palette().text),
        );
        ui.add_space(12.0);
        egui::Grid::new("interface-font-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                font_family_row(
                    ui,
                    language.text("界面字体", "Interface font"),
                    "settings-interface-font",
                    &mut interface_typography.font_family,
                    interface_font_families,
                );
                settings_slider_row(
                    ui,
                    language.text("界面字号", "Interface font size"),
                    &mut interface_typography.font_size,
                    10.0,
                    24.0,
                    1.0,
                    " px",
                );
            });
    });
}

fn reader_font_settings(
    ui: &mut egui::Ui,
    language: AppLanguage,
    typography: &mut rebook_layout::ReaderTypography,
    font_families: &[String],
) {
    settings_card(ui, |ui| {
        ui.label(
            RichText::new(language.text("阅读正文", "Reading content"))
                .strong()
                .color(palette().text),
        );
        ui.add_space(12.0);
        egui::Grid::new("font-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_row_label(ui, language.text("默认字体", "Default font"));
                settings_row_control_sized(ui, SETTINGS_FONT_SELECT_WIDTH, |ui| {
                    let serif = typography.default_font == ReaderDefaultFont::Serif;
                    if choice_button(ui, language.text("衬线", "Serif"), serif, 72.0).clicked() {
                        typography.default_font = ReaderDefaultFont::Serif;
                    }
                    let sans_serif = typography.default_font == ReaderDefaultFont::SansSerif;
                    if choice_button(ui, language.text("无衬线", "Sans serif"), sans_serif, 72.0)
                        .clicked()
                    {
                        typography.default_font = ReaderDefaultFont::SansSerif;
                    }
                });
                ui.end_row();

                font_family_row(
                    ui,
                    language.text("中文字体", "CJK font"),
                    "settings-cjk-font",
                    &mut typography.default_cjk_font,
                    font_families,
                );
                font_family_row(
                    ui,
                    language.text("衬线字体", "Serif font"),
                    "settings-serif-font",
                    &mut typography.serif_font,
                    font_families,
                );
                font_family_row(
                    ui,
                    language.text("无衬线字体", "Sans-serif font"),
                    "settings-sans-font",
                    &mut typography.sans_serif_font,
                    font_families,
                );
                font_family_row(
                    ui,
                    language.text("等宽字体", "Monospace font"),
                    "settings-monospace-font",
                    &mut typography.monospace_font,
                    font_families,
                );

                settings_slider_row(
                    ui,
                    language.text("字号", "Font size"),
                    &mut typography.font_size,
                    12.0,
                    28.0,
                    1.0,
                    " px",
                );

                settings_slider_row(
                    ui,
                    language.text("最小字号", "Minimum font size"),
                    &mut typography.minimum_font_size,
                    8.0,
                    18.0,
                    1.0,
                    " px",
                );

                settings_u16_slider_row(
                    ui,
                    language.text("字重", "Font weight"),
                    &mut typography.font_weight,
                    300,
                    800,
                    100,
                );
            });
    });
}

fn ai_provider_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_plugin_settings;
    let mut remove_provider = None;
    let mut remove_model = None;
    let can_remove_provider = settings.providers.len() > 1;
    for index in 0..settings.providers.len() {
        settings_card(ui, |ui| {
            let provider = &mut settings.providers[index];
            field_label(ui, language.text("提供商", "Provider"));
            let mut selected_kind = provider.kind;
            egui::ComboBox::from_id_salt(("ai-provider-kind", &provider.id))
                .width(SETTINGS_SELECT_WIDTH)
                .selected_text(selected_kind.label())
                .show_ui(ui, |ui| {
                    for kind in AiProviderKind::ALL {
                        ui.selectable_value(&mut selected_kind, kind, kind.label());
                    }
                });
            if selected_kind != provider.kind {
                provider.select_kind(selected_kind);
            }
            ui.horizontal(|ui| {
                field_label(ui, language.text("名称", "Name"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if can_remove_provider
                        && icon_button(ui, Icon::Trash2)
                            .on_hover_text(language.text("删除服务", "Remove provider"))
                            .clicked()
                    {
                        remove_provider = Some(index);
                    }
                });
            });
            text_field(ui, &mut provider.name, false);
            if provider.kind == AiProviderKind::Custom {
                field_label(ui, "Base URL");
                text_field(ui, &mut provider.base_url, false);
            }
            field_label(ui, "API Key");
            text_field(ui, &mut provider.api_key, true);
            field_label(ui, language.text("模型", "Models"));
            for model_index in 0..provider.models.len() {
                let can_remove_model = provider.models.len() > 1;
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), 36.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ai_model_kind_selector(
                            ui,
                            &provider.id,
                            model_index,
                            &mut provider.models[model_index],
                        );
                        let available_width = if can_remove_model {
                            ui.available_width() - 40.0
                        } else {
                            ui.available_width()
                        };
                        let width = available_width.clamp(96.0, 520.0);
                        text_field_sized(ui, &mut provider.models[model_index].id, false, width);
                        if can_remove_model && icon_button(ui, Icon::Minus).clicked() {
                            remove_model = Some((index, model_index));
                        }
                    },
                );
            }
            if secondary_button(ui, language.text("添加模型", "Add model")).clicked() {
                provider.models.push(AiModelConfig::language(String::new()));
            }
        });
        ui.add_space(8.0);
    }
    if let Some((provider, model)) = remove_model {
        settings.remove_model(provider, model);
    }
    if let Some(index) = remove_provider {
        settings.remove_provider(index);
    }
    if secondary_button(ui, language.text("添加提供商", "Add provider")).clicked() {
        settings.add_provider();
    }
}

fn ai_model_kind_selector(
    ui: &mut egui::Ui,
    provider_id: &str,
    model_index: usize,
    model: &mut AiModelConfig,
) {
    let mut selected_kind = model.kind;
    egui::ComboBox::from_id_salt(("ai-model-kind", provider_id, model_index))
        .width(SETTINGS_MODEL_KIND_WIDTH)
        .selected_text(selected_kind.label())
        .show_ui(ui, |ui| {
            for kind in AiModelKind::ALL {
                ui.selectable_value(&mut selected_kind, kind, kind.label());
            }
        });
    model.kind = selected_kind;
}

fn ai_chat_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_plugin_settings;
    let options = configured_model_options(settings, AiModelKind::Language);
    settings_card(ui, |ui| {
        field_label(ui, language.text("对话模型", "Chat model"));
        configured_model_selector(
            ui,
            "chat-model",
            &options,
            &mut settings.chat_provider,
            &mut settings.chat_model,
            language,
        );
        ui.add_space(10.0);
        egui::Grid::new("ai-chat-limits-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_u16_slider_row(
                    ui,
                    language.text("工具调用轮数", "Tool call steps"),
                    &mut settings.chat_max_tool_steps,
                    CHAT_TOOL_STEPS_MIN,
                    CHAT_TOOL_STEPS_MAX,
                    1,
                );
                settings_u16_slider_row(
                    ui,
                    language.text("历史记录轮数", "History turns"),
                    &mut settings.chat_history_turns,
                    CHAT_HISTORY_TURNS_MIN,
                    CHAT_HISTORY_TURNS_MAX,
                    1,
                );
            });
    });
}

fn ocr_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_plugin_settings;
    let options = configured_model_options(settings, AiModelKind::Language);
    settings_card(ui, |ui| {
        ui.checkbox(
            &mut settings.ocr_enabled,
            language.text("启用元数据提取", "Enable metadata extraction"),
        );
        ui.add_space(8.0);
        field_label(ui, language.text("视觉识别模型", "Vision model"));
        configured_model_selector(
            ui,
            "ocr-model",
            &options,
            &mut settings.ocr_provider,
            &mut settings.ocr_model,
            language,
        );
    });
    ui.add_space(12.0);
    settings_card(ui, |ui| {
        ui.checkbox(
            &mut settings.pdf_ocr_enabled,
            language.text("启用 PDF 正文 OCR", "Enable PDF document OCR"),
        );
        ui.add_enabled_ui(settings.pdf_ocr_enabled, |ui| {
            ui.checkbox(
                &mut settings.pdf_ocr_reflow_enabled,
                language.text("启用内容重排", "Enable content reflow"),
            );
        });
        ui.add_space(8.0);
        field_label(ui, language.text("OCR 服务", "OCR provider"));
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 28.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                egui::ComboBox::from_id_salt("pdf-ocr-provider")
                    .width(SETTINGS_MODEL_SELECT_WIDTH)
                    .selected_text(settings.pdf_ocr_provider.label())
                    .show_ui(ui, |ui| {
                        for provider in PdfOcrProviderKind::ALL {
                            ui.selectable_value(
                                &mut settings.pdf_ocr_provider,
                                provider,
                                provider.label(),
                            );
                        }
                    });
                provider_credential_button(
                    ui,
                    settings.pdf_ocr_provider.credential_url(),
                    language.text("获取 API Token", "Get API token"),
                );
            },
        );
        ui.add_space(8.0);
        match settings.pdf_ocr_provider {
            PdfOcrProviderKind::PaddleOcr => {
                field_label(ui, language.text("识别模型", "Recognition model"));
                egui::ComboBox::from_id_salt("paddle-ocr-model")
                    .width(SETTINGS_MODEL_SELECT_WIDTH)
                    .selected_text(&settings.paddle_ocr_model)
                    .show_ui(ui, |ui| {
                        for model in [
                            "PaddleOCR-VL-1.6",
                            "PaddleOCR-VL-1.5",
                            "PaddleOCR-VL",
                            "PP-StructureV3",
                        ] {
                            ui.selectable_value(
                                &mut settings.paddle_ocr_model,
                                model.to_owned(),
                                model,
                            );
                        }
                    });
                field_label(ui, "Access Token");
                text_field(ui, &mut settings.paddle_ocr_token, true);
            }
            PdfOcrProviderKind::MinerU => {
                field_label(ui, language.text("识别模型", "Recognition model"));
                egui::ComboBox::from_id_salt("mineru-ocr-model")
                    .width(SETTINGS_MODEL_SELECT_WIDTH)
                    .selected_text(&settings.mineru_model)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut settings.mineru_model, "vlm".into(), "VLM");
                        ui.selectable_value(
                            &mut settings.mineru_model,
                            "pipeline".into(),
                            "Pipeline",
                        );
                    });
                field_label(ui, "API Token");
                text_field(ui, &mut settings.mineru_token, true);
            }
        }
    });
}

fn semantic_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_plugin_settings;
    let options = configured_model_options(settings, AiModelKind::Embedding);
    settings_card(ui, |ui| {
        ui.checkbox(
            &mut settings.semantic_search_enabled,
            language.text("启用语义搜索", "Enable semantic search"),
        );
        ui.add_space(10.0);
        field_label(ui, language.text("Embedding 模型", "Embedding model"));
        configured_model_selector(
            ui,
            "embedding-model",
            &options,
            &mut settings.embedding_provider,
            &mut settings.embedding_model,
            language,
        );
    });
}

fn translation_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_plugin_settings;
    let options = configured_model_options(settings, AiModelKind::Language);
    settings_card(ui, |ui| {
        field_label(ui, language.text("翻译模型", "Translation model"));
        configured_model_selector(
            ui,
            "translation-model",
            &options,
            &mut settings.translation_provider,
            &mut settings.translation_model,
            language,
        );
        field_label(ui, language.text("翻译目录", "Translate table of contents"));
        ui.horizontal(|ui| {
            if choice_button(
                ui,
                language.text("开启", "On"),
                settings.translate_toc,
                72.0,
            )
            .clicked()
            {
                settings.translate_toc = true;
            }
            if choice_button(
                ui,
                language.text("关闭", "Off"),
                !settings.translate_toc,
                72.0,
            )
            .clicked()
            {
                settings.translate_toc = false;
            }
        });
        field_label(ui, language.text("显示模式", "Display mode"));
        ui.horizontal(|ui| {
            let bilingual = settings.translation_mode == TranslationMode::Bilingual;
            if choice_button(ui, language.text("双语", "Bilingual"), bilingual, 72.0).clicked() {
                settings.translation_mode = TranslationMode::Bilingual;
            }
            let replace = settings.translation_mode == TranslationMode::Replace;
            if choice_button(ui, language.text("替换", "Replace"), replace, 72.0).clicked() {
                settings.translation_mode = TranslationMode::Replace;
            }
        });
        field_label(ui, language.text("目标语言", "Target language"));
        let selected_language = target_language_label(&settings.target_language, language);
        egui::ComboBox::from_id_salt("translation-target")
            .width(SETTINGS_SELECT_WIDTH)
            .selected_text(selected_language)
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.target_language,
                    TARGET_LANGUAGE_INTERFACE.into(),
                    language.text("跟随界面", "Interface language"),
                );
                ui.selectable_value(
                    &mut settings.target_language,
                    TARGET_LANGUAGE_SIMPLIFIED_CHINESE.into(),
                    "简体中文",
                );
                ui.selectable_value(
                    &mut settings.target_language,
                    TARGET_LANGUAGE_ENGLISH.into(),
                    "English",
                );
            });
    });
}

fn configured_model_options(settings: &PluginSettings, kind: AiModelKind) -> Vec<ConfiguredModel> {
    let mut options = Vec::new();
    for (index, provider) in settings.providers.iter().enumerate() {
        let provider_name = if provider.name.trim().is_empty() {
            format!("Provider {}", index + 1)
        } else {
            provider.name.trim().to_owned()
        };
        for model in &provider.models {
            let id = model.id.trim();
            if model.kind == kind && !id.is_empty() {
                options.push(ConfiguredModel {
                    provider_id: provider.id.clone(),
                    provider_name: provider_name.clone(),
                    model: id.to_owned(),
                });
            }
        }
    }
    options
}

fn configure_settings_scrollbar(ui: &mut egui::Ui) {
    let scroll = &mut ui.style_mut().spacing.scroll;
    scroll.floating = true;
    scroll.floating_allocated_width = 0.0;
    scroll.floating_width = 3.0;
    scroll.bar_width = 7.0;
    scroll.bar_inner_margin = 4.0;
    scroll.bar_outer_margin = 0.0;
    scroll.foreground_color = false;
    scroll.dormant_background_opacity = 0.0;
    scroll.active_background_opacity = 0.08;
    scroll.interact_background_opacity = 0.16;
    scroll.dormant_handle_opacity = 0.28;
    scroll.active_handle_opacity = 0.45;
    scroll.interact_handle_opacity = 0.68;
}

fn configured_model_selector(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    options: &[ConfiguredModel],
    selected_provider: &mut String,
    selected_model: &mut String,
    language: AppLanguage,
) {
    let selected_text = options
        .iter()
        .find(|option| option.provider_id == *selected_provider && option.model == *selected_model)
        .map_or_else(
            || language.text("请选择模型", "Select a model").into(),
            |option| format!("{} / {}", option.provider_name, option.model),
        );
    egui::ComboBox::from_id_salt(id_salt)
        .width(SETTINGS_MODEL_SELECT_WIDTH)
        .truncate()
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            if options.is_empty() {
                ui.weak(language.text(
                    "请先在 AI 提供商中添加模型",
                    "Add a model under AI providers first",
                ));
            }
            for option in options {
                let selected =
                    option.provider_id == *selected_provider && option.model == *selected_model;
                let label = format!("{} / {}", option.provider_name, option.model);
                if ui
                    .selectable_label(selected, label)
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    selected_provider.clone_from(&option.provider_id);
                    selected_model.clone_from(&option.model);
                }
            }
        });
}

fn font_family_selector(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    selected: &mut String,
    font_families: &[String],
) {
    let popup_state_id = ui.make_persistent_id((id_salt, "popup-was-open"));
    let was_open = ui
        .ctx()
        .data(|data| data.get_temp::<bool>(popup_state_id).unwrap_or(false));
    let selected_text = selected.clone();
    let response = egui::ComboBox::from_id_salt(id_salt)
        .width(SETTINGS_FONT_SELECT_WIDTH)
        .truncate()
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for family in font_families {
                let is_selected = *selected == *family;
                let response = ui.selectable_value(selected, family.clone(), family);
                if !was_open && is_selected {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
    let is_open = egui::ComboBox::is_open(ui.ctx(), response.response.id);
    ui.ctx()
        .data_mut(|data| data.insert_temp(popup_state_id, is_open));
}

fn choice_button(ui: &mut egui::Ui, text: &str, selected: bool, width: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if selected {
        palette().accent_soft
    } else if response.hovered() || response.is_pointer_button_down_on() {
        palette().surface_muted
    } else {
        palette().surface
    };
    let stroke = if selected {
        egui::Stroke::new(1.0, palette().accent.gamma_multiply(0.38))
    } else {
        egui::Stroke::new(1.0, palette().border)
    };
    let text_color = if selected {
        palette().accent
    } else {
        palette().text
    };
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            text,
            egui::TextStyle::Button.resolve(ui.style()),
            text_color,
        );
    }
    response
}

fn cloud_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_sync_settings;
    settings_card(ui, |ui| {
        ui.checkbox(
            &mut settings.enabled,
            language.text("启用 WebDAV 同步", "Enable WebDAV sync"),
        );
        ui.add_space(8.0);
        field_label(ui, language.text("云盘提供商", "Cloud provider"));
        let mut selected_provider = settings.provider;
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 28.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                egui::ComboBox::from_id_salt("cloud-provider")
                    .width(SETTINGS_SELECT_WIDTH)
                    .selected_text(selected_provider.label())
                    .show_ui(ui, |ui| {
                        for provider in CloudProviderKind::ALL {
                            ui.selectable_value(&mut selected_provider, provider, provider.label());
                        }
                    });
                provider_credential_button(
                    ui,
                    selected_provider.credential_url(),
                    language.text("获取连接凭据", "Get connection credentials"),
                );
            },
        );
        if selected_provider != settings.provider {
            settings.select_provider(selected_provider);
        }
        if settings.provider == CloudProviderKind::Custom {
            field_label(ui, "WebDAV URL");
            text_field(ui, &mut settings.base_url, false);
        }
        field_label(ui, language.text("用户名", "Username"));
        text_field(ui, &mut settings.username, false);
        field_label(ui, language.text("密码", "Password"));
        text_field(ui, &mut state.draft_sync_password, true);
        field_label(ui, language.text("设备名称", "Device name"));
        text_field(ui, &mut settings.device_name, false);
    });
}

fn provider_credential_button(ui: &mut egui::Ui, url: &str, tooltip: &str) {
    if icon_button(ui, Icon::ExternalLink)
        .on_hover_text(tooltip)
        .clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

fn target_language_label(value: &str, language: AppLanguage) -> String {
    match value {
        TARGET_LANGUAGE_INTERFACE => language.text("跟随界面", "Interface language").into(),
        TARGET_LANGUAGE_SIMPLIFIED_CHINESE => "简体中文".into(),
        TARGET_LANGUAGE_ENGLISH => "English".into(),
        _ => value.into(),
    }
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(crate::ui::scaled_font_size(12.0))
            .color(palette().muted),
    );
}

fn settings_row_label(ui: &mut egui::Ui, text: &str) {
    settings_row_control(ui, |ui| field_label(ui, text));
}

fn settings_row_control(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    settings_row_control_sized(ui, 0.0, content);
}

fn settings_row_control_sized(ui: &mut egui::Ui, width: f32, content: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 32.0),
        egui::Layout::left_to_right(egui::Align::Center),
        content,
    );
}

fn font_family_row(
    ui: &mut egui::Ui,
    label: &str,
    id_salt: &'static str,
    selected: &mut String,
    font_families: &[String],
) {
    settings_row_label(ui, label);
    settings_row_control_sized(ui, SETTINGS_FONT_SELECT_WIDTH, |ui| {
        font_family_selector(ui, id_salt, selected, font_families);
    });
    ui.end_row();
}

fn settings_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    minimum: f32,
    maximum: f32,
    step: f32,
    suffix: &str,
) {
    settings_row_label(ui, label);
    settings_row_control_sized(ui, 250.0, |ui| {
        let (response, next) = smooth_numeric_slider(ui, *value, minimum, maximum, step, suffix);
        if response.changed() {
            *value = next;
        }
    });
    ui.end_row();
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn settings_u16_slider_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u16,
    minimum: u16,
    maximum: u16,
    step: u16,
) {
    settings_row_label(ui, label);
    settings_row_control_sized(ui, 250.0, |ui| {
        let (response, next) = smooth_numeric_slider(
            ui,
            f32::from(*value),
            f32::from(minimum),
            f32::from(maximum),
            f32::from(step),
            "",
        );
        if response.changed() {
            *value = next.round() as u16;
        }
    });
    ui.end_row();
}

fn smooth_numeric_slider(
    ui: &mut egui::Ui,
    value: f32,
    minimum: f32,
    maximum: f32,
    step: f32,
    suffix: &str,
) -> (Response, f32) {
    let desired_size = Vec2::new(250.0, 24.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());
    let value_width = 48.0;
    let track_rect = egui::Rect::from_center_size(
        egui::pos2(
            rect.left() + (rect.width() - value_width) / 2.0,
            rect.center().y,
        ),
        Vec2::new(rect.width() - value_width - 8.0, 6.0),
    );
    let span = (maximum - minimum).max(f32::EPSILON);
    let mut next = value.clamp(minimum, maximum);
    if (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let normalized = ((pointer.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
        let raw = minimum + normalized * span;
        next = ((raw - minimum) / step).round() * step + minimum;
        next = next.clamp(minimum, maximum);
        if (next - value).abs() > f32::EPSILON {
            response.mark_changed();
        }
    }

    let normalized = (next - minimum) / span;
    let thumb_x = egui::lerp(track_rect.left()..=track_rect.right(), normalized);
    let filled_rect = egui::Rect::from_min_max(
        track_rect.min,
        egui::pos2(thumb_x.max(track_rect.left() + 3.0), track_rect.bottom()),
    );
    let thumb_rect = egui::Rect::from_center_size(
        egui::pos2(thumb_x, track_rect.center().y),
        Vec2::new(4.0, 18.0),
    );
    ui.painter()
        .rect_filled(track_rect, 3.0, palette().surface_muted);
    ui.painter()
        .rect_filled(filled_rect, 3.0, palette().accent_soft);
    ui.painter().rect_filled(thumb_rect, 2.0, palette().accent);
    ui.painter().text(
        egui::pos2(rect.right(), rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{next:.0}{suffix}"),
        egui::TextStyle::Body.resolve(ui.style()),
        palette().text,
    );
    response = response.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    (response, next)
}

fn settings_card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width();
    egui::Frame::new()
        .fill(palette().card_fill)
        .stroke(egui::Stroke::new(1.0, palette().border))
        .corner_radius(9)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width((width - 28.0).max(1.0));
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            content(ui);
        });
}

fn text_field(ui: &mut egui::Ui, value: &mut String, password: bool) -> Response {
    let width = ui.available_width().clamp(180.0, 520.0);
    text_field_sized(ui, value, password, width)
}

fn text_field_sized(ui: &mut egui::Ui, value: &mut String, password: bool, width: f32) -> Response {
    ui.add_sized(
        [width, 36.0],
        egui::TextEdit::singleline(value)
            .password(password)
            .vertical_align(egui::Align::Center)
            .margin(egui::Margin::symmetric(10, 0)),
    )
}

fn secondary_button(ui: &mut egui::Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(palette().accent))
            .min_size(Vec2::new(0.0, 32.0))
            .fill(crate::ui::palette().accent_soft)
            .stroke(egui::Stroke::new(
                1.0,
                palette().accent.gamma_multiply(0.22),
            ))
            .corner_radius(6),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn secondary_button_sized(ui: &mut egui::Ui, text: &str, width: f32) -> Response {
    ui.add_sized(
        [width, 32.0],
        egui::Button::new(RichText::new(text).color(palette().accent))
            .fill(crate::ui::palette().accent_soft)
            .stroke(egui::Stroke::new(
                1.0,
                palette().accent.gamma_multiply(0.22),
            ))
            .corner_radius(6),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_buttons_keep_dynamic_and_fixed_widths_separate() {
        egui::__run_test_ui(|ui| {
            let dynamic = secondary_button(ui, "Add provider");
            let fixed = secondary_button_sized(ui, "Check", 72.0);

            assert!(dynamic.rect.width() > 0.0);
            assert!((dynamic.rect.height() - 32.0).abs() < 0.01);
            assert!((fixed.rect.width() - 72.0).abs() < 0.01);
            assert!((fixed.rect.height() - 32.0).abs() < 0.01);
        });
    }
}
