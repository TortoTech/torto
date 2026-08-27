use std::collections::HashMap;

use peniko::Blob;
use rebook_layout::{
    LayoutEngine, ReaderFontFamilies, ReaderTypesetting, ReaderTypography, SpreadMode,
};
use rebook_reader::SelectionGranularity;

use crate::plugins::PluginSettings;
use crate::preferences::{
    self, AppLanguage, AppTheme, InterfaceTypography, ReaderPreferences, ReadingMode,
    ShortcutPreferences,
};
use crate::sync::SyncSettings;
use crate::{async_task::TaskSlot, platform::UserEvent};

mod egui_view;
mod provider_models;

pub(crate) use egui_view::settings_overlay;
pub(crate) use provider_models::ProviderModelsMessage;
use provider_models::{ProviderModelsRequest, fetch_provider_models};

#[derive(Clone, PartialEq)]
pub(crate) struct AppliedSettings {
    pub(crate) spread: SpreadMode,
    pub(crate) reading_mode: ReadingMode,
    pub(crate) interface_typography: InterfaceTypography,
    pub(crate) typography: ReaderTypography,
    pub(crate) typesetting: ReaderTypesetting,
    pub(crate) plugin_settings: PluginSettings,
    pub(crate) language: AppLanguage,
    pub(crate) theme: AppTheme,
    pub(crate) selection_granularity: SelectionGranularity,
    pub(crate) shortcuts: ShortcutPreferences,
    pub(crate) sync_settings: SyncSettings,
    pub(crate) sync_password: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderSettingsChange {
    Spread(SpreadMode),
    ReadingMode(ReadingMode),
    Theme(AppTheme),
    SelectionGranularity(SelectionGranularity),
}

pub(crate) struct SettingsFeature {
    settings_tab: SettingsTab,
    draft_spread: SpreadMode,
    draft_reading_mode: ReadingMode,
    draft_interface_typography: InterfaceTypography,
    draft_typography: ReaderTypography,
    draft_typesetting: ReaderTypesetting,
    draft_plugin_settings: PluginSettings,
    draft_language: AppLanguage,
    draft_theme: AppTheme,
    draft_shortcuts: ShortcutPreferences,
    draft_sync_settings: SyncSettings,
    draft_sync_password: String,
    available_reader_font_families: ReaderFontFamilies,
    available_interface_font_families: Vec<String>,
    applied: AppliedSettings,
    revision: u64,
    error: Option<String>,
    open: bool,
    capturing_shortcut: Option<ShortcutAction>,
    provider_models_task: TaskSlot<ProviderModelsRequest>,
    provider_models_cache: HashMap<String, Vec<String>>,
    provider_models_errors: HashMap<String, String>,
    provider_models_loading: Option<String>,
    #[cfg(target_os = "windows")]
    update_check_requested: bool,
    #[cfg(target_os = "windows")]
    update_requested: bool,
    #[cfg(target_os = "windows")]
    update_check_status: UpdateCheckStatus,
}

impl SettingsFeature {
    pub(crate) fn new(reader_fonts: &[Blob<u8>]) -> Self {
        let preferences = preferences::load_reader_preferences().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load reader preferences; using defaults");
            ReaderPreferences::default()
        });
        let plugin_settings = PluginSettings::load_default().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load plugin settings; using defaults");
            PluginSettings::default()
        });
        let sync_settings = SyncSettings::load_default().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load WebDAV settings; using defaults");
            SyncSettings::new_device()
        });
        let sync_password = sync_settings.load_password().unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to load WebDAV credential");
            String::new()
        });
        let mut available_reader_font_families =
            LayoutEngine::with_fonts(reader_fonts.iter().cloned()).available_reader_font_families();
        available_reader_font_families.include_configured(&preferences.typography);
        let available_interface_font_families = crate::ui::available_interface_font_families();
        let applied = AppliedSettings {
            spread: preferences.spread,
            reading_mode: preferences.reading_mode,
            interface_typography: preferences.interface_typography,
            typography: preferences.typography,
            typesetting: preferences.typesetting,
            plugin_settings,
            language: preferences.language,
            theme: preferences.theme,
            selection_granularity: preferences.selection_granularity,
            shortcuts: preferences.shortcuts,
            sync_settings,
            sync_password,
        };
        Self {
            settings_tab: SettingsTab::System,
            draft_spread: applied.spread,
            draft_reading_mode: applied.reading_mode,
            draft_interface_typography: applied.interface_typography.clone(),
            draft_typography: applied.typography.clone(),
            draft_typesetting: applied.typesetting.clone(),
            draft_plugin_settings: applied.plugin_settings.clone(),
            draft_language: applied.language,
            draft_theme: applied.theme,
            draft_shortcuts: applied.shortcuts.clone(),
            draft_sync_settings: applied.sync_settings.clone(),
            draft_sync_password: applied.sync_password.clone(),
            available_reader_font_families,
            available_interface_font_families,
            applied,
            revision: 0,
            error: None,
            open: false,
            capturing_shortcut: None,
            provider_models_task: TaskSlot::default(),
            provider_models_cache: HashMap::new(),
            provider_models_errors: HashMap::new(),
            provider_models_loading: None,
            #[cfg(target_os = "windows")]
            update_check_requested: false,
            #[cfg(target_os = "windows")]
            update_requested: false,
            #[cfg(target_os = "windows")]
            update_check_status: UpdateCheckStatus::Idle,
        }
    }

    pub(crate) fn open(&mut self) {
        self.settings_tab = SettingsTab::System;
        self.draft_spread = self.applied.spread;
        self.draft_reading_mode = self.applied.reading_mode;
        self.draft_interface_typography
            .clone_from(&self.applied.interface_typography);
        self.draft_typography.clone_from(&self.applied.typography);
        self.draft_typesetting.clone_from(&self.applied.typesetting);
        self.draft_plugin_settings
            .clone_from(&self.applied.plugin_settings);
        self.draft_language = self.applied.language;
        self.draft_theme = self.applied.theme;
        self.draft_shortcuts.clone_from(&self.applied.shortcuts);
        self.draft_sync_settings
            .clone_from(&self.applied.sync_settings);
        self.draft_sync_password
            .clone_from(&self.applied.sync_password);
        self.error = None;
        self.capturing_shortcut = None;
        self.open = true;
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn applied(&self) -> &AppliedSettings {
        &self.applied
    }

    pub(crate) fn spawn_pending_tasks(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        let Some(request) = self.provider_models_task.take_pending() else {
            return;
        };
        let proxy = proxy.clone();
        runtime.spawn(async move {
            let result = fetch_provider_models(&request.payload).await;
            let _ = proxy.send_event(UserEvent::SettingsProviderModels(ProviderModelsMessage {
                id: request.id,
                result,
            }));
        });
    }

    pub(crate) fn complete_provider_models(&mut self, message: ProviderModelsMessage) {
        let Some(request) = self.provider_models_task.complete(message.id) else {
            return;
        };
        if self.provider_models_loading.as_deref() == Some(&request.provider_id) {
            self.provider_models_loading = None;
        }
        match message.result {
            Ok(models) => {
                self.provider_models_errors.remove(&request.provider_id);
                self.provider_models_cache
                    .insert(request.provider_id, models);
            }
            Err(error) => {
                self.provider_models_errors
                    .insert(request.provider_id, error);
            }
        }
    }

    fn request_provider_models(&mut self, request: ProviderModelsRequest) {
        self.provider_models_errors.remove(&request.provider_id);
        self.provider_models_loading = Some(request.provider_id.clone());
        self.provider_models_task.begin(request);
    }

    fn invalidate_provider_models(&mut self, provider_id: &str) {
        self.provider_models_cache.remove(provider_id);
        self.provider_models_errors.remove(provider_id);
        if self.provider_models_loading.as_deref() == Some(provider_id) {
            self.provider_models_task.cancel();
            self.provider_models_loading = None;
        }
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn take_update_check_request(&mut self) -> bool {
        std::mem::take(&mut self.update_check_requested)
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn take_update_request(&mut self) -> bool {
        std::mem::take(&mut self.update_requested)
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn complete_update_check(
        &mut self,
        result: crate::updater::ManualUpdateCheckResult,
    ) {
        self.update_check_status = match result {
            crate::updater::ManualUpdateCheckResult::UpToDate => UpdateCheckStatus::UpToDate,
            crate::updater::ManualUpdateCheckResult::Available(version) => {
                UpdateCheckStatus::Available(version)
            }
            crate::updater::ManualUpdateCheckResult::Failed(error) => {
                UpdateCheckStatus::Failed(error)
            }
        };
    }

    pub(crate) fn apply_reader_change(
        &mut self,
        change: ReaderSettingsChange,
    ) -> Result<(), String> {
        let mut preferences = ReaderPreferences {
            interface_typography: self.applied.interface_typography.clone(),
            typography: self.applied.typography.clone(),
            typesetting: self.applied.typesetting.clone(),
            language: self.applied.language,
            spread: self.applied.spread,
            reading_mode: self.applied.reading_mode,
            theme: self.applied.theme,
            selection_granularity: self.applied.selection_granularity,
            shortcuts: self.applied.shortcuts.clone(),
        };
        let layout_changed = matches!(
            change,
            ReaderSettingsChange::Spread(_)
                | ReaderSettingsChange::ReadingMode(_)
                | ReaderSettingsChange::Theme(_)
        );
        match change {
            ReaderSettingsChange::Spread(spread) => preferences.spread = spread,
            ReaderSettingsChange::ReadingMode(mode) => preferences.reading_mode = mode,
            ReaderSettingsChange::Theme(theme) => preferences.theme = theme,
            ReaderSettingsChange::SelectionGranularity(granularity) => {
                preferences.selection_granularity = granularity;
            }
        }
        let values_unchanged = preferences.spread == self.applied.spread
            && preferences.reading_mode == self.applied.reading_mode
            && preferences.theme == self.applied.theme
            && preferences.selection_granularity == self.applied.selection_granularity;
        if !reader_change_needs_apply(values_unchanged, layout_changed) {
            return Ok(());
        }
        if !values_unchanged {
            preferences::save_reader_preferences(&preferences).map_err(|error| {
                format!(
                    "{}: {error}",
                    self.applied
                        .language
                        .text("保存阅读设置失败", "Failed to save reader settings")
                )
            })?;
        }
        self.applied.spread = preferences.spread;
        self.applied.reading_mode = preferences.reading_mode;
        self.applied.theme = preferences.theme;
        self.applied.selection_granularity = preferences.selection_granularity;
        self.draft_spread = preferences.spread;
        self.draft_reading_mode = preferences.reading_mode;
        self.draft_theme = preferences.theme;
        if layout_changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(())
    }

    fn close_overlay(&mut self) {
        self.capturing_shortcut = None;
        self.open = false;
    }

    fn close_and_apply(&mut self) {
        if !settings_close_needs_apply(&self.draft_settings(), &self.applied) {
            self.close_overlay();
        } else {
            self.apply_settings();
        }
    }

    fn draft_settings(&self) -> AppliedSettings {
        AppliedSettings {
            spread: self.draft_spread,
            reading_mode: self.draft_reading_mode,
            interface_typography: self.draft_interface_typography.clone(),
            typography: self.draft_typography.clone(),
            typesetting: self.draft_typesetting.clone(),
            plugin_settings: self.draft_plugin_settings.clone(),
            language: self.draft_language,
            theme: self.draft_theme,
            selection_granularity: self.applied.selection_granularity,
            shortcuts: self.draft_shortcuts.clone(),
            sync_settings: self.draft_sync_settings.clone(),
            sync_password: if self.draft_sync_password.is_empty() {
                self.applied.sync_password.clone()
            } else {
                self.draft_sync_password.clone()
            },
        }
    }

    fn apply_settings(&mut self) {
        let mut plugin_settings = self.draft_plugin_settings.clone();
        plugin_settings.normalize();
        let mut typography = self.draft_typography.clone();
        typography.normalize();
        let mut typesetting = self.draft_typesetting.clone();
        typesetting.normalize();
        let mut interface_typography = self.draft_interface_typography.clone();
        interface_typography.normalize();
        let mut sync_settings = self.draft_sync_settings.clone();
        sync_settings.normalize();
        let language = self.draft_language;
        let theme = self.draft_theme;
        if self.draft_shortcuts.has_conflicts() {
            self.error = Some(
                language
                    .text(
                        "快捷键存在重复，请修改后再保存",
                        "Some shortcuts conflict. Change them before saving.",
                    )
                    .into(),
            );
            return;
        }
        if self.draft_shortcuts.has_oversized_chords() {
            self.error = Some(
                language
                    .text(
                        "快捷键最多支持三个按键",
                        "Shortcuts support up to three keys.",
                    )
                    .into(),
            );
            return;
        }
        let reader_preferences = ReaderPreferences {
            interface_typography: interface_typography.clone(),
            typography: typography.clone(),
            typesetting: typesetting.clone(),
            language,
            theme,
            spread: self.draft_spread,
            reading_mode: self.draft_reading_mode,
            selection_granularity: self.applied.selection_granularity,
            shortcuts: self.draft_shortcuts.clone(),
        };
        if sync_settings.enabled
            && let Err(error) = sync_settings.validate()
        {
            self.error = Some(format!(
                "{}: {error}",
                language.text("云盘设置无效", "Invalid cloud settings")
            ));
            return;
        }
        let sync_password = if self.draft_sync_password.is_empty() {
            self.applied.sync_password.clone()
        } else {
            self.draft_sync_password.clone()
        };
        let next_applied = AppliedSettings {
            spread: self.draft_spread,
            reading_mode: self.draft_reading_mode,
            interface_typography,
            typography,
            typesetting,
            plugin_settings,
            language,
            theme,
            selection_granularity: self.applied.selection_granularity,
            shortcuts: self.draft_shortcuts.clone(),
            sync_settings,
            sync_password,
        };
        if next_applied == self.applied {
            self.close_overlay();
            return;
        }
        if let Err(error) = persist_settings(
            &reader_preferences,
            &next_applied.plugin_settings,
            &next_applied.sync_settings,
            &self.draft_sync_password,
        ) {
            self.error = Some(error);
            return;
        }
        self.applied = next_applied;
        self.draft_sync_password
            .clone_from(&self.applied.sync_password);
        self.error = None;
        self.revision = self.revision.wrapping_add(1);
        self.close_overlay();
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
}

const fn reader_change_needs_apply(values_unchanged: bool, layout_changed: bool) -> bool {
    !values_unchanged || layout_changed
}

fn settings_close_needs_apply(draft: &AppliedSettings, applied: &AppliedSettings) -> bool {
    draft != applied
}

fn persist_settings(
    reader_preferences: &ReaderPreferences,
    plugin_settings: &PluginSettings,
    sync_settings: &SyncSettings,
    sync_password: &str,
) -> Result<(), String> {
    let language = reader_preferences.language;
    plugin_settings.save_default().map_err(|error| {
        format!(
            "{}: {error}",
            language.text("保存 AI 设置失败", "Failed to save AI settings")
        )
    })?;
    preferences::save_reader_preferences(reader_preferences).map_err(|error| {
        format!(
            "{}: {error}",
            language.text("保存阅读设置失败", "Failed to save reader settings")
        )
    })?;
    sync_settings.save_default().map_err(|error| {
        format!(
            "{}: {error}",
            language.text("保存云盘设置失败", "Failed to save cloud settings")
        )
    })?;
    if !sync_password.is_empty() {
        sync_settings
            .save_password(sync_password)
            .map_err(|error| {
                format!(
                    "{}: {error}",
                    language.text(
                        "保存 Windows 凭据失败",
                        "Failed to save the Windows credential"
                    )
                )
            })?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsTab {
    #[default]
    System,
    Typography,
    Ai,
    AiChat,
    Ocr,
    Translation,
    Shortcuts,
    Cloud,
    About,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortcutAction {
    Fullscreen,
    ToggleLeftSidebar,
    ToggleRightSidebar,
    ToggleTranslation,
    Search,
    Copy,
    ReturnToShelf,
    FocusActions,
    FocusChat,
    FocusHighlight,
    FocusNote,
    FocusStructure,
    FocusFootnotes,
    FocusExtendSelectionPrevious,
    FocusExtendSelectionNext,
}

impl ShortcutAction {
    fn binding(self, shortcuts: &ShortcutPreferences) -> egui::KeyboardShortcut {
        match self {
            Self::Fullscreen => shortcuts.fullscreen,
            Self::ToggleLeftSidebar => shortcuts.toggle_left_sidebar,
            Self::ToggleRightSidebar => shortcuts.toggle_right_sidebar,
            Self::ToggleTranslation => shortcuts.toggle_translation,
            Self::Search => shortcuts.search,
            Self::Copy => shortcuts.copy,
            Self::ReturnToShelf => shortcuts.return_to_shelf,
            Self::FocusActions => shortcuts.focus_actions,
            Self::FocusChat => shortcuts.focus_chat,
            Self::FocusHighlight => shortcuts.focus_highlight,
            Self::FocusNote => shortcuts.focus_note,
            Self::FocusStructure => shortcuts.focus_structure,
            Self::FocusFootnotes => shortcuts.focus_footnotes,
            Self::FocusExtendSelectionPrevious => shortcuts.focus_extend_selection_previous,
            Self::FocusExtendSelectionNext => shortcuts.focus_extend_selection_next,
        }
    }

    fn set_binding(self, shortcuts: &mut ShortcutPreferences, binding: egui::KeyboardShortcut) {
        match self {
            Self::Fullscreen => shortcuts.fullscreen = binding,
            Self::ToggleLeftSidebar => shortcuts.toggle_left_sidebar = binding,
            Self::ToggleRightSidebar => shortcuts.toggle_right_sidebar = binding,
            Self::ToggleTranslation => shortcuts.toggle_translation = binding,
            Self::Search => shortcuts.search = binding,
            Self::Copy => shortcuts.copy = binding,
            Self::ReturnToShelf => shortcuts.return_to_shelf = binding,
            Self::FocusActions => shortcuts.focus_actions = binding,
            Self::FocusChat => shortcuts.focus_chat = binding,
            Self::FocusHighlight => shortcuts.focus_highlight = binding,
            Self::FocusNote => shortcuts.focus_note = binding,
            Self::FocusStructure => shortcuts.focus_structure = binding,
            Self::FocusFootnotes => shortcuts.focus_footnotes = binding,
            Self::FocusExtendSelectionPrevious => {
                shortcuts.focus_extend_selection_previous = binding;
            }
            Self::FocusExtendSelectionNext => shortcuts.focus_extend_selection_next = binding,
        }
    }
}

#[cfg(target_os = "windows")]
enum UpdateCheckStatus {
    Idle,
    Checking,
    UpToDate,
    Available(String),
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_layout_request_still_reapplies_the_active_reader() {
        assert!(reader_change_needs_apply(true, true));
        assert!(!reader_change_needs_apply(true, false));
        assert!(reader_change_needs_apply(false, false));
    }

    #[test]
    fn closing_settings_applies_only_changed_drafts() {
        let preferences = ReaderPreferences::default();
        let applied = AppliedSettings {
            spread: preferences.spread,
            reading_mode: preferences.reading_mode,
            interface_typography: preferences.interface_typography,
            typography: preferences.typography,
            typesetting: preferences.typesetting,
            plugin_settings: PluginSettings::default(),
            language: preferences.language,
            theme: preferences.theme,
            selection_granularity: preferences.selection_granularity,
            shortcuts: preferences.shortcuts,
            sync_settings: SyncSettings::new_device(),
            sync_password: String::new(),
        };
        assert!(!settings_close_needs_apply(&applied, &applied));

        let mut changed = applied.clone();
        changed.theme = match applied.theme {
            AppTheme::System => AppTheme::Light,
            AppTheme::Light => AppTheme::Dark,
            AppTheme::Dark => AppTheme::System,
        };
        assert!(settings_close_needs_apply(&changed, &applied));
    }
}
