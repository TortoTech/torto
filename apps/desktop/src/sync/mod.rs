mod derived;
mod engine;
mod protocol;
mod settings;
mod store;
mod webdav;

use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;

pub(crate) use engine::{LocalSyncBook, SyncProgress, SyncReport, SyncStage, run_sync};
pub(crate) use settings::{CloudProviderKind, SyncSettings};
pub(crate) use store::SyncStore;

pub(crate) type SyncResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) fn format_error_chain(error: &(dyn Error + 'static)) -> String {
    let mut messages = Vec::new();
    let mut current = Some(error);
    while let Some(error) = current {
        let message = error.to_string();
        if messages.last() != Some(&message) {
            messages.push(message);
        }
        current = error.source();
    }
    messages.join(": ")
}

pub(crate) fn append_sync_log(level: &str, message: &str) -> io::Result<PathBuf> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定同步日志目录"))?;
    let log_dir = project.data_local_dir().join("logs");
    fs::create_dir_all(&log_dir)?;
    let path = log_dir.join("sync.log");
    if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 1_048_576) {
        fs::write(&path, [])?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    writeln!(file, "[{timestamp}] {level} {message}")?;
    Ok(path)
}
pub(crate) use derived::{DerivedDataKind, mark_derived_dirty};
