use crate::{Result, error};
use rebook_app_core::credentials;
use std::{collections::HashMap, path::PathBuf, sync::Arc};

/// Fresh iOS profiles use the shared store without Android's migration callback.
#[cfg(any(target_os = "ios", feature = "ios-interop"))]
#[uniffi::export]
pub fn initialize_ios_credential_storage(path: String) -> Result<()> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(error("An absolute credential file path is required"));
    }
    credentials::install(Arc::new(credentials::FileStore::new(path, None))).map_err(error)
}

/// One-time import of old Android Keystore preferences. All new reads/writes
/// use the same Rust JSON store as Windows and macOS.
#[uniffi::export(with_foreign)]
pub trait LegacyCredentialStorage: Send + Sync {
    fn read_all(&self) -> Result<HashMap<String, String>>;
    fn clear(&self) -> Result<()>;
}
struct MigrationAdapter(Arc<dyn LegacyCredentialStorage>);
impl credentials::LegacyCredentials for MigrationAdapter {
    fn load(&self) -> credentials::Result<credentials::Migration> {
        let android_entries = self
            .0
            .read_all()
            .map_err(|_| credentials::Error::Store("无法迁移旧凭据，原数据已保留".into()))?
            .into_iter()
            .collect();
        Ok(credentials::Migration {
            android_entries,
            ..Default::default()
        })
    }
    fn cleanup(&self) -> credentials::Result<()> {
        self.0
            .clear()
            .map_err(|_| credentials::Error::Store("无法移除已迁移的旧凭据".into()))
    }
}
#[uniffi::export]
pub fn initialize_credential_storage(
    path: String,
    legacy: Arc<dyn LegacyCredentialStorage>,
) -> Result<()> {
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(error("An absolute credential file path is required"));
    }
    credentials::install(Arc::new(credentials::FileStore::new(
        path,
        Some(Arc::new(MigrationAdapter(legacy))),
    )))
    .map_err(error)
}
