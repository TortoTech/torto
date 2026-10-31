//! Native settings boundary; secrets never enter a returned DTO.
use crate::*;
use rebook_app_core::plugins::{
    self, AiModelConfig, AiProvider, AiProviderKind, PluginSettings, ReasoningEffort,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct ReadingOptions {
    pub cjk_font: String,
    pub cjk_latin_font: String,
    pub latin_font: String,
    pub latin_cjk_font: String,
    pub monospace_font: String,
    pub font_weight: u16,
    pub focus_style: String,
    pub sentence_split: bool,
    pub scroll_focus: bool,
    pub translated: bool,
    pub ai_prefetch: bool,
}
impl Default for ReadingOptions {
    fn default() -> Self {
        Self {
            cjk_font: "LXGW WenKai GB Screen".into(),
            cjk_latin_font: "Literata".into(),
            latin_font: "Literata".into(),
            latin_cjk_font: String::new(),
            monospace_font: "monospace".into(),
            font_weight: 400,
            focus_style: "dim".into(),
            sentence_split: false,
            scroll_focus: true,
            translated: false,
            ai_prefetch: true,
        }
    }
}
pub(crate) fn load_reading(root: &std::path::Path) -> ReadingOptions {
    std::fs::read(root.join("android-reading.json"))
        .ok()
        .filter(|v| v.len() < 16384)
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}
pub(crate) fn apply_typography(style: &mut ReaderStyle, options: &ReadingOptions) {
    use rebook_layout::{ReaderDefaultFont, ReaderFontChoice};
    let t = &mut style.typography;
    t.default_cjk_font = options.cjk_font.clone();
    t.default_font = ReaderDefaultFont::Other;
    t.other_font = options.latin_font.clone();
    t.cjk_default_font = (!options.cjk_latin_font.is_empty()).then(|| ReaderFontChoice {
        category: ReaderDefaultFont::Other,
        family: options.cjk_latin_font.clone(),
    });
    t.latin_cjk_font = (!options.latin_cjk_font.is_empty()).then(|| options.latin_cjk_font.clone());
    t.monospace_font = options.monospace_font.clone();
    t.font_weight = options.font_weight;
    t.normalize();
}
#[derive(Clone, uniffi::Record)]
pub struct FontFamilies {
    pub cjk: Vec<String>,
    pub latin: Vec<String>,
    pub monospace: Vec<String>,
}
#[derive(Clone, uniffi::Record)]
pub struct ProviderKind {
    pub id: String,
    pub name: String,
    pub base_url: String,
}
#[derive(Clone, uniffi::Record)]
pub struct Provider {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub key_saved: bool,
}
#[derive(Clone, uniffi::Record)]
pub struct AiOptions {
    pub layout_enabled: bool,
    pub layout_provider: String,
    pub layout_model: String,
    pub layout_reasoning: String,
    pub translation_provider: String,
    pub translation_model: String,
    pub translation_reasoning: String,
    pub target_language: String,
    pub show_original: bool,
    pub translate_toc: bool,
    pub expert_translation: bool,
    pub chat_provider: String,
    pub chat_model: String,
    pub chat_reasoning: String,
    pub chat_steps: u16,
    pub chat_history: u16,
}
fn wire<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn effort(v: &str) -> Result<ReasoningEffort> {
    serde_json::from_value(serde_json::Value::String(v.into())).map_err(error)
}
fn kind(v: &str) -> Result<AiProviderKind> {
    serde_json::from_value(serde_json::Value::String(v.into())).map_err(error)
}
fn dto(s: &PluginSettings) -> AiOptions {
    AiOptions {
        layout_enabled: s.semantic_layout.enabled,
        layout_provider: s.semantic_layout.provider.clone(),
        layout_model: s.semantic_layout.model.clone(),
        layout_reasoning: wire(&s.semantic_layout.reasoning_effort),
        translation_provider: s.translation_provider.clone(),
        translation_model: s.translation_model.clone(),
        translation_reasoning: wire(&s.translation_reasoning_effort),
        target_language: s.target_language.clone(),
        show_original: s.translation_mode == plugins::TranslationMode::Bilingual,
        translate_toc: s.translate_toc,
        expert_translation: s.expert_translation,
        chat_provider: s.chat_provider.clone(),
        chat_model: s.chat_model.clone(),
        chat_reasoning: wire(&s.chat_reasoning_effort),
        chat_steps: s.chat_max_tool_steps,
        chat_history: s.chat_history_turns,
    }
}
fn validate_url(url: &str) -> Result<()> {
    rebook_app_core::provider_models::validate_provider_url(url).map_err(error)
}
#[uniffi::export]
impl MobileCore {
    pub async fn reading_options(self: Arc<Self>) -> Result<ReadingOptions> {
        work(move || Ok(load_reading(&self.root))).await
    }
    pub async fn save_reading_options(self: Arc<Self>, options: ReadingOptions) -> Result<()> {
        work(move || {
            if !(200..=900).contains(&options.font_weight)
                || !matches!(options.focus_style.as_str(), "dim" | "highlight")
                || [
                    &options.cjk_font,
                    &options.cjk_latin_font,
                    &options.latin_font,
                    &options.latin_cjk_font,
                    &options.monospace_font,
                ]
                .iter()
                .any(|v| v.len() > 256)
            {
                return Err(error("字体或专注设置无效"));
            }
            rebook_app_core::persistence::write_json_atomic(
                &self.root.join("android-reading.json"),
                &options,
            )
            .map_err(error)
        })
        .await
    }
    pub async fn font_families(self: Arc<Self>) -> Result<FontFamilies> {
        work(move || {
            let fonts =
                rebook_app_core::fonts::store::load(&self.root.join("fonts"), Default::default());
            let mut engine =
                rebook_layout::LayoutEngine::with_fonts(fonts.reader_fonts().iter().cloned());
            let mut found = engine.available_reader_font_families();
            found.include_downloadable_defaults();
            found.monospace.push("monospace".into());
            found.chinese.extend(["sans-serif".into(), "serif".into()]);
            let mut latin = found.serif;
            latin.extend(found.sans_serif);
            latin.extend(found.other);
            latin.extend(["serif".into(), "sans-serif".into()]);
            latin.sort();
            latin.dedup();
            Ok(FontFamilies {
                cjk: found.chinese,
                latin,
                monospace: found.monospace,
            })
        })
        .await
    }
    pub async fn provider_kinds(self: Arc<Self>) -> Result<Vec<ProviderKind>> {
        work(move || {
            Ok(AiProviderKind::ALL
                .iter()
                .map(|k| {
                    let mut p = AiProvider::default();
                    p.select_kind(*k);
                    ProviderKind {
                        id: wire(k),
                        name: k.label().into(),
                        base_url: p.base_url,
                    }
                })
                .collect())
        })
        .await
    }
    pub async fn providers(self: Arc<Self>) -> Result<Vec<Provider>> {
        work(move || {
            let s = PluginSettings::load_default().map_err(error)?;
            Ok(s.providers
                .iter()
                .map(|p| Provider {
                    id: p.id.clone(),
                    kind: wire(&p.kind),
                    name: p.name.clone(),
                    base_url: p.base_url.clone(),
                    models: p.models.iter().map(|m| m.id.clone()).collect(),
                    key_saved: !p.api_key.is_empty(),
                })
                .collect())
        })
        .await
    }
    pub async fn save_provider(
        self: Arc<Self>,
        provider: Provider,
        key: Option<String>,
        clear_key: bool,
    ) -> Result<()> {
        work(move || {
            if provider.name.trim().is_empty()
                || provider.name.len() > 160
                || provider.models.len() > 256
                || provider.models.iter().any(|m| m.len() > 256)
            {
                return Err(error("提供商名称或模型列表无效"));
            }
            validate_url(&provider.base_url)?;
            let mut s = PluginSettings::load_default().map_err(error)?;
            let id = if provider.id.is_empty() {
                s.add_provider();
                s.providers.last().unwrap().id.clone()
            } else {
                provider.id.clone()
            };
            let p = s
                .providers
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| error("提供商不存在"))?;
            p.kind = kind(&provider.kind)?;
            p.name = provider.name.trim().into();
            p.base_url = provider.base_url.trim().trim_end_matches('/').into();
            p.models = provider
                .models
                .iter()
                .map(|m| m.trim())
                .filter(|m| !m.is_empty())
                .map(AiModelConfig::language)
                .collect();
            if clear_key {
                p.api_key.clear();
            } else if let Some(key) = key {
                p.api_key = key;
            }
            s.save_default().map_err(error)
        })
        .await
    }
    pub async fn delete_provider(self: Arc<Self>, id: String) -> Result<()> {
        work(move || {
            let mut s = PluginSettings::load_default().map_err(error)?;
            if s.providers.len() <= 1 {
                return Err(error("请至少保留一个提供商"));
            }
            let index = s
                .providers
                .iter()
                .position(|p| p.id == id)
                .ok_or_else(|| error("提供商不存在"))?;
            s.remove_provider(index);
            if s.semantic_layout.provider == id {
                s.semantic_layout.enabled = false;
                s.semantic_layout.provider.clear();
                s.semantic_layout.model.clear();
            }
            s.save_default().map_err(error)?;
            PluginSettings::delete_provider_key(&id).map_err(error)
        })
        .await
    }
    pub async fn fetch_models(
        self: Arc<Self>,
        provider: Provider,
        key: Option<String>,
    ) -> Result<Vec<String>> {
        work(move || {
            validate_url(&provider.base_url)?;
            let s = PluginSettings::load_default().map_err(error)?;
            let key = key
                .or_else(|| {
                    s.providers
                        .iter()
                        .find(|p| p.id == provider.id)
                        .map(|p| p.api_key.clone())
                })
                .unwrap_or_default();
            runtime()
                .block_on(rebook_app_core::provider_models::fetch_provider_models(
                    &rebook_app_core::provider_models::ProviderModelsRequest {
                        provider_id: provider.id,
                        kind: kind(&provider.kind)?,
                        base_url: provider.base_url,
                        api_key: key,
                    },
                ))
                .map_err(error)
        })
        .await
    }
    pub async fn ai_options(self: Arc<Self>) -> Result<AiOptions> {
        work(move || Ok(dto(&PluginSettings::load_default().map_err(error)?))).await
    }
    pub async fn save_ai_options(self: Arc<Self>, options: AiOptions) -> Result<AiOptions> {
        work(move || {
            let mut s = PluginSettings::load_default().map_err(error)?;
            for (provider, model, needed) in [
                (
                    &options.layout_provider,
                    &options.layout_model,
                    options.layout_enabled,
                ),
                (
                    &options.translation_provider,
                    &options.translation_model,
                    !options.translation_model.is_empty(),
                ),
                (
                    &options.chat_provider,
                    &options.chat_model,
                    !options.chat_model.is_empty(),
                ),
            ] {
                if needed
                    && !s
                        .providers
                        .iter()
                        .any(|p| p.id == *provider && p.models.iter().any(|m| m.id == *model))
                {
                    return Err(error("请从提供商中选择有效的模型"));
                }
            }
            s.semantic_layout.enabled = options.layout_enabled;
            s.semantic_layout.provider = options.layout_provider;
            s.semantic_layout.model = options.layout_model;
            s.semantic_layout.reasoning_effort = effort(&options.layout_reasoning)?;
            s.translation_provider = options.translation_provider;
            s.translation_model = options.translation_model;
            s.translation_reasoning_effort = effort(&options.translation_reasoning)?;
            s.target_language = options.target_language;
            s.translation_mode = if options.show_original {
                plugins::TranslationMode::Bilingual
            } else {
                plugins::TranslationMode::Replace
            };
            s.translate_toc = options.translate_toc;
            s.expert_translation = options.expert_translation;
            s.chat_provider = options.chat_provider;
            s.chat_model = options.chat_model;
            s.chat_reasoning_effort = effort(&options.chat_reasoning)?;
            s.chat_max_tool_steps = options.chat_steps.clamp(1, 64);
            s.chat_history_turns = options.chat_history.clamp(1, 50);
            s.save_default().map_err(error)?;
            Ok(dto(&s))
        })
        .await
    }
}
#[uniffi::export]
impl MobileReader {
    pub async fn apply_reading_options(
        self: Arc<Self>,
        options: ReadingOptions,
    ) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                let locator = s.session.current_locator();
                let mut style = s.session.style();
                apply_typography(&mut style, &options);
                s.pipeline
                    .structure
                    .set_all_active(options.sentence_split && s.focus)
                    .map_err(error)?;
                s.options = options;
                style.spread = if s.focus && s.options.scroll_focus {
                    SpreadMode::Scroll
                } else {
                    SpreadMode::Single
                };
                let fonts =
                    rebook_app_core::fonts::store::load(&s.root.join("fonts"), Default::default());
                rebook_app_core::fonts::install(Arc::new(fonts.clone()));
                s.session = ReaderSession::open_with_fonts_at_locator(
                    s.source.clone(),
                    s.session.viewport(),
                    style,
                    fonts.reader_fonts(),
                    &locator,
                )
                .map_err(error)?;
                s.session
                    .set_cache_policy(rebook_reader::ReaderCachePolicy {
                        layout_bytes: 24 * 1024 * 1024,
                        raster_bytes: 48 * 1024 * 1024,
                        prefetch_distance: 1,
                    });
                s.scroll_offset = 0.0;
                s.scroll_center = true;
                s.scroll_slices.clear();
                render(s)
            })
        })
        .await
    }
    pub async fn reload_ai_options(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                let settings = PluginSettings::load_default().map_err(error)?;
                let config = rebook_app_core::translation_cache::configuration_key(&settings)
                    .map_err(error)?;
                if s.translation_config.as_ref() != Some(&config) {
                    s.pipeline.translation.clear().map_err(error)?;
                    s.translation_config = Some(config);
                }
                s.toc_labels.clear();
                s.toc_config = None;
                s.pipeline
                    .translation
                    .set_mode(settings.translation_mode)
                    .map_err(error)?;
                s.pipeline.semantic.configure(
                    &plugins::pdf_native::cache_identity(&s.book_id, s.pipeline.rewrite.as_ref()),
                    &settings,
                );
                s.session.refresh_source().map_err(error)?;
                s.scroll_center = true;
                s.scroll_slices.clear();
                render(s)
            })
        })
        .await
    }
}

#[uniffi::export]
pub fn ai_reasoning_levels(provider_kind: String, model: String) -> Result<Vec<String>> {
    Ok(
        plugins::llm::reasoning_levels(kind(&provider_kind)?, &model)
            .iter()
            .map(wire)
            .collect(),
    )
}
#[derive(uniffi::Record)]
pub struct FontFile {
    pub path: String,
    pub index: u32,
}
#[uniffi::export]
impl MobileCore {
    pub async fn monospace_file(self: Arc<Self>, family: String) -> Result<Option<FontFile>> {
        work(move || {
            Ok(
                rebook_app_core::fonts::system_font_file(&family).map(|(path, index)| FontFile {
                    path: path.to_string_lossy().into_owned(),
                    index,
                }),
            )
        })
        .await
    }
}
