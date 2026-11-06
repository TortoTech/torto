//! Coarse commands reuse desktop services; network/filesystem work stays off the UI.
use crate::*;
use rebook_app_core::{
    plugins::{self, PluginSettings},
    sync::{SyncSettings, SyncStore},
};
pub(crate) mod enhancements;
pub(crate) mod requests;
pub(crate) mod driver;

pub(crate) async fn cancellable<T, E: From<&'static str>>(
    request: &chat::ChatRequest,
    future: impl std::future::Future<Output = std::result::Result<T, E>>,
) -> std::result::Result<T, E> {
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
        let mut options = self.clone().cloud_options().await?;
        options.provider = "custom".into();
        options.url = url;
        options.username = username;
        options.enabled = true;
        self.save_cloud_options(options, password).await.map(|_| ())
    }

    pub async fn sync_library(self: Arc<Self>) -> Result<SyncSummary> {
        let o = self.clone().cloud_options().await?;
        let run = self.sync_scheduled(o.account, cloud::CloudSyncTrigger::Manual, chat::ChatRequest::new()).await?;
        if let Some(error_message) = run.error { return Err(error(error_message)); }
        run.summary.ok_or_else(|| error("同步已排队，等待重试期限"))
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
        runtime().spawn(async move {
            let target=work({let reader=self.clone();move||reader.with(requests::target)}).await?;
            self.scheduler.update(vec![(target.index,target.ranges.clone())],false);
            requests::translate(self.clone(),target,enabled,request.clone()).await?;
            work(move||self.with(render)).await
        }).await.map_err(error)?
    }
    pub async fn recognize_page(self: Arc<Self>) -> Result<Arc<MobilePage>> {
        self.recognize_page_cancellable(chat::ChatRequest::new())
            .await
    }
    pub async fn recognize_page_cancellable(
        self: Arc<Self>,
        request: Arc<chat::ChatRequest>,
    ) -> Result<Arc<MobilePage>> {
        runtime().spawn(async move {
            let target=work({let reader=self.clone();move||reader.with(requests::target)}).await?;
            self.scheduler.update(vec![(target.index,target.ranges.clone())],false);
            requests::recognize(self.clone(),target,request).await?;
            work(move||self.with(render)).await
        }).await.map_err(error)?
    }
    pub async fn trim_memory(self: Arc<Self>) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.scroll_cache = None;
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

pub(crate) fn toc_items(s: &ReaderState) -> Vec<TocItem> {
    s.session
        .toc_items()
        .iter()
        .map(|t| TocItem {
                id: t.id.clone(),ancestors:t.ancestors.clone(),has_children:t.has_children,
                title: s.toc_labels.get(&t.id).filter(|_|s.options.translated).cloned().unwrap_or_else(||t.label.clone()),
                href: t.target.as_ref().map(ToString::to_string).unwrap_or_default(),depth:t.depth as u32,
            })
        .collect()
}
#[uniffi::export]
impl MobileReader {
    pub async fn translate_toc_cancellable(
        self: Arc<Self>,
        request: Arc<chat::ChatRequest>,
    ) -> Result<Vec<TocItem>> {
        runtime().spawn(requests::contents(self,request)).await.map_err(error)?
    }
    pub async fn prefetch_ai(
        self: Arc<Self>,
        translate: bool,
        layout: bool,
        request: Arc<chat::ChatRequest>,
    ) -> Result<()> {
        runtime().spawn(async move {
            let next=work({let reader=self.clone();move||reader.with(|s|{
                if request.cancelled.load(std::sync::atomic::Ordering::Acquire){return Err(error("已停止处理"));}
                let original=s.session.current_locator();let before=scroll::position(s);
                let focus=s.focus_anchor.clone();let distance=s.scroll_distance;let context=s.scroll_context;
                let cache=s.scroll_cache.take();let offset=s.scroll_offset;let center=s.scroll_center;
                let slices=std::mem::take(&mut s.scroll_slices);
                let result=(||{
                    s.session.turn_page(PageDirection::Next).map_err(error)?;
                    if scroll::position(s)==before{return Ok(None);}
                    let mut target=requests::target(s)?;target.prefetch=true;Ok(Some(target))
                })();
                scroll::restore_position(s,&original,before)?;
                s.focus_anchor=focus;s.scroll_distance=distance;s.scroll_context=context;s.scroll_cache=cache;
                s.scroll_offset=offset;s.scroll_center=center;s.scroll_slices=slices;
                Ok((result,request))
            })}).await?;
            let (next,request)=next;let Some(target)=next? else{return Ok(());};
            request.report("prefetch",0,1);
            if layout{requests::recognize(self.clone(),target.clone(),request.clone()).await?;}
            if translate{requests::translate(self.clone(),target,true,request.clone()).await?;}
            request.report("prefetch",1,1);Ok(())
        }).await.map_err(error)?
    }
}
