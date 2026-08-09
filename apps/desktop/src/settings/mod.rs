use peniko::Blob;
use rebook_layout::{LayoutEngine, ReaderTypography, SpreadMode};
use rebook_reader::SelectionGranularity;

use crate::plugins::PluginSettings;
use crate::preferences::{self, AppLanguage, AppTheme, InterfaceTypography, ReaderPreferences};
use crate::sync::SyncSettings;

mod egui_view;

pub(crate) use egui_view::settings_overlay;

#[derive(Clone)]
pub(crate) struct AppliedSettings {
    pub(crate) spread: SpreadMode,
    pub(crate) interface_typography: InterfaceTypography,
    pub(crate) typography: ReaderTypography,
    pub(crate) plugin_settings: PluginSettings,
    pub(crate) language: AppLanguage,
    pub(crate) theme: AppTheme,
    pub(crate) selection_granularity: SelectionGranularity,
    pub(crate) sync_settings: SyncSettings,
    pub(crate) sync_password: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderSettingsChange {
    Spread(SpreadMode),
    Theme(AppTheme),
    SelectionGranularity(SelectionGranularity),
}

pub(crate) struct SettingsFeature {
    settings_tab: SettingsTab,
    draft_spread: SpreadMode,
    draft_interface_typography: InterfaceTypography,
    draft_typography: ReaderTypography,
    draft_plugin_settings: PluginSettings,
    draft_language: AppLanguage,
    draft_theme: AppTheme,
    draft_sync_settings: SyncSettings,
    draft_sync_password: String,
    available_font_families: Vec<String>,
    available_interface_font_families: Vec<String>,
    applied: AppliedSettings,
    revision: u64,
    error: Option<String>,
    open: bool,
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
        let available_font_families =
            LayoutEngine::with_fonts(reader_fonts.iter().cloned()).available_font_families();
        let available_interface_font_families = crate::ui::available_interface_font_families();
        let applied = AppliedSettings {
            spread: preferences.spread,
            interface_typography: preferences.interface_typography,
            typography: preferences.typography,
            plugin_settings,
            language: preferences.language,
            theme: preferences.theme,
            selection_granularity: preferences.selection_granularity,
            sync_settings,
            sync_password,
        };
        Self {
            settings_tab: SettingsTab::Reading,
            draft_spread: applied.spread,
            draft_interface_typography: applied.interface_typography.clone(),
            draft_typography: applied.typography.clone(),
            draft_plugin_settings: applied.plugin_settings.clone(),
            draft_language: applied.language,
            draft_theme: applied.theme,
            draft_sync_settings: applied.sync_settings.clone(),
            draft_sync_password: applied.sync_password.clone(),
            available_font_families,
            available_interface_font_families,
            applied,
            revision: 0,
            error: None,
            open: false,
            #[cfg(target_os = "windows")]
            update_check_requested: false,
            #[cfg(target_os = "windows")]
            update_requested: false,
            #[cfg(target_os = "windows")]
            update_check_status: UpdateCheckStatus::Idle,
        }
    }

    pub(crate) fn open(&mut self) {
        self.settings_tab = SettingsTab::Reading;
        self.draft_spread = self.applied.spread;
        self.draft_interface_typography
            .clone_from(&self.applied.interface_typography);
        self.draft_typography.clone_from(&self.applied.typography);
        self.draft_plugin_settings
            .clone_from(&self.applied.plugin_settings);
        self.draft_language = self.applied.language;
        self.draft_theme = self.applied.theme;
        self.draft_sync_settings
            .clone_from(&self.applied.sync_settings);
        self.draft_sync_password
            .clone_from(&self.applied.sync_password);
        self.error = None;
        self.open = true;
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn applied(&self) -> &AppliedSettings {
        &self.applied
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
            language: self.applied.language,
            spread: self.applied.spread,
            theme: self.applied.theme,
            selection_granularity: self.applied.selection_granularity,
        };
        let layout_changed = matches!(
            change,
            ReaderSettingsChange::Spread(_) | ReaderSettingsChange::Theme(_)
        );
        match change {
            ReaderSettingsChange::Spread(spread) => preferences.spread = spread,
            ReaderSettingsChange::Theme(theme) => preferences.theme = theme,
            ReaderSettingsChange::SelectionGranularity(granularity) => {
                preferences.selection_granularity = granularity;
            }
        }
        if preferences.spread == self.applied.spread
            && preferences.theme == self.applied.theme
            && preferences.selection_granularity == self.applied.selection_granularity
        {
            return Ok(());
        }
        preferences::save_reader_preferences(&preferences).map_err(|error| {
            format!(
                "{}: {error}",
                self.applied
                    .language
                    .text("保存阅读设置失败", "Failed to save reader settings")
            )
        })?;
        self.applied.spread = preferences.spread;
        self.applied.theme = preferences.theme;
        self.applied.selection_granularity = preferences.selection_granularity;
        self.draft_spread = preferences.spread;
        self.draft_theme = preferences.theme;
        if layout_changed {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(())
    }

    fn close_overlay(&mut self) {
        self.open = false;
    }

    fn apply_settings(&mut self) {
        let mut plugin_settings = self.draft_plugin_settings.clone();
        plugin_settings.normalize();
        let mut typography = self.draft_typography.clone();
        typography.normalize();
        let mut interface_typography = self.draft_interface_typography.clone();
        interface_typography.normalize();
        let mut sync_settings = self.draft_sync_settings.clone();
        sync_settings.normalize();
        let language = self.draft_language;
        let theme = self.draft_theme;
        let reader_preferences = ReaderPreferences {
            interface_typography: interface_typography.clone(),
            typography: typography.clone(),
            language,
            theme,
            spread: self.draft_spread,
            selection_granularity: self.applied.selection_granularity,
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
        if let Err(error) = persist_settings(
            &reader_preferences,
            &plugin_settings,
            &sync_settings,
            &self.draft_sync_password,
        ) {
            self.error = Some(error);
            return;
        }
        let sync_password = if self.draft_sync_password.is_empty() {
            self.applied.sync_password.clone()
        } else {
            self.draft_sync_password.clone()
        };
        self.applied = AppliedSettings {
            spread: self.draft_spread,
            interface_typography,
            typography,
            plugin_settings,
            language,
            theme,
            selection_granularity: self.applied.selection_granularity,
            sync_settings,
            sync_password,
        };
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
    Reading,
    Font,
    Ai,
    AiChat,
    Ocr,
    Semantic,
    Translation,
    Cloud,
    About,
}

#[cfg(target_os = "windows")]
enum UpdateCheckStatus {
    Idle,
    Checking,
    UpToDate,
    Available(String),
    Failed(String),
}
