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
    /// Legacy on-disk preference; paragraph structure is now controlled per focused unit.
    pub sentence_split: bool,
    pub translated: bool,
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
            sentence_split: false,
            translated: false,
        }
    }
}
pub(crate) fn load_reading(root: &std::path::Path) -> ReadingOptions {
    let mut options: ReadingOptions = std::fs::read(root.join("android-reading.json"))
        .ok()
        .filter(|v| v.len() < 16384)
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    // Translation belongs to the current reader session, as on desktop.
    options.translated = false;
    options.font_weight = 400;
    options
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
    t.font_weight = 400;
    t.normalize();
}
#[derive(Clone, uniffi::Record)]
pub struct FontFamilies {
    pub cjk: Vec<String>,
    pub latin: Vec<String>,
    pub monospace: Vec<String>,
}
#[derive(Clone, uniffi::Record)]
pub struct FontOption {
    pub family: String,
    pub label: String,
    pub downloaded: bool,
}
#[derive(Clone, uniffi::Record)]
pub struct FontGroup {
    pub id: String,
    pub title_zh: String,
    pub title_en: String,
    pub items: Vec<FontOption>,
}
#[derive(Clone, uniffi::Record)]
pub struct ReaderFontCatalog {
    pub cjk_groups: Vec<FontGroup>,
    pub latin_groups: Vec<FontGroup>,
    pub monospace: Vec<FontOption>,
}
#[derive(Clone, uniffi::Record)]
pub struct ImportedFont {
    pub family: String,
    pub label: String,
    pub is_cjk: bool,
}

struct CustomMobileFont {
    family: String,
    display_name: String,
    is_cjk: bool,
    data: peniko::Blob<u8>,
}

fn load_custom_fonts(root: &std::path::Path) -> Vec<CustomMobileFont> {
    let dir = root.join("fonts").join("custom");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut list: Vec<CustomMobileFont> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if let Some((family, display_name, is_cjk)) = rebook_layout::inspect_font_bytes(&bytes) {
            let data = peniko::Blob::new(Arc::new(bytes));
            rebook_layout::register_font_blob_family_override(data.id(), family.clone());
            if !list
                .iter()
                .any(|item| item.family.eq_ignore_ascii_case(&family))
            {
                list.push(CustomMobileFont {
                    family,
                    display_name,
                    is_cjk,
                    data,
                });
            }
        }
    }
    list.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
    });
    list
}

pub(crate) fn load_all_reader_fonts(root: &std::path::Path) -> Arc<[peniko::Blob<u8>]> {
    let fonts = rebook_app_core::fonts::store::load(&root.join("fonts"), Default::default());
    rebook_app_core::fonts::install(Arc::new(fonts.clone()));
    let custom = load_custom_fonts(root);
    let mut blobs: Vec<peniko::Blob<u8>> = fonts.reader_fonts().iter().cloned().collect();
    for font in custom {
        rebook_layout::register_font_blob_family_override(font.data.id(), font.family.clone());
        if !blobs.iter().any(|blob| blob.id() == font.data.id()) {
            blobs.push(font.data);
        }
    }
    blobs.into()
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
    // Retained in the wire record for binding compatibility; fixed defaults,
    // ignored on save. These are no longer user settings.
    pub chat_steps: u16,
    pub chat_history: u16,
    pub image_provider: String,
    pub image_model: String,
    pub image_reasoning: String,
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
        image_provider: s.semantic_layout.image_provider.clone(),
        image_model: s.semantic_layout.image_model.clone(),
        image_reasoning: wire(&s.semantic_layout.image_reasoning_effort),
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
        chat_steps: plugins::DEFAULT_CHAT_MAX_TOOL_STEPS,
        chat_history: plugins::DEFAULT_CHAT_HISTORY_TURNS,
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
    pub async fn probe_layout_model(
        self: Arc<Self>,
        provider: String,
        model: String,
    ) -> Result<String> {
        work(move || {
            let settings = PluginSettings::load_default().map_err(error)?;
            let provider = settings
                .providers
                .iter()
                .find(|entry| {
                    entry.id == provider && entry.models.iter().any(|entry| entry.id == model)
                })
                .ok_or_else(|| error("请从提供商中选择有效的模型"))?;
            let protocol = runtime().block_on(plugins::semantic_layout::probe_layout_model(
                provider, &model,
            ));
            Ok(wire(&protocol))
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
                    &options.image_provider,
                    &options.image_model,
                    !options.image_model.is_empty(),
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
            s.semantic_layout.image_provider = options.image_provider;
            s.semantic_layout.image_model = options.image_model;
            s.semantic_layout.image_reasoning_effort = effort(&options.image_reasoning)?;
            s.fill_layout_image_model();
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
            s.save_default().map_err(error)?;
            Ok(dto(&s))
        })
        .await
    }
}
#[uniffi::export]
impl MobileReader {
    /// Translation is a live reader choice, independent of saved typography.
    pub async fn set_translation_enabled(self: Arc<Self>, enabled: bool) -> Result<()> {
        work(move || {
            let changed = self.with(|s| {
                if s.options.translated == enabled {
                    return Ok(false);
                }
                s.pipeline.translation.set_enabled(enabled).map_err(error)?;
                s.options.translated = enabled;
                s.enhancements.dirty = true;
                Ok(true)
            })?;
            if changed {
                self.ai.lock().map_err(error)?.mark_view_dirty();
            }
            Ok(())
        })
        .await
    }
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
                    .set_enabled(s.focus).map_err(error)?;
                s.pipeline
                    .translation
                    .set_enabled(options.translated)
                    .map_err(error)?;
                s.options = options;
                style.spread = if s.focus {
                    SpreadMode::Scroll
                } else {
                    SpreadMode::Single
                };
                let reader_fonts = load_all_reader_fonts(&s.root);
                s.session = ReaderSession::open_with_fonts_at_locator(
                    s.source.clone(),
                    s.session.viewport(),
                    style,
                    reader_fonts,
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
                scroll::invalidate(s);
                render(s)
            })
        })
        .await
    }
    pub async fn reload_ai_options(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        self.retry_ai();
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
                s.pipeline.semantic.configure_presentation(
                    &plugins::pdf_native::cache_identity(&s.book_id, s.pipeline.rewrite.as_ref()),
                    &settings,s.session.style().typesetting.mode==TypesettingMode::Unified,
                );
                reflow::refresh(s)?;
                render(s)
            })
        })
        .await
    }
}

#[uniffi::export]
pub fn is_layout_decision_model(base_url: String, model: String) -> bool {
    let provider = AiProvider { base_url, ..AiProvider::default() };
    plugins::semantic_layout::layout_model_protocol(&provider, &model)
        == plugins::semantic_layout::LayoutModelProtocol::Decision
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
    pub async fn reader_font_catalog(self: Arc<Self>) -> Result<ReaderFontCatalog> {
        work(move || {
            let fonts =
                rebook_app_core::fonts::store::load(&self.root.join("fonts"), Default::default());
            let custom = load_custom_fonts(&self.root);
            let is_downloaded = |family: &str| {
                let indices = rebook_app_core::fonts::catalog::indices_for_family(family);
                !indices.is_empty() && indices.iter().all(|&i| fonts.faces[i].is_some())
            };

            let mut cjk_system = vec![
                FontOption {
                    family: "sans-serif".into(),
                    label: "系统字体".into(),
                    downloaded: true,
                },
                FontOption {
                    family: "LXGW WenKai GB Screen".into(),
                    label: rebook_app_core::fonts::catalog::display_name("LXGW WenKai GB Screen")
                        .into(),
                    downloaded: is_downloaded("LXGW WenKai GB Screen"),
                },
            ];
            for f in custom.iter().filter(|f| f.is_cjk) {
                cjk_system.push(FontOption {
                    family: f.family.clone(),
                    label: f.display_name.clone(),
                    downloaded: true,
                });
            }
            let catalog_group = |id: &str, zh: &str, en: &str| FontGroup {
                id: id.into(),
                title_zh: zh.into(),
                title_en: en.into(),
                items: rebook_app_core::fonts::catalog::cjk_families_in_group(id)
                    .into_iter()
                    .map(|(family, label)| FontOption {
                        family: family.into(),
                        label: label.into(),
                        downloaded: is_downloaded(family),
                    })
                    .collect(),
            };
            let cjk_groups = vec![
                FontGroup {
                    id: "system".into(),
                    title_zh: "内置".into(),
                    title_en: "Built-in".into(),
                    items: cjk_system,
                },
                catalog_group("canger", "仓耳", "Tsanger"),
                catalog_group("fangzheng", "方正", "Founder"),
                catalog_group("other", "其他", "Other"),
            ];

            let mut latin_builtin = vec![
                FontOption {
                    family: "serif".into(),
                    label: "系统字体".into(),
                    downloaded: true,
                },
                FontOption {
                    family: "Literata".into(),
                    label: "Literata".into(),
                    downloaded: is_downloaded("Literata"),
                },
            ];
            for f in custom.iter().filter(|f| !f.is_cjk) {
                latin_builtin.push(FontOption {
                    family: f.family.clone(),
                    label: f.display_name.clone(),
                    downloaded: true,
                });
            }
            let latin_groups = vec![
                FontGroup {
                    id: "builtin".into(),
                    title_zh: "内置".into(),
                    title_en: "Built-in".into(),
                    items: latin_builtin,
                },
                catalog_group("latin-extra", "其他", "Other"),
            ];

            let mut engine =
                rebook_layout::LayoutEngine::with_fonts(fonts.reader_fonts().iter().cloned());
            let found = engine.available_reader_font_families();
            let default_mono = rebook_layout::default_monospace_font();
            let mut monospace = vec![FontOption {
                family: default_mono.into(),
                label: default_mono.into(),
                downloaded: true,
            }];
            for family in found.monospace {
                if !family.starts_with('.')
                    && !monospace
                        .iter()
                        .any(|item| item.family.eq_ignore_ascii_case(&family))
                {
                    monospace.push(FontOption {
                        label: family.clone(),
                        family,
                        downloaded: true,
                    });
                }
            }
            for f in custom.iter().filter(|f| !f.is_cjk) {
                if !monospace
                    .iter()
                    .any(|item| item.family.eq_ignore_ascii_case(&f.family))
                {
                    monospace.push(FontOption {
                        family: f.family.clone(),
                        label: f.display_name.clone(),
                        downloaded: true,
                    });
                }
            }

            Ok(ReaderFontCatalog {
                cjk_groups,
                latin_groups,
                monospace,
            })
        })
        .await
    }
    pub async fn download_font_family(self: Arc<Self>, family: String) -> Result<()> {
        let indices = rebook_app_core::fonts::catalog::indices_for_family(&family);
        if indices.is_empty() {
            return Ok(());
        }
        let client = rebook_app_core::fonts::download::client().map_err(error)?;
        let fonts_root = self.root.join("fonts");
        let catalog = rebook_app_core::fonts::catalog::catalog();
        for index in indices {
            let asset = catalog.fonts[index].clone();
            if rebook_app_core::fonts::store::load_cached(&fonts_root, &asset).is_some() {
                continue;
            }
            rebook_app_core::fonts::download::fetch(&client, fonts_root.clone(), asset, |_| {})
                .await
                .map_err(error)?;
        }
        Ok(())
    }
    pub async fn import_custom_font(self: Arc<Self>, path: String) -> Result<ImportedFont> {
        work(move || {
            let src = std::path::PathBuf::from(&path);
            let bytes = std::fs::read(&src).map_err(error)?;
            let Some((family, display_name, is_cjk)) = rebook_layout::inspect_font_bytes(&bytes)
            else {
                return Err(error("不支持或无效的字体文件"));
            };
            let dir = self.root.join("fonts").join("custom");
            std::fs::create_dir_all(&dir).map_err(error)?;
            let file_name = src
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("custom-font.ttf");
            std::fs::write(dir.join(file_name), &bytes).map_err(error)?;
            Ok(ImportedFont {
                family,
                label: display_name,
                is_cjk,
            })
        })
        .await
    }
}

#[cfg(test)]
mod reading_tests {
    use super::*;
    #[test]
    fn legacy_translation_toggle_does_not_leak_into_new_readers() {
        let root =
            std::env::temp_dir().join(format!("torto-reading-options-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let options = ReadingOptions {
            translated: true,
            cjk_font: "Existing font".into(),
            ..Default::default()
        };
        std::fs::write(
            root.join("android-reading.json"),
            serde_json::to_vec(&options).unwrap(),
        )
        .unwrap();
        let loaded = load_reading(&root);
        std::fs::remove_dir_all(root).unwrap();
        assert!(!loaded.translated);
        assert_eq!(loaded.cjk_font, "Existing font");
    }
}
