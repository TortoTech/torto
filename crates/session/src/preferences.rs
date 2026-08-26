use std::fs;
use std::io::Write as _;
use std::path::Path;

use atomicwrites::{AllowOverwrite, AtomicFile};
use directories::ProjectDirs;
use rebook_layout::{
    LineBreakStrategy, ReaderStyle, ReaderTypesetting, ReaderTypography, SpreadMode,
    TypesettingMode,
};
use rebook_reader::{ReaderPresentationPolicy, ReadingMode, SelectionGranularity};
use serde::Deserialize;
use serde_json::{Map, Value};

const SETTINGS_VERSION: u32 = 1;
const SETTINGS_FILE: &str = "reader-settings.json";
const LEGACY_BODY_LINE_HEIGHT: f32 = 1.72;
const LEGACY_PARAGRAPH_GAP_EM: f32 = 0.75;

/// Toolkit-neutral subset of Torto's persisted reader preferences.
///
/// Window theme, interface typography, language and key bindings remain
/// frontend concerns. These values are the complete document-facing input to
/// reader presentation and layout, and both desktop frontends resolve them
/// through the same policy before opening a [`rebook_reader::ReaderSession`].
#[derive(Clone, Debug, PartialEq)]
pub struct ReaderDocumentPreferences {
    pub typography: ReaderTypography,
    pub typesetting: ReaderTypesetting,
    pub spread: SpreadMode,
    pub reading_mode: ReadingMode,
    pub selection_granularity: SelectionGranularity,
}

impl ReaderDocumentPreferences {
    /// Reads the document-facing fields from the existing desktop settings
    /// file. Serde intentionally ignores egui-specific sibling fields.
    pub fn load_default() -> Result<Self, String> {
        let project = ProjectDirs::from("com", "Rebook", "Rebook")
            .ok_or_else(|| "无法确定阅读设置目录".to_owned())?;
        Self::load_from(project.config_dir().join(SETTINGS_FILE))
    }

    /// Loads one persisted settings document while preserving version-1
    /// compatibility with the legacy desktop application.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("读取阅读设置失败：{error}")),
        };
        let stored = serde_json::from_slice::<StoredReaderDocumentPreferences>(&bytes)
            .map_err(|error| format!("阅读设置 JSON 无效：{error}"))?;
        if stored.version != SETTINGS_VERSION {
            return Err(format!("不支持的阅读设置版本：{}", stored.version));
        }
        let mut preferences = Self {
            typography: stored.typography,
            typesetting: stored.typesetting,
            spread: stored.spread.into(),
            reading_mode: stored.reading_mode.into(),
            selection_granularity: stored.selection_granularity.into(),
        };
        preferences.normalize();
        Ok(preferences)
    }

    /// Atomically updates only the document-facing fields in the existing
    /// desktop settings file. Frontend-owned siblings such as theme, interface
    /// typography and shortcuts are retained byte-for-value through the JSON
    /// object, so either desktop frontend can persist this shared subset.
    pub fn save_default(&self) -> Result<(), String> {
        let project = ProjectDirs::from("com", "Rebook", "Rebook")
            .ok_or_else(|| "无法确定阅读设置目录".to_owned())?;
        self.save_to(project.config_dir().join(SETTINGS_FILE))
    }

    /// Saves the normalized document-facing fields to one version-1 settings
    /// document without replacing frontend-specific sibling fields.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let mut document = read_settings_document(path)?;
        let object = document
            .as_object_mut()
            .ok_or_else(|| "阅读设置 JSON 顶层必须是对象".to_owned())?;
        validate_settings_version(object)?;

        let mut normalized = self.clone();
        normalized.normalize();
        object.insert("version".into(), Value::from(SETTINGS_VERSION));
        object.insert(
            "typography".into(),
            serde_json::to_value(&normalized.typography)
                .map_err(|error| format!("序列化阅读字体设置失败：{error}"))?,
        );
        object.insert(
            "typesetting".into(),
            serde_json::to_value(&normalized.typesetting)
                .map_err(|error| format!("序列化阅读版式设置失败：{error}"))?,
        );
        object.insert("spread".into(), Value::from(spread_name(normalized.spread)));
        object.insert(
            "reading_mode".into(),
            Value::from(reading_mode_name(normalized.reading_mode)),
        );
        object.insert(
            "selection_granularity".into(),
            Value::from(selection_granularity_name(normalized.selection_granularity)),
        );
        write_settings_document(path, &document)
    }

    /// Applies one frontend-independent preference command and normalizes the
    /// result before it can become a layout cache key. Returns whether the
    /// effective persisted document preferences changed.
    pub fn apply(&mut self, change: ReaderDocumentPreferenceChange) -> bool {
        let previous = self.clone();
        match change {
            ReaderDocumentPreferenceChange::Typography(typography) => {
                self.typography = typography;
            }
            ReaderDocumentPreferenceChange::Typesetting(typesetting) => {
                self.typesetting = typesetting;
            }
            ReaderDocumentPreferenceChange::Spread(spread) => self.spread = spread,
            ReaderDocumentPreferenceChange::ReadingMode(mode) => self.reading_mode = mode,
            ReaderDocumentPreferenceChange::SelectionGranularity(granularity) => {
                self.selection_granularity = granularity;
            }
        }
        self.normalize();
        *self != previous
    }

    /// Repairs externally supplied or legacy persisted values before they enter
    /// layout cache keys or presentation policy resolution.
    pub fn normalize(&mut self) {
        self.typography.normalize();
        self.typesetting.normalize();
        if self.typesetting.mode == TypesettingMode::Unified {
            self.typesetting.line_break_strategy = LineBreakStrategy::Optimized;
        }
        if (self.typesetting.body_line_height - LEGACY_BODY_LINE_HEIGHT).abs() < f32::EPSILON {
            self.typesetting.body_line_height = 1.5;
        }
        if (self.typesetting.paragraph_gap_em - LEGACY_PARAGRAPH_GAP_EM).abs() < f32::EPSILON {
            self.typesetting.paragraph_gap_em = 0.5;
        }
    }

    /// Resolves requested preferences into the exact layout-facing style and
    /// effective presentation consumed by a reader session.
    #[must_use]
    pub fn resolve(
        &self,
        focus_supported: bool,
        fixed_page: bool,
    ) -> ResolvedReaderDocumentPreferences {
        let mut normalized = self.clone();
        normalized.normalize();
        let presentation = ReaderPresentationPolicy::resolve(
            normalized.reading_mode,
            normalized.spread,
            focus_supported,
        );
        let mut style = ReaderStyle {
            typography: normalized.typography,
            typesetting: normalized.typesetting,
            ..ReaderStyle::default()
        };
        presentation.apply_to_style(&mut style);
        if fixed_page {
            style.column_gap = 0.0;
        }
        ResolvedReaderDocumentPreferences {
            style,
            presentation,
            selection_granularity: normalized.selection_granularity,
        }
    }
}

/// One toolkit-neutral edit to persisted document reading preferences.
#[derive(Clone, Debug, PartialEq)]
pub enum ReaderDocumentPreferenceChange {
    Typography(ReaderTypography),
    Typesetting(ReaderTypesetting),
    Spread(SpreadMode),
    ReadingMode(ReadingMode),
    SelectionGranularity(SelectionGranularity),
}

impl Default for ReaderDocumentPreferences {
    fn default() -> Self {
        Self {
            typography: ReaderTypography::default(),
            typesetting: ReaderTypesetting::unified(),
            spread: SpreadMode::Single,
            reading_mode: ReadingMode::Focus,
            selection_granularity: SelectionGranularity::Free,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedReaderDocumentPreferences {
    pub style: ReaderStyle,
    pub presentation: ReaderPresentationPolicy,
    pub selection_granularity: SelectionGranularity,
}

#[derive(Deserialize)]
struct StoredReaderDocumentPreferences {
    version: u32,
    #[serde(default)]
    typography: ReaderTypography,
    #[serde(default = "default_typesetting")]
    typesetting: ReaderTypesetting,
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

const fn default_spread() -> StoredSpreadMode {
    StoredSpreadMode::Single
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredSpreadMode {
    #[default]
    Single,
    Double,
    Scroll,
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

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum StoredSelectionGranularity {
    #[default]
    Free,
    Word,
    Sentence,
    Paragraph,
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

fn read_settings_document(path: &Path) -> Result<Value, String> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("阅读设置 JSON 无效：{error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(error) => Err(format!("读取阅读设置失败：{error}")),
    }
}

fn validate_settings_version(object: &Map<String, Value>) -> Result<(), String> {
    let Some(version) = object.get("version") else {
        return Ok(());
    };
    let Some(version) = version.as_u64() else {
        return Err("阅读设置版本必须是整数".to_owned());
    };
    if version != u64::from(SETTINGS_VERSION) {
        return Err(format!("不支持的阅读设置版本：{version}"));
    }
    Ok(())
}

fn write_settings_document(path: &Path, document: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "阅读设置路径没有父目录".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建阅读设置目录失败：{error}"))?;
    let bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| format!("序列化阅读设置失败：{error}"))?;
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| {
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .map_err(|error| format!("写入阅读设置失败：{error}"))
}

const fn spread_name(spread: SpreadMode) -> &'static str {
    match spread {
        SpreadMode::Single => "single",
        SpreadMode::Double => "double",
        SpreadMode::Scroll => "scroll",
    }
}

const fn reading_mode_name(mode: ReadingMode) -> &'static str {
    match mode {
        ReadingMode::Classic => "classic",
        ReadingMode::Focus => "focus",
    }
}

const fn selection_granularity_name(granularity: SelectionGranularity) -> &'static str {
    match granularity {
        SelectionGranularity::Free => "free",
        SelectionGranularity::Word => "word",
        SelectionGranularity::Sentence => "sentence",
        SelectionGranularity::Paragraph => "paragraph",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use rebook_layout::{ReaderDefaultFont, TypesettingMode};

    use super::*;

    #[test]
    fn reads_the_legacy_desktop_file_without_egui_fields() {
        let path = test_path();
        fs::write(
            &path,
            r#"{
                "version": 1,
                "interface_typography": { "font_family": "UI", "font_size": 15 },
                "typography": {
                    "default_font": "sans-serif",
                    "default_cjk_font": " Microsoft YaHei ",
                    "font_size": 18,
                    "minimum_font_size": 12,
                    "font_weight": 550
                },
                "typesetting": {
                    "mode": "unified",
                    "line_break_strategy": "greedy",
                    "body_line_height": 1.72,
                    "paragraph_gap_em": 0.75
                },
                "spread": "double",
                "reading_mode": "classic",
                "selection_granularity": "sentence",
                "theme": "dark",
                "shortcuts": { "frontend_only": true }
            }"#,
        )
        .unwrap();

        let preferences = ReaderDocumentPreferences::load_from(&path).unwrap();

        assert_eq!(
            preferences.typography.default_font,
            ReaderDefaultFont::SansSerif
        );
        assert_eq!(preferences.typography.default_cjk_font, "Microsoft YaHei");
        assert_eq!(preferences.typography.font_weight, 600);
        assert_eq!(preferences.typesetting.mode, TypesettingMode::Unified);
        assert_eq!(
            preferences.typesetting.line_break_strategy,
            LineBreakStrategy::Optimized
        );
        assert!((preferences.typesetting.body_line_height - 1.5).abs() < f32::EPSILON);
        assert!((preferences.typesetting.paragraph_gap_em - 0.5).abs() < f32::EPSILON);
        assert_eq!(preferences.spread, SpreadMode::Double);
        assert_eq!(preferences.reading_mode, ReadingMode::Classic);
        assert_eq!(
            preferences.selection_granularity,
            SelectionGranularity::Sentence
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn resolution_is_shared_for_focus_classic_and_fixed_page_books() {
        let preferences = ReaderDocumentPreferences::default();
        let focus = preferences.resolve(true, false);
        assert_eq!(focus.presentation.mode, ReadingMode::Focus);
        assert_eq!(focus.style.spread, SpreadMode::Scroll);
        assert!(focus.style.focus_footnote_icons);
        assert_eq!(focus.style.typesetting, ReaderTypesetting::unified());

        let classic_fallback = preferences.resolve(false, true);
        assert_eq!(classic_fallback.presentation.mode, ReadingMode::Classic);
        assert_eq!(classic_fallback.style.spread, SpreadMode::Single);
        assert!(!classic_fallback.style.focus_footnote_icons);
        assert!(classic_fallback.style.column_gap.abs() < f32::EPSILON);
    }

    #[test]
    fn missing_file_uses_the_same_focus_unified_defaults() {
        let path = test_path();
        assert_eq!(
            ReaderDocumentPreferences::load_from(path).unwrap(),
            ReaderDocumentPreferences::default()
        );
    }

    #[test]
    fn shared_save_preserves_frontend_fields_and_round_trips_changes() {
        let path = test_path();
        fs::write(
            &path,
            r#"{
                "version": 1,
                "interface_typography": { "font_family": "UI", "font_size": 15 },
                "typography": { "font_size": 20 },
                "theme": "dark",
                "shortcuts": { "focus_chat": "Tab" },
                "frontend_extension": { "future": true }
            }"#,
        )
        .unwrap();
        let mut preferences = ReaderDocumentPreferences::load_from(&path).unwrap();
        preferences.apply(ReaderDocumentPreferenceChange::Spread(SpreadMode::Double));
        preferences.apply(ReaderDocumentPreferenceChange::ReadingMode(
            ReadingMode::Classic,
        ));
        preferences.apply(ReaderDocumentPreferenceChange::SelectionGranularity(
            SelectionGranularity::Sentence,
        ));
        preferences.save_to(&path).unwrap();

        assert_eq!(
            ReaderDocumentPreferences::load_from(&path).unwrap(),
            preferences
        );
        let document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["theme"], "dark");
        assert_eq!(document["interface_typography"]["font_family"], "UI");
        assert_eq!(document["shortcuts"]["focus_chat"], "Tab");
        assert_eq!(document["frontend_extension"]["future"], true);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn shared_save_refuses_to_overwrite_an_unknown_settings_version() {
        let path = test_path();
        let original = br#"{"version":99,"theme":"dark"}"#;
        fs::write(&path, original).unwrap();

        let error = ReaderDocumentPreferences::default()
            .save_to(&path)
            .unwrap_err();

        assert!(error.contains("99"));
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn preference_commands_normalize_before_reporting_effective_changes() {
        let mut preferences = ReaderDocumentPreferences::default();
        assert!(
            !preferences.apply(ReaderDocumentPreferenceChange::ReadingMode(
                ReadingMode::Focus
            ))
        );
        let mut typography = preferences.typography.clone();
        typography.font_size = 10_000.0;
        assert!(preferences.apply(ReaderDocumentPreferenceChange::Typography(typography)));
        assert!((preferences.typography.font_size - 120.0).abs() < f32::EPSILON);

        let mut typesetting = ReaderTypesetting::unified();
        typesetting.line_break_strategy = LineBreakStrategy::Greedy;
        assert!(!preferences.apply(ReaderDocumentPreferenceChange::Typesetting(typesetting)));
        assert_eq!(
            preferences.typesetting.line_break_strategy,
            LineBreakStrategy::Optimized
        );
    }

    fn test_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "torto-reader-document-preferences-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
