#[allow(unused_imports)]
use std::time::Duration;
#[allow(unused_imports)]
use reqwest::header::{ACCEPT,USER_AGENT};
#[allow(unused_imports)]
use serde::Deserialize;
#[allow(unused_imports)]
use rebook_app_core::updates::*;
mod history;
use history::{ReleaseNote, display_notes};


use egui::RichText;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
#[cfg(test)]
use sha2::{Digest, Sha256};

use crate::async_task::{TaskResult, TaskSlot};
use crate::http::github;
#[cfg(target_os = "windows")]
use crate::platform::UserEvent;
use crate::preferences::AppLanguage;
use crate::ui::{dialog_action_button, palette};

#[cfg(test)]
#[path = "updater/asset_download_tests.rs"]
mod asset_download_tests;
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
    History(TaskResult<Vec<ReleaseNote>>),
}

pub(crate) enum ManualUpdateCheckResult {
    UpToDate,
    Available(String),
    Failed(String),
}

pub(crate) struct WindowsUpdater {
    check_task: TaskSlot<()>,
    download_task: TaskSlot<UpdateRelease>,
    history_task: TaskSlot<UpdateRelease>,
    history: Option<(String, Result<Vec<ReleaseNote>, String>)>,
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
            history_task: TaskSlot::default(),
            history: None,
            state: UpdateState::Idle,
            install_request: None,
            manual_check: false,
            dialog_visible: false,
            release_notes_cache: CommonMarkCache::default(),
        }
    }

    #[cfg(target_os = "windows")]
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
        if let Some(request) = self.history_task.take_pending() {
            let proxy = proxy.clone();
            let worker = runtime.spawn(async move {
                let result = history::fetch(env!("CARGO_PKG_VERSION"), &request.payload).await;
                let _ =
                    proxy.send_event(UserEvent::Update(UpdateTaskMessage::History(TaskResult {
                        id: request.id,
                        result,
                    })));
            });
            self.history_task.attach_worker(worker);
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
        if !manual_updates_supported() {
            return;
        }
        self.manual_check = true;
        if !self.check_task.is_pending() {
            self.check_task.begin(());
        }
    }

    pub(crate) fn request_update(&mut self) {
        if !manual_updates_supported() {
            return;
        }
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
                        self.begin_history(&release);
                        self.state = UpdateState::Available(release);
                        self.dialog_visible = !manual;
                        return manual.then_some(result);
                    }
                    Ok(None) => {
                        self.history_task.cancel();
                        self.history = None;
                        self.state = UpdateState::Idle;
                        self.dialog_visible = false;
                        return manual.then_some(ManualUpdateCheckResult::UpToDate);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "automatic update check failed");
                        self.history_task.cancel();
                        self.history = None;
                        self.state = UpdateState::Idle;
                        self.dialog_visible = false;
                        return manual.then_some(ManualUpdateCheckResult::Failed(error));
                    }
                }
            }
            UpdateTaskMessage::History(message) => {
                let release = self.history_task.complete(message.id)?;
                self.history = Some((release.version, message.result));
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

    fn begin_history(&mut self, release: &UpdateRelease) {
        if self
            .history
            .as_ref()
            .is_some_and(|(version, _)| version == &release.version)
            || self
                .history_task
                .active()
                .is_some_and(|active| active.version == release.version)
        {
            return;
        }
        self.history_task.cancel();
        self.history = None;
        self.history_task.begin(release.clone());
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
        self.begin_history(view.release());
        let mut retry_history = false;
        let mut action = None;
        let modal = crate::ui::modal(ctx, egui::Id::unique("windows-update-modal"), 0.42)
            .frame(
                egui::Frame::new()
                    .fill(palette().surface)
                    .stroke(egui::Stroke::new(1.0, palette().border))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::symmetric(22, 18)),
            )
            .show(ctx, |ui| {
                ui.set_width(
                    460.0_f32.min((crate::ui::overlay_rect(ctx).width() - 32.0).max(280.0)),
                );
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
                        if let Some((version, Ok(releases))) = &self.history
                            && version == &view.release().version
                        {
                            for release in releases {
                                ui.strong(format!("v{}", release.version));
                                let notes = display_notes(&release.notes, language);
                                show_release_notes(ui, &mut self.release_notes_cache, &notes);
                                ui.add_space(12.0);
                            }
                        } else {
                            ui.strong(format!("v{}", view.release().version));
                            let notes = display_notes(&view.release().notes, language);
                            show_release_notes(ui, &mut self.release_notes_cache, &notes);
                        }
                        if self.history_task.is_pending() {
                            ui.horizontal(|ui| {
                                ui.add(crate::ui::LoadingSpinner::new().size(14.0));
                                ui.label(language.text(
                                    "正在加载其他版本的更新内容…",
                                    "Loading changes from other versions…",
                                ));
                            });
                        } else if self
                            .history
                            .as_ref()
                            .is_some_and(|(_, result)| result.is_err())
                        {
                            ui.label(language.text(
                                "其他版本的更新内容暂时无法加载，仍可更新。",
                                "Other release notes are unavailable. You can still update.",
                            ));
                            retry_history = ui
                                .small_button(language.text("重新加载", "Reload notes"))
                                .clicked();
                        }
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
                            ui.add(crate::ui::LoadingSpinner::new().size(18.0));
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
        if retry_history {
            self.history = None;
            self.begin_history(view.release());
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

#[cfg(target_os = "windows")]
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

fn automatic_check_enabled() -> bool {
    manual_updates_supported()
        && (!cfg!(debug_assertions)
            || std::env::var_os("TORTO_ENABLE_UPDATE_CHECK").is_some_and(|value| value == "1"))
}

pub(crate) fn manual_updates_supported() -> bool {
    std::env::current_exe().map_or(true, |path| {
        !path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("WindowsApps")
        })
    })
}

async fn check_for_update() -> Result<Option<UpdateRelease>, String> {
    check_for_update_version(env!("CARGO_PKG_VERSION")).await
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn github_release(tag: &str, digest: Option<&str>) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.into(),
            body: Some("## Feature\n\n- Update support".into()),
            draft: false,
            prerelease: false,
            assets: vec![GitHubAsset {
                id: 42,
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
    fn history_failure_does_not_block_download_and_stale_history_is_rejected() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let release = release_from_github(github_release("v0.9.0", Some(&digest)), "0.8.3")
            .unwrap()
            .unwrap();
        let mut updater = WindowsUpdater::new();
        updater.check_task.cancel();
        updater.state = UpdateState::Available(release.clone());
        updater.begin_history(&release);
        let old = updater.history_task.take_pending().unwrap();
        updater.history_task.cancel();
        updater.begin_history(&release);
        let current = updater.history_task.take_pending().unwrap();
        updater.complete(UpdateTaskMessage::History(TaskResult {
            id: old.id,
            result: Ok(vec![]),
        }));
        assert!(updater.history.is_none());
        assert!(updater.history_task.is_pending());
        updater.complete(UpdateTaskMessage::History(TaskResult {
            id: current.id,
            result: Err("unreachable".into()),
        }));
        assert!(updater.history.as_ref().unwrap().1.is_err());
        updater.request_update();
        assert!(matches!(updater.state, UpdateState::Downloading(_)));
        assert!(updater.download_task.is_pending());
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
            github_asset_id: None,
            sha256: "a".repeat(64),
            size: 1,
        };

        assert!(validate_asset(&asset).is_err());
    }

    #[test]
    fn updater_uses_the_current_github_organization() {
        assert_eq!(
            github::TORTO.latest_release_url(),
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
            github_asset_id: Some(42),
            url:
                "https://github.com/TortoTech/torto/releases/download/v0.2.12/Torto-0.2.12-x86_64.msi"
                    .into(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size: u64::try_from(bytes.len()).unwrap(),
        };

        assert!(verify_installer_bytes(&asset, bytes).is_ok());
        assert!(verify_installer_bytes(&asset, b"tampered installer").is_err());
    }

    pub(super) fn gitee_fixture() -> (GiteeRelease, GiteeManifest) {
        let release = GiteeRelease {
            tag_name: "v0.2.12".into(), body: Some("中文说明".into()), prerelease: false,
            assets: vec![GiteeAsset { name: "Torto-0.2.12-x86_64.msi".into(),
                browser_download_url: "https://gitee.com/TortoTech/torto/releases/download/v0.2.12/Torto-0.2.12-x86_64.msi".into() }],
        };
        let manifest = GiteeManifest {
            version: "0.2.12".into(),
            tag: "v0.2.12".into(),
            asset: ManifestAsset {
                name: "Torto-0.2.12-x86_64.msi".into(),
                sha256: "a".repeat(64),
                size: 32 * 1024 * 1024,
            },
        };
        (release, manifest)
    }

    #[test]
    fn gitee_manifest_must_match_release_and_uploaded_installer() {
        let (release, manifest) = gitee_fixture();
        let update = release_from_gitee(release, manifest, "0.2.11")
            .unwrap()
            .unwrap();
        assert_eq!(update.notes, "中文说明");
        for invalid in 0..4 {
            let (mut release, mut manifest) = gitee_fixture();
            match invalid {
                0 => manifest.version = "0.2.13".into(),
                1 => manifest.tag = "v0.2.13".into(),
                2 => release.assets.clear(),
                _ => manifest.asset.sha256.clear(),
            }
            assert!(release_from_gitee(release, manifest, "0.2.11").is_err());
        }
    }

    #[test]
    fn update_selection_uses_available_newest_source_and_rejects_conflicts() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let github = release_from_github(github_release("v0.2.12", Some(&digest)), "0.2.11")
            .unwrap()
            .unwrap();
        let (release, manifest) = gitee_fixture();
        let gitee = release_from_gitee(release, manifest, "0.2.11")
            .unwrap()
            .unwrap();
        assert_eq!(
            select_update(Err("unreachable".into()), Ok(Some(gitee.clone()))).unwrap(),
            Some(gitee.clone())
        );
        assert_eq!(
            select_update(Ok(Some(github.clone())), Err("incomplete mirror".into())).unwrap(),
            Some(github.clone())
        );
        assert!(
            select_update(Err("unreachable".into()), Ok(None))
                .unwrap()
                .is_none()
        );
        assert!(select_update(Err("unreachable".into()), Err("unreachable".into())).is_err());
        let older = release_from_github(github_release("v0.2.11", Some(&digest)), "0.2.10")
            .unwrap()
            .unwrap();
        assert_eq!(
            select_update(Ok(Some(older)), Ok(Some(gitee.clone()))).unwrap(),
            Some(gitee.clone())
        );
        let mut conflict = gitee;
        conflict.asset.sha256 = "b".repeat(64);
        assert!(select_update(Ok(Some(github)), Ok(Some(conflict))).is_err());
    }

    #[test]
    fn both_download_origins_require_exact_version_filename_and_https() {
        let (release, manifest) = gitee_fixture();
        let asset = release_from_gitee(release, manifest, "0.2.11")
            .unwrap()
            .unwrap()
            .asset;
        assert!(validate_asset(&asset).is_ok());
        for url in [
            asset.url.replace("https:", "http:"),
            asset.url.replace("gitee.com", "gitee.com.evil.example"),
            asset.url.replace("TortoTech", "another-owner"),
            asset.url.replace("v0.2.12/", "v0.2.13/"),
            format!("{}?redirect=evil", asset.url),
            asset.url.replace("gitee.com", "gitee.com:444"),
            asset.url.replace("gitee.com", "user@gitee.com"),
        ] {
            assert!(
                validate_asset(&UpdateAsset {
                    url,
                    ..asset.clone()
                })
                .is_err()
            );
        }
        assert!(trusted_download_redirect(
            &reqwest::Url::parse("https://foruda.gitee.com/attach_file/1/file.msi").unwrap()
        ));
        assert!(!trusted_download_redirect(
            &reqwest::Url::parse("https://evil.example/file.msi").unwrap()
        ));
    }

    #[test]
    fn failed_or_corrupt_primary_download_retries_same_asset_from_other_origin() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let bytes = b"verified installer";
        let (release, manifest) = gitee_fixture();
        let mut asset = release_from_gitee(release, manifest, "0.2.11")
            .unwrap()
            .unwrap()
            .asset;
        asset.sha256 = format!("{:x}", Sha256::digest(bytes));
        asset.size = bytes.len() as u64;
        for corrupt in [false, true] {
            let mut calls = Vec::new();
            let result = runtime
                .block_on(download_with_fallback(&asset, |url| {
                    calls.push(url);
                    let result = if calls.len() == 1 {
                        if corrupt {
                            Ok(vec![0; bytes.len()])
                        } else {
                            Err("offline".into())
                        }
                    } else {
                        Ok(bytes.to_vec())
                    };
                    std::future::ready(result)
                }))
                .unwrap();
            assert_eq!(result, bytes);
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[1], asset.url.replace("gitee.com", "github.com"));
        }
        assert!(
            runtime
                .block_on(download_with_fallback(&asset, |_| std::future::ready(Err(
                    "offline".into()
                ))))
                .is_err()
        );
    }

    #[test]
    #[ignore = "uses public release APIs; run explicitly after mirror synchronization"]
    fn live_gitee_update_manifest_is_usable() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let update = runtime
            .block_on(check_gitee(&crate::http::client(), "0.0.0"))
            .unwrap()
            .unwrap();
        validate_asset(&update.asset).unwrap();
        assert!(
            update
                .asset
                .url
                .starts_with("https://gitee.com/TortoTech/torto/releases/download/")
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn update_dialog_dims_caption_controls_including_over_settings() {
        use crate::app::window_chrome;

        let digest = format!("sha256:{}", "a".repeat(64));
        let release = release_from_github(github_release("v0.2.12", Some(&digest)), "0.2.11")
            .unwrap()
            .unwrap();
        let mut updater = WindowsUpdater::new();
        updater.state = UpdateState::Available(release);
        let ctx = egui::Context::default();
        crate::ui::apply_interface_typography(
            &ctx,
            &crate::preferences::InterfaceTypography::default(),
            rebook_layout::default_monospace_font(),
            AppLanguage::English,
        );
        for (visible, settings_opacity) in [(false, 0.0), (true, 0.0), (true, 0.46)] {
            updater.dialog_visible = visible;
            let mut output = ctx.run_ui(egui::RawInput::default(), |_| {
                window_chrome::begin_frame(&ctx);
                window_chrome::header(
                    &ctx,
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 44.0)),
                );
                if settings_opacity > 0.0 {
                    let _ =
                        crate::ui::modal(&ctx, egui::Id::unique("test-settings"), settings_opacity)
                            .show(&ctx, |ui| {
                                ui.label("Settings");
                            });
                }
                updater.overlay(&ctx, AppLanguage::English);
                let geometry = window_chrome::geometry(&ctx);
                let expected = if visible {
                    1.0 - (1.0 - settings_opacity) * (1.0 - 0.42)
                } else {
                    settings_opacity
                };
                assert!((geometry.overlay_dim - expected).abs() < 0.0001);
                assert_eq!(geometry.drag_enabled, !visible);
            });
            output.textures_delta.clear();
        }
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
            rebook_layout::default_monospace_font(),
            crate::preferences::AppLanguage::English,
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
