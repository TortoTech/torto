use crate::*;
use rebook_app_core::sync::{self, SyncMode, SyncSchedule, SyncSettings};
use serde::{Deserialize, Serialize};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
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
            reset_schedule(&self)?;
            options(&self)
        })
        .await
    }
    pub async fn sync_scheduled(
        self: Arc<Self>,
        account: String,
        trigger: CloudSyncTrigger,
        request: Arc<chat::ChatRequest>,
    ) -> Result<CloudSyncRun> {
        work(move || {
            let _guard = self.library_lock.lock().map_err(error)?;
            if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err(error("已停止处理"));
            }
            let settings = SyncSettings::load_default().map_err(error)?;
            if sync::account_key(&settings) != account {
                return Err(error("同步配置已改变"));
            }
            if !settings.enabled {
                return Ok(CloudSyncRun::disabled());
            }
            let now = Instant::now();
            let mut schedule = load_schedule(&self, &settings, now)?;
            let previous = schedule.clone();
            let store = services::store()?;
            let latest = store
                .pending_reading(&account)
                .map_err(error)?
                .into_iter()
                .map(|(_, _, changed)| changed)
                .max();
            schedule.observe_derived(sync::derived_change_token().map_err(error)?);
            let mode = match trigger {
                CloudSyncTrigger::Manual => {
                    let mode = SyncMode::Full {
                        force_statistics: true,
                    };
                    schedule.start_requested(mode, now).then_some(mode)
                }
                CloudSyncTrigger::Automatic | CloudSyncTrigger::FlushReading => {
                    if matches!(trigger, CloudSyncTrigger::FlushReading) {
                        schedule.request(SyncMode::Reading);
                    }
                    let mode = schedule.next(now, wall_ms(), latest);
                    if let Some(mode) = mode {
                        schedule.started(mode);
                    }
                    mode
                }
            };
            let Some(mode) = mode else {
                if schedule != previous {
                    save_schedule(&self, &settings, &schedule)?;
                }
                return Ok(CloudSyncRun::waiting(&schedule, now));
            };
            // Journal the intent before IO. A killed worker must not lose the
            // selected mode or restart a full library scan in its place.
            let mut in_flight = schedule.clone();
            in_flight.request(mode);
            save_schedule(&self, &settings, &in_flight)?;
            let result = execute(&self, &settings, mode, &request);
            let finished = Instant::now();
            if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                schedule.request(mode);
                save_schedule(&self, &settings, &schedule)?;
                return Err(error("已停止处理"));
            }
            schedule.finished(
                mode,
                result
                    .as_ref()
                    .map_err(|e| e.downcast_ref::<sync::RateLimited>().map(|e| e.retry_after)),
                finished,
            );
            save_schedule(&self, &settings, &schedule)?;
            let mut outcome = CloudSyncRun::waiting(&schedule, finished);
            match result {
                Ok(report) => {
                    rebook_app_core::statistics::flush();
                    outcome.error = report.statistics_error.clone();
                    outcome.summary = Some(services::SyncSummary {
                        uploaded: report.uploaded_books as u32,
                        downloaded: report.downloaded_books as u32,
                        annotations: report.merged_annotations as u32,
                        progress: report.updated_progress as u32,
                        warning: report.statistics_error,
                    });
                }
                Err(e) => outcome.error = Some(sync::format_error_chain(e.as_ref())),
            }
            Ok(outcome)
        })
        .await
    }
}

#[derive(Clone, Copy, uniffi::Enum)]
pub enum CloudSyncTrigger {
    Automatic,
    Manual,
    FlushReading,
}

#[derive(uniffi::Record)]
pub struct CloudSyncRun {
    pub summary: Option<services::SyncSummary>,
    pub error: Option<String>,
    pub next_poll_ms: u64,
    pub retry_after_ms: Option<u64>,
}
impl CloudSyncRun {
    fn disabled() -> Self {
        Self {
            summary: None,
            error: None,
            next_poll_ms: 0,
            retry_after_ms: None,
        }
    }
    fn waiting(schedule: &SyncSchedule, now: Instant) -> Self {
        Self {
            summary: None,
            error: None,
            next_poll_ms: schedule.poll_delay(now).as_millis() as u64,
            retry_after_ms: schedule
                .retry_delay(now)
                .map(|d| d.as_millis().max(1_000) as u64),
        }
    }
}

const SCHEDULE_FILE: &str = "cloud-schedule-v1.json";
#[derive(Serialize, Deserialize)]
struct StoredSchedule {
    version: u32,
    account: String,
    snapshot: sync::ScheduleSnapshot,
}
struct ActiveSchedule {
    root: PathBuf,
    account: String,
    schedule: SyncSchedule,
}
static ACTIVE_SCHEDULE: Mutex<Option<ActiveSchedule>> = Mutex::new(None);

fn remember_schedule(
    core: &MobileCore,
    settings: &SyncSettings,
    schedule: &SyncSchedule,
) -> Result<()> {
    *ACTIVE_SCHEDULE.lock().map_err(error)? = Some(ActiveSchedule {
        root: core.root.clone(),
        account: sync::account_key(settings),
        schedule: schedule.clone(),
    });
    Ok(())
}

#[cfg(test)]
pub(crate) fn forget_schedule_cache() {
    *ACTIVE_SCHEDULE.lock().unwrap() = None;
}
fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
fn load_schedule(core: &MobileCore, settings: &SyncSettings, now: Instant) -> Result<SyncSchedule> {
    // Keep live deadlines monotonic. Wall-clock conversion is only needed on
    // process start, not on every foreground probe.
    if let Some(active) = ACTIVE_SCHEDULE.lock().map_err(error)?.as_ref()
        && active.root == core.root
        && active.account == sync::account_key(settings)
    {
        return Ok(active.schedule.clone());
    }
    let path = core.root.join("config").join(SCHEDULE_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() <= 16_384)
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(saved) = serde_json::from_slice::<StoredSchedule>(&bytes)
        && saved.version == 1
        && saved.account == sync::account_key(settings)
    {
        let schedule = SyncSchedule::restore(saved.snapshot, now, wall_ms());
        remember_schedule(core, settings, &schedule)?;
        return Ok(schedule);
    }
    let mut schedule = SyncSchedule::new(now);
    schedule.request(SyncMode::Full {
        force_statistics: false,
    });
    remember_schedule(core, settings, &schedule)?;
    Ok(schedule)
}
fn save_schedule(
    core: &MobileCore,
    settings: &SyncSettings,
    schedule: &SyncSchedule,
) -> Result<()> {
    rebook_app_core::persistence::write_json_atomic(
        &core.root.join("config").join(SCHEDULE_FILE),
        &StoredSchedule {
            version: 1,
            account: sync::account_key(settings),
            snapshot: schedule.snapshot(Instant::now(), wall_ms()),
        },
    )
    .map_err(error)?;
    remember_schedule(core, settings, schedule)
}
pub(crate) fn reset_schedule(core: &MobileCore) -> Result<()> {
    match std::fs::remove_file(core.root.join("config").join(SCHEDULE_FILE)) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(error(e)),
    };
    *ACTIVE_SCHEDULE.lock().map_err(error)? = None;
    Ok(())
}
pub(crate) fn library_changed(core: &MobileCore) -> Result<()> {
    let settings = SyncSettings::load_default().map_err(error)?;
    if settings.enabled {
        let mut schedule = load_schedule(core, &settings, Instant::now())?;
        schedule.request(SyncMode::Full {
            force_statistics: false,
        });
        save_schedule(core, &settings, &schedule)?;
    }
    Ok(())
}
fn execute(
    core: &MobileCore,
    settings: &SyncSettings,
    mode: SyncMode,
    request: &chat::ChatRequest,
) -> sync::SyncResult<sync::SyncReport> {
    let password = settings.load_password()?;
    let mut library = LocalLibrary::load_from(core.root.join("library"))?;
    let books = if matches!(mode, SyncMode::Reading | SyncMode::Statistics) {
        Vec::new()
    } else {
        library
            .books()
            .iter()
            .map(|b| sync::LocalSyncBook {
                id: b.id.clone(),
                title: b.title.clone(),
                authors: b.authors.clone(),
                file_name: b.file_name.clone(),
                path: b.path.clone(),
                cover_bytes: if mode == SyncMode::Derived {
                    None
                } else {
                    b.cover_bytes.clone()
                },
                added_at: b.added_at,
            })
            .collect()
    };
    let mut import_error = None;
    let result = runtime().block_on(services::cancellable(
        request,
        sync::run_sync(
            settings.clone(),
            password,
            books,
            mode,
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
                        sync::SyncStage::Downloading | sync::SyncStage::PreparingDownload => {
                            "downloading"
                        }
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
                    Err(e) => import_error = Some(e),
                },
            },
        ),
    ))?;
    if let Some(e) = import_error {
        return Err(e);
    }
    Ok(result)
}
