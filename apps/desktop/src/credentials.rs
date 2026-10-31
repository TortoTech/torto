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
        entry(s, a)
            .and_then(|e| e.delete_credential())
            .map_err(core_error)
    }
}
fn core_error(e: keyring::Error) -> rebook_app_core::credentials::Error {
    match e {
        keyring::Error::NoEntry => rebook_app_core::credentials::Error::NoEntry,
        e => rebook_app_core::credentials::Error::Store(e.to_string()),
    }
}
pub fn initialize_core() -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dirs) = crate::smoke::project_dirs() {
        rebook_app_core::environment::initialize(rebook_app_core::environment::AppPaths {
            data: dirs.data_local_dir().into(),
            roaming: dirs.data_dir().into(),
            config: dirs.config_dir().into(),
            cache: dirs.cache_dir().into(),
        })?;
    }
    let secrets: std::sync::Arc<dyn rebook_app_core::credentials::SecretStore> =
        if crate::smoke::enabled() {
            std::sync::Arc::new(rebook_app_core::credentials::MemoryStore::default())
        } else {
            std::sync::Arc::new(rebook_app_core::credentials::VaultStore::new(
                rebook_app_core::environment::paths()
                    .ok_or("Core environment unavailable")?
                    .config
                    .join("credentials.v1.vault"),
                std::sync::Arc::new(CoreSecrets),
            ))
        };
    rebook_app_core::credentials::install(secrets)?;
    Ok(())
}
