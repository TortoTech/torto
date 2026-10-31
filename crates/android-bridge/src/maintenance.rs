use crate::*;
use rebook_app_core::updates;
use std::io::Read;
#[derive(Clone, uniffi::Record)]
pub struct AppUpdate {
    pub version: String,
    pub notes: String,
    pub size: u64,
    pub source: String,
    pub token: String,
}
fn platform(abi: &str) -> Result<updates::Platform> {
    match abi {
        "arm64-v8a" => Ok(updates::Platform::AndroidArm64),
        "x86_64" => Ok(updates::Platform::AndroidX64),
        _ => Err(error("此设备没有对应的安装包")),
    }
}
#[uniffi::export]
impl MobileCore {
    pub async fn check_update(
        self: Arc<Self>,
        current: String,
        abi: String,
        source: String,
    ) -> Result<Option<AppUpdate>> {
        work(move || {
            let source = match source.as_str() {
                "automatic" => updates::UpdateSource::Automatic,
                "github" => updates::UpdateSource::Github,
                "gitee" => updates::UpdateSource::Gitee,
                _ => return Err(error("更新来源无效")),
            };
            let found = runtime()
                .block_on(updates::check_release(
                    current.split('-').next().unwrap_or(&current),
                    platform(&abi)?,
                    source,
                ))
                .map_err(error)?;
            found
                .map(|r| {
                    Ok(AppUpdate {
                        version: r.version.clone(),
                        notes: r.notes.clone(),
                        size: r.asset.size,
                        source: if r.asset.github_asset_id.is_some() {
                            "GitHub"
                        } else {
                            "Gitee"
                        }
                        .into(),
                        token: serde_json::to_string(&r).map_err(error)?,
                    })
                })
                .transpose()
        })
        .await
    }
    pub async fn download_update(
        self: Arc<Self>,
        token: String,
        abi: String,
        request: Arc<chat::ChatRequest>,
    ) -> Result<String> {
        work(move || {
            if token.len() > 1024 * 1024 {
                return Err(error("更新元数据过大"));
            }
            let release: updates::UpdateRelease = serde_json::from_str(&token).map_err(error)?;
            if release.asset.name != platform(&abi)?.name(&release.version) {
                return Err(error("安装包与设备不匹配"));
            }
            let progress = request.clone();
            let result = runtime()
                .block_on(services::cancellable(
                    &request,
                    updates::download_update_progress(
                        release,
                        Arc::new(move |p| progress.report("update", p as usize, 100)),
                    ),
                ))
                .map_err(error)?;
            request.report("update", 100, 100);
            Ok(result.installer_path.to_string_lossy().into_owned())
        })
        .await
    }
    pub async fn export_diagnostics(self: Arc<Self>, device_info: String) -> Result<String> {
        work(move || {
            if device_info.len() > 8192 {
                return Err(error("设备信息过长"));
            }
            let mut secrets = Vec::new();
            if let Ok(s) = rebook_app_core::plugins::PluginSettings::load_default() {
                secrets.extend(
                    s.providers
                        .into_iter()
                        .map(|p| p.api_key)
                        .filter(|k| !k.is_empty()),
                );
            }
            if let Ok(s) = rebook_app_core::sync::SyncSettings::load_default() {
                if let Ok(p) = s.load_password() {
                    if !p.is_empty() {
                        secrets.push(p);
                    }
                }
            }
            let mut report = format!(
                "Torto diagnostics\nCore version: {}\n{}\n",
                env!("CARGO_PKG_VERSION"),
                device_info
            );
            for name in ["runtime.log", "reader-ui.log", "sync.log", "android.log"] {
                let path = self.root.join("logs").join(name);
                let Ok(file) = std::fs::File::open(&path) else {
                    continue;
                };
                let mut data = Vec::new();
                file.take(256 * 1024)
                    .read_to_end(&mut data)
                    .map_err(error)?;
                report.push_str(&format!("\n--- {name} ---\n"));
                for line in String::from_utf8_lossy(&data).lines() {
                    let lower = line.to_ascii_lowercase();
                    if [
                        "authorization",
                        "api_key",
                        "apikey",
                        "api key",
                        "password",
                        "bearer ",
                        "access_token",
                        "refresh_token",
                    ]
                    .iter()
                    .any(|v| lower.contains(v))
                    {
                        report.push_str("[sensitive log entry omitted]\n");
                        continue;
                    }
                    let mut safe =
                        line.replace(&self.root.to_string_lossy().to_string(), "[APP_DATA]");
                    for secret in &secrets {
                        safe = safe.replace(secret, "[REDACTED]");
                    }
                    report.push_str(&safe);
                    report.push('\n');
                }
            }
            let folder = self.root.join("cache/reports");
            std::fs::create_dir_all(&folder).map_err(error)?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = folder.join(format!("torto-diagnostics-{stamp}.txt"));
            rebook_app_core::persistence::write_bytes_atomic(&path, report.as_bytes())
                .map_err(error)?;
            let mut reports = std::fs::read_dir(&folder)
                .map_err(error)?
                .flatten()
                .filter(|e| {
                    e.file_type().is_ok_and(|t| t.is_file())
                        && e.file_name()
                            .to_string_lossy()
                            .starts_with("torto-diagnostics-")
                })
                .collect::<Vec<_>>();
            reports.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
            for old in reports.into_iter().skip(3) {
                let _ = std::fs::remove_file(old.path());
            }
            Ok(path.to_string_lossy().into_owned())
        })
        .await
    }
}

#[uniffi::export]
pub fn update_release_notes(markdown: String, language: String) -> String {
    let language = if language == "zh-CN" {
        rebook_app_core::preferences::AppLanguage::SimplifiedChinese
    } else {
        rebook_app_core::preferences::AppLanguage::English
    };
    updates::localized_release_notes(&markdown, language).to_owned()
}
