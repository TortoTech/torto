//! Portable data can travel between machines without reusing their sync actors.
use super::{SyncResult, SyncSettings};
use crate::credentials::{Error as CredentialError, SecretStore};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};
const REGISTRY: &str = "portable-devices.json";
#[derive(Default, Serialize, Deserialize)]
struct Registry {
    version: u8,
    devices: BTreeMap<String, String>,
}
fn load(config: &Path) -> SyncResult<Registry> {
    let registry = match fs::read(config.join(REGISTRY)) {
        Ok(bytes) if bytes.len() <= 64 * 1024 => serde_json::from_slice::<Registry>(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry {
                version: 1,
                ..Default::default()
            });
        }
        _ => return Err(std::io::Error::other("无法读取便携版同步设备记录").into()),
    };
    if registry.version != 1
        || registry.devices.len() > 128
        || registry
            .devices
            .values()
            .any(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err(std::io::Error::other("便携版同步设备记录无效").into());
    }
    Ok(registry)
}
pub(super) fn bind(
    settings: &mut SyncSettings,
    scope: &str,
    config: &Path,
    credentials: &dyn SecretStore,
) -> SyncResult<bool> {
    let mut registry = load(config)?;
    let new_machine = !registry.devices.contains_key(scope);
    if new_machine && registry.devices.len() >= 128 {
        return Err(std::io::Error::other("便携版同步设备记录已达上限").into());
    }
    let id = if let Some(id) = registry.devices.get(scope) {
        id.clone()
    } else {
        let id = if registry.devices.is_empty() {
            settings.device_id.clone()
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        registry.devices.insert(scope.into(), id.clone());
        id
    };
    let changed = settings.device_id != id;
    if changed {
        // Keep old account entries, since they also belong to other computers
        // represented by this portable profile. Copy only the current password.
        match credentials.read("Rebook WebDAV", &settings.device_id) {
            Ok(password) => credentials.write("Rebook WebDAV", &id, &password)?,
            Err(CredentialError::NoEntry) => {}
            Err(error) => return Err(error.into()),
        }
    }
    if new_machine {
        crate::persistence::write_json_atomic(&config.join(REGISTRY), &registry)?;
    }
    settings.device_id = id;
    Ok(changed)
}
pub(super) fn previous_devices(config: &Path, current: &str) -> SyncResult<Vec<String>> {
    let mut devices: Vec<_> = load(config)?
        .devices
        .into_values()
        .filter(|id| id != current)
        .collect();
    devices.sort();
    devices.dedup();
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::FileStore;
    #[test]
    fn moving_profile_changes_actor_but_returning_restores_it_and_carries_password() {
        let root =
            std::env::temp_dir().join(format!("torto-portable-device-{}", uuid::Uuid::new_v4()));
        let credentials = FileStore::new(root.join("credentials.json"), None);
        let mut settings = SyncSettings::new_device();
        let first = settings.device_id.clone();
        credentials
            .write("Rebook WebDAV", &first, "cloud-password")
            .unwrap();
        assert!(!bind(&mut settings, "machine-one", &root, &credentials).unwrap());
        assert!(bind(&mut settings, "machine-two", &root, &credentials).unwrap());
        let second = settings.device_id.clone();
        assert_ne!(first, second);
        assert_eq!(
            credentials.read("Rebook WebDAV", &second).unwrap(),
            "cloud-password"
        );
        assert_eq!(
            previous_devices(&root, &second).unwrap(),
            vec![first.clone()]
        );
        assert!(bind(&mut settings, "machine-one", &root, &credentials).unwrap());
        assert_eq!(settings.device_id, first);
        assert_eq!(previous_devices(&root, &first).unwrap(), vec![second]);
        fs::remove_dir_all(root).unwrap();
    }
}
