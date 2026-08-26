//! UI-independent persistence and merge model for reading progress and annotations.

mod highlights;
pub mod protocol;
mod store;

use std::{fs, io, path::PathBuf};

use directories::ProjectDirs;
use serde::Deserialize;

pub use highlights::{HighlightRepository, HighlightResult, HighlightStore, StoredHighlight};
pub use store::{StoredProgress, SyncStore};

pub type SyncResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Deserialize)]
struct StoredDeviceSettings {
    settings: DeviceIdentity,
}

#[derive(Deserialize)]
struct DeviceIdentity {
    device_id: String,
}

/// Reads the device identity already owned by the shared `WebDAV` settings
/// without coupling a frontend to provider credentials or settings widgets.
pub fn configured_device_id() -> SyncResult<Option<String>> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定同步设置目录"))?;
    let path = project.data_local_dir().join("webdav-sync.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let stored: StoredDeviceSettings = serde_json::from_slice(&bytes)?;
    let device_id = stored.settings.device_id.trim().to_owned();
    Ok((!device_id.is_empty()).then_some(device_id))
}

/// Opens the production store with the configured device identity. Before
/// cloud sync is configured, a stable machine-local name keeps annotations
/// usable without manufacturing a new identity on every launch.
pub fn open_default_store() -> SyncResult<SyncStore> {
    let device_id = configured_device_id()?.unwrap_or_else(|| {
        let machine = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "desktop".into());
        format!("local-{}", machine.trim().to_lowercase())
    });
    if let Some(path) = std::env::var_os("REBOOK_SYNC_DATABASE") {
        return SyncStore::open_at(PathBuf::from(path), device_id);
    }
    SyncStore::open_default(device_id)
}
