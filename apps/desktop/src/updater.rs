use std::path::PathBuf;
use std::time::Duration;

use directories::ProjectDirs;
use egui::RichText;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::async_task::{TaskResult, TaskSlot};
use crate::persistence::write_bytes_atomic;
use crate::platform::UserEvent;
use crate::preferences::AppLanguage;
use crate::ui::{dialog_action_button, palette};

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/TortoTech/torto/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "/TortoTech/torto/releases/download/";
const CHINESE_RELEASE_NOTES_SUMMARY: &str = "中文更新说明";
const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;
const WINDOWS_INSTALL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$targetProcess = Get-Process -Id ([int]$env:TORTO_UPDATE_PARENT_PID) -ErrorAction SilentlyContinue
if ($null -ne $targetProcess) { $targetProcess.WaitForExit() }
$quotedInstaller = '"' + $env:TORTO_UPDATE_INSTALLER.Replace('"', '""') + '"'
$quotedInstallDirectory = '"' + $env:TORTO_UPDATE_INSTALL_DIR.Replace('"', '""') + '"'
$arguments = '/i ' + $quotedInstaller + ' APPLICATIONFOLDER=' + $quotedInstallDirectory + ' /passive /norestart'
$installerProcess = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList $arguments -Wait -PassThru
if (Test-Path -LiteralPath $env:TORTO_UPDATE_RELAUNCH) {
    Start-Process -FilePath $env:TORTO_UPDATE_RELAUNCH
}
exit $installerProcess.ExitCode
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpdateRelease {
    version: String,
    notes: String,
    asset: UpdateAsset,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UpdateAsset {
    name: String,
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DownloadedUpdate {
    release: UpdateRelease,
    installer_path: PathBuf,
}

#[derive(Clone, Debug)]
enum UpdateState {
    Idle,
    Available(UpdateRelease),
    Downloading(UpdateRelease),
    Ready(DownloadedUpdate),
    DownloadFailed {
        release: UpdateRelease,
        message: String,
    },
    InstallFailed {
        update: DownloadedUpdate,
        message: String,
    },
    Installing,
}

#[derive(Clone, Debug)]
pub(crate) struct InstallRequest {
    update: DownloadedUpdate,
}

#[derive(Debug)]
pub(crate) enum UpdateTaskMessage {
    Check(TaskResult<Option<UpdateRelease>>),
    Download(TaskResult<DownloadedUpdate>),
}

pub(crate) enum ManualUpdateCheckResult {
    UpToDate,
    Available(String),
    Failed(String),
}

pub(crate) struct WindowsUpdater {
    check_task: TaskSlot<()>,
    download_task: TaskSlot<UpdateRelease>,
    state: UpdateState,
    install_request: Option<InstallRequest>,
    manual_check: bool,
    dialog_visible: bool,
    release_notes_cache: CommonMarkCache,
}

impl WindowsUpdater {
    pub(crate) fn new() -> Self {
        let mut check_task = TaskSlot::default();
        if automatic_check_enabled() {
            check_task.begin(());
        }
        Self {
            check_task,
            download_task: TaskSlot::default(),
            state: UpdateState::Idle,
            install_request: None,
            manual_check: false,
            dialog_visible: false,
            release_notes_cache: CommonMarkCache::default(),
        }
    }

    pub(crate) fn spawn_pending_tasks(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if let Some(request) = self.check_task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let result = check_for_update().await;
                let _ = proxy.send_event(UserEvent::Update(UpdateTaskMessage::Check(TaskResult {
                    id: request.id,
                    result,
                })));
            });
        }
        if let Some(request) = self.download_task.take_pending() {
            let proxy = proxy.clone();
            runtime.spawn(async move {
                let result = download_update(request.payload).await;
                let _ =
                    proxy.send_event(UserEvent::Update(UpdateTaskMessage::Download(TaskResult {
                        id: request.id,
                        result,
                    })));
            });
        }
    }

    pub(crate) fn request_check(&mut self) {
        self.manual_check = true;
        if !self.check_task.is_pending() {
            self.check_task.begin(());
        }
    }

    pub(crate) fn request_update(&mut self) {
        match self.state.clone() {
            UpdateState::Available(release) | UpdateState::DownloadFailed { release, .. } => {
                self.download_task.begin(release.clone());
                self.state = UpdateState::Downloading(release);
                self.dialog_visible = true;
            }
            UpdateState::Ready(_) | UpdateState::InstallFailed { .. } => {
                self.dialog_visible = true;
            }
            UpdateState::Idle | UpdateState::Downloading(_) | UpdateState::Installing => {}
        }
    }

    pub(crate) fn complete(
        &mut self,
        message: UpdateTaskMessage,
    ) -> Option<ManualUpdateCheckResult> {
        match message {
            UpdateTaskMessage::Check(message) => {
                self.check_task.complete(message.id)?;
                let manual = std::mem::take(&mut self.manual_check);
                match message.result {
                    Ok(Some(release)) => {
                        let result = ManualUpdateCheckResult::Available(release.version.clone());
                        self.state = UpdateState::Available(release);
                        self.dialog_visible = !manual;
                        return manual.then_some(result);
                    }
                    Ok(None) => {
                        self.state = UpdateState::Idle;
                        self.dialog_visible = false;
                        return manual.then_some(ManualUpdateCheckResult::UpToDate);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "automatic update check failed");
                        self.state = UpdateState::Idle;
                        self.dialog_visible = false;
                        return manual.then_some(ManualUpdateCheckResult::Failed(error));
                    }
                }
            }
            UpdateTaskMessage::Download(message) => {
                let release = self.download_task.complete(message.id)?;
                match message.result {
                    Ok(update) => self.state = UpdateState::Ready(update),
                    Err(message) => {
                        self.state = UpdateState::DownloadFailed { release, message };
                    }
                }
            }
        }
        None
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the update modal keeps its state-specific actions in one auditable flow"
    )]
    pub(crate) fn overlay(&mut self, ctx: &egui::Context, language: AppLanguage) {
        if !self.dialog_visible {
            return;
        }
        let Some(view) = UpdateDialogView::from_state(&self.state) else {
            return;
        };
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("windows-update-modal"))
            .area(egui::Modal::default_area(egui::Id::new(
                "windows-update-modal",
            )))
            .backdrop_color(egui::Color32::BLACK.gamma_multiply(0.42))
            .frame(
                egui::Frame::new()
                    .fill(palette().surface)
                    .stroke(egui::Stroke::new(1.0, palette().border))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| {
                ui.set_width(460.0_f32.min((ctx.content_rect().width() - 32.0).max(280.0)));
                ui.heading(
                    RichText::new(language.text("发现新版本", "Update available"))
                        .size(crate::ui::scaled_font_size(19.0))
                        .color(palette().text),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "{}  →  {}",
                        env!("CARGO_PKG_VERSION"),
                        view.release().version
                    ))
                    .strong()
                    .color(palette().accent),
                );
                ui.add_space(10.0);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let notes = localized_release_notes(&view.release().notes, language);
                        show_release_notes(ui, &mut self.release_notes_cache, notes);
                    });
                if let Some(message) = view.message() {
                    ui.add_space(10.0);
                    ui.colored_label(palette().error, message);
                }
                ui.add_space(16.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match &view {
                        UpdateDialogView::Available(_) => {
                            if dialog_action_button(ui, language.text("更新", "Update"), true)
                                .clicked()
                            {
                                action = Some(UpdateAction::Download);
                            }
                        }
                        UpdateDialogView::Downloading(_) => {
                            ui.add(egui::Spinner::new().size(18.0));
                            ui.label(language.text("正在下载…", "Downloading…"));
                        }
                        UpdateDialogView::Ready(_) | UpdateDialogView::InstallFailed(_, _) => {
                            if dialog_action_button(ui, language.text("安装", "Install"), true)
                                .clicked()
                            {
                                action = Some(UpdateAction::Install);
                            }
                        }
                        UpdateDialogView::DownloadFailed(_, _) => {
                            if dialog_action_button(ui, language.text("重试", "Retry"), true)
                                .clicked()
                            {
                                action = Some(UpdateAction::Download);
                            }
                        }
                    }
                    if !matches!(view, UpdateDialogView::Downloading(_))
                        && dialog_action_button(ui, language.text("稍后", "Later"), false).clicked()
                    {
                        action = Some(UpdateAction::Dismiss);
                    }
                });
            });
        if modal.should_close() && !matches!(view, UpdateDialogView::Downloading(_)) {
            action = Some(UpdateAction::Dismiss);
        }
        match action {
            Some(UpdateAction::Dismiss) => self.dialog_visible = false,
            Some(UpdateAction::Download) => {
                let release = view.release().clone();
                self.download_task.begin(release.clone());
                self.state = UpdateState::Downloading(release);
            }
            Some(UpdateAction::Install) => {
                if let Some(update) = view.downloaded() {
                    self.install_request = Some(InstallRequest {
                        update: update.clone(),
                    });
                    self.state = UpdateState::Installing;
                    self.dialog_visible = false;
                }
            }
            None => {}
        }
    }

    pub(crate) fn take_install_request(&mut self) -> Option<InstallRequest> {
        self.install_request.take()
    }

    pub(crate) fn report_install_error(&mut self, request: InstallRequest, message: String) {
        if !matches!(self.state, UpdateState::Installing) {
            return;
        }
        self.state = UpdateState::InstallFailed {
            update: request.update,
            message,
        };
    }
}

pub(crate) fn launch_installer_after_exit(request: &InstallRequest) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    verify_installer_file(&request.update)?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let install_directory = executable
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "The current installation directory is unavailable".to_owned())?;
    Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            WINDOWS_INSTALL_SCRIPT,
        ])
        .env("TORTO_UPDATE_PARENT_PID", std::process::id().to_string())
        .env("TORTO_UPDATE_INSTALLER", &request.update.installer_path)
        .env("TORTO_UPDATE_INSTALL_DIR", install_directory)
        .env("TORTO_UPDATE_RELAUNCH", &executable)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[derive(Clone)]
enum UpdateDialogView {
    Available(UpdateRelease),
    Downloading(UpdateRelease),
    Ready(DownloadedUpdate),
    DownloadFailed(UpdateRelease, String),
    InstallFailed(DownloadedUpdate, String),
}

impl UpdateDialogView {
    fn from_state(state: &UpdateState) -> Option<Self> {
        match state {
            UpdateState::Available(release) => Some(Self::Available(release.clone())),
            UpdateState::Downloading(release) => Some(Self::Downloading(release.clone())),
            UpdateState::Ready(update) => Some(Self::Ready(update.clone())),
            UpdateState::DownloadFailed { release, message } => {
                Some(Self::DownloadFailed(release.clone(), message.clone()))
            }
            UpdateState::InstallFailed { update, message } => {
                Some(Self::InstallFailed(update.clone(), message.clone()))
            }
            UpdateState::Idle | UpdateState::Installing => None,
        }
    }

    fn release(&self) -> &UpdateRelease {
        match self {
            Self::Available(release)
            | Self::Downloading(release)
            | Self::DownloadFailed(release, _) => release,
            Self::Ready(update) | Self::InstallFailed(update, _) => &update.release,
        }
    }

    fn downloaded(&self) -> Option<&DownloadedUpdate> {
        match self {
            Self::Ready(update) | Self::InstallFailed(update, _) => Some(update),
            Self::Available(_) | Self::Downloading(_) | Self::DownloadFailed(_, _) => None,
        }
    }

    fn message(&self) -> Option<&str> {
        match self {
            Self::DownloadFailed(_, message) | Self::InstallFailed(_, message) => Some(message),
            Self::Available(_) | Self::Downloading(_) | Self::Ready(_) => None,
        }
    }
}

enum UpdateAction {
    Dismiss,
    Download,
    Install,
}

fn show_release_notes(ui: &mut egui::Ui, cache: &mut CommonMarkCache, markdown: &str) {
    ui.scope(|ui| {
        ui.visuals_mut().override_text_color = Some(palette().text);
        ui.style_mut().interaction.selectable_labels = true;
        CommonMarkViewer::new()
            .indentation_spaces(2)
            .show(ui, cache, markdown);
    });
}

fn localized_release_notes(markdown: &str, language: AppLanguage) -> &str {
    let Some((english, details)) = markdown.split_once("<details>") else {
        return markdown;
    };
    let details = details.trim_start();
    let Some(summary_and_rest) = details.strip_prefix("<summary>") else {
        return markdown;
    };
    let Some((summary, chinese_and_rest)) = summary_and_rest.split_once("</summary>") else {
        return markdown;
    };
    if summary.trim() != CHINESE_RELEASE_NOTES_SUMMARY {
        return markdown;
    }
    let Some((chinese, _)) = chinese_and_rest.split_once("</details>") else {
        return markdown;
    };

    let localized = match language {
        AppLanguage::SimplifiedChinese => chinese.trim(),
        AppLanguage::English => english.trim(),
    };
    if localized.is_empty() {
        markdown
    } else {
        localized
    }
}

fn automatic_check_enabled() -> bool {
    !cfg!(debug_assertions)
        || std::env::var_os("TORTO_ENABLE_UPDATE_CHECK").is_some_and(|value| value == "1")
}

async fn check_for_update() -> Result<Option<UpdateRelease>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(LATEST_RELEASE_URL)
        .header(USER_AGENT, format!("Torto/{}", env!("CARGO_PKG_VERSION")))
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let release = response
        .json::<GitHubRelease>()
        .await
        .map_err(|error| error.to_string())?;
    release_from_github(release, env!("CARGO_PKG_VERSION"))
}

async fn download_update(release: UpdateRelease) -> Result<DownloadedUpdate, String> {
    validate_asset(&release.asset)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_mins(3))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(&release.asset.url)
        .header(USER_AGENT, format!("Torto/{}", env!("CARGO_PKG_VERSION")))
        .header(ACCEPT, "application/octet-stream")
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_INSTALLER_BYTES || length != release.asset.size)
    {
        return Err("The installer response has an unexpected file size".into());
    }
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    verify_installer_bytes(&release.asset, &bytes)?;
    let project = ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or_else(|| "Unable to resolve the local update directory".to_string())?;
    let path = project
        .cache_dir()
        .join("updates")
        .join(&release.version)
        .join(&release.asset.name);
    write_bytes_atomic(&path, &bytes).map_err(|error| error.to_string())?;
    Ok(DownloadedUpdate {
        release,
        installer_path: path,
    })
}

fn verify_installer_file(update: &DownloadedUpdate) -> Result<(), String> {
    let bytes = std::fs::read(&update.installer_path)
        .map_err(|error| format!("Unable to read the downloaded installer: {error}"))?;
    verify_installer_bytes(&update.release.asset, &bytes)
}

fn verify_installer_bytes(asset: &UpdateAsset, bytes: &[u8]) -> Result<(), String> {
    let actual_size = u64::try_from(bytes.len()).map_err(|error| error.to_string())?;
    if actual_size != asset.size || actual_size > MAX_INSTALLER_BYTES {
        return Err(format!(
            "Downloaded installer size mismatch: expected {}, received {actual_size}",
            asset.size
        ));
    }
    let actual_digest = format!("{:x}", Sha256::digest(bytes));
    if actual_digest != asset.sha256 {
        return Err("Downloaded installer failed SHA-256 verification".into());
    }
    Ok(())
}

fn release_from_github(
    release: GitHubRelease,
    current_version: &str,
) -> Result<Option<UpdateRelease>, String> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let current = AppVersion::parse(current_version)?;
    let latest = AppVersion::parse(release.tag_name.trim_start_matches('v'))?;
    if latest <= current {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name.ends_with("-x86_64.msi"))
        .ok_or_else(|| "The latest release does not include a Windows x86_64 MSI".to_string())?;
    let digest = asset
        .digest
        .and_then(|digest| digest.strip_prefix("sha256:").map(str::to_owned))
        .ok_or_else(|| "The Windows installer does not include a SHA-256 digest".to_string())?
        .to_ascii_lowercase();
    let update = UpdateRelease {
        version: latest.to_string(),
        notes: release
            .body
            .filter(|body| !body.trim().is_empty())
            .unwrap_or_else(|| "This release does not include release notes.".into()),
        asset: UpdateAsset {
            name: asset.name,
            url: asset.browser_download_url,
            sha256: digest,
            size: asset.size,
        },
    };
    validate_asset(&update.asset)?;
    Ok(Some(update))
}

fn validate_asset(asset: &UpdateAsset) -> Result<(), String> {
    if asset.name.contains(['/', '\\']) || !asset.name.ends_with("-x86_64.msi") {
        return Err("The Windows installer has an invalid file name".into());
    }
    if asset.size == 0 || asset.size > MAX_INSTALLER_BYTES {
        return Err("The Windows installer has an invalid file size".into());
    }
    if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The Windows installer has an invalid SHA-256 digest".into());
    }
    let url = reqwest::Url::parse(&asset.url).map_err(|error| error.to_string())?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.path().starts_with(RELEASE_DOWNLOAD_PREFIX)
    {
        return Err("The Windows installer download URL is not trusted".into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AppVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl AppVersion {
    fn parse(value: &str) -> Result<Self, String> {
        let mut parts = value.split('.');
        let major = parse_version_part(parts.next(), value)?;
        let minor = parse_version_part(parts.next(), value)?;
        let patch = parse_version_part(parts.next(), value)?;
        if parts.next().is_some() {
            return Err(format!("Unsupported application version: {value}"));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for AppVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_version_part(part: Option<&str>, full: &str) -> Result<u64, String> {
    let part = part.ok_or_else(|| format!("Unsupported application version: {full}"))?;
    if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("Unsupported application version: {full}"));
    }
    part.parse::<u64>()
        .map_err(|_| format!("Unsupported application version: {full}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github_release(tag: &str, digest: Option<&str>) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.into(),
            body: Some("## Feature\n\n- Update support".into()),
            draft: false,
            prerelease: false,
            assets: vec![GitHubAsset {
                name: format!("Torto-{}-x86_64.msi", tag.trim_start_matches('v')),
                browser_download_url: format!(
                    "https://github.com/TortoTech/torto/releases/download/{tag}/Torto-{}-x86_64.msi",
                    tag.trim_start_matches('v')
                ),
                digest: digest.map(str::to_owned),
                size: 32 * 1024 * 1024,
            }],
        }
    }

    #[test]
    fn semantic_versions_compare_numerically() {
        assert!(AppVersion::parse("0.10.0").unwrap() > AppVersion::parse("0.9.9").unwrap());
        assert!(AppVersion::parse("1.0.0").unwrap() > AppVersion::parse("0.99.99").unwrap());
        assert!(AppVersion::parse("1.0").is_err());
        assert!(AppVersion::parse("1.0.0-beta").is_err());
    }

    #[test]
    fn latest_release_requires_a_newer_version_and_verified_msi() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let update = release_from_github(github_release("v0.2.12", Some(&digest)), "0.2.11")
            .unwrap()
            .unwrap();

        assert_eq!(update.version, "0.2.12");
        assert_eq!(update.asset.sha256, "a".repeat(64));
        assert!(
            release_from_github(github_release("v0.2.11", Some(&digest)), "0.2.11")
                .unwrap()
                .is_none()
        );
        assert!(release_from_github(github_release("v0.2.12", None), "0.2.11").is_err());
    }

    #[test]
    fn installer_url_must_point_to_the_project_release() {
        let asset = UpdateAsset {
            name: "Torto-0.2.12-x86_64.msi".into(),
            url: "https://example.com/Torto-0.2.12-x86_64.msi".into(),
            sha256: "a".repeat(64),
            size: 1,
        };

        assert!(validate_asset(&asset).is_err());
    }

    #[test]
    fn updater_uses_the_current_github_organization() {
        assert_eq!(
            LATEST_RELEASE_URL,
            "https://api.github.com/repos/TortoTech/torto/releases/latest"
        );
        assert_eq!(
            RELEASE_DOWNLOAD_PREFIX,
            "/TortoTech/torto/releases/download/"
        );
    }

    #[test]
    fn installer_bytes_must_match_size_and_digest() {
        let bytes = b"verified installer";
        let asset = UpdateAsset {
            name: "Torto-0.2.12-x86_64.msi".into(),
            url:
                "https://github.com/TortoTech/torto/releases/download/v0.2.12/Torto-0.2.12-x86_64.msi"
                    .into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size: u64::try_from(bytes.len()).unwrap(),
        };

        assert!(verify_installer_bytes(&asset, bytes).is_ok());
        assert!(verify_installer_bytes(&asset, b"tampered installer").is_err());
    }

    #[test]
    fn update_action_starts_download_only_after_a_release_is_available() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let release = release_from_github(github_release("v0.2.12", Some(&digest)), "0.2.11")
            .unwrap()
            .unwrap();
        let mut updater = WindowsUpdater::new();

        updater.request_update();
        assert!(!updater.dialog_visible);
        assert!(!updater.download_task.is_pending());

        updater.state = UpdateState::Available(release);
        updater.request_update();
        assert!(updater.dialog_visible);
        assert!(updater.download_task.is_pending());
        assert!(matches!(updater.state, UpdateState::Downloading(_)));
    }

    #[test]
    fn release_notes_render_markdown_instead_of_source_markers() {
        fn collect_text(shape: &egui::epaint::Shape, output: &mut String) {
            match shape {
                egui::epaint::Shape::Text(text) => output.push_str(text.galley.text()),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_text(shape, output);
                    }
                }
                _ => {}
            }
        }

        let ctx = egui::Context::default();
        crate::ui::apply_interface_typography(
            &ctx,
            &crate::preferences::InterfaceTypography::default(),
        );
        let mut cache = CommonMarkCache::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show_release_notes(ui, &mut cache, "## Feature\n\n- Update support");
        });
        let mut painted_text = String::new();
        for shape in &output.shapes {
            collect_text(&shape.shape, &mut painted_text);
        }
        output.textures_delta.clear();

        assert!(painted_text.contains("Feature"));
        assert!(painted_text.contains("Update support"));
        assert!(!painted_text.contains("## Feature"));
        assert!(!painted_text.contains("- Update support"));
    }

    #[test]
    fn release_notes_follow_the_interface_language() {
        let notes = "## Feature\n\n- Faster opening\n\n<details>\n<summary>中文更新说明</summary>\n\n## 功能\n\n- 更快地打开书籍\n\n</details>";

        assert_eq!(
            localized_release_notes(notes, AppLanguage::English),
            "## Feature\n\n- Faster opening"
        );
        assert_eq!(
            localized_release_notes(notes, AppLanguage::SimplifiedChinese),
            "## 功能\n\n- 更快地打开书籍"
        );
    }

    #[test]
    fn release_notes_fall_back_to_the_original_body() {
        let legacy = "## Feature\n\n- Update support";
        let incomplete = "English\n\n<details>\n<summary>中文更新说明</summary>\n中文";

        assert_eq!(
            localized_release_notes(legacy, AppLanguage::SimplifiedChinese),
            legacy
        );
        assert_eq!(
            localized_release_notes(incomplete, AppLanguage::English),
            incomplete
        );
    }

    #[test]
    fn unattended_upgrade_passes_the_existing_install_directory_to_msi() {
        assert!(WINDOWS_INSTALL_SCRIPT.contains("TORTO_UPDATE_INSTALL_DIR"));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("APPLICATIONFOLDER="));
        assert!(WINDOWS_INSTALL_SCRIPT.contains("/passive /norestart"));
    }
}
