//! macOS installer protocol. All archive, filesystem and process work runs on a worker.
use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const ID: &str = "io.github.l-chris.torto";
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Job {
    target: PathBuf,
    update: DownloadedUpdate,
    parent: u32,
    token: String,
}
static HEALTH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static ROLLBACK: std::sync::OnceLock<()> = std::sync::OnceLock::new();
static HEALTH_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FRAME_PRESENTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = path.with_extension("tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(temp, path).map_err(|e| e.to_string())
}
fn state(dir: &Path, stage: &str, detail: &str) -> Result<(), String> {
    private_write(
        &dir.join("state.json"),
        &serde_json::to_vec(
            &serde_json::json!({"stage":stage,"detail":detail,"pid":std::process::id()}),
        )
        .map_err(|e| e.to_string())?,
    )
}
fn read_job(dir: &Path) -> Result<Job, String> {
    let meta = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
    // An installed app can be root-owned while the updater runs as the user.
    // Authenticate the job against the effective user, not the binary's owner.
    let owner: u32 = output("/usr/bin/id", &["-u".as_ref()])?
        .parse()
        .map_err(|_| "Could not determine the update user")?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != owner
        || meta.mode() & 0o077 != 0
    {
        return Err("Update job is not a private owned directory".into());
    }
    let file = dir.join("request.json");
    let m = fs::symlink_metadata(&file).map_err(|e| e.to_string())?;
    if !m.is_file()
        || m.file_type().is_symlink()
        || m.uid() != owner
        || m.len() > 16 * 1024
        || m.mode() & 0o077 != 0
    {
        return Err("Invalid update request file".into());
    }
    let job: Job = serde_json::from_slice(&fs::read(file).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if uuid::Uuid::parse_str(&job.token).is_err()
        || dir
            .file_name()
            .is_none_or(|name| name != job.token.as_str())
        || job.parent <= 1
        || Path::new(&job.update.release.asset.name)
            .file_name()
            .is_none_or(|name| name != job.update.release.asset.name.as_str())
        || job.update.installer_path != dir.join(&job.update.release.asset.name)
    {
        return Err("Invalid update session".into());
    }
    Ok(job)
}
fn output(tool: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let result = Command::new(tool)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "{} failed: {}",
            tool,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_string())
}
fn plist(app: &Path, key: &str) -> Result<String, String> {
    output(
        "/usr/libexec/PlistBuddy",
        &[
            "-c".as_ref(),
            format!("Print :{key}").as_ref(),
            app.join("Contents/Info.plist").as_os_str(),
        ],
    )
}
fn verify_bundle(app: &Path, version: Option<&str>) -> Result<(), String> {
    if plist(app, "CFBundleIdentifier")? != ID || plist(app, "CFBundleExecutable")? != "torto" {
        return Err("The update is not a Torto application".into());
    }
    if let Some(v) = version {
        if plist(app, "CFBundleShortVersionString")? != v {
            return Err("Update bundle version mismatch".into());
        }
    }
    output(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            app.as_os_str(),
        ],
    )?;
    Ok(())
}
fn team(app: &Path) -> Result<Option<String>, String> {
    let out = Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=4"])
        .arg(app)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("Could not read the Apple signing identity".into());
    }
    Ok(String::from_utf8_lossy(&out.stderr).lines().find_map(|l| {
        l.strip_prefix("TeamIdentifier=")
            .filter(|s| *s != "not set")
            .map(str::to_string)
    }))
}
fn current_bundle() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("Run the installed Torto.app to update")?;
    if app.extension().is_none_or(|s| s != "app") {
        return Err("请先将 Torto.app 安装到应用程序目录，再使用自动更新".into());
    }
    validate_target(app)?;
    Ok(app.to_path_buf())
}
fn validate_target(app: &Path) -> Result<(), String> {
    if fs::symlink_metadata(app)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
        || fs::canonicalize(app).map_err(|e| e.to_string())? != app
    {
        return Err("The installed application path must not contain symbolic links".into());
    }
    if app.starts_with("/System") || app.to_string_lossy().contains("AppTranslocation") {
        return Err("请先将应用从磁盘映像移到应用程序目录".into());
    }
    verify_bundle(app, None)
}
fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
fn other_instance(job: &Job) -> Result<bool, String> {
    let executable = job.target.join("Contents/MacOS/torto");
    let processes = output("/bin/ps", &["-axo".as_ref(), "pid=,comm=".as_ref()])?;
    Ok(processes.lines().any(|line| {
        let line = line.trim();
        let Some((pid, command)) = line.split_once(char::is_whitespace) else {
            return false;
        };
        pid.parse::<u32>()
            .is_ok_and(|pid| pid != job.parent && Path::new(command.trim()) == executable)
    }))
}
fn extract(zip: &Path, to: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(fs::File::open(zip).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if archive.len() > 16384 {
        return Err("Too many files in update".into());
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().map_err(|e| e.to_string())?.into_owned();
        let path = Path::new(&name);
        if name.contains('\\')
            || path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || !path.starts_with("Torto.app")
            || file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
        {
            return Err("Unsafe path or symbolic link in update archive".into());
        }
        total = total
            .checked_add(file.size())
            .filter(|n| *n <= MAX_EXPANDED)
            .ok_or("Expanded update exceeds 512 MiB")?;
        let dest = to.join(path);
        if file.is_dir() {
            fs::create_dir_all(dest).map_err(|e| e.to_string())?;
            continue;
        }
        fs::create_dir_all(dest.parent().ok_or("Invalid archive path")?)
            .map_err(|e| e.to_string())?;
        let expected = file.size();
        let mode = if file.unix_mode().unwrap_or(0) & 0o111 != 0 {
            0o755
        } else {
            0o644
        };
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&dest)
            .map_err(|e| e.to_string())?;
        let count = std::io::copy(&mut (&mut file).take(expected + 1), &mut out)
            .map_err(|e| e.to_string())?;
        if count != expected {
            return Err("Incomplete archive member".into());
        }
    }
    Ok(())
}
fn launch(app: &Path, job: Option<(&str, &Path)>) -> Result<Child, String> {
    let mut cmd = Command::new(app.join("Contents/MacOS/torto"));
    if let Some((flag, dir)) = job {
        cmd.arg(flag).arg(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())
}
pub(crate) fn prepare(
    update: &DownloadedUpdate,
    save: Box<dyn FnOnce() -> Result<(), String> + Send>,
) -> Result<(), String> {
    verify_installer_file(update)?;
    let target = current_bundle()?;
    let root = update
        .installer_path
        .parent()
        .ok_or("No update cache directory")?
        .join("sessions");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    // Completed sessions only; do not delete an installer that is still active.
    let mut old: Vec<_> = fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| e.path().join("state.json").exists())
        .collect();
    old.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    for e in old.iter().take(old.len().saturating_sub(2)) {
        if let Ok(bytes) = fs::read(e.path().join("state.json")) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if matches!(v["stage"].as_str(), Some("complete" | "failed")) {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    let dir = root.join(&token);
    fs::create_dir(&dir).map_err(|e| e.to_string())?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let mut copy = update.clone();
    copy.release.notes.clear(); // Installation jobs do not need potentially long release notes.
    copy.installer_path = dir.join(&update.release.asset.name);
    fs::copy(&update.installer_path, &copy.installer_path).map_err(|e| e.to_string())?;
    let job = Job {
        target,
        update: copy,
        parent: std::process::id(),
        token,
    };
    private_write(
        &dir.join("request.json"),
        &serde_json::to_vec(&job).map_err(|e| e.to_string())?,
    )?;
    let helper = dir.join("TortoUpdater");
    fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &helper)
        .map_err(|e| e.to_string())?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o500)).map_err(|e| e.to_string())?;
    if let Err(error) = save() {
        let _ = fs::remove_dir_all(&dir);
        return Err(error);
    }
    let mut child = Command::new(helper)
        .arg("--torto-update-helper")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Ok(bytes) = fs::read(dir.join("state.json")) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if v["stage"] == "waiting" {
                    return Ok(());
                }
                if v["stage"] == "failed" {
                    return Err(v["detail"]
                        .as_str()
                        .unwrap_or("Update preparation failed")
                        .into());
                }
            }
        }
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            return Err("Update helper exited before it was ready".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let staging = job
        .target
        .parent()
        .ok_or("No installation directory")?
        .join(format!(".torto-update-{}", job.token));
    if !staging.join("previous.app").exists() {
        let _ = fs::remove_dir_all(staging);
    }
    let _ = state(&dir, "failed", "Update preparation timed out");
    let _ = fs::remove_file(&job.update.installer_path);
    let _ = fs::remove_file(dir.join("TortoUpdater"));
    Err("Update preparation timed out; application remains open".into())
}
pub(crate) fn helper(dir: &Path) -> Result<(), String> {
    let job = read_job(dir)?;
    let staging = job
        .target
        .parent()
        .ok_or("No installation directory")?
        .join(format!(".torto-update-{}", job.token));
    let mut parent_exited = false;
    let result: Result<(), String> = (|| {
        verify_installer_file(&job.update)?;
        validate_target(&job.target)?;
        let old_version = plist(&job.target, "CFBundleShortVersionString")?;
        if AppVersion::parse(&job.update.release.version)? <= AppVersion::parse(&old_version)? {
            return Err("Refusing an update downgrade".into());
        }
        fs::create_dir(&staging).map_err(
            |_| "当前安装目录不可写；请移到用户的 Applications 目录后更新，或手动安装新版",
        )?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let source = staging.join("Torto.app");
        extract(&job.update.installer_path, &staging)?;
        verify_bundle(&source, Some(&job.update.release.version))?;
        if let Some(old) = team(&job.target)? {
            if team(&source)?.as_ref() != Some(&old) {
                return Err("The Apple signing team changed".into());
            }
        }
        let arch = if cfg!(target_arch = "aarch64") {
            Platform::MacosArm64
        } else {
            Platform::MacosX64
        };
        if job.update.release.asset.name != arch.name(&job.update.release.version) {
            return Err("Update architecture mismatch".into());
        }
        let arch_name = if arch == Platform::MacosArm64 {
            "arm64"
        } else {
            "x86_64"
        };
        output(
            "/usr/bin/lipo",
            &[
                "-verify_arch".as_ref(),
                arch_name.as_ref(),
                source.join("Contents/MacOS/torto").as_os_str(),
            ],
        )?;
        if other_instance(&job)? {
            return Err("请先关闭同一安装目录中运行的其他 Torto 窗口，再重试更新".into());
        }
        state(dir, "waiting", "")?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while alive(job.parent) {
            if Instant::now() >= deadline {
                return Err("The current application did not exit".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        parent_exited = true;
        if other_instance(&job)? {
            parent_exited = false; // Another instance already keeps the old app open.
            return Err("Another instance opened while preparing the update".into());
        }
        state(dir, "installing", "")?;
        let backup = staging.join("previous.app");
        fs::rename(&job.target, &backup).map_err(|e| e.to_string())?;
        let install: Result<(), String> = (|| {
            fs::rename(&source, &job.target).map_err(|e| e.to_string())?;
            verify_bundle(&job.target, Some(&job.update.release.version))?;
            let mut child = launch(&job.target, Some(("--torto-update-health", dir)))?;
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if fs::read_to_string(dir.join("healthy")).is_ok_and(|s| s == job.token) {
                    return Ok(());
                }
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    return Err("The new application exited during startup".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
            Err("The new application did not confirm startup".into())
        })();
        if let Err(error) = install {
            if job.target.exists() {
                fs::rename(&job.target, staging.join("failed.app")).map_err(|e| {
                    format!(
                        "{error}; previous app preserved at {}: {e}",
                        backup.display()
                    )
                })?;
            }
            fs::rename(&backup, &job.target).map_err(|e| {
                format!(
                    "{error}; previous app preserved at {}: {e}",
                    backup.display()
                )
            })?;
            return Err(error);
        }
        parent_exited = false; // The replacement has confirmed its first frame.
        state(dir, "complete", "")?;
        Ok(())
    })();
    if let Err(error) = &result {
        let _ = state(dir, "failed", error);
        if parent_exited && !staging.join("previous.app").exists() && job.target.exists() {
            let _ = launch(&job.target, Some(("--torto-update-rollback", dir)));
        }
    }
    // Never delete the only remaining old installation if rollback itself failed.
    if !staging.join("previous.app").exists() {
        let _ = fs::remove_dir_all(&staging);
    } else if result.is_ok() {
        let _ = fs::remove_dir_all(&staging);
    }
    // Keep small diagnostics, rather than another ZIP and executable per update.
    let _ = fs::remove_file(&job.update.installer_path);
    let _ = fs::remove_file(dir.join("TortoUpdater"));
    result
}
pub(crate) fn set_health_job(dir: PathBuf) -> Result<(), String> {
    let job = read_job(&dir)?;
    macos_auth::verify_release(&job.update.release)?;
    if job.update.release.version != env!("CARGO_PKG_VERSION") {
        return Err("Update startup version mismatch".into());
    }
    HEALTH
        .set(dir)
        .map_err(|_| "Duplicate update startup request".into())
}
pub(crate) fn set_rollback_job(dir: PathBuf) -> Result<(), String> {
    let job = read_job(&dir)?;
    macos_auth::verify_release(&job.update.release)?;
    ROLLBACK
        .set(())
        .map_err(|_| "Duplicate rollback notice".into())
}
pub(crate) fn was_rolled_back() -> bool {
    ROLLBACK.get().is_some()
}
pub(crate) fn frame_presented() {
    FRAME_PRESENTED.store(true, std::sync::atomic::Ordering::Relaxed);
}
pub(crate) fn confirm_health(runtime: &tokio::runtime::Runtime) {
    if !FRAME_PRESENTED.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(dir) = HEALTH.get() {
        if !HEALTH_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            let dir = dir.clone();
            runtime.spawn_blocking(move || {
                if let Ok(job) = read_job(&dir) {
                    let _ = private_write(&dir.join("healthy"), job.token.as_bytes());
                }
            });
        }
    }
}
