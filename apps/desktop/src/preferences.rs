use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use rebook_layout::{ReaderTypesetting, ReaderTypography, SpreadMode};
use rebook_reader::SelectionGranularity;
use serde::{Deserialize, Serialize};

use crate::persistence::write_json_atomic;

const SETTINGS_VERSION: u32 = 1;
const SETTINGS_FILE: &str = "reader-settings.json";
pub(crate) const SYSTEM_INTERFACE_FONT: &str = "System UI";
pub(crate) const DEFAULT_INTERFACE_FONT_SIZE: f32 = 14.0;

pub type PreferencesResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AppLanguage {
    #[default]
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

impl AppLanguage {
    pub(crate) const fn text(
        self,
        simplified_chinese: &'static str,
        english: &'static str,
    ) -> &'static str {
        match self {
            Self::SimplifiedChinese => simplified_chinese,
            Self::English => english,
        }
    }

    pub(crate) const fn translation_target(self) -> &'static str {
        match self {
            Self::SimplifiedChinese => "简体中文",
            Self::English => "English",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AppTheme {
    #[default]
    #[serde(rename = "light")]
    Light,
    #[serde(rename = "dark")]
    Dark,
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
}

impl Default for ReaderPreferences {
    fn default() -> Self {
        Self {
            interface_typography: InterfaceTypography::default(),
            typography: ReaderTypography::default(),
            typesetting: ReaderTypesetting::unified(),
            language: AppLanguage::default(),
            spread: SpreadMode::Double,
            reading_mode: ReadingMode::Focus,
            theme: AppTheme::default(),
            selection_granularity: SelectionGranularity::Free,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReadingMode {
    Classic,
    #[default]
    Focus,
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
    Single,
    #[default]
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
    Light,
    Dark,
    Glass,
}

impl From<StoredAppTheme> for AppTheme {
    fn from(value: StoredAppTheme) -> Self {
        match value {
            StoredAppTheme::Light | StoredAppTheme::Glass => Self::Light,
            StoredAppTheme::Dark => Self::Dark,
        }
    }
}

impl From<AppTheme> for StoredAppTheme {
    fn from(value: AppTheme) -> Self {
        match value {
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
    StoredSpreadMode::Double
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
    Ok(ReaderPreferences {
        interface_typography,
        typography,
        typesetting,
        language: stored.language,
        spread: stored.spread.into(),
        reading_mode: stored.reading_mode.into(),
        theme: stored.theme.into(),
        selection_granularity: stored.selection_granularity.into(),
    })
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
    };
    write_json_atomic(path, &stored)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_layout::{ReaderDefaultFont, TypesettingMode};
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
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_preferences_default_to_simplified_chinese() {
        let json = r#"{"version":1}"#;
        let stored: StoredReaderPreferences = serde_json::from_str(json).unwrap();
        assert_eq!(stored.language, AppLanguage::SimplifiedChinese);
        assert_eq!(stored.interface_typography, InterfaceTypography::default());
        assert_eq!(stored.typesetting.mode, TypesettingMode::Unified);
        assert!(matches!(stored.spread, StoredSpreadMode::Double));
        assert!(matches!(stored.reading_mode, StoredReadingMode::Focus));
        assert!(matches!(stored.theme, StoredAppTheme::Light));
        assert!(matches!(
            stored.selection_granularity,
            StoredSelectionGranularity::Free
        ));
    }

    #[test]
    fn legacy_glass_theme_migrates_to_light() {
        let json = r#"{"version":1,"theme":"glass"}"#;
        let stored: StoredReaderPreferences = serde_json::from_str(json).unwrap();

        assert_eq!(AppTheme::from(stored.theme), AppTheme::Light);
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
