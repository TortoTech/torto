mod changes;
mod derived;
mod engine;
mod listing;
mod protocol;
mod rate_limit;
mod settings;
mod store;
#[cfg(any(test, feature = "test-support"))]
pub use engine::tests::FakeWebDav;
pub mod webdav;

use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub use engine::{LocalSyncBook, SyncMode, SyncProgress, SyncReport, SyncStage, run_sync};
pub use rate_limit::RateLimited;
pub use settings::{CloudProviderKind, SyncSettings};
pub use store::SyncStore;

pub type SyncResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn account_key(settings: &SyncSettings) -> String {
    use sha2::{Digest, Sha256};
    let endpoint = reqwest::Url::parse(&settings.base_url)
        .map_or_else(|_| settings.base_url.clone(), |url| url.to_string());
    format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{:?}\n{}\n{}\n{}",
                settings.provider,
                endpoint.trim_end_matches('/'),
                settings.username,
                settings.device_id
            )
            .as_bytes()
        )
    )
}

pub fn format_error_chain(error: &(dyn Error + 'static)) -> String {
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

pub fn append_sync_log(level: &str, message: &str) -> io::Result<PathBuf> {
    let project = crate::environment::paths()
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
pub use derived::{DerivedDataKind, derived_change_token, mark_derived_dirty};
