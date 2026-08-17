//! Built-in reader plugins. Plugins consume publication semantics and return
//! stable source-backed results; none of them depend on Xilem or the renderer.

mod ai;
mod commands;
mod llm_json;
mod pdf_ocr;
mod pdf_toc;
mod pdf_vision;
mod rewrite;
mod search;
mod translation;

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::persistence::write_json_atomic;

pub(crate) use ai::{
    CHAT_CITATION_PREFIX, ChatAnnotationAction, ChatSelection, chat_citation_link,
    chat_citation_marker_from_link,
};
pub use ai::{
    ChatReadingContext, ChatResponse, ChatRole, ChatTurn, chat_with_book, translate_blocks,
    translate_blocks_incremental,
};
pub use commands::{
    ChatCommand, ChatCommandResolution, ChatRequestKind, chat_command_suggestions,
    resolve_chat_command,
};
pub(crate) use pdf_ocr::{
    PDF_PAGE_ANCHOR_PREFIX, PdfOcrPageRole, PdfOcrPageRoleAssignment, PdfOcrSourceController,
    PdfOcrSyncData, PdfOcrViewMode, export_pdf_ocr_sync_data, has_pending_pdf_ocr_task,
    import_pdf_ocr_sync_data, load_pdf_ocr_source, recognize_pdf, save_pdf_ocr_page_roles,
    set_pdf_ocr_view_mode,
};
pub(crate) use pdf_toc::{PdfMetadataExtraction, extract_pdf_metadata};
pub use rewrite::RewriteBookSource;
pub use search::{BookSearchResult, search_book};
pub(crate) use search::{section_title, text_block_text};
pub use translation::{BlockTranslation, TranslationBlockInput, TranslationBookSource};

const SETTINGS_FILE: &str = "plugins.json";
const AI_CREDENTIAL_SERVICE: &str = "Rebook AI";
const PDF_OCR_CREDENTIAL_SERVICE: &str = "Rebook PDF OCR";
const DEFAULT_PROVIDER_ID: &str = "openai";
const DEFAULT_MODEL: &str = "gpt-4o-mini";
const DEFAULT_CHAT_MAX_TOOL_STEPS: u16 = 24;
const CHAT_TOOL_DEFAULTS_VERSION: u8 = 1;
const OCR_SELECTION_DEFAULTS_VERSION: u8 = 1;
const DEFAULT_CHAT_HISTORY_TURNS: u16 = 10;
pub(crate) const CHAT_TOOL_STEPS_MIN: u16 = 1;
pub(crate) const CHAT_TOOL_STEPS_MAX: u16 = 24;
pub(crate) const CHAT_HISTORY_TURNS_MIN: u16 = 1;
pub(crate) const CHAT_HISTORY_TURNS_MAX: u16 = 50;
pub(crate) const TARGET_LANGUAGE_INTERFACE: &str = "interface";
pub(crate) const TARGET_LANGUAGE_SIMPLIFIED_CHINESE: &str = "zh-CN";
pub(crate) const TARGET_LANGUAGE_ENGLISH: &str = "en";
pub(crate) const PADDLE_OCR_JOBS_URL: &str = "https://paddleocr.aistudio-app.com/api/v2/ocr/jobs";
pub(crate) const MINERU_API_URL: &str = "https://mineru.net/api/v4";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PdfOcrProviderKind {
    #[default]
    PaddleOcr,
    MinerU,
}

impl PdfOcrProviderKind {
    pub(crate) const ALL: [Self; 2] = [Self::PaddleOcr, Self::MinerU];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::PaddleOcr => "PaddleOCR",
            Self::MinerU => "MinerU",
        }
    }

    pub(crate) const fn credential_url(self) -> &'static str {
        match self {
            Self::PaddleOcr => "https://aistudio.baidu.com/paddleocr/task",
            Self::MinerU => "https://mineru.net/apiManage/docs",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AiProviderKind {
    #[default]
    Custom,
    OpenAi,
    DeepSeek,
    OpenRouter,
    SiliconFlow,
}

impl AiProviderKind {
    pub(crate) const ALL: [Self; 5] = [
        Self::Custom,
        Self::OpenAi,
        Self::DeepSeek,
        Self::OpenRouter,
        Self::SiliconFlow,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Custom => "Custom",
            Self::OpenAi => "OpenAI",
            Self::DeepSeek => "DeepSeek",
            Self::OpenRouter => "OpenRouter",
            Self::SiliconFlow => "SiliconFlow",
        }
    }

    const fn base_url(self) -> Option<&'static str> {
        match self {
            Self::Custom => None,
            Self::OpenAi => Some("https://api.openai.com/v1"),
            Self::DeepSeek => Some("https://api.deepseek.com"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::SiliconFlow => Some("https://api.siliconflow.cn/v1"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiProvider {
    pub id: String,
    pub(crate) kind: AiProviderKind,
    pub name: String,
    pub base_url: String,
    pub models: Vec<AiModelConfig>,
    /// Secrets are excluded from JSON and stored in Windows Credential Manager.
    /// `REBOOK_AI_API_KEY` can override the default provider at runtime.
    #[serde(skip)]
    pub api_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiModelConfig {
    pub id: String,
}

impl Default for AiModelConfig {
    fn default() -> Self {
        Self::language(DEFAULT_MODEL)
    }
}

impl AiModelConfig {
    pub(crate) fn language(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TranslationMode {
    Replace,
    #[default]
    Bilingual,
}

impl Default for AiProvider {
    fn default() -> Self {
        Self {
            id: DEFAULT_PROVIDER_ID.into(),
            kind: AiProviderKind::Custom,
            name: AiProviderKind::Custom.label().into(),
            base_url: String::new(),
            models: vec![AiModelConfig::language(DEFAULT_MODEL)],
            api_key: String::new(),
        }
    }
}

impl AiProvider {
    pub(crate) fn select_kind(&mut self, kind: AiProviderKind) {
        let old_kind = self.kind;
        self.kind = kind;
        if let Some(base_url) = kind.base_url() {
            self.base_url = base_url.into();
        }
        if self.name.trim().is_empty() || self.name == old_kind.label() {
            self.name = kind.label().into();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "plugin settings persist independent user-facing feature toggles"
)]
pub struct PluginSettings {
    pub providers: Vec<AiProvider>,
    pub chat_provider: String,
    pub chat_model: String,
    pub chat_max_tool_steps: u16,
    pub chat_history_turns: u16,
    pub ocr_enabled: bool,
    pub ocr_provider: String,
    pub ocr_model: String,
    pub pdf_ocr_enabled: bool,
    pub pdf_ocr_reflow_enabled: bool,
    pub pdf_ocr_provider: PdfOcrProviderKind,
    pub paddle_ocr_model: String,
    #[serde(skip)]
    pub paddle_ocr_token: String,
    pub mineru_model: String,
    #[serde(skip)]
    pub mineru_token: String,
    pub translation_provider: String,
    pub translation_model: String,
    pub target_language: String,
    pub translation_mode: TranslationMode,
    pub translate_toc: bool,
    #[serde(default)]
    chat_tool_defaults_version: u8,
    #[serde(default)]
    ocr_selection_defaults_version: u8,
    #[serde(default, rename = "base_url", skip_serializing)]
    legacy_base_url: Option<String>,
    #[serde(default, rename = "api_key", skip_serializing)]
    legacy_api_key: Option<String>,
}

impl Default for PluginSettings {
    fn default() -> Self {
        Self {
            providers: vec![AiProvider::default()],
            chat_provider: DEFAULT_PROVIDER_ID.into(),
            chat_model: DEFAULT_MODEL.into(),
            chat_max_tool_steps: DEFAULT_CHAT_MAX_TOOL_STEPS,
            chat_history_turns: DEFAULT_CHAT_HISTORY_TURNS,
            ocr_enabled: true,
            ocr_provider: DEFAULT_PROVIDER_ID.into(),
            ocr_model: DEFAULT_MODEL.into(),
            pdf_ocr_enabled: false,
            pdf_ocr_reflow_enabled: false,
            pdf_ocr_provider: PdfOcrProviderKind::PaddleOcr,
            paddle_ocr_model: "PaddleOCR-VL-1.6".into(),
            paddle_ocr_token: String::new(),
            mineru_model: "vlm".into(),
            mineru_token: String::new(),
            translation_provider: DEFAULT_PROVIDER_ID.into(),
            translation_model: DEFAULT_MODEL.into(),
            target_language: TARGET_LANGUAGE_INTERFACE.into(),
            translation_mode: TranslationMode::Bilingual,
            translate_toc: true,
            chat_tool_defaults_version: CHAT_TOOL_DEFAULTS_VERSION,
            ocr_selection_defaults_version: 0,
            legacy_base_url: None,
            legacy_api_key: None,
        }
    }
}

impl PluginSettings {
    pub fn load_default() -> io::Result<Self> {
        let path = settings_path()?;
        let mut settings = match fs::read(&path) {
            Ok(bytes) => deserialize_settings(&bytes)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => return Err(error),
        };
        settings.migrate_legacy();
        settings.normalize();
        settings.load_api_keys()?;
        settings.load_pdf_ocr_tokens()?;
        if let Ok(value) = env::var("REBOOK_AI_BASE_URL")
            && !value.trim().is_empty()
            && let Some(provider) = settings.providers.first_mut()
        {
            provider.kind = AiProviderKind::Custom;
            provider.base_url = value;
        }
        if let Ok(value) = env::var("REBOOK_AI_MODEL")
            && !value.trim().is_empty()
        {
            if let Some(provider) = settings.providers.first_mut()
                && !provider.models.iter().any(|model| model.id == value)
            {
                provider.models.push(AiModelConfig::language(value.clone()));
            }
            if let Some(provider) = settings.providers.first() {
                settings.chat_provider.clone_from(&provider.id);
                settings.ocr_provider.clone_from(&provider.id);
                settings.translation_provider.clone_from(&provider.id);
            }
            settings.chat_model.clone_from(&value);
            settings.ocr_model.clone_from(&value);
            settings.translation_model = value;
        }
        if let Ok(value) = env::var("REBOOK_AI_API_KEY")
            && let Some(provider) = settings.providers.first_mut()
        {
            provider.api_key = value;
        }
        settings.normalize();
        Ok(settings)
    }

    pub fn save_default(&self) -> io::Result<()> {
        let mut settings = self.clone();
        settings.normalize();
        let path = settings_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "插件设置路径没有父目录"))?;
        fs::create_dir_all(parent)?;
        write_json_atomic(&path, &settings)?;
        settings.save_api_keys()?;
        settings.save_pdf_ocr_tokens()
    }

    pub fn normalize(&mut self) {
        self.migrate_legacy();
        if self.chat_tool_defaults_version < CHAT_TOOL_DEFAULTS_VERSION {
            if self.chat_max_tool_steps == 5 {
                self.chat_max_tool_steps = DEFAULT_CHAT_MAX_TOOL_STEPS;
            }
            self.chat_tool_defaults_version = CHAT_TOOL_DEFAULTS_VERSION;
        }
        if self.ocr_selection_defaults_version < OCR_SELECTION_DEFAULTS_VERSION {
            self.ocr_provider.clone_from(&self.chat_provider);
            self.ocr_model.clone_from(&self.chat_model);
            self.ocr_selection_defaults_version = OCR_SELECTION_DEFAULTS_VERSION;
        }
        if self.providers.is_empty() {
            self.providers.push(AiProvider::default());
        }

        let mut ids = std::collections::HashSet::new();
        for (index, provider) in self.providers.iter_mut().enumerate() {
            let fallback_id = format!("provider-{}", index + 1);
            if provider.id.trim().is_empty() || !ids.insert(provider.id.clone()) {
                provider.id = fallback_id;
                while !ids.insert(provider.id.clone()) {
                    provider.id.push('-');
                }
            }
            if provider.name.trim().is_empty() {
                provider.name = format!("Provider {}", index + 1);
            }
            if let Some(base_url) = provider.kind.base_url() {
                provider.base_url = base_url.into();
            }
            provider.models = normalized_models(std::mem::take(&mut provider.models));
        }

        normalize_selection(
            &self.providers,
            &mut self.chat_provider,
            &mut self.chat_model,
        );
        normalize_selection(&self.providers, &mut self.ocr_provider, &mut self.ocr_model);
        normalize_selection(
            &self.providers,
            &mut self.translation_provider,
            &mut self.translation_model,
        );
        self.chat_max_tool_steps = self
            .chat_max_tool_steps
            .clamp(CHAT_TOOL_STEPS_MIN, CHAT_TOOL_STEPS_MAX);
        self.chat_history_turns = self
            .chat_history_turns
            .clamp(CHAT_HISTORY_TURNS_MIN, CHAT_HISTORY_TURNS_MAX);
        self.target_language = normalize_target_language(&self.target_language);
        if self.paddle_ocr_model.trim().is_empty() {
            self.paddle_ocr_model = "PaddleOCR-VL-1.6".into();
        }
        self.mineru_model = match self.mineru_model.trim() {
            "pipeline" => "pipeline".into(),
            _ => "vlm".into(),
        };
    }

    pub fn add_provider(&mut self) {
        let mut suffix = self.providers.len() + 1;
        let id = loop {
            let candidate = format!("provider-{suffix}");
            if self
                .providers
                .iter()
                .all(|provider| provider.id != candidate)
            {
                break candidate;
            }
            suffix += 1;
        };
        self.providers.push(AiProvider {
            id,
            kind: AiProviderKind::Custom,
            name: format!("Custom {suffix}"),
            base_url: String::new(),
            models: vec![AiModelConfig::language(DEFAULT_MODEL)],
            api_key: String::new(),
        });
    }

    pub fn remove_provider(&mut self, index: usize) {
        if self.providers.len() <= 1 || index >= self.providers.len() {
            return;
        }
        self.providers.remove(index);
        normalize_selection(
            &self.providers,
            &mut self.chat_provider,
            &mut self.chat_model,
        );
        normalize_selection(&self.providers, &mut self.ocr_provider, &mut self.ocr_model);
        normalize_selection(
            &self.providers,
            &mut self.translation_provider,
            &mut self.translation_model,
        );
    }

    pub fn remove_model(&mut self, provider_index: usize, model_index: usize) {
        let Some(provider) = self.providers.get_mut(provider_index) else {
            return;
        };
        if provider.models.len() <= 1 || model_index >= provider.models.len() {
            return;
        }
        provider.models.remove(model_index);
        normalize_selection(
            &self.providers,
            &mut self.chat_provider,
            &mut self.chat_model,
        );
        normalize_selection(&self.providers, &mut self.ocr_provider, &mut self.ocr_model);
        normalize_selection(
            &self.providers,
            &mut self.translation_provider,
            &mut self.translation_model,
        );
    }

    pub fn chat_endpoint(&self) -> Result<(&AiProvider, &str), String> {
        self.endpoint(&self.chat_provider, &self.chat_model, "AI Chat")
    }

    pub fn ocr_endpoint(&self) -> Result<(&AiProvider, &str), String> {
        self.endpoint(&self.ocr_provider, &self.ocr_model, "OCR")
    }

    pub fn translation_endpoint(&self) -> Result<(&AiProvider, &str), String> {
        self.endpoint(&self.translation_provider, &self.translation_model, "翻译")
    }

    pub(crate) fn resolved_target_language(&self, interface_language: &str) -> String {
        match self.target_language.as_str() {
            TARGET_LANGUAGE_INTERFACE => interface_language.to_owned(),
            TARGET_LANGUAGE_SIMPLIFIED_CHINESE => "简体中文".into(),
            TARGET_LANGUAGE_ENGLISH => "English".into(),
            custom => custom.to_owned(),
        }
    }

    fn endpoint<'a>(
        &'a self,
        provider_id: &str,
        model: &'a str,
        feature: &str,
    ) -> Result<(&'a AiProvider, &'a str), String> {
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .ok_or_else(|| format!("请先在“设置 → {feature}”中选择 Provider"))?;
        if provider.api_key.trim().is_empty() {
            return Err(format!(
                "请先在“设置 → AI”中填写 {} 的 API Key",
                provider.name
            ));
        }
        let base_url = provider.base_url.trim();
        if base_url.is_empty() {
            return Err(format!("{} 的 API 地址不能为空", provider.name));
        }
        if !base_url.starts_with("https://") && !base_url.starts_with("http://") {
            return Err(format!(
                "{} 的 API 地址必须使用 http:// 或 https://",
                provider.name
            ));
        }
        let model = model.trim();
        if model.is_empty()
            || !provider
                .models
                .iter()
                .any(|candidate| candidate.id == model)
        {
            return Err(format!(
                "请先在“设置 → {feature}”中选择 {} 下的模型",
                provider.name
            ));
        }
        Ok((provider, model))
    }

    fn migrate_legacy(&mut self) {
        let Some(base_url) = self.legacy_base_url.take() else {
            return;
        };
        if self.providers.is_empty() {
            self.providers.push(AiProvider::default());
        }
        if let Some(provider) = self.providers.first_mut() {
            if !base_url.trim().is_empty() {
                provider.base_url = base_url;
            }
            if let Some(api_key) = self.legacy_api_key.take() {
                provider.api_key = api_key;
            }
            for model in [&self.chat_model, &self.ocr_model, &self.translation_model] {
                if !model.trim().is_empty()
                    && !provider
                        .models
                        .iter()
                        .any(|candidate| candidate.id == *model)
                {
                    provider.models.push(AiModelConfig::language(model.clone()));
                }
            }
            self.chat_provider.clone_from(&provider.id);
            self.ocr_provider.clone_from(&provider.id);
            self.translation_provider.clone_from(&provider.id);
        }
    }

    fn load_api_keys(&mut self) -> io::Result<()> {
        for provider in &mut self.providers {
            match ai_credential_entry(&provider.id)?.get_password() {
                Ok(api_key) => provider.api_key = api_key,
                Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        Ok(())
    }

    fn save_api_keys(&self) -> io::Result<()> {
        for provider in &self.providers {
            let entry = ai_credential_entry(&provider.id)?;
            if provider.api_key.trim().is_empty() {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(error) => return Err(io::Error::other(error)),
                }
            } else {
                entry
                    .set_password(provider.api_key.trim())
                    .map_err(io::Error::other)?;
            }
        }
        Ok(())
    }

    fn load_pdf_ocr_tokens(&mut self) -> io::Result<()> {
        for (account, destination) in [
            ("paddle-ocr", &mut self.paddle_ocr_token),
            ("mineru", &mut self.mineru_token),
        ] {
            match Entry::new(PDF_OCR_CREDENTIAL_SERVICE, account)
                .map_err(io::Error::other)?
                .get_password()
            {
                Ok(token) => *destination = token,
                Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        Ok(())
    }

    fn save_pdf_ocr_tokens(&self) -> io::Result<()> {
        for (account, token) in [
            ("paddle-ocr", self.paddle_ocr_token.trim()),
            ("mineru", self.mineru_token.trim()),
        ] {
            let entry =
                Entry::new(PDF_OCR_CREDENTIAL_SERVICE, account).map_err(io::Error::other)?;
            if token.is_empty() {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(error) => return Err(io::Error::other(error)),
                }
            } else {
                entry.set_password(token).map_err(io::Error::other)?;
            }
        }
        Ok(())
    }
}

fn deserialize_settings(bytes: &[u8]) -> io::Result<PluginSettings> {
    let mut value = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let embedding_provider = value
        .get("embedding_provider")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let embedding_model = value
        .get("embedding_model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let Some(providers) = value
        .get_mut("providers")
        .and_then(serde_json::Value::as_array_mut)
    {
        for provider in providers {
            let provider_id = provider
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let Some(models) = provider
                .get_mut("models")
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            if models.iter().all(serde_json::Value::is_string) {
                *models = models
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|id| !(provider_id == embedding_provider && *id == embedding_model))
                    .map(|id| serde_json::json!({ "id": id }))
                    .collect();
            } else {
                models.retain(|model| {
                    model.get("kind").and_then(serde_json::Value::as_str) != Some("embedding")
                });
            }
        }
    }
    serde_json::from_value(value).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn ai_credential_entry(provider_id: &str) -> io::Result<Entry> {
    Entry::new(AI_CREDENTIAL_SERVICE, provider_id).map_err(io::Error::other)
}

fn normalized_models(models: Vec<AiModelConfig>) -> Vec<AiModelConfig> {
    let mut seen = std::collections::HashSet::new();
    let mut models = models
        .into_iter()
        .map(|mut model| {
            model.id = model.id.trim().to_owned();
            model
        })
        .filter(|model| !model.id.is_empty() && seen.insert(model.id.clone()))
        .collect::<Vec<_>>();
    if models.is_empty() {
        models.push(AiModelConfig::language(DEFAULT_MODEL));
    }
    models
}

fn normalize_target_language(value: &str) -> String {
    match value.trim() {
        "" | TARGET_LANGUAGE_INTERFACE => TARGET_LANGUAGE_INTERFACE.into(),
        "简体中文" | TARGET_LANGUAGE_SIMPLIFIED_CHINESE => {
            TARGET_LANGUAGE_SIMPLIFIED_CHINESE.into()
        }
        "English" | TARGET_LANGUAGE_ENGLISH => TARGET_LANGUAGE_ENGLISH.into(),
        custom => custom.to_owned(),
    }
}

fn normalize_selection(providers: &[AiProvider], provider_id: &mut String, model: &mut String) {
    if let Some(provider) = providers
        .iter()
        .find(|provider| provider.id == *provider_id)
        && provider
            .models
            .iter()
            .any(|candidate| candidate.id == *model)
    {
        return;
    }
    if let Some((provider, selected)) = providers
        .iter()
        .find_map(|provider| provider.models.first().map(|model| (provider, model)))
    {
        provider_id.clone_from(&provider.id);
        model.clone_from(&selected.id);
    } else {
        provider_id.clone_from(&providers[0].id);
        model.clear();
    }
}

fn settings_path() -> io::Result<PathBuf> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定插件配置目录"))?;
    Ok(project.config_dir().join(SETTINGS_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_plugin_settings_never_contain_the_api_key() {
        let mut settings = PluginSettings::default();
        settings.providers[0].api_key = "top-secret".into();
        let json = serde_json::to_string(&settings).unwrap();

        assert!(!json.contains("top-secret"));
        assert!(!json.contains("api_key"));
        assert!(!json.contains("paddle_ocr_url"));
        assert!(!json.contains("mineru_url"));
    }

    #[test]
    fn ai_provider_defaults_to_custom_and_presets_supply_the_endpoint() {
        let mut provider = AiProvider::default();
        assert_eq!(provider.kind, AiProviderKind::Custom);
        assert!(provider.base_url.is_empty());

        provider.select_kind(AiProviderKind::DeepSeek);

        assert_eq!(provider.kind, AiProviderKind::DeepSeek);
        assert_eq!(provider.name, "DeepSeek");
        assert_eq!(provider.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn legacy_typed_embedding_settings_are_ignored_and_embedding_models_are_removed() {
        let settings = deserialize_settings(
            br#"{
                "providers": [{
                    "id": "provider",
                    "name": "Provider",
                    "base_url": "https://example.test/v1",
                    "models": [
                        { "id": "chat", "kind": "language" },
                        { "id": "embed", "kind": "embedding" }
                    ]
                }],
                "chat_provider": "provider",
                "chat_model": "chat",
                "semantic_search_enabled": true,
                "embedding_provider": "provider",
                "embedding_model": "embed"
            }"#,
        )
        .unwrap();

        assert_eq!(
            settings.providers[0].models,
            vec![AiModelConfig::language("chat")]
        );
        let serialized = serde_json::to_string(&settings).unwrap();
        assert!(!serialized.contains("semantic_search"));
        assert!(!serialized.contains("embedding"));
    }

    #[test]
    fn legacy_string_model_names_drop_the_selected_embedding_model() {
        let settings = deserialize_settings(
            br#"{
                "providers": [{
                    "id": "provider",
                    "models": ["chat", "embed"]
                }],
                "chat_provider": "provider",
                "chat_model": "chat",
                "embedding_provider": "provider",
                "embedding_model": "embed"
            }"#,
        )
        .unwrap();

        assert_eq!(
            settings.providers[0].models,
            vec![AiModelConfig::language("chat")]
        );
    }

    #[test]
    fn ai_chat_limits_are_normalized_to_supported_ranges() {
        let mut settings = PluginSettings {
            chat_max_tool_steps: 0,
            chat_history_turns: u16::MAX,
            ..PluginSettings::default()
        };

        settings.normalize();

        assert_eq!(settings.chat_max_tool_steps, CHAT_TOOL_STEPS_MIN);
        assert_eq!(settings.chat_history_turns, CHAT_HISTORY_TURNS_MAX);
    }

    #[test]
    fn legacy_default_tool_limit_migrates_to_web_parity() {
        let mut settings: PluginSettings =
            serde_json::from_str(r#"{ "chat_max_tool_steps": 5 }"#).unwrap();

        settings.normalize();

        assert_eq!(settings.chat_max_tool_steps, 24);
    }

    #[test]
    fn translation_mode_round_trips_through_settings_json() {
        let settings = PluginSettings {
            translation_mode: TranslationMode::Replace,
            ..PluginSettings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: PluginSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.translation_mode, TranslationMode::Replace);
    }

    #[test]
    fn translation_target_defaults_to_interface_language_and_migrates_labels() {
        let settings = PluginSettings::default();
        assert_eq!(settings.target_language, TARGET_LANGUAGE_INTERFACE);
        assert_eq!(settings.resolved_target_language("English"), "English");

        let mut legacy = PluginSettings {
            target_language: "简体中文".into(),
            ..PluginSettings::default()
        };
        legacy.normalize();
        assert_eq!(legacy.target_language, TARGET_LANGUAGE_SIMPLIFIED_CHINESE);
        assert_eq!(legacy.resolved_target_language("English"), "简体中文");
    }

    #[test]
    fn removing_a_selected_provider_repairs_all_feature_selections() {
        let mut settings = PluginSettings::default();
        settings.add_provider();
        let second = settings.providers[1].id.clone();
        settings.chat_provider.clone_from(&second);
        settings.ocr_provider.clone_from(&second);
        settings.translation_provider = second;

        settings.remove_provider(1);

        assert_eq!(settings.chat_provider, DEFAULT_PROVIDER_ID);
        assert_eq!(settings.ocr_provider, DEFAULT_PROVIDER_ID);
        assert_eq!(settings.translation_provider, DEFAULT_PROVIDER_ID);
        assert_eq!(settings.chat_model, DEFAULT_MODEL);
        assert_eq!(settings.ocr_model, DEFAULT_MODEL);
        assert_eq!(settings.translation_model, DEFAULT_MODEL);
    }

    #[test]
    fn ocr_selection_round_trips_and_only_accepts_configured_models() {
        let mut settings = PluginSettings {
            ocr_enabled: false,
            pdf_ocr_reflow_enabled: true,
            ..PluginSettings::default()
        };
        settings.providers[0]
            .models
            .push(AiModelConfig::language("qwen/base"));
        settings.providers[0].api_key = "secret-key".into();
        settings.providers[0].base_url = "https://example.com/v1".into();
        settings.ocr_model = "qwen/base".into();
        settings.ocr_selection_defaults_version = OCR_SELECTION_DEFAULTS_VERSION;
        settings.normalize();

        let json = serde_json::to_string(&settings).unwrap();
        let restored: PluginSettings = serde_json::from_str(&json).unwrap();
        assert!(!restored.ocr_enabled);
        assert!(restored.pdf_ocr_reflow_enabled);
        assert_eq!(restored.ocr_provider, DEFAULT_PROVIDER_ID);
        assert_eq!(restored.ocr_model, "qwen/base");

        let (provider, model) = settings.ocr_endpoint().unwrap();
        assert_eq!(provider.id, DEFAULT_PROVIDER_ID);
        assert_eq!(model, "qwen/base");

        settings.ocr_model = "not-configured".into();
        settings.normalize();
        assert_eq!(settings.ocr_model, DEFAULT_MODEL);
    }

    #[test]
    fn configured_api_key_survives_normalization_and_enables_translation() {
        let mut settings = PluginSettings::default();
        settings.providers[0].select_kind(AiProviderKind::OpenAi);
        settings.providers[0].api_key = "  secret-key  ".into();

        settings.normalize();

        let (provider, model) = settings.translation_endpoint().unwrap();
        assert_eq!(provider.api_key, "  secret-key  ");
        assert_eq!(model, DEFAULT_MODEL);
    }
}
