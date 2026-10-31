//! Coarse commands reuse desktop services; network/filesystem work stays off the UI.
use crate::*;
use rebook_app_core::{
    plugins::{self, PluginSettings},
    sync::{self, SyncSettings, SyncStore},
};

pub(crate) async fn cancellable<T>(
    request: &chat::ChatRequest,
    future: impl std::future::Future<Output = std::result::Result<T, String>>,
) -> std::result::Result<T, String> {
    if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err("已停止处理".into());
    }
    let cancel = std::pin::pin!(request.notify.notified());
    let future = std::pin::pin!(future);
    match futures_util::future::select(cancel, future).await {
        futures_util::future::Either::Left(_) => Err("已停止处理".into()),
        futures_util::future::Either::Right((result, _)) => result,
    }
}

pub(crate) fn store() -> Result<SyncStore> {
    let settings = SyncSettings::load_default().map_err(error)?;
    SyncStore::open_default(settings.device_id).map_err(error)
}
#[derive(Clone, uniffi::Record)]
pub struct ServiceSettings {
    pub ai_url: String,
    pub layout_model: String,
    pub translation_model: String,
    pub chat_model: String,
    pub target_language: String,
    pub ai_key_saved: bool,
    pub sync_url: String,
    pub sync_user: String,
    pub sync_password_saved: bool,
}
#[derive(Clone, uniffi::Record)]
pub struct SyncSummary {
    pub uploaded: u32,
    pub downloaded: u32,
    pub annotations: u32,
    pub progress: u32,
    pub warning: Option<String>,
}
#[uniffi::export]
impl MobileCore {
    pub async fn service_settings(self: Arc<Self>) -> Result<ServiceSettings> {
        work(move || {
            let settings = PluginSettings::load_default().map_err(error)?;
            let provider = settings.providers.iter().find(|p| p.id == "android");
            let sync = SyncSettings::load_default().map_err(error)?;
            Ok(ServiceSettings {
                ai_url: provider.map(|p| p.base_url.clone()).unwrap_or_default(),
                ai_key_saved: provider.is_some_and(|p| !p.api_key.is_empty()),
                layout_model: settings.semantic_layout.model,
                translation_model: settings.translation_model,
                chat_model: settings.chat_model,
                target_language: settings.target_language,
                sync_password_saved: !sync.load_password().map_err(error)?.is_empty(),
                sync_url: sync.base_url,
                sync_user: sync.username,
            })
        })
        .await
    }
    pub async fn save_ai_settings(
        self: Arc<Self>,
        url: String,
        key: Option<String>,
        layout_model: String,
        translation_model: String,
        chat_model: String,
        target_language: String,
    ) -> Result<()> {
        work(move || {
            if !url.starts_with("https://") {
                return Err(error("AI 服务地址必须使用 HTTPS"));
            }
            let mut settings = PluginSettings::load_default().map_err(error)?;
            let previous = settings
                .providers
                .iter()
                .find(|p| p.id == "android")
                .cloned()
                .unwrap_or_default();
            let mut provider = plugins::AiProvider {
                id: "android".into(),
                kind: plugins::AiProviderKind::Custom,
                name: "Android".into(),
                base_url: url,
                models: [&layout_model, &translation_model, &chat_model]
                    .into_iter()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| plugins::AiModelConfig::language(s.trim()))
                    .collect(),
                api_key: key.unwrap_or(previous.api_key),
                ..Default::default()
            };
            provider.models.dedup();
            settings.providers.retain(|p| p.id != "android");
            settings.providers.push(provider);
            settings.semantic_layout.provider = "android".into();
            settings.semantic_layout.model = layout_model;
            settings.semantic_layout.enabled = !settings.semantic_layout.model.is_empty();
            settings.translation_provider = "android".into();
            settings.translation_model = translation_model;
            settings.chat_provider = "android".into();
            settings.chat_model = chat_model;
            settings.translation_reasoning_effort = plugins::ReasoningEffort::Low;
            settings.target_language = target_language;
            settings.save_default().map_err(error)
        })
        .await
    }
    pub async fn save_sync_settings(
        self: Arc<Self>,
        url: String,
        username: String,
        password: Option<String>,
    ) -> Result<()> {
        work(move || {
            if !url.starts_with("https://") {
                return Err(error("WebDAV 服务地址必须使用 HTTPS"));
            }
            let mut settings = SyncSettings::load_default().map_err(error)?;
            settings.provider = sync::CloudProviderKind::Custom;
            settings.base_url = url;
            settings.username = username;
            settings.enabled = true;
            settings.device_name = "Torto Android".into();
            settings.normalize();
            if let Some(password) = password {
                settings.save_password(&password).map_err(error)?;
            }
            settings.save_default().map_err(error)
        })
        .await
    }
    pub async fn sync_library(self: Arc<Self>) -> Result<SyncSummary> {
        let o = self.clone().cloud_options().await?;
        self.sync_cancellable(o.account, false, chat::ChatRequest::new())
            .await
    }
    pub async fn download_reading_fonts(self: Arc<Self>) -> Result<()> {
        work(move || {
            let client = rebook_app_core::fonts::download::client().map_err(error)?;
            let root = self.root.join("fonts");
            for asset in &rebook_app_core::fonts::catalog::catalog().fonts {
                if rebook_app_core::fonts::store::load_cached(&root, asset).is_some() {
                    continue;
                }
                runtime()
                    .block_on(rebook_app_core::fonts::download::fetch(
                        &client,
                        root.clone(),
                        asset.clone(),
                        |_| {},
                    ))
                    .map_err(error)?;
            }
            Ok(())
        })
        .await
    }
}
#[uniffi::export]
impl MobileReader {
    /// Only the visible source range is translated; navigation and source keys
    /// remain identical to desktop. The UI serializes commands for this session.
    pub async fn translate_page(self: Arc<Self>, enabled: bool) -> Result<Arc<MobilePage>> {
        self.translate_page_cancellable(enabled, chat::ChatRequest::new())
            .await
    }
    pub async fn translate_page_cancellable(
        self: Arc<Self>,
        enabled: bool,
        request: Arc<chat::ChatRequest>,
    ) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                translate_work(s, enabled, &request)?;
                render(s)
            })
        })
        .await
    }
    pub async fn recognize_page(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        self.recognize_page_cancellable(chat::ChatRequest::new())
            .await
    }
    pub async fn recognize_page_cancellable(
        self: Arc<Self>,
        request: Arc<chat::ChatRequest>,
    ) -> Result<Arc<MobilePage>> {
        work(move || {
            self.with(|s| {
                recognize_work(s, &request)?;
                render(s)
            })
        })
        .await
    }
    pub async fn trim_memory(self: Arc<Self>) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.session
                    .set_cache_policy(rebook_reader::ReaderCachePolicy {
                        layout_bytes: 4 * 1024 * 1024,
                        raster_bytes: 8 * 1024 * 1024,
                        prefetch_distance: 0,
                    });
                Ok(())
            })
        })
        .await
    }
}

pub(crate) fn translate_work(
    s: &mut ReaderState,
    enabled: bool,
    request: &chat::ChatRequest,
) -> Result<()> {
    if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(error("已停止处理"));
    }
    let mut settings = PluginSettings::load_default().map_err(error)?;
    s.pipeline
        .translation
        .set_mode(settings.translation_mode)
        .map_err(error)?;
    s.options.translated = enabled;
    settings.target_language = settings.resolved_target_language(
        rebook_app_core::preferences::AppLanguage::system_translation_target(),
    );
    let config = rebook_app_core::translation_cache::configuration_key(&settings).map_err(error)?;
    if s.translation_config.as_ref() != Some(&config) {
        s.pipeline.translation.clear().map_err(error)?;
        s.translation_config = Some(config);
    }
    s.pipeline
        .translation
        .set_target_language(&settings.target_language)
        .map_err(error)?;
    if enabled {
        let index = s.session.snapshot().location.section_index;
        let original = s.pipeline.rewrite.parse_section(index).map_err(error)?;
        let mut section = original.clone();
        s.pipeline.semantic.prepare_citation_input(
            index,
            &plugins::semantic_layout::fingerprint(&original),
            &mut section,
        );
        for (block, (raw, prepared)) in original.blocks.iter().zip(&section.blocks).enumerate() {
            s.pipeline
                .translation
                .remember_translation_input(index, block, raw, prepared);
        }
        let prepared = plugins::prepare_translation_inputs(
            &section,
            s.source.book().metadata.layout == rebook_publication::RenditionLayout::PrePaginated,
        );
        let mut ranges = s
            .session
            .current_visible_content_sources()
            .map_err(error)?
            .into_iter()
            .flat_map(|(_, ranges)| ranges)
            .collect::<Vec<_>>();
        ranges.extend(scroll::visible_ranges(s));
        let mut cache = rebook_app_core::translation_cache::TranslationCache::open(
            &s.book_id, index, &settings, &prepared,
        )
        .map_err(error)?;
        s.pipeline
            .translation
            .store_batch(index, &cache.load())
            .map_err(error)?;
        let inputs = s
            .pipeline
            .translation
            .untranslated_prepared(index, &prepared, &ranges)
            .map_err(error)?;
        request.report("translation", 0, inputs.len());
        let total = inputs.len();
        let mut completed = 0;
        let mut failure = None;
        if !inputs.is_empty() {
            runtime()
                .block_on(cancellable(
                    &request,
                    plugins::translate_blocks_with_glossary(settings, inputs, None, |batch| {
                        completed += batch.len();
                        request.report("translation", completed, total);
                        if let Err(e) = s.pipeline.translation.store_batch(index, &batch) {
                            failure = Some(e);
                        } else if let Err(_) = cache.store(&batch) {
                            tracing::warn!("translation cache could not be persisted");
                        }
                    }),
                ))
                .map_err(error)?;
        }
        if let Some(e) = failure {
            return Err(error(e));
        }
    }
    s.pipeline.translation.set_enabled(enabled).map_err(error)?;
    s.session.refresh_source().map_err(error)?;
    s.scroll_center = true;
    s.scroll_slices.clear();

    Ok(())
}

pub(crate) fn recognize_work(s: &mut ReaderState, request: &chat::ChatRequest) -> Result<()> {
    if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(error("已停止处理"));
    }
    let mut settings = PluginSettings::load_default().map_err(error)?;
    settings.semantic_layout.enabled = true;
    settings.semantic_layout_endpoint().map_err(error)?;
    let index = s.session.snapshot().location.section_index;
    let section = s.pipeline.translation.parse_section(index).map_err(error)?;
    let mut ranges = s
        .session
        .current_visible_content_sources()
        .map_err(error)?
        .into_iter()
        .flat_map(|(_, ranges)| ranges)
        .collect::<Vec<_>>();
    ranges.extend(scroll::visible_ranges(s));
    // The shared recognizer groups quote/code runs before classification.
    let target = plugins::semantic_layout::semantic_units(&section, 0..section.blocks.len())
        .into_iter()
        .filter(|unit| {
            section.blocks[unit.clone()]
                .iter()
                .flat_map(plugins::semantic_layout::block_ranges)
                .any(|source| {
                    ranges
                        .iter()
                        .any(|range| plugins::source_range_nodes_overlap(&source, range))
                })
        })
        .collect::<Vec<_>>();
    s.pipeline.semantic.configure(
        &plugins::pdf_native::cache_identity(&s.book_id, s.pipeline.rewrite.as_ref()),
        &settings,
    );
    let total = target.len();
    request.report("layout", 0, total);
    for (completed, range) in target.into_iter().enumerate() {
        let result = runtime()
            .block_on(cancellable(
                &request,
                plugins::semantic_layout::recognize_visible(
                    &section,
                    range.clone(),
                    &s.book_id,
                    &settings,
                    s.pipeline.translation.as_ref(),
                ),
            ))
            .map_err(error)?;
        request.report("layout", completed + 1, total);
        s.pipeline.semantic.install_prepared_scope(
            index,
            &section,
            &plugins::semantic_layout::fingerprint(&section),
            range,
            result,
        );
    }
    let mut style = s.session.style();
    style.typesetting.mode = TypesettingMode::Unified;
    s.session.refresh_source_with_style(style).map_err(error)?;
    s.scroll_center = true;
    s.scroll_slices.clear();

    Ok(())
}

pub(crate) fn toc_items(s: &ReaderState) -> Vec<TocItem> {
    s.session
        .toc_items()
        .iter()
        .filter_map(|t| {
            t.target.as_ref().map(|href| TocItem {
                title: s
                    .toc_labels
                    .get(&t.id)
                    .filter(|_| s.options.translated)
                    .cloned()
                    .unwrap_or_else(|| t.label.clone()),
                href: href.to_string(),
                depth: t.depth as u32,
            })
        })
        .collect()
}
#[uniffi::export]
impl MobileReader {
    pub async fn translate_toc_cancellable(
        self: Arc<Self>,
        request: Arc<chat::ChatRequest>,
    ) -> Result<Vec<TocItem>> {
        work(move || {
            self.with(|s| {
                let mut settings = PluginSettings::load_default().map_err(error)?;
                if !s.options.translated || !settings.translate_toc {
                    s.toc_labels.clear();
                    return Ok(toc_items(s));
                }
                settings.target_language = settings.resolved_target_language(
                    rebook_app_core::preferences::AppLanguage::system_translation_target(),
                );
                let config = rebook_app_core::translation_cache::configuration_key(&settings)
                    .map_err(error)?;
                if s.toc_config.as_ref() != Some(&config) {
                    s.toc_labels.clear();
                    s.toc_config = Some(config);
                }
                let rows = s.session.toc_items().to_vec();
                let inputs = rows
                    .iter()
                    .enumerate()
                    .map(|(index, row)| plugins::TranslationBlockInput {
                        block_index: index,
                        segment_index: None,
                        text: row.label.clone(),
                    })
                    .collect::<Vec<_>>();
                let mut cache =
                    rebook_app_core::translation_cache::TranslationCache::open_text_inputs(
                        &format!("{}-toc", s.book_id),
                        &settings,
                        &inputs,
                    )
                    .map_err(error)?;
                let mut translations = cache.load();
                let cached = translations
                    .iter()
                    .map(|t| t.block_index)
                    .collect::<std::collections::HashSet<_>>();
                let missing = inputs
                    .into_iter()
                    .filter(|i| !i.text.trim().is_empty() && !cached.contains(&i.block_index))
                    .collect::<Vec<_>>();
                let total = missing.len();
                let mut completed = 0;
                request.report("toc", 0, total);
                if !missing.is_empty() {
                    runtime()
                        .block_on(cancellable(
                            &request,
                            plugins::translate_blocks_with_glossary(
                                settings.clone(),
                                missing,
                                None,
                                |batch| {
                                    completed += batch.len();
                                    request.report("toc", completed, total);
                                    let _ = cache.store(&batch);
                                    translations.extend(batch);
                                },
                            ),
                        ))
                        .map_err(error)?;
                }
                for translated in translations {
                    if let Some(row) = rows.get(translated.block_index) {
                        s.toc_labels.insert(
                            row.id.clone(),
                            if settings.translation_mode == plugins::TranslationMode::Bilingual {
                                format!("{}\n{}", row.label, translated.text)
                            } else {
                                translated.text
                            },
                        );
                    }
                }
                Ok(toc_items(s))
            })
        })
        .await
    }
    pub async fn prefetch_ai(
        self: Arc<Self>,
        translate: bool,
        layout: bool,
        request: Arc<chat::ChatRequest>,
    ) -> Result<()> {
        work(move || {
            self.with(|s| {
                if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(error("已停止处理"));
                }
                let original = s.session.current_locator();
                let before = scroll::position(s);
                let focus = s.focus_anchor.clone();
                let offset = s.scroll_offset;
                let center = s.scroll_center;
                let slices = std::mem::take(&mut s.scroll_slices);
                let result = (|| {
                    s.session.turn_page(PageDirection::Next).map_err(error)?;
                    if scroll::position(s) == before {
                        return Ok(());
                    }
                    request.report("prefetch", 0, 1);
                    if translate {
                        translate_work(s, true, &request)?;
                    }
                    if layout {
                        recognize_work(s, &request)?;
                    }
                    request.report("prefetch", 1, 1);
                    Ok(())
                })();
                s.session.restore_locator(&original).map_err(error)?;
                s.focus_anchor = focus;
                s.scroll_offset = offset;
                s.scroll_center = center;
                s.scroll_slices = slices;
                result
            })
        })
        .await
    }
}
