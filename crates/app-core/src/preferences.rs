use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppLanguage {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

impl AppLanguage {
    pub fn text(self, simplified_chinese: &'static str, english: &'static str) -> &'static str {
        match self.resolved() {
            Self::System => unreachable!("resolved language cannot be system"),
            Self::SimplifiedChinese => simplified_chinese,
            Self::English => english,
        }
    }

    pub fn translation_target(self) -> &'static str {
        match self.resolved() {
            Self::System => unreachable!("resolved language cannot be system"),
            Self::SimplifiedChinese => "简体中文",
            Self::English => "English",
        }
    }

    pub fn system_translation_target() -> &'static str {
        SYSTEM_LANGUAGE.translation_target()
    }

    pub fn resolved(self) -> Self {
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

pub fn language_from_system_locale(locale: &str) -> AppLanguage {
    if locale.trim().to_ascii_lowercase().starts_with("zh") {
        AppLanguage::SimplifiedChinese
    } else {
        AppLanguage::English
    }
}
