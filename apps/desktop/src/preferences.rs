use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use directories::ProjectDirs;
use rebook_layout::{
    LineBreakStrategy, ReaderTypesetting, ReaderTypography, SpreadMode, TypesettingMode,
};
use rebook_reader::SelectionGranularity;
use serde::{Deserialize, Serialize};

use crate::persistence::write_json_atomic;

const SETTINGS_VERSION: u32 = 1;
const LEGACY_BODY_LINE_HEIGHT: f32 = 1.72;
const LEGACY_PARAGRAPH_GAP_EM: f32 = 0.75;
const SETTINGS_FILE: &str = "reader-settings.json";
pub(crate) const SYSTEM_INTERFACE_FONT: &str = "System UI";
pub(crate) const DEFAULT_INTERFACE_FONT_SIZE: f32 = 14.0;

pub type PreferencesResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AppLanguage {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

impl AppLanguage {
    pub(crate) fn text(
        self,
        simplified_chinese: &'static str,
        english: &'static str,
    ) -> &'static str {
        match self.resolved() {
            Self::System => unreachable!("resolved language cannot be system"),
            Self::SimplifiedChinese => simplified_chinese,
            Self::English => english,
        }
    }

    pub(crate) fn translation_target(self) -> &'static str {
        match self.resolved() {
            Self::System => unreachable!("resolved language cannot be system"),
            Self::SimplifiedChinese => "简体中文",
            Self::English => "English",
        }
    }

    pub(crate) fn system_translation_target() -> &'static str {
        SYSTEM_LANGUAGE.translation_target()
    }

    pub(crate) fn resolved(self) -> Self {
        match self {
            Self::System => *SYSTEM_LANGUAGE,
            language => language,
        }
    }
}

static SYSTEM_LANGUAGE: LazyLock<AppLanguage> = LazyLock::new(|| {
    sys_locale::get_locale()
        .as_deref()
        .map_or(AppLanguage::English, language_from_system_locale)
});

fn language_from_system_locale(locale: &str) -> AppLanguage {
    if locale.trim().to_ascii_lowercase().starts_with("zh") {
        AppLanguage::SimplifiedChinese
    } else {
        AppLanguage::English
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AppTheme {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "light")]
    Light,
    #[serde(rename = "dark")]
    Dark,
}

impl AppTheme {
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReaderPreferences {
    pub(crate) interface_typography: InterfaceTypography,
    pub(crate) typography: ReaderTypography,
    pub(crate) typesetting: ReaderTypesetting,
    pub(crate) language: AppLanguage,
    pub(crate) spread: SpreadMode,
    pub(crate) reading_mode: ReadingMode,
    pub(crate) theme: AppTheme,
    pub(crate) selection_granularity: SelectionGranularity,
    pub(crate) shortcuts: ShortcutPreferences,
}

impl Default for ReaderPreferences {
    fn default() -> Self {
        Self {
            interface_typography: InterfaceTypography::default(),
            typography: ReaderTypography::default(),
            typesetting: ReaderTypesetting::unified(),
            language: AppLanguage::default(),
            spread: SpreadMode::Single,
            reading_mode: ReadingMode::Focus,
            theme: AppTheme::default(),
            selection_granularity: SelectionGranularity::Free,
            shortcuts: ShortcutPreferences::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReadingMode {
    Classic,
    #[default]
    Focus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ShortcutPreferences {
    #[serde(default = "default_fullscreen_shortcut")]
    pub(crate) fullscreen: egui::KeyboardShortcut,
    #[serde(default = "default_toggle_left_sidebar_shortcut")]
    pub(crate) toggle_left_sidebar: egui::KeyboardShortcut,
    #[serde(default = "default_toggle_right_sidebar_shortcut")]
    pub(crate) toggle_right_sidebar: egui::KeyboardShortcut,
    #[serde(default = "default_toggle_translation_shortcut")]
    pub(crate) toggle_translation: egui::KeyboardShortcut,
    #[serde(default = "default_return_to_shelf_shortcut")]
    pub(crate) return_to_shelf: egui::KeyboardShortcut,
    #[serde(default = "default_focus_actions_shortcut")]
    pub(crate) focus_actions: egui::KeyboardShortcut,
    #[serde(default = "default_focus_chat_shortcut")]
    pub(crate) focus_chat: egui::KeyboardShortcut,
    #[serde(default = "default_focus_highlight_shortcut")]
    pub(crate) focus_highlight: egui::KeyboardShortcut,
    #[serde(default = "default_focus_note_shortcut")]
    pub(crate) focus_note: egui::KeyboardShortcut,
    #[serde(default = "default_focus_structure_shortcut")]
    pub(crate) focus_structure: egui::KeyboardShortcut,
    #[serde(default = "default_focus_footnotes_shortcut")]
    pub(crate) focus_footnotes: egui::KeyboardShortcut,
}

impl ShortcutPreferences {
    pub(crate) fn has_conflicts(&self) -> bool {
        let bindings = self.bindings();
        bindings
            .iter()
            .enumerate()
            .any(|(index, binding)| bindings[index + 1..].contains(binding))
    }

    pub(crate) fn has_oversized_chords(&self) -> bool {
        self.bindings()
            .into_iter()
            .any(|binding| shortcut_chord_key_count(binding.modifiers) > MAX_SHORTCUT_KEYS)
    }

    fn bindings(&self) -> [egui::KeyboardShortcut; 11] {
        [
            self.fullscreen,
            self.toggle_left_sidebar,
            self.toggle_right_sidebar,
            self.toggle_translation,
            self.return_to_shelf,
            self.focus_actions,
            self.focus_chat,
            self.focus_highlight,
            self.focus_note,
            self.focus_structure,
            self.focus_footnotes,
        ]
    }
}

pub(crate) const MAX_SHORTCUT_KEYS: usize = 3;

pub(crate) fn shortcut_chord_key_count(modifiers: egui::Modifiers) -> usize {
    let command_alias = modifiers.command && !modifiers.ctrl && !modifiers.mac_cmd;
    1 + [
        modifiers.alt,
        modifiers.ctrl,
        modifiers.shift,
        modifiers.mac_cmd,
        command_alias,
    ]
    .into_iter()
    .filter(|enabled| *enabled)
    .count()
}

impl Default for ShortcutPreferences {
    fn default() -> Self {
        Self {
            fullscreen: default_fullscreen_shortcut(),
            toggle_left_sidebar: default_toggle_left_sidebar_shortcut(),
            toggle_right_sidebar: default_toggle_right_sidebar_shortcut(),
            toggle_translation: default_toggle_translation_shortcut(),
            return_to_shelf: default_return_to_shelf_shortcut(),
            focus_actions: default_focus_actions_shortcut(),
            focus_chat: default_focus_chat_shortcut(),
            focus_highlight: default_focus_highlight_shortcut(),
            focus_note: default_focus_note_shortcut(),
            focus_structure: default_focus_structure_shortcut(),
            focus_footnotes: default_focus_footnotes_shortcut(),
        }
    }
}

const fn default_fullscreen_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::F11)
}

const fn default_toggle_left_sidebar_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::B)
}

const fn default_toggle_right_sidebar_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::E)
}

const fn default_toggle_translation_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::T)
}

const fn default_return_to_shelf_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Q)
}

const fn default_focus_actions_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Space)
}

const fn default_focus_chat_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Tab)
}

const fn default_focus_highlight_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Num1)
}

const fn default_focus_note_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Num2)
}

const fn default_focus_structure_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Num3)
}

const fn default_focus_footnotes_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::AltLeft)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterfaceTypography {
    pub(crate) font_family: String,
    pub(crate) font_size: f32,
}

impl InterfaceTypography {
    pub(crate) fn normalize(&mut self) {
        self.font_family = self.font_family.trim().to_owned();
        if self.font_family.is_empty() {
            self.font_family = SYSTEM_INTERFACE_FONT.into();
        }
        if !self.font_size.is_finite() {
            self.font_size = DEFAULT_INTERFACE_FONT_SIZE;
        }
        self.font_size = self.font_size.clamp(10.0, 24.0);
    }
}

impl Default for InterfaceTypography {
    fn default() -> Self {
        Self {
            font_family: SYSTEM_INTERFACE_FONT.into(),
            font_size: DEFAULT_INTERFACE_FONT_SIZE,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredReaderPreferences {
    version: u32,
    #[serde(default)]
    interface_typography: InterfaceTypography,
    #[serde(default)]
    typography: ReaderTypography,
    #[serde(default = "default_typesetting")]
    typesetting: ReaderTypesetting,
    #[serde(default)]
    language: AppLanguage,
    #[serde(default)]
    theme: StoredAppTheme,
    #[serde(default = "default_spread")]
    spread: StoredSpreadMode,
    #[serde(default)]
    reading_mode: StoredReadingMode,
    #[serde(default)]
    selection_granularity: StoredSelectionGranularity,
    #[serde(default)]
    shortcuts: ShortcutPreferences,
}

fn default_typesetting() -> ReaderTypesetting {
    ReaderTypesetting::unified()
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredReadingMode {
    Classic,
    #[default]
    Focus,
}

impl From<StoredReadingMode> for ReadingMode {
    fn from(value: StoredReadingMode) -> Self {
        match value {
            StoredReadingMode::Classic => Self::Classic,
            StoredReadingMode::Focus => Self::Focus,
        }
    }
}

impl From<ReadingMode> for StoredReadingMode {
    fn from(value: ReadingMode) -> Self {
        match value {
            ReadingMode::Classic => Self::Classic,
            ReadingMode::Focus => Self::Focus,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredSpreadMode {
    #[default]
    Single,
    Double,
    Scroll,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredSelectionGranularity {
    #[default]
    Free,
    Word,
    Sentence,
    Paragraph,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StoredAppTheme {
    #[default]
    System,
    Light,
    Dark,
    Glass,
}

impl From<StoredAppTheme> for AppTheme {
    fn from(value: StoredAppTheme) -> Self {
        match value {
            StoredAppTheme::System => Self::System,
            StoredAppTheme::Light | StoredAppTheme::Glass => Self::Light,
            StoredAppTheme::Dark => Self::Dark,
        }
    }
}

impl From<AppTheme> for StoredAppTheme {
    fn from(value: AppTheme) -> Self {
        match value {
            AppTheme::System => Self::System,
            AppTheme::Light => Self::Light,
            AppTheme::Dark => Self::Dark,
        }
    }
}

impl From<StoredSelectionGranularity> for SelectionGranularity {
    fn from(value: StoredSelectionGranularity) -> Self {
        match value {
            StoredSelectionGranularity::Free => Self::Free,
            StoredSelectionGranularity::Word => Self::Word,
            StoredSelectionGranularity::Sentence => Self::Sentence,
            StoredSelectionGranularity::Paragraph => Self::Paragraph,
        }
    }
}

impl From<SelectionGranularity> for StoredSelectionGranularity {
    fn from(value: SelectionGranularity) -> Self {
        match value {
            SelectionGranularity::Free => Self::Free,
            SelectionGranularity::Word => Self::Word,
            SelectionGranularity::Sentence => Self::Sentence,
            SelectionGranularity::Paragraph => Self::Paragraph,
        }
    }
}

impl From<StoredSpreadMode> for SpreadMode {
    fn from(value: StoredSpreadMode) -> Self {
        match value {
            StoredSpreadMode::Single => Self::Single,
            StoredSpreadMode::Double => Self::Double,
            StoredSpreadMode::Scroll => Self::Scroll,
        }
    }
}

impl From<SpreadMode> for StoredSpreadMode {
    fn from(value: SpreadMode) -> Self {
        match value {
            SpreadMode::Single => Self::Single,
            SpreadMode::Double => Self::Double,
            SpreadMode::Scroll => Self::Scroll,
        }
    }
}

const fn default_spread() -> StoredSpreadMode {
    StoredSpreadMode::Single
}

pub(crate) fn load_reader_preferences() -> PreferencesResult<ReaderPreferences> {
    load_from(settings_path()?)
}

pub(crate) fn save_reader_preferences(preferences: &ReaderPreferences) -> PreferencesResult<()> {
    save_to(&settings_path()?, preferences)
}

pub(crate) fn load_app_language() -> PreferencesResult<AppLanguage> {
    Ok(load_reader_preferences()?.language)
}

fn settings_path() -> io::Result<PathBuf> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定阅读设置目录"))?;
    Ok(project.config_dir().join(SETTINGS_FILE))
}

fn load_from(path: PathBuf) -> PreferencesResult<ReaderPreferences> {
    if !path.exists() {
        return Ok(ReaderPreferences::default());
    }
    let stored: StoredReaderPreferences = serde_json::from_slice(&fs::read(path)?)?;
    if stored.version != SETTINGS_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("不支持的阅读设置版本：{}", stored.version),
        )
        .into());
    }
    let mut interface_typography = stored.interface_typography;
    interface_typography.normalize();
    let mut typography = stored.typography;
    typography.normalize();
    let mut typesetting = stored.typesetting;
    typesetting.normalize();
    migrate_legacy_typesetting_defaults(&mut typesetting);
    Ok(ReaderPreferences {
        interface_typography,
        typography,
        typesetting,
        language: stored.language,
        spread: stored.spread.into(),
        reading_mode: stored.reading_mode.into(),
        theme: stored.theme.into(),
        selection_granularity: stored.selection_granularity.into(),
        shortcuts: stored.shortcuts,
    })
}

fn migrate_legacy_typesetting_defaults(typesetting: &mut ReaderTypesetting) {
    if typesetting.mode == TypesettingMode::Unified {
        typesetting.line_break_strategy = LineBreakStrategy::Optimized;
    }
    if (typesetting.body_line_height - LEGACY_BODY_LINE_HEIGHT).abs() < f32::EPSILON {
        typesetting.body_line_height = 1.5;
    }
    if (typesetting.paragraph_gap_em - LEGACY_PARAGRAPH_GAP_EM).abs() < f32::EPSILON {
        typesetting.paragraph_gap_em = 0.5;
    }
}

fn save_to(path: &Path, preferences: &ReaderPreferences) -> PreferencesResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "阅读设置路径没有父目录"))?;
    fs::create_dir_all(parent)?;
    let mut interface_typography = preferences.interface_typography.clone();
    interface_typography.normalize();
    let mut typography = preferences.typography.clone();
    typography.normalize();
    let mut typesetting = preferences.typesetting.clone();
    typesetting.normalize();
    let stored = StoredReaderPreferences {
        version: SETTINGS_VERSION,
        interface_typography,
        typography,
        typesetting,
        language: preferences.language,
        spread: preferences.spread.into(),
        reading_mode: preferences.reading_mode.into(),
        theme: preferences.theme.into(),
        selection_granularity: preferences.selection_granularity.into(),
        shortcuts: preferences.shortcuts.clone(),
    };
    write_json_atomic(path, &stored)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::ReaderDefaultFont;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn typography_round_trips_and_normalizes() {
        let path = test_path();
        let typography = ReaderTypography {
            default_font: ReaderDefaultFont::SansSerif,
            default_cjk_font: "  Microsoft YaHei  ".into(),
            serif_font: "Literata".into(),
            sans_serif_font: "Noto Sans".into(),
            monospace_font: "Fira Code".into(),
            font_size: 18.0,
            minimum_font_size: 9.0,
            font_weight: 550,
        };

        let preferences = ReaderPreferences {
            interface_typography: InterfaceTypography {
                font_family: "  Microsoft YaHei UI  ".into(),
                font_size: 15.0,
            },
            typography,
            typesetting: ReaderTypesetting {
                heading_scale: 1.8,
                paragraph_gap_em: 0.9,
                ..ReaderTypesetting::unified()
            },
            language: AppLanguage::English,
            spread: SpreadMode::Single,
            reading_mode: ReadingMode::Focus,
            theme: AppTheme::Dark,
            selection_granularity: SelectionGranularity::Sentence,
            shortcuts: ShortcutPreferences {
                toggle_left_sidebar: egui::KeyboardShortcut::new(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::L,
                ),
                ..ShortcutPreferences::default()
            },
        };
        save_to(&path, &preferences).unwrap();
        let loaded = load_from(path.clone()).unwrap();

        assert_eq!(loaded.typography.default_font, ReaderDefaultFont::SansSerif);
        assert_eq!(
            loaded.interface_typography.font_family,
            "Microsoft YaHei UI"
        );
        assert!((loaded.interface_typography.font_size - 15.0).abs() < f32::EPSILON);
        assert_eq!(loaded.typography.default_cjk_font, "Microsoft YaHei");
        assert!((loaded.typography.font_size - 18.0).abs() < f32::EPSILON);
        assert!((loaded.typography.minimum_font_size - 9.0).abs() < f32::EPSILON);
        assert_eq!(loaded.typography.font_weight, 600);
        assert_eq!(loaded.typesetting.mode, TypesettingMode::Unified);
        assert!((loaded.typesetting.heading_scale - 1.8).abs() < f32::EPSILON);
        assert!((loaded.typesetting.paragraph_gap_em - 0.9).abs() < f32::EPSILON);
        assert_eq!(loaded.language, AppLanguage::English);
        assert_eq!(loaded.spread, SpreadMode::Single);
        assert_eq!(loaded.reading_mode, ReadingMode::Focus);
        assert_eq!(loaded.theme, AppTheme::Dark);
        assert_eq!(loaded.selection_granularity, SelectionGranularity::Sentence);
        assert_eq!(
            loaded.shortcuts.toggle_left_sidebar,
            egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                egui::Key::L
            )
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn preferences_without_a_language_follow_the_system() {
        let json = r#"{"version":1}"#;
        let stored: StoredReaderPreferences = serde_json::from_str(json).unwrap();
        assert_eq!(stored.language, AppLanguage::System);
        assert_eq!(stored.interface_typography, InterfaceTypography::default());
        assert_eq!(stored.typesetting.mode, TypesettingMode::Unified);
        assert!(matches!(stored.spread, StoredSpreadMode::Single));
        assert!(matches!(stored.reading_mode, StoredReadingMode::Focus));
        assert!(matches!(stored.theme, StoredAppTheme::System));
        assert!(matches!(
            stored.selection_granularity,
            StoredSelectionGranularity::Free
        ));
        assert_eq!(stored.shortcuts, ShortcutPreferences::default());
    }

    #[test]
    fn legacy_typesetting_defaults_migrate_to_the_compact_profile() {
        let mut typesetting = ReaderTypesetting {
            line_break_strategy: LineBreakStrategy::Greedy,
            body_line_height: LEGACY_BODY_LINE_HEIGHT,
            paragraph_gap_em: LEGACY_PARAGRAPH_GAP_EM,
            ..ReaderTypesetting::unified()
        };

        migrate_legacy_typesetting_defaults(&mut typesetting);

        assert!((typesetting.body_line_height - 1.5).abs() < f32::EPSILON);
        assert!((typesetting.paragraph_gap_em - 0.5).abs() < f32::EPSILON);
        assert_eq!(
            typesetting.line_break_strategy,
            LineBreakStrategy::Optimized
        );
    }

    #[test]
    fn system_locale_is_resolved_to_a_supported_interface_language() {
        assert_eq!(
            language_from_system_locale("zh-CN"),
            AppLanguage::SimplifiedChinese
        );
        assert_eq!(
            language_from_system_locale("zh-TW"),
            AppLanguage::SimplifiedChinese
        );
        assert_eq!(language_from_system_locale("en-US"), AppLanguage::English);
        assert_eq!(AppLanguage::default(), AppLanguage::System);
    }

    #[test]
    fn shortcut_defaults_match_the_reader_contract_and_detect_conflicts() {
        let mut shortcuts = ShortcutPreferences::default();
        assert_eq!(
            shortcuts.fullscreen,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::F11)
        );
        assert_eq!(
            shortcuts.toggle_left_sidebar,
            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::B)
        );
        assert_eq!(
            shortcuts.toggle_right_sidebar,
            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::E)
        );
        assert_eq!(
            shortcuts.toggle_translation,
            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::T)
        );
        assert_eq!(
            shortcuts.return_to_shelf,
            egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::Q)
        );
        assert_eq!(
            shortcuts.focus_footnotes,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::AltLeft)
        );
        assert_eq!(
            shortcuts.focus_structure,
            egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::Num3)
        );
        assert!(!shortcuts.has_conflicts());

        shortcuts.focus_note = shortcuts.focus_highlight;
        assert!(shortcuts.has_conflicts());

        shortcuts.focus_note = egui::KeyboardShortcut::new(
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT | egui::Modifiers::ALT,
            egui::Key::N,
        );
        assert_eq!(shortcut_chord_key_count(shortcuts.focus_note.modifiers), 4);
        assert!(shortcuts.has_oversized_chords());
    }

    #[test]
    fn legacy_glass_theme_migrates_to_light() {
        let json = r#"{"version":1,"theme":"glass"}"#;
        let stored: StoredReaderPreferences = serde_json::from_str(json).unwrap();

        assert_eq!(AppTheme::from(stored.theme), AppTheme::Light);
    }

    #[test]
    fn app_theme_cycles_through_system_light_and_dark() {
        assert_eq!(AppTheme::System.next(), AppTheme::Light);
        assert_eq!(AppTheme::Light.next(), AppTheme::Dark);
        assert_eq!(AppTheme::Dark.next(), AppTheme::System);
    }

    #[test]
    fn scroll_mode_round_trips_through_stored_preferences() {
        let stored = StoredSpreadMode::from(SpreadMode::Scroll);
        let json = serde_json::to_string(&stored).unwrap();

        assert_eq!(json, "\"scroll\"");
        assert_eq!(SpreadMode::from(stored), SpreadMode::Scroll);
    }

    #[test]
    fn selection_granularity_round_trips_through_stored_preferences() {
        for granularity in [
            SelectionGranularity::Free,
            SelectionGranularity::Word,
            SelectionGranularity::Sentence,
            SelectionGranularity::Paragraph,
        ] {
            let stored = StoredSelectionGranularity::from(granularity);
            let json = serde_json::to_string(&stored).unwrap();
            let decoded: StoredSelectionGranularity = serde_json::from_str(&json).unwrap();

            assert_eq!(SelectionGranularity::from(decoded), granularity);
        }
    }

    #[test]
    fn interface_typography_normalizes_missing_and_out_of_range_values() {
        let mut typography = InterfaceTypography {
            font_family: "   ".into(),
            font_size: f32::NAN,
        };
        typography.normalize();
        assert_eq!(typography, InterfaceTypography::default());

        typography.font_size = 100.0;
        typography.normalize();
        assert!((typography.font_size - 24.0).abs() < f32::EPSILON);
    }

    fn test_path() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rebook-reader-settings-{}-{timestamp}.json",
            std::process::id()
        ))
    }
}
