use crate::*;
use futures_util::TryFutureExt;
use rebook_app_core::sync::{self, SyncSettings};
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize, uniffi::Record)]
#[serde(default)]
pub struct CloudOptions {
    pub enabled: bool,
    pub provider: String,
    pub url: String,
    pub username: String,
    pub device_name: String,
    pub foreground: bool,
    pub background: bool,
    pub password_saved: bool,
    pub account: String,
}
impl Default for CloudOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "custom".into(),
            url: String::new(),
            username: String::new(),
            device_name: "Torto Android".into(),
            foreground: true,
            background: true,
            password_saved: false,
            account: String::new(),
        }
    }
}
#[derive(uniffi::Record)]
pub struct CloudPreset {
    pub id: String,
    pub name: String,
    pub url: String,
    pub help_url: String,
}
fn options(core: &MobileCore) -> Result<CloudOptions> {
    let s = SyncSettings::load_default().map_err(error)?;
    let mut o: CloudOptions = std::fs::read(core.root.join("android-cloud.json"))
        .ok()
        .filter(|b| b.len() < 16384)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    o.enabled = s.enabled;
    o.provider = serde_json::to_value(s.provider)
        .unwrap()
        .as_str()
        .unwrap()
        .into();
    o.url = s.base_url.clone();
    o.username = s.username.clone();
    o.device_name = if s.device_name == "Torto Desktop" {
        "Torto Android".into()
    } else {
        s.device_name.clone()
    };
    o.password_saved = !s.load_password().map_err(error)?.is_empty();
    o.account = sync::account_key(&s);
    Ok(o)
}
#[uniffi::export]
impl MobileCore {
    pub async fn cloud_options(self: Arc<Self>) -> Result<CloudOptions> {
        work(move || options(&self)).await
    }
    pub async fn cloud_presets(self: Arc<Self>) -> Result<Vec<CloudPreset>> {
        work(move || {
            Ok(sync::CloudProviderKind::ALL
                .iter()
                .map(|p| {
                    let mut s = SyncSettings::new_device();
                    s.select_provider(*p);
                    CloudPreset {
                        id: serde_json::to_value(p).unwrap().as_str().unwrap().into(),
                        name: p.label().into(),
                        url: if *p == sync::CloudProviderKind::Custom {
                            String::new()
                        } else {
                            s.base_url
                        },
                        help_url: p.credential_url().into(),
                    }
                })
                .collect())
        })
        .await
    }
    pub async fn save_cloud_options(
        self: Arc<Self>,
        o: CloudOptions,
        password: Option<String>,
    ) -> Result<CloudOptions> {
        work(move || {
            let _guard = self.library_lock.lock().map_err(error)?;
            let mut s = SyncSettings::load_default().map_err(error)?;
            s.provider = serde_json::from_value(serde_json::Value::String(o.provider.clone()))
                .map_err(error)?;
            s.enabled = o.enabled;
            s.base_url = o.url.clone();
            s.username = o.username.trim().into();
            s.device_name = o.device_name.trim().into();
            if s.device_name.is_empty()
                || s.device_name.len() > 160
                || s.username.len() > 512
                || s.base_url.len() > 2048
            {
                return Err(error("同步配置无效"));
            }
            s.normalize();
            if s.enabled {
                s.validate().map_err(error)?;
                rebook_app_core::provider_models::validate_provider_url(&s.base_url)
                    .map_err(error)?;
            }
            if let Some(password) = password {
                s.save_password(&password).map_err(error)?;
            }
            s.save_default().map_err(error)?;
            let mut persisted = o;
            persisted.password_saved = false;
            persisted.account.clear();
            rebook_app_core::persistence::write_json_atomic(
                &self.root.join("android-cloud.json"),
                &persisted,
            )
            .map_err(error)?;
            options(&self)
        })
        .await
    }
    pub async fn sync_cancellable(
        self: Arc<Self>,
        account: String,
        reading_only: bool,
        request: Arc<chat::ChatRequest>,
    ) -> Result<services::SyncSummary> {
        work(move || {
            let _guard = self.library_lock.lock().map_err(error)?;
            let settings = SyncSettings::load_default().map_err(error)?;
            if !settings.enabled || sync::account_key(&settings) != account {
                return Err(error("同步配置已改变"));
            }
            let password = settings.load_password().map_err(error)?;
            let mut library = LocalLibrary::load_from(self.root.join("library")).map_err(error)?;
            let books = library
                .books()
                .iter()
                .map(|b| sync::LocalSyncBook {
                    id: b.id.clone(),
                    title: b.title.clone(),
                    authors: b.authors.clone(),
                    file_name: b.file_name.clone(),
                    path: b.path.clone(),
                    cover_bytes: b.cover_bytes.clone(),
                    added_at: b.added_at,
                })
                .collect();
            let mut import_error = None;
            let result = runtime().block_on(services::cancellable(
                &request,
                sync::run_sync(
                    settings,
                    password,
                    books,
                    if reading_only {
                        sync::SyncMode::Reading
                    } else {
                        sync::SyncMode::Full {
                            force_statistics: true,
                        }
                    },
                    |event| match event {
                        sync::SyncProgress::Stage {
                            stage,
                            completed,
                            total,
                        } => request.report(
                            match stage {
                                sync::SyncStage::Preparing => "preparing",
                                sync::SyncStage::Checking => "checking",
                                sync::SyncStage::Uploading => "uploading",
                                sync::SyncStage::Downloading
                                | sync::SyncStage::PreparingDownload => "downloading",
                                sync::SyncStage::Statistics => "statistics",
                                _ => "merging",
                            },
                            completed as usize,
                            total as usize,
                        ),
                        sync::SyncProgress::Downloaded {
                            book, cache_path, ..
                        } => match library.import_remote(*book) {
                            Ok(_) => {
                                let _ = std::fs::remove_file(cache_path);
                            }
                            Err(e) => import_error = Some(e.to_string()),
                        },
                    },
                )
                .map_err(|e| sync::format_error_chain(e.as_ref())),
            ));
            let report = result.map_err(error)?;
            if let Some(e) = import_error {
                return Err(error(e));
            }
            rebook_app_core::statistics::flush();
            Ok(services::SyncSummary {
                uploaded: report.uploaded_books as u32,
                downloaded: report.downloaded_books as u32,
                annotations: report.merged_annotations as u32,
                progress: report.updated_progress as u32,
                warning: report.statistics_error,
            })
        })
        .await
    }
}
