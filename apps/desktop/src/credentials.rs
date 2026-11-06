//! Keep unit tests away from the user's system credential store.
#[cfg(not(test))]
pub(crate) fn entry(service: &str, account: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(service, account)
}

#[cfg(test)]
pub(crate) fn entry(service: &str, account: &str) -> keyring::Result<keyring::Entry> {
    use keyring_core::api::CredentialStoreApi;
    use std::sync::{Arc, OnceLock};

    // Construct mock entries directly. Changing the process-wide default store
    // would race with keyring v1's lazy native-store initialization.
    static STORE: OnceLock<Arc<keyring_core::mock::Store>> = OnceLock::new();
    let store =
        STORE.get_or_init(|| keyring_core::mock::Store::new().expect("mock credential store"));
    Ok(keyring::Entry {
        inner: store.build(service, account, None)?,
    })
}

#[cfg(test)]
mod tests {
    use super::entry;

    #[test]
    fn credentials_are_mocked_shared_across_threads_and_scoped_by_service() {
        let account = uuid::Uuid::new_v4().to_string();
        let credential = entry("test-credentials", &account).unwrap();
        assert!(credential.inner.as_any().is::<keyring_core::mock::Cred>());
        assert!(matches!(
            credential.get_password(),
            Err(keyring::Error::NoEntry)
        ));
        credential.set_password("fixture-secret").unwrap();
        let reader_account = account.clone();
        let password = std::thread::spawn(move || {
            entry("test-credentials", &reader_account)
                .unwrap()
                .get_password()
                .unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(password, "fixture-secret");
        assert!(matches!(
            entry("other-service", &account).unwrap().get_password(),
            Err(keyring::Error::NoEntry)
        ));
        credential.delete_credential().unwrap();
        assert!(matches!(
            entry("test-credentials", &account).unwrap().get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }
}

struct CoreSecrets;
impl rebook_app_core::credentials::SecretStore for CoreSecrets {
    fn read(&self, s: &str, a: &str) -> rebook_app_core::credentials::Result<String> {
        entry(s, a)
            .and_then(|e| e.get_password())
            .map_err(core_error)
    }
    fn write(&self, s: &str, a: &str, v: &str) -> rebook_app_core::credentials::Result<()> {
        entry(s, a)
            .and_then(|e| e.set_password(v))
            .map_err(core_error)
    }
    fn delete(&self, s: &str, a: &str) -> rebook_app_core::credentials::Result<()> {
        delete_native(s, a)
    }
}

#[cfg(any(not(target_os = "macos"), test))]
fn delete_native(s: &str, a: &str) -> rebook_app_core::credentials::Result<()> {
    entry(s, a)
        .and_then(|e| e.delete_credential())
        .map_err(core_error)
}

#[cfg(all(target_os = "macos", not(test)))]
fn delete_native(s: &str, a: &str) -> rebook_app_core::credentials::Result<()> {
    use security_framework::{
        item::{ItemClass, ItemSearchOptions},
        os::macos::keychain::{SecKeychain, SecPreferencesDomain},
    };
    // The keyring backend reads the secret before deleting it, which can ask
    // for authorization again after migration. Delete by exact metadata only,
    // in the same user-domain keychain as keyring's reads and writes.
    if s.is_empty() || a.is_empty() {
        return Err(rebook_app_core::credentials::Error::Store(
            "安全凭据标识不能为空".into(),
        ));
    }
    let delete = || {
        let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)?;
        ItemSearchOptions::new()
            .keychains(&[keychain])
            .class(ItemClass::generic_password())
            .service(s)
            .account(a)
            .delete()
    };
    delete().map_err(|error| match error.code() {
        -25300 => rebook_app_core::credentials::Error::NoEntry, // errSecItemNotFound
        _ => rebook_app_core::credentials::Error::Store(error.to_string()),
    })
}
fn core_error(e: keyring::Error) -> rebook_app_core::credentials::Error {
    match e {
        keyring::Error::NoEntry => rebook_app_core::credentials::Error::NoEntry,
        e => rebook_app_core::credentials::Error::Store(e.to_string()),
    }
}
pub fn initialize_core() -> Result<(), Box<dyn std::error::Error>> {
    let secrets: std::sync::Arc<dyn rebook_app_core::credentials::SecretStore> =
        if crate::smoke::enabled() && !crate::smoke::portable() {
            std::sync::Arc::new(rebook_app_core::credentials::MemoryStore::default())
        } else {
            let config = rebook_app_core::environment::paths().ok_or("Core environment unavailable")?.config;
            // Capture this before services create default settings. A fresh
            // profile must never query a native credential store at all.
            let has_legacy = !crate::profile::is_portable() &&
                ["credentials.v1.vault", "plugins.json", "webdav-sync.json"].iter().any(|name| config.join(name).exists());
            let legacy = has_legacy.then(|| {
                std::sync::Arc::new(DesktopMigration {
                    vault: rebook_app_core::credentials::VaultStore::new(config.join("credentials.v1.vault"), std::sync::Arc::new(CoreSecrets)),
                    config: config.clone(),
                }) as std::sync::Arc<dyn rebook_app_core::credentials::LegacyCredentials>
            });
            std::sync::Arc::new(rebook_app_core::credentials::FileStore::new(config.join("credentials.json"), legacy))
        };
    rebook_app_core::credentials::install(secrets)?;
    Ok(())
}

struct DesktopMigration {
    vault: rebook_app_core::credentials::VaultStore,
    config: std::path::PathBuf,
}
impl rebook_app_core::credentials::LegacyCredentials for DesktopMigration {
    fn load(&self) -> rebook_app_core::credentials::Result<rebook_app_core::credentials::Migration> {
        use rebook_app_core::credentials::{Error, Migration, SecretStore, identity};
        let mut entries = if self.config.join("credentials.v1.vault").exists() {
            self.vault.migration_snapshot()?
        } else { std::collections::BTreeMap::new() };
        let mut accounts = if self.config.join("plugins.json").exists() {
            rebook_app_core::plugins::PluginSettings::load_config()
                .map_err(|_| Error::Store("无法读取旧凭据的设置，未覆盖原数据".into()))?.legacy_credential_accounts()
        } else { Vec::new() };
        match std::fs::read(self.config.join("webdav-sync.json")) {
            Ok(bytes) => {
                let value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| Error::Store("无法读取旧同步设置，未覆盖原数据".into()))?;
                if let Some(device) = value.pointer("/settings/device_id").and_then(|v| v.as_str()) {
                    accounts.push(("Rebook WebDAV", device.into()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Store("无法读取旧同步设置，未覆盖原数据".into())),
        }
        for (service, account) in accounts {
            if account.is_empty() { continue; }
            let id = identity(service, &account)?;
            if entries.contains_key(&id) { continue; }
            match CoreSecrets.read(service, &account) {
                Ok(value) => { entries.insert(id, Some(value)); }
                Err(Error::NoEntry) => {}
                Err(_) => return Err(Error::Store("无法迁移旧凭据，请允许系统授权后在设置中重试".into())),
            }
        }
        Ok(Migration { entries, ..Migration::default() })
    }
    fn cleanup(&self) -> rebook_app_core::credentials::Result<()> {
        // Leave the native master key for older installed copies. Reading the
        // new file never needs it; removing it could request another permission.
        match std::fs::remove_file(self.config.join("credentials.v1.vault")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(rebook_app_core::credentials::Error::Store("无法移除已迁移的旧凭据文件".into())),
        }
    }
    fn retry(&self) {
        use rebook_app_core::credentials::SecretStore;
        self.vault.retry_unlock();
    }
}
