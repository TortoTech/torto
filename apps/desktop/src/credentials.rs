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
