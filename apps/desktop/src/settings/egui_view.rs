use egui::{Align2, Color32, Response, RichText, Vec2};
use rebook_layout::{ReaderDefaultFont, SpreadMode, TypesettingMode};

use super::{ProviderModelsRequest, SettingsFeature, SettingsTab, ShortcutAction};
use crate::plugins::{
    AiModelConfig, AiProviderKind, CHAT_HISTORY_TURNS_MAX, CHAT_HISTORY_TURNS_MIN,
    CHAT_TOOL_STEPS_MAX, CHAT_TOOL_STEPS_MIN, PdfOcrProviderKind, PluginSettings,
    TARGET_LANGUAGE_ENGLISH, TARGET_LANGUAGE_SIMPLIFIED_CHINESE, TARGET_LANGUAGE_SYSTEM,
    TranslationMode,
};
use crate::preferences::{
    AppLanguage, AppTheme, MAX_SHORTCUT_KEYS, ReadingMode, SYSTEM_INTERFACE_FONT,
    shortcut_chord_key_count,
};
use crate::sync::CloudProviderKind;
use crate::ui::{
    Icon, icon, icon_button, navigation_button, paint_icon, palette, small_icon_button,
};

const SETTINGS_SELECT_WIDTH: f32 = 156.0;
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
            ui.allocate_ui_with_layout(
                modal_size,
                egui::Layout::left_to_right(egui::Align::Min),
                |ui| {
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
                },
            );
        });
    ctx.layer_painter(response.response.layer_id).rect_stroke(
        response.response.rect,
        12,
        egui::Stroke::new(1.0, palette().border),
        egui::StrokeKind::Inside,
    );
    if response.should_close() {
        state.close_and_apply();
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
        (SettingsTab::System, Icon::Settings, "系统", "System"),
        (
            SettingsTab::Typography,
            Icon::BookOpen,
            "排版",
            "Typography",
        ),
        (
            SettingsTab::Shortcuts,
            Icon::Keyboard,
            "快捷键",
            "Shortcuts",
        ),
        (SettingsTab::Ai, Icon::Server, "AI 提供商", "AI providers"),
        (SettingsTab::AiChat, Icon::Bot, "对话", "Chat"),
        (
            SettingsTab::Translation,
            Icon::Languages,
            "翻译",
            "Translation",
        ),
        (SettingsTab::Ocr, Icon::ScanText, "PDF OCR", "PDF OCR"),
        (SettingsTab::Cloud, Icon::Cloud, "云同步", "Cloud sync"),
        (SettingsTab::About, Icon::Info, "关于", "About"),
    ] {
        let selected = state.settings_tab == tab;
        if navigation_button(ui, glyph, state.draft_language.text(zh, en), selected).clicked() {
            state.settings_tab = tab;
            state.capturing_shortcut = None;
        }
        ui.add_space(3.0);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the settings shell keeps its tab routing and content together"
)]
fn settings_content(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    const HEADER_HEIGHT: f32 = 42.0;
    const SEPARATOR_HEIGHT: f32 = 1.0;

    let available = ui.available_size();
    let body_height = (available.y - HEADER_HEIGHT - SEPARATOR_HEIGHT).max(120.0);
    ui.spacing_mut().item_spacing.y = 0.0;

    ui.allocate_ui_with_layout(
        Vec2::new(available.x, HEADER_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            let title = match state.settings_tab {
                SettingsTab::System => state.draft_language.text("系统", "System"),
                SettingsTab::Typography => state.draft_language.text("排版", "Typography"),
                SettingsTab::Shortcuts => state.draft_language.text("快捷键", "Shortcuts"),
                SettingsTab::Ai => state.draft_language.text("AI 提供商", "AI providers"),
                SettingsTab::AiChat => state.draft_language.text("对话", "Chat"),
                SettingsTab::Ocr => "PDF OCR",
                SettingsTab::Translation => state.draft_language.text("翻译", "Translation"),
                SettingsTab::Cloud => state.draft_language.text("云同步", "Cloud sync"),
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
                    state.close_and_apply();
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
                SettingsTab::System => system_settings(ui, state),
                SettingsTab::Typography => typography_settings(ui, state),
                SettingsTab::Shortcuts => shortcut_settings(ui, state),
                SettingsTab::Ai => ai_provider_settings(ui, state),
                SettingsTab::AiChat => ai_chat_settings(ui, state),
                SettingsTab::Ocr => ocr_settings(ui, state),
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
}

fn shortcut_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    capture_shortcut_input(ui, state);
    let language = state.draft_language;
    shortcut_group(
        ui,
        state,
        "layout-shortcuts-grid",
        language.text("布局", "Layout"),
        &[
            (
                ShortcutAction::Fullscreen,
                language.text("全屏", "Fullscreen"),
            ),
            (
                ShortcutAction::ToggleLeftSidebar,
                language.text("切换左侧栏", "Toggle left sidebar"),
            ),
            (
                ShortcutAction::ToggleRightSidebar,
                language.text("切换对话框", "Toggle chat panel"),
            ),
        ],
    );
    ui.add_space(12.0);
    shortcut_group(
        ui,
        state,
        "operation-shortcuts-grid",
        language.text("操作", "Actions"),
        &[
            (
                ShortcutAction::ToggleTranslation,
                language.text("翻译开关", "Toggle translation"),
            ),
            (
                ShortcutAction::ReturnToShelf,
                language.text("返回书架", "Back to library"),
            ),
        ],
    );
    ui.add_space(12.0);
    shortcut_group(
        ui,
        state,
        "focus-shortcuts-grid",
        language.text("专注模式", "Focus mode"),
        &[
            (
                ShortcutAction::FocusActions,
                language.text("唤起工具栏", "Open action toolbar"),
            ),
            (
                ShortcutAction::FocusChat,
                language.text("唤起对话框", "Open chat panel"),
            ),
            (
                ShortcutAction::FocusHighlight,
                language.text("高亮段落", "Highlight paragraph"),
            ),
            (
                ShortcutAction::FocusNote,
                language.text("唤起批注框", "Open note panel"),
            ),
            (
                ShortcutAction::FocusStructure,
                language.text("按句分段", "Split by sentence"),
            ),
            (
                ShortcutAction::FocusFootnotes,
                language.text("脚注开关", "Toggle footnotes"),
            ),
        ],
    );
    ui.add_space(12.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if secondary_button(ui, language.text("恢复默认", "Restore defaults")).clicked() {
            state.draft_shortcuts = crate::preferences::ShortcutPreferences::default();
            state.capturing_shortcut = None;
            state.error = None;
        }
    });
    if state.draft_shortcuts.has_conflicts() {
        ui.add_space(8.0);
        ui.colored_label(
            palette().error,
            language.text(
                "快捷键存在重复，请重新设置冲突项",
                "Some shortcuts conflict. Reassign the conflicting actions.",
            ),
        );
    }
    if state.draft_shortcuts.has_oversized_chords() {
        ui.add_space(8.0);
        ui.colored_label(
            palette().error,
            language.text(
                "快捷键最多支持三个按键",
                "Shortcuts support up to three keys.",
            ),
        );
    }
}

fn shortcut_group(
    ui: &mut egui::Ui,
    state: &mut SettingsFeature,
    grid_id: &'static str,
    title: &str,
    actions: &[(ShortcutAction, &str)],
) {
    settings_card(ui, |ui| {
        settings_module_label(ui, title);
        ui.add_space(4.0);
        egui::Grid::new(grid_id)
            .num_columns(2)
            .spacing([24.0, 12.0])
            .show(ui, |ui| {
                for &(action, label) in actions {
                    settings_row_label(ui, label);
                    settings_row_control_sized(ui, 180.0, |ui| {
                        shortcut_binding_button(ui, state, action);
                    });
                    ui.end_row();
                }
            });
    });
}

fn shortcut_binding_button(ui: &mut egui::Ui, state: &mut SettingsFeature, action: ShortcutAction) {
    let capturing = state.capturing_shortcut == Some(action);
    let label = if capturing {
        let modifiers = ui.input(|input| canonical_shortcut_modifiers(input.modifiers));
        shortcut_capture_label(ui.ctx(), state.draft_language, modifiers)
    } else {
        ui.ctx()
            .format_shortcut(&action.binding(&state.draft_shortcuts))
    };
    let response = ui
        .add_sized(
            [180.0, 32.0],
            egui::Button::new(RichText::new(label).color(if capturing {
                palette().surface
            } else {
                palette().text
            }))
            .fill(if capturing {
                palette().accent
            } else {
                palette().surface_muted
            })
            .stroke(egui::Stroke::new(
                1.0,
                if capturing {
                    palette().accent
                } else {
                    palette().border
                },
            ))
            .corner_radius(6),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if response.clicked() {
        state.capturing_shortcut = if capturing { None } else { Some(action) };
        state.error = None;
    }
}

fn shortcut_capture_label(
    ctx: &egui::Context,
    language: AppLanguage,
    modifiers: egui::Modifiers,
) -> String {
    if shortcut_chord_key_count(modifiers) > MAX_SHORTCUT_KEYS {
        return language.text("最多支持三个按键", "Up to three keys").into();
    }
    let modifiers = ctx.format_modifiers(modifiers);
    if modifiers.is_empty() {
        language.text("请按快捷键…", "Press shortcut…").into()
    } else {
        format!("{modifiers}+…")
    }
}

fn capture_shortcut_input(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let Some(action) = state.capturing_shortcut else {
        return;
    };
    let captured = ui.input_mut(|input| {
        let captured = input.events.iter().find_map(|event| {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                return None;
            };
            if should_ignore_shortcut_capture_key(action, *key) {
                return None;
            }
            Some((*key, *modifiers))
        });
        if let Some((key, modifiers)) = captured {
            input.consume_key(modifiers, key);
        }
        captured
    });
    let Some((key, modifiers)) = captured else {
        return;
    };
    if key == egui::Key::Escape {
        state.capturing_shortcut = None;
        return;
    }
    let modifiers = if action == ShortcutAction::FocusFootnotes && key == egui::Key::AltLeft {
        egui::Modifiers::NONE
    } else {
        canonical_shortcut_modifiers(modifiers)
    };
    if shortcut_chord_key_count(modifiers) > MAX_SHORTCUT_KEYS {
        state.error = Some(
            state
                .draft_language
                .text(
                    "快捷键最多支持三个按键",
                    "Shortcuts support up to three keys.",
                )
                .into(),
        );
        return;
    }
    action.set_binding(
        &mut state.draft_shortcuts,
        egui::KeyboardShortcut::new(modifiers, key),
    );
    state.capturing_shortcut = None;
    state.error = None;
}

fn is_shortcut_modifier_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

fn should_ignore_shortcut_capture_key(action: ShortcutAction, key: egui::Key) -> bool {
    is_shortcut_modifier_key(key)
        && !(action == ShortcutAction::FocusFootnotes && key == egui::Key::AltLeft)
}

fn canonical_shortcut_modifiers(modifiers: egui::Modifiers) -> egui::Modifiers {
    egui::Modifiers {
        alt: modifiers.alt,
        ctrl: modifiers.ctrl,
        shift: modifiers.shift,
        mac_cmd: modifiers.mac_cmd,
        command: modifiers.command && !modifiers.ctrl && !modifiers.mac_cmd,
    }
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
        if crate::updater::manual_updates_supported() {
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
        } else {
            ui.label(
                RichText::new(state.draft_language.text(
                    "更新由 Microsoft Store 管理。",
                    "Updates are managed by Microsoft Store.",
                ))
                .color(palette().muted),
            );
        }

        #[cfg(target_os = "windows")]
        if crate::updater::manual_updates_supported() {
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
fn system_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    settings_card(ui, |ui| {
        egui::Grid::new("system-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_row_label(ui, state.draft_language.text("主题", "Theme"));
                settings_row_control_sized(ui, 330.0, |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (theme, glyph) in [
                        (AppTheme::System, Icon::Monitor),
                        (AppTheme::Light, Icon::Sun),
                        (AppTheme::Dark, Icon::Moon),
                    ] {
                        if choice_icon_button(
                            ui,
                            glyph,
                            theme_label(state.draft_language, theme),
                            state.draft_theme == theme,
                        )
                        .clicked()
                        {
                            state.draft_theme = theme;
                            crate::ui::set_theme(ui.ctx(), theme);
                            crate::ui::apply_visuals(ui.ctx(), &crate::ui::palette());
                            ui.ctx().request_repaint();
                        }
                    }
                });
                ui.end_row();

                settings_row_label(ui, state.draft_language.text("界面语言", "Language"));
                settings_row_control_sized(ui, SETTINGS_FONT_SELECT_WIDTH, |ui| {
                    let follow_system_label =
                        state.draft_language.text("跟随系统", "Follow system");
                    egui::ComboBox::from_id_salt("settings-language")
                        .width(SETTINGS_FONT_SELECT_WIDTH)
                        .selected_text(match state.draft_language {
                            AppLanguage::System => follow_system_label,
                            AppLanguage::SimplifiedChinese => "简体中文",
                            AppLanguage::English => "English",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.draft_language,
                                AppLanguage::System,
                                follow_system_label,
                            );
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

                font_family_row(
                    ui,
                    state.draft_language.text("界面字体", "Interface font"),
                    "settings-interface-font",
                    &mut state.draft_interface_typography.font_family,
                    &state.available_interface_font_families,
                    Some(state.draft_language),
                );
                settings_slider_row(
                    ui,
                    state.draft_language.text("界面字号", "Interface font size"),
                    &mut state.draft_interface_typography.font_size,
                    10.0,
                    24.0,
                    1.0,
                    " px",
                );
            });
    });
}

fn typography_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    reader_font_settings(
        ui,
        language,
        &mut state.draft_reading_mode,
        &mut state.draft_spread,
        &mut state.draft_typography,
        &mut state.draft_typesetting,
        &state.available_reader_font_families,
    );
}

fn reader_font_settings(
    ui: &mut egui::Ui,
    language: AppLanguage,
    reading_mode: &mut ReadingMode,
    spread: &mut SpreadMode,
    typography: &mut rebook_layout::ReaderTypography,
    typesetting: &mut rebook_layout::ReaderTypesetting,
    font_families: &rebook_layout::ReaderFontFamilies,
) {
    settings_card(ui, |ui| {
        egui::Grid::new("typography-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_row_label(ui, language.text("界面布局", "Interface layout"));
                settings_row_control_sized(ui, 250.0, |ui| {
                    let focus = *reading_mode == ReadingMode::Focus;
                    if choice_button(ui, language.text("专注模式", "Focus mode"), focus).clicked()
                    {
                        *reading_mode = ReadingMode::Focus;
                    }
                    let classic = *reading_mode == ReadingMode::Classic;
                    if choice_button(ui, language.text("经典模式", "Classic mode"), classic)
                        .clicked()
                    {
                        *reading_mode = ReadingMode::Classic;
                    }
                });
                ui.end_row();

                if *reading_mode == ReadingMode::Classic {
                    settings_row_label(ui, language.text("正文布局", "Content layout"));
                    settings_row_control_sized(ui, 300.0, |ui| {
                        let single = *spread == SpreadMode::Single;
                        if choice_button(ui, language.text("单栏", "Single column"), single)
                            .clicked()
                        {
                            *spread = SpreadMode::Single;
                        }
                        let double = *spread == SpreadMode::Double;
                        if choice_button(ui, language.text("双栏", "Two columns"), double).clicked()
                        {
                            *spread = SpreadMode::Double;
                        }
                        let scroll = *spread == SpreadMode::Scroll;
                        if choice_button(ui, language.text("滑动", "Scroll"), scroll).clicked() {
                            *spread = SpreadMode::Scroll;
                        }
                    });
                    ui.end_row();
                }

                settings_row_label(ui, language.text("正文样式", "Content style"));
                settings_row_control_sized(ui, SETTINGS_FONT_SELECT_WIDTH, |ui| {
                    let unified = typesetting.mode == TypesettingMode::Unified;
                    if choice_button(ui, language.text("统一覆盖", "Unified override"), unified)
                        .clicked()
                    {
                        typesetting.mode = TypesettingMode::Unified;
                    }
                    let book = typesetting.mode == TypesettingMode::Book;
                    if choice_button(ui, language.text("跟随书籍", "Follow book"), book).clicked()
                    {
                        typesetting.mode = TypesettingMode::Book;
                    }
                });
                ui.end_row();

                settings_row_label(ui, language.text("默认字体", "Default font"));
                settings_row_control_sized(ui, SETTINGS_FONT_SELECT_WIDTH, |ui| {
                    default_font_selector(ui, language, typography, font_families);
                });
                ui.end_row();

                font_family_row(
                    ui,
                    language.text("中文字体", "CJK font"),
                    "settings-cjk-font",
                    &mut typography.default_cjk_font,
                    &font_families.chinese,
                    None,
                );
                font_family_row(
                    ui,
                    language.text("代码字体", "Code font"),
                    "settings-monospace-font",
                    &mut typography.monospace_font,
                    &font_families.monospace,
                    None,
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

#[allow(
    clippy::too_many_lines,
    reason = "the provider card keeps its dependent connection and model controls together"
)]
fn ai_provider_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let mut remove_provider = None;
    let can_remove_provider = state.draft_plugin_settings.providers.len() > 1;
    for index in 0..state.draft_plugin_settings.providers.len() {
        let provider_id = state.draft_plugin_settings.providers[index].id.clone();
        let fetched_models = state.provider_models_cache.get(&provider_id).cloned();
        let fetch_error = state.provider_models_errors.get(&provider_id).cloned();
        let loading = state.provider_models_loading.as_deref() == Some(&provider_id);
        let mut provider_changed = false;
        let mut refresh_requested = false;
        settings_card(ui, |ui| {
            let provider = &mut state.draft_plugin_settings.providers[index];
            egui::Grid::new(("ai-provider-settings-grid", &provider.id))
                .num_columns(2)
                .spacing([24.0, 16.0])
                .show(ui, |ui| {
                    settings_row_label(ui, language.text("提供商", "Provider"));
                    settings_row_control_sized(ui, 360.0, |ui| {
                        let mut selected_kind = provider.kind;
                        egui::ComboBox::from_id_salt(("ai-provider-kind", &provider.id))
                            .width(SETTINGS_SELECT_WIDTH)
                            .selected_text(ai_provider_kind_label(language, selected_kind))
                            .show_ui(ui, |ui| {
                                for kind in AiProviderKind::ALL {
                                    ui.selectable_value(
                                        &mut selected_kind,
                                        kind,
                                        ai_provider_kind_label(language, kind),
                                    );
                                }
                            });
                        field_label(
                            ui,
                            language.text("仅支持 OpenAI 兼容协议", "OpenAI-compatible APIs only"),
                        );
                        if selected_kind != provider.kind {
                            provider.select_kind(selected_kind);
                            provider_changed = true;
                        }
                    });
                    ui.end_row();

                    settings_row_label(ui, language.text("名称", "Name"));
                    settings_row_control_sized(ui, 324.0, |ui| {
                        text_field_sized(
                            ui,
                            &mut provider.name,
                            false,
                            SETTINGS_MODEL_SELECT_WIDTH,
                        );
                        if can_remove_provider
                            && icon_button(ui, Icon::Trash2)
                                .on_hover_text(language.text("删除服务", "Remove provider"))
                                .clicked()
                        {
                            remove_provider = Some(index);
                        }
                    });
                    ui.end_row();

                    if provider.kind == AiProviderKind::Custom {
                        settings_row_label(ui, language.text("接口地址", "Base URL"));
                        settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                            if text_field_sized_with_hint(
                                ui,
                                &mut provider.base_url,
                                false,
                                SETTINGS_MODEL_SELECT_WIDTH,
                                "https://api.openai.com/v1",
                            )
                            .changed()
                            {
                                provider_changed = true;
                            }
                        });
                        ui.end_row();
                    }

                    settings_row_label(ui, "API Key");
                    settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                        if text_field_sized(
                            ui,
                            &mut provider.api_key,
                            true,
                            SETTINGS_MODEL_SELECT_WIDTH,
                        )
                        .changed()
                        {
                            provider_changed = true;
                        }
                    });
                    ui.end_row();

                    settings_row_label(ui, language.text("模型", "Models"));
                    settings_row_control_sized(ui, 392.0, |ui| {
                        refresh_requested = provider_models_selector(
                            ui,
                            &provider.id,
                            &mut provider.models,
                            fetched_models.as_deref(),
                            loading,
                            fetch_error.as_deref(),
                            language,
                        );
                    });
                    ui.end_row();
                });
        });
        if provider_changed {
            state.invalidate_provider_models(&provider_id);
        }
        let provider = state.draft_plugin_settings.providers[index].clone();
        let needs_initial_fetch = !state.provider_models_cache.contains_key(&provider.id)
            && !state.provider_models_errors.contains_key(&provider.id)
            && !state.provider_models_task.is_pending()
            && !provider_changed
            && !provider.base_url.trim().is_empty();
        if refresh_requested || needs_initial_fetch {
            state.request_provider_models(ProviderModelsRequest {
                provider_id: provider.id,
                base_url: provider.base_url,
                api_key: provider.api_key,
            });
        }
        ui.add_space(8.0);
    }
    if let Some(index) = remove_provider {
        if let Some(provider) = state.draft_plugin_settings.providers.get(index) {
            state.provider_models_cache.remove(&provider.id);
            state.provider_models_errors.remove(&provider.id);
            if state.provider_models_loading.as_deref() == Some(&provider.id) {
                state.provider_models_task.cancel();
                state.provider_models_loading = None;
            }
        }
        state.draft_plugin_settings.remove_provider(index);
    }
    if secondary_button(ui, language.text("添加提供商", "Add provider")).clicked() {
        state.draft_plugin_settings.add_provider();
    }
}

fn provider_models_selector(
    ui: &mut egui::Ui,
    provider_id: &str,
    selected_models: &mut Vec<AiModelConfig>,
    fetched_models: Option<&[String]>,
    loading: bool,
    error: Option<&str>,
    language: AppLanguage,
) -> bool {
    let selected_text = match selected_models.as_slice() {
        [] => language.text("请选择模型", "Select models").into(),
        [model] => model.id.clone(),
        models => format!(
            "{} {} {}",
            language.text("已选择", "Selected"),
            models.len(),
            language.text("个模型", "models")
        ),
    };
    let mut options = std::collections::BTreeSet::new();
    if let Some(models) = fetched_models {
        options.extend(models.iter().cloned());
    }
    options.extend(
        selected_models
            .iter()
            .map(|model| model.id.trim().to_owned())
            .filter(|model| !model.is_empty()),
    );
    egui::ComboBox::from_id_salt(("ai-provider-models", provider_id))
        .width(SETTINGS_MODEL_SELECT_WIDTH)
        .truncate()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            ui.set_min_width(SETTINGS_MODEL_SELECT_WIDTH);
            if loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(language.text("正在获取模型…", "Loading models…"));
                });
            } else if let Some(error) = error {
                ui.colored_label(palette().error, error);
            } else if fetched_models.is_some_and(<[String]>::is_empty) {
                ui.weak(language.text("接口未返回可用模型", "The API returned no models"));
            }
            if (loading || error.is_some() || fetched_models.is_some_and(<[String]>::is_empty))
                && !options.is_empty()
            {
                ui.separator();
            }
            for model in options {
                let selected = selected_models
                    .iter()
                    .any(|candidate| candidate.id == model);
                let response = ui
                    .selectable_label(selected, &model)
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    if selected {
                        if selected_models.len() > 1 {
                            selected_models.retain(|candidate| candidate.id != model);
                        }
                    } else {
                        selected_models.push(AiModelConfig::language(model));
                    }
                }
            }
        });
    let refresh_requested = secondary_button(ui, language.text("刷新", "Refresh")).clicked();
    if loading {
        ui.spinner();
    } else if let Some(error) = error {
        ui.add(icon(Icon::AlertCircle).size(16.0).color(palette().error))
            .on_hover_text(error);
    }
    refresh_requested
}

fn ai_chat_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_plugin_settings;
    let options = configured_model_options(settings);
    settings_card(ui, |ui| {
        egui::Grid::new("ai-chat-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_row_label(ui, language.text("对话模型", "Chat model"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    configured_model_selector(
                        ui,
                        "chat-model",
                        &options,
                        &mut settings.chat_provider,
                        &mut settings.chat_model,
                        language,
                    );
                });
                ui.end_row();

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
    let options = configured_model_options(settings);
    settings_card(ui, |ui| {
        egui::Grid::new("metadata-ocr-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_module_row_label(ui, language.text("元数据识别", "Metadata recognition"));
                settings_row_control_sized(ui, 260.0, |ui| {
                    toggle_switch(ui, &mut settings.ocr_enabled);
                    field_label(
                        ui,
                        language.text(
                            "识别目录、作者、书名",
                            "Recognize contents, author, and title",
                        ),
                    );
                });
                ui.end_row();

                settings_row_label(ui, language.text("识别模型", "Recognition model"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    configured_model_selector(
                        ui,
                        "ocr-model",
                        &options,
                        &mut settings.ocr_provider,
                        &mut settings.ocr_model,
                        language,
                    );
                });
                ui.end_row();
            });
    });
    ui.add_space(12.0);
    settings_card(ui, |ui| {
        egui::Grid::new("pdf-ocr-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_module_row_label(ui, language.text("正文识别", "Document recognition"));
                settings_row_control_sized(ui, 44.0, |ui| {
                    toggle_switch(ui, &mut settings.pdf_ocr_enabled);
                });
                ui.end_row();

                settings_row_label(ui, language.text("提供商", "Provider"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
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
                });
                ui.end_row();

                settings_row_label(ui, language.text("识别模型", "Recognition model"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    match settings.pdf_ocr_provider {
                        PdfOcrProviderKind::PaddleOcr => {
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
                        }
                        PdfOcrProviderKind::MinerU => {
                            egui::ComboBox::from_id_salt("mineru-ocr-model")
                                .width(SETTINGS_MODEL_SELECT_WIDTH)
                                .selected_text(&settings.mineru_model)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut settings.mineru_model,
                                        "vlm".into(),
                                        "VLM",
                                    );
                                    ui.selectable_value(
                                        &mut settings.mineru_model,
                                        "pipeline".into(),
                                        "Pipeline",
                                    );
                                });
                        }
                    }
                });
                ui.end_row();

                settings_row_label(
                    ui,
                    match settings.pdf_ocr_provider {
                        PdfOcrProviderKind::PaddleOcr => "Access Token",
                        PdfOcrProviderKind::MinerU => "API Token",
                    },
                );
                settings_row_control_sized(ui, 324.0, |ui| {
                    match settings.pdf_ocr_provider {
                        PdfOcrProviderKind::PaddleOcr => text_field_sized(
                            ui,
                            &mut settings.paddle_ocr_token,
                            true,
                            SETTINGS_MODEL_SELECT_WIDTH,
                        ),
                        PdfOcrProviderKind::MinerU => text_field_sized(
                            ui,
                            &mut settings.mineru_token,
                            true,
                            SETTINGS_MODEL_SELECT_WIDTH,
                        ),
                    };
                    provider_credential_button(
                        ui,
                        settings.pdf_ocr_provider.credential_url(),
                        language.text("访问提供商官网获取", "Visit the provider website"),
                    );
                });
                ui.end_row();

                settings_row_label(ui, language.text("流式版式", "Reflow layout"));
                settings_row_control_sized(ui, 44.0, |ui| {
                    ui.add_enabled_ui(settings.pdf_ocr_enabled, |ui| {
                        toggle_switch(ui, &mut settings.pdf_ocr_reflow_enabled);
                    });
                });
                ui.end_row();
            });
    });
}

fn translation_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_plugin_settings;
    let options = configured_model_options(settings);
    settings_card(ui, |ui| {
        egui::Grid::new("translation-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_row_label(ui, language.text("翻译模型", "Translation model"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    configured_model_selector(
                        ui,
                        "translation-model",
                        &options,
                        &mut settings.translation_provider,
                        &mut settings.translation_model,
                        language,
                    );
                });
                ui.end_row();

                settings_row_label(ui, language.text("翻译为", "Translate to"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    let selected_language =
                        target_language_label(&settings.target_language, language);
                    egui::ComboBox::from_id_salt("translation-target")
                        .width(SETTINGS_MODEL_SELECT_WIDTH)
                        .selected_text(selected_language)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut settings.target_language,
                                TARGET_LANGUAGE_SYSTEM.into(),
                                language.text("跟随系统", "Follow system"),
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
                ui.end_row();

                settings_row_label(ui, language.text("显示原文", "Show original text"));
                settings_row_control_sized(ui, 44.0, |ui| {
                    let mut show_original = settings.translation_mode == TranslationMode::Bilingual;
                    if toggle_switch(ui, &mut show_original).changed() {
                        settings.translation_mode = if show_original {
                            TranslationMode::Bilingual
                        } else {
                            TranslationMode::Replace
                        };
                    }
                });
                ui.end_row();

                settings_row_label(ui, language.text("翻译目录", "Translate table of contents"));
                settings_row_control_sized(ui, 44.0, |ui| {
                    toggle_switch(ui, &mut settings.translate_toc);
                });
                ui.end_row();
            });
    });
}

fn configured_model_options(settings: &PluginSettings) -> Vec<ConfiguredModel> {
    let mut options = Vec::new();
    for (index, provider) in settings.providers.iter().enumerate() {
        let provider_name = if provider.name.trim().is_empty() {
            format!("Provider {}", index + 1)
        } else {
            provider.name.trim().to_owned()
        };
        for model in &provider.models {
            let id = model.id.trim();
            if !id.is_empty() {
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

fn default_font_selector(
    ui: &mut egui::Ui,
    language: AppLanguage,
    typography: &mut rebook_layout::ReaderTypography,
    font_families: &rebook_layout::ReaderFontFamilies,
) {
    let (category, family) = match typography.default_font {
        ReaderDefaultFont::Serif => (language.text("衬线", "Serif"), &typography.serif_font),
        ReaderDefaultFont::SansSerif => (
            language.text("无衬线", "Sans serif"),
            &typography.sans_serif_font,
        ),
    };
    let selected_text = format!("{category} · {family}");
    let arrow_id = ui.make_persistent_id("settings-default-font-arrow");
    let arrow_size = 14.0;
    let button = egui::Button::new((
        selected_text,
        egui::Atom::grow(),
        egui::Atom::custom(arrow_id, Vec2::splat(arrow_size)),
    ))
    .min_size(Vec2::new(SETTINGS_FONT_SELECT_WIDTH, 0.0))
    .truncate()
    .corner_radius(6);
    let (response, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        ui.set_min_width(SETTINGS_SELECT_WIDTH);
        default_font_submenu(
            ui,
            language.text("衬线", "Serif"),
            ReaderDefaultFont::Serif,
            typography,
            &font_families.serif,
            language,
        );
        default_font_submenu(
            ui,
            language.text("无衬线", "Sans serif"),
            ReaderDefaultFont::SansSerif,
            typography,
            &font_families.sans_serif,
            language,
        );
    });
    if ui.is_rect_visible(response.rect) {
        let padding = ui.spacing().button_padding;
        let arrow_rect = egui::Rect::from_center_size(
            egui::pos2(
                response.rect.right() - padding.x - arrow_size / 2.0,
                response.rect.center().y,
            ),
            Vec2::splat(arrow_size),
        );
        paint_icon(
            ui,
            arrow_rect,
            Icon::ChevronDown,
            ui.style().interact(&response).fg_stroke.color,
        );
    }
}

fn default_font_submenu(
    ui: &mut egui::Ui,
    label: &str,
    category: ReaderDefaultFont,
    typography: &mut rebook_layout::ReaderTypography,
    font_families: &[String],
    language: AppLanguage,
) {
    let current_category = typography.default_font == category;
    let label = RichText::new(label).color(if current_category {
        palette().accent
    } else {
        palette().text
    });
    ui.menu_button(label, |ui| {
        ui.set_width(SETTINGS_FONT_SELECT_WIDTH);
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.set_width(SETTINGS_FONT_SELECT_WIDTH);
                if font_families.is_empty() {
                    ui.weak(language.text("没有可用字体", "No available fonts"));
                    return;
                }
                for family in font_families {
                    let selected_family = match category {
                        ReaderDefaultFont::Serif => &typography.serif_font,
                        ReaderDefaultFont::SansSerif => &typography.sans_serif_font,
                    };
                    let selected = current_category && selected_family == family;
                    let row_width = ui.available_width();
                    if ui
                        .add(
                            egui::Button::selectable(selected, family)
                                .min_size(Vec2::new(row_width, 0.0))
                                .truncate(),
                        )
                        .on_hover_text(family)
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        typography.default_font = category;
                        match category {
                            ReaderDefaultFont::Serif => typography.serif_font.clone_from(family),
                            ReaderDefaultFont::SansSerif => {
                                typography.sans_serif_font.clone_from(family);
                            }
                        }
                        ui.close();
                    }
                }
            });
    });
}

fn font_family_selector(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    selected: &mut String,
    font_families: &[String],
    language: Option<AppLanguage>,
) {
    let popup_state_id = ui.make_persistent_id((id_salt, "popup-was-open"));
    let was_open = ui
        .ctx()
        .data(|data| data.get_temp::<bool>(popup_state_id).unwrap_or(false));
    let selected_text = font_family_label(selected, language);
    let response = egui::ComboBox::from_id_salt(id_salt)
        .width(SETTINGS_FONT_SELECT_WIDTH)
        .truncate()
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for family in font_families {
                let is_selected = *selected == *family;
                let response = ui.selectable_value(
                    selected,
                    family.clone(),
                    font_family_label(family, language),
                );
                if !was_open && is_selected {
                    response.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });
    let is_open = egui::ComboBox::is_open(ui.ctx(), response.response.id);
    ui.ctx()
        .data_mut(|data| data.insert_temp(popup_state_id, is_open));
}

fn font_family_label(family: &str, language: Option<AppLanguage>) -> String {
    if family == SYSTEM_INTERFACE_FONT
        && language.is_some_and(|language| language.resolved() == AppLanguage::SimplifiedChinese)
    {
        "跟随系统".into()
    } else {
        family.into()
    }
}

fn choice_button(ui: &mut egui::Ui, text: &str, selected: bool) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), font, palette().text);
    let text_width = galley.mesh_bounds.width();
    let width = choice_button_width(text_width, false);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let (fill, stroke, color) = choice_button_visuals(&response, selected);
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
        paint_centered_button_content(ui, rect, None, galley, color);
    }
    response
}

fn toggle_switch(ui: &mut egui::Ui, value: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(Vec2::new(42.0, 24.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let progress = ui.ctx().animate_bool_with_time(response.id, *value, 0.14);
    if ui.is_rect_visible(rect) {
        let enabled = ui.is_enabled();
        let track = if *value {
            palette().accent
        } else if response.hovered() && enabled {
            palette().hovered_stroke
        } else {
            palette().border
        };
        let track = if enabled {
            track
        } else {
            track.gamma_multiply(0.55)
        };
        ui.painter().rect_filled(rect, rect.height() / 2.0, track);
        let knob_radius = 9.0;
        let knob_x = egui::lerp(
            (rect.left() + 3.0 + knob_radius)..=(rect.right() - 3.0 - knob_radius),
            progress,
        );
        ui.painter().circle_filled(
            egui::pos2(knob_x, rect.center().y),
            knob_radius,
            Color32::WHITE.gamma_multiply(if enabled { 1.0 } else { 0.72 }),
        );
    }
    response
}

fn choice_icon_button(ui: &mut egui::Ui, glyph: Icon, text: &str, selected: bool) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), font, palette().text);
    let text_width = galley.mesh_bounds.width();
    let width = choice_button_width(text_width, true);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let (fill, stroke, color) = choice_button_visuals(&response, selected);
    if ui.is_rect_visible(rect) {
        ui.painter()
            .rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
        paint_centered_button_content(ui, rect, Some(glyph), galley, color);
    }
    response
}

fn choice_button_visuals(response: &Response, selected: bool) -> (Color32, egui::Stroke, Color32) {
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
    let color = if selected {
        palette().accent
    } else {
        palette().text
    };
    (fill, stroke, color)
}

fn choice_button_width(text_width: f32, with_icon: bool) -> f32 {
    const HORIZONTAL_PADDING: f32 = 14.0;
    const ICON_SIZE: f32 = 14.0;
    const ICON_GAP: f32 = 6.0;
    let content_width = text_width + if with_icon { ICON_SIZE + ICON_GAP } else { 0.0 };
    (content_width + HORIZONTAL_PADDING * 2.0).max(if with_icon { 72.0 } else { 56.0 })
}

fn paint_centered_button_content(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    glyph: Option<Icon>,
    galley: std::sync::Arc<egui::Galley>,
    color: Color32,
) {
    const ICON_SIZE: f32 = 14.0;
    const ICON_GAP: f32 = 6.0;
    let icon_advance = if glyph.is_some() {
        ICON_SIZE + ICON_GAP
    } else {
        0.0
    };
    let content_width = icon_advance + galley.mesh_bounds.width();
    let content_left = rect.center().x - content_width / 2.0;
    if let Some(glyph) = glyph {
        paint_icon(
            ui,
            egui::Rect::from_center_size(
                egui::pos2(content_left + ICON_SIZE / 2.0, rect.center().y),
                Vec2::splat(ICON_SIZE),
            ),
            glyph,
            color,
        );
    }
    let text_visual_left = content_left + icon_advance;
    let origin = egui::pos2(
        text_visual_left - galley.mesh_bounds.min.x,
        rect.center().y - galley.mesh_bounds.center().y,
    );
    ui.painter()
        .galley_with_override_text_color(origin, galley, color);
}

fn cloud_settings(ui: &mut egui::Ui, state: &mut SettingsFeature) {
    let language = state.draft_language;
    let settings = &mut state.draft_sync_settings;
    settings_card(ui, |ui| {
        egui::Grid::new("cloud-settings-grid")
            .num_columns(2)
            .spacing([24.0, 16.0])
            .show(ui, |ui| {
                settings_module_row_label(ui, language.text("WebDAV 同步", "WebDAV sync"));
                settings_row_control_sized(ui, 44.0, |ui| {
                    toggle_switch(ui, &mut settings.enabled);
                });
                ui.end_row();

                settings_row_label(ui, language.text("提供商", "Provider"));
                settings_row_control_sized(ui, 324.0, |ui| {
                    let mut selected_provider = settings.provider;
                    egui::ComboBox::from_id_salt("cloud-provider")
                        .width(SETTINGS_MODEL_SELECT_WIDTH)
                        .selected_text(cloud_provider_kind_label(language, selected_provider))
                        .show_ui(ui, |ui| {
                            for provider in CloudProviderKind::ALL {
                                ui.selectable_value(
                                    &mut selected_provider,
                                    provider,
                                    cloud_provider_kind_label(language, provider),
                                );
                            }
                        });
                    provider_credential_button(
                        ui,
                        selected_provider.credential_url(),
                        language.text("访问帮助文档", "Open help documentation"),
                    );
                    if selected_provider != settings.provider {
                        settings.select_provider(selected_provider);
                    }
                });
                ui.end_row();

                if settings.provider == CloudProviderKind::Custom {
                    settings_row_label(ui, language.text("接口地址", "Base URL"));
                    settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                        text_field_sized(
                            ui,
                            &mut settings.base_url,
                            false,
                            SETTINGS_MODEL_SELECT_WIDTH,
                        );
                    });
                    ui.end_row();
                }

                settings_row_label(ui, language.text("用户名", "Username"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    text_field_sized(
                        ui,
                        &mut settings.username,
                        false,
                        SETTINGS_MODEL_SELECT_WIDTH,
                    );
                });
                ui.end_row();

                settings_row_label(ui, language.text("密码", "Password"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    text_field_sized(
                        ui,
                        &mut state.draft_sync_password,
                        true,
                        SETTINGS_MODEL_SELECT_WIDTH,
                    );
                });
                ui.end_row();

                settings_row_label(ui, language.text("设备名称", "Device name"));
                settings_row_control_sized(ui, SETTINGS_MODEL_SELECT_WIDTH, |ui| {
                    text_field_sized(
                        ui,
                        &mut settings.device_name,
                        false,
                        SETTINGS_MODEL_SELECT_WIDTH,
                    );
                });
                ui.end_row();
            });
    });
}

fn provider_credential_button(ui: &mut egui::Ui, url: &str, tooltip: &str) {
    if small_icon_button(ui, Icon::ExternalLink)
        .on_hover_text(tooltip)
        .clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
    }
}

fn target_language_label(value: &str, language: AppLanguage) -> String {
    match value {
        TARGET_LANGUAGE_SYSTEM => language.text("跟随系统", "Follow system").into(),
        TARGET_LANGUAGE_SIMPLIFIED_CHINESE => "简体中文".into(),
        TARGET_LANGUAGE_ENGLISH => "English".into(),
        _ => value.into(),
    }
}

fn theme_label(language: AppLanguage, theme: AppTheme) -> &'static str {
    match theme {
        AppTheme::System => language.text("跟随系统", "Follow system"),
        AppTheme::Light => language.text("浅色模式", "Light"),
        AppTheme::Dark => language.text("深色模式", "Dark"),
    }
}

fn ai_provider_kind_label(language: AppLanguage, kind: AiProviderKind) -> &'static str {
    if language.resolved() == AppLanguage::SimplifiedChinese && kind == AiProviderKind::Custom {
        "自定义"
    } else {
        kind.label()
    }
}

fn cloud_provider_kind_label(language: AppLanguage, kind: CloudProviderKind) -> &'static str {
    if language.resolved() == AppLanguage::SimplifiedChinese && kind == CloudProviderKind::Custom {
        "自定义"
    } else {
        kind.label()
    }
}

fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(crate::ui::scaled_font_size(12.0))
            .color(palette().muted),
    );
}

fn settings_module_label(ui: &mut egui::Ui, text: &str) {
    const SYNTHETIC_BOLD_OFFSET: f32 = 0.45;
    let color = palette().text;
    let font = egui::FontId::proportional(crate::ui::scaled_font_size(14.0));
    let galley = ui.painter().layout_no_wrap(text.into(), font, color);
    let desired_size = Vec2::new(
        galley.mesh_bounds.width() + SYNTHETIC_BOLD_OFFSET,
        galley.size().y,
    );
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let origin = egui::pos2(
            rect.left() - galley.mesh_bounds.min.x,
            rect.center().y - galley.mesh_bounds.center().y,
        );
        ui.painter()
            .galley_with_override_text_color(origin, galley.clone(), color);
        ui.painter().galley_with_override_text_color(
            origin + egui::vec2(SYNTHETIC_BOLD_OFFSET, 0.0),
            galley,
            color,
        );
    }
}

fn settings_module_row_label(ui: &mut egui::Ui, text: &str) {
    settings_row_control(ui, |ui| settings_module_label(ui, text));
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
    language: Option<AppLanguage>,
) {
    settings_row_label(ui, label);
    settings_row_control_sized(ui, SETTINGS_FONT_SELECT_WIDTH, |ui| {
        font_family_selector(ui, id_salt, selected, font_families, language);
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
        if step < 1.0 {
            format!("{next:.1}{suffix}")
        } else {
            format!("{next:.0}{suffix}")
        },
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

fn text_field_sized(ui: &mut egui::Ui, value: &mut String, password: bool, width: f32) -> Response {
    text_field_sized_with_hint(ui, value, password, width, "")
}

fn text_field_sized_with_hint(
    ui: &mut egui::Ui,
    value: &mut String,
    password: bool,
    width: f32,
    hint: &str,
) -> Response {
    ui.add_sized(
        [width, 36.0],
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .password(password)
            .vertical_align(egui::Align::Center)
            .margin(egui::Margin::symmetric(10, 0)),
    )
}

fn secondary_button(ui: &mut egui::Ui, text: &str) -> Response {
    secondary_button_with_width(ui, text, None)
}

fn secondary_button_sized(ui: &mut egui::Ui, text: &str, width: f32) -> Response {
    secondary_button_with_width(ui, text, Some(width))
}

fn secondary_button_with_width(
    ui: &mut egui::Ui,
    text: &str,
    fixed_width: Option<f32>,
) -> Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), font, palette().accent);
    let text_width = galley.mesh_bounds.width();
    let width = fixed_width.unwrap_or_else(|| (text_width + 28.0).max(56.0));
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if response.is_pointer_button_down_on() {
        palette().accent_soft.gamma_multiply(0.82)
    } else if response.hovered() {
        palette().accent_soft.gamma_multiply(0.92)
    } else {
        palette().accent_soft
    };
    if ui.is_rect_visible(rect) {
        ui.painter().rect(
            rect,
            6.0,
            fill,
            egui::Stroke::new(1.0, palette().accent.gamma_multiply(0.22)),
            egui::StrokeKind::Inside,
        );
        paint_centered_button_content(ui, rect, None, galley, palette().accent);
    }
    response
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

    #[test]
    fn toggle_switch_uses_compact_pill_geometry() {
        egui::__run_test_ui(|ui| {
            let mut enabled = false;
            let response = toggle_switch(ui, &mut enabled);
            assert!((response.rect.width() - 42.0).abs() < 0.01);
            assert!((response.rect.height() - 24.0).abs() < 0.01);
            assert!(!enabled);
        });
    }

    #[test]
    fn default_font_menu_button_matches_native_select_height() {
        egui::__run_test_ui(|ui| {
            let native = egui::ComboBox::from_id_salt("native-select-height")
                .width(SETTINGS_FONT_SELECT_WIDTH)
                .selected_text("Serif · Bitter")
                .show_ui(ui, |_| {})
                .response;
            let arrow_id = ui.make_persistent_id("test-default-font-arrow");
            let button = egui::Button::new((
                "Serif · Bitter",
                egui::Atom::grow(),
                egui::Atom::custom(arrow_id, Vec2::splat(14.0)),
            ))
            .min_size(Vec2::new(SETTINGS_FONT_SELECT_WIDTH, 0.0));
            let (menu, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |_| {});

            assert!((menu.rect.height() - native.rect.height()).abs() < 0.01);
        });
    }

    #[test]
    fn custom_provider_labels_are_localized_only_for_chinese_ui() {
        assert_eq!(
            ai_provider_kind_label(AppLanguage::SimplifiedChinese, AiProviderKind::Custom),
            "自定义"
        );
        assert_eq!(
            cloud_provider_kind_label(AppLanguage::SimplifiedChinese, CloudProviderKind::Custom),
            "自定义"
        );
        assert_eq!(
            ai_provider_kind_label(AppLanguage::English, AiProviderKind::Custom),
            "Custom"
        );
        assert_eq!(
            cloud_provider_kind_label(AppLanguage::English, CloudProviderKind::Custom),
            "Custom"
        );
    }

    #[test]
    fn system_interface_font_label_is_localized_without_changing_its_value() {
        assert_eq!(
            font_family_label(SYSTEM_INTERFACE_FONT, Some(AppLanguage::SimplifiedChinese)),
            "跟随系统"
        );
        assert_eq!(
            font_family_label(SYSTEM_INTERFACE_FONT, Some(AppLanguage::English)),
            SYSTEM_INTERFACE_FONT
        );
        assert_eq!(
            font_family_label(SYSTEM_INTERFACE_FONT, Some(AppLanguage::System)),
            AppLanguage::System.text("跟随系统", SYSTEM_INTERFACE_FONT)
        );
        assert_eq!(font_family_label("Arial", None), "Arial");
    }

    #[test]
    fn choice_buttons_use_content_width_with_uniform_horizontal_padding() {
        let short_text = 28.0;
        let long_text = 56.0;
        assert!((choice_button_width(long_text, false) - long_text - 28.0).abs() < 0.01);
        assert!(choice_button_width(long_text, false) > choice_button_width(short_text, false));
        assert!(choice_button_width(long_text, true) > choice_button_width(long_text, false));
    }

    #[test]
    fn captured_shortcuts_drop_the_cross_platform_command_alias() {
        assert_eq!(
            canonical_shortcut_modifiers(egui::Modifiers {
                ctrl: true,
                command: true,
                ..egui::Modifiers::NONE
            }),
            egui::Modifiers::CTRL
        );
        assert_eq!(
            canonical_shortcut_modifiers(egui::Modifiers {
                mac_cmd: true,
                command: true,
                ..egui::Modifiers::NONE
            }),
            egui::Modifiers::MAC_CMD
        );
    }

    #[test]
    fn shortcut_capture_waits_for_the_primary_key_in_a_chord() {
        for modifier in [
            egui::Key::ShiftLeft,
            egui::Key::ShiftRight,
            egui::Key::ControlLeft,
            egui::Key::ControlRight,
            egui::Key::AltLeft,
            egui::Key::AltRight,
            egui::Key::SuperLeft,
            egui::Key::SuperRight,
        ] {
            assert!(is_shortcut_modifier_key(modifier));
        }
        assert!(!is_shortcut_modifier_key(egui::Key::B));
        assert!(!is_shortcut_modifier_key(egui::Key::F11));
        assert!(!should_ignore_shortcut_capture_key(
            ShortcutAction::FocusFootnotes,
            egui::Key::AltLeft,
        ));
        assert!(should_ignore_shortcut_capture_key(
            ShortcutAction::FocusNote,
            egui::Key::AltLeft,
        ));
    }

    #[test]
    fn shortcut_capture_label_previews_held_modifiers() {
        egui::__run_test_ui(|ui| {
            assert_eq!(
                shortcut_capture_label(
                    ui.ctx(),
                    AppLanguage::SimplifiedChinese,
                    egui::Modifiers::NONE,
                ),
                "请按快捷键…"
            );
            let preview = shortcut_capture_label(
                ui.ctx(),
                AppLanguage::SimplifiedChinese,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            );
            assert!(preview.contains("Ctrl"));
            assert!(preview.contains("Shift"));
            assert!(preview.ends_with('…'));
            assert_eq!(
                shortcut_capture_label(
                    ui.ctx(),
                    AppLanguage::SimplifiedChinese,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT | egui::Modifiers::ALT,
                ),
                "最多支持三个按键"
            );
        });
    }
}
