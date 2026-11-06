//! Portable Windows updates replace only code, after the reader has saved and exited.
use rebook_app_core::updates::{DownloadedUpdate, Platform, verify_installer_file};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_EXE: u64 = 256 * 1024 * 1024;
pub(crate) const SCRIPT: &str = include_str!("../../../../scripts/update-windows-portable.ps1");
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    target: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    parent: u32,
    sha256: String,
    token: String,
}
static HEALTH: OnceLock<(PathBuf, String)> = OnceLock::new();
static HEALTH_SENT: AtomicBool = AtomicBool::new(false);
static FRAME_PRESENTED: AtomicBool = AtomicBool::new(false);
static ROLLBACK: AtomicBool = AtomicBool::new(false);

fn extract_executable(archive: &Path, destination: &Path) -> Result<String, String> {
    let mut archive = zip::ZipArchive::new(fs::File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if archive.len() > 4 {
        return Err("Unexpected portable ZIP contents".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?;
        if !matches!(
            name.as_ref(),
            "torto.exe" | "torto-portable.json" | "LICENSE" | "README.txt"
        ) || !names.insert(name.into_owned())
            || !entry.is_file()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            || entry.size() > MAX_EXE
        {
            return Err("Invalid portable ZIP entry".into());
        }
    }
    let mut marker = Vec::new();
    archive
        .by_name(crate::profile::MARKER)
        .map_err(|_| "Missing portable marker")?
        .take(4097)
        .read_to_end(&mut marker)
        .map_err(|e| e.to_string())?;
    let marker: serde_json::Value =
        serde_json::from_slice(&marker).map_err(|_| "Invalid portable marker")?;
    if marker != serde_json::json!({"version":1}) {
        return Err("Unsupported portable marker".into());
    }
    let entry = archive
        .by_name("torto.exe")
        .map_err(|_| "Missing portable executable")?;
    if entry.size() < 64 {
        return Err("Invalid portable executable".into());
    }
    let mut bytes = Vec::new();
    entry
        .take(MAX_EXE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_EXE || !bytes.starts_with(b"MZ") {
        return Err("Invalid portable executable".into());
    }
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn stage(update: &DownloadedUpdate, executable: &Path) -> Result<PathBuf, String> {
    verify_installer_file(update)?;
    if update.release.asset.name != Platform::WindowsPortableX64.name(&update.release.version) {
        return Err("A portable installation requires a portable ZIP update".into());
    }
    let root = executable.parent().ok_or("Missing portable directory")?;
    let updates = root.join("Data/cache/updates");
    prune_finished_sessions(&updates);
    let dir = updates.join(format!("portable-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = (|| {
        let staged = dir.join("new.exe");
        let sha256 = extract_executable(&update.installer_path, &staged)?;
        let job = Job {
            target: executable.into(),
            staged,
            backup: dir.join("previous.exe"),
            parent: std::process::id(),
            sha256,
            token: uuid::Uuid::new_v4().to_string(),
        };
        rebook_app_core::persistence::write_json_atomic(&dir.join("request.json"), &job)
            .map_err(|e| e.to_string())?;
        fs::write(dir.join("update.ps1"), SCRIPT).map_err(|e| e.to_string())?;
        Ok(dir.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    result
}

fn prune_finished_sessions(updates: &Path) {
    // Keep the two most recent failed/completed sessions for diagnosis, but
    // never discard an active session or an executable retained for recovery.
    let Ok(entries) = fs::read_dir(updates) else {
        return;
    };
    let mut finished: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || !entry.file_name().to_string_lossy().starts_with("portable-")
                || path.join("previous.exe").exists()
            {
                return None;
            }
            let state = fs::read_to_string(path.join("state.txt")).ok()?;
            if !matches!(state.as_str(), "completed" | "failed") {
                return None;
            }
            Some((metadata.modified().ok()?, path))
        })
        .collect();
    finished.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in finished.into_iter().skip(2) {
        let _ = fs::remove_dir_all(path);
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn launch(update: &DownloadedUpdate) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = stage(update, &executable)?;
    let powershell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or("Windows directory unavailable")?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let result = std::process::Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(dir.join("update.ps1"))
        .env("TORTO_UPDATE_JOB", &dir)
        .creation_flags(0x0800_0000)
        .spawn();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&dir);
        return Err(error.to_string());
    }
    Ok(())
}

pub(crate) fn set_health_job(dir: PathBuf, rollback: bool) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let root = executable.parent().ok_or("Missing executable directory")?;
    let expected = fs::canonicalize(root.join("Data/cache/updates")).map_err(|e| e.to_string())?;
    let dir = fs::canonicalize(dir).map_err(|e| e.to_string())?;
    if dir.parent() != Some(expected.as_path())
        || !dir
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("portable-"))
    {
        return Err("Invalid portable update session".into());
    }
    let bytes = fs::read(dir.join("request.json")).map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 {
        return Err("Oversized portable update session".into());
    }
    let job: Job = serde_json::from_slice(&bytes).map_err(|_| "Invalid portable update session")?;
    if fs::canonicalize(&job.target).map_err(|e| e.to_string())?
        != fs::canonicalize(&executable).map_err(|e| e.to_string())?
        || uuid::Uuid::parse_str(&job.token).is_err()
        || !root.join(crate::profile::MARKER).is_file()
    {
        return Err("Portable update target mismatch".into());
    }
    if rollback {
        ROLLBACK.store(true, Ordering::Relaxed);
    } else {
        HEALTH
            .set((dir.join("healthy"), job.token))
            .map_err(|_| "Duplicate health session")?;
    }
    Ok(())
}
pub(crate) fn was_rolled_back() -> bool {
    ROLLBACK.load(Ordering::Relaxed)
}
pub(crate) fn frame_presented() {
    FRAME_PRESENTED.store(true, Ordering::Relaxed);
}
pub(crate) fn confirm_health(runtime: &tokio::runtime::Runtime) {
    if !FRAME_PRESENTED.load(Ordering::Relaxed) {
        return;
    }
    if let Some((path, token)) = HEALTH.get() {
        if !HEALTH_SENT.swap(true, Ordering::Relaxed) {
            let (path, token) = (path.clone(), token.clone());
            runtime.spawn_blocking(move || {
                rebook_app_core::persistence::write_bytes_atomic(&path, token.as_bytes())
            });
        }
    }
}

#[cfg(test)]
mod tests;
