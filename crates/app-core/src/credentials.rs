//! Platform credential storage. The core never writes secrets into its settings files.
use std::sync::{Arc, OnceLock};
mod vault;
pub use vault::VaultStore;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Credential not found")]
    NoEntry,
    #[error("{0}")]
    Store(String),
}
pub type Result<T> = std::result::Result<T, Error>;
pub trait SecretStore: Send + Sync {
    fn read(&self, service: &str, account: &str) -> Result<String>;
    fn write(&self, service: &str, account: &str, value: &str) -> Result<()>;
    fn delete(&self, service: &str, account: &str) -> Result<()>;
    /// Only called after an explicit user retry.
    fn retry_unlock(&self) {}
}
static STORE: OnceLock<Arc<dyn SecretStore>> = OnceLock::new();
pub fn install(store: Arc<dyn SecretStore>) -> Result<()> {
    STORE
        .set(store)
        .map_err(|_| Error::Store("Credential store already configured".into()))
}
pub fn retry_unlock() -> Result<()> {
    store()?.retry_unlock();
    Ok(())
}
pub struct Entry {
    service: String,
    account: String,
}
pub fn entry(service: &str, account: &str) -> Result<Entry> {
    Ok(Entry {
        service: service.into(),
        account: account.into(),
    })
}
impl Entry {
    pub fn get_password(&self) -> Result<String> {
        store()?.read(&self.service, &self.account)
    }
    pub fn set_password(&self, value: &str) -> Result<()> {
        store()?.write(&self.service, &self.account, value)
    }
    pub fn delete_credential(&self) -> Result<()> {
        store()?.delete(&self.service, &self.account)
    }
}
fn store() -> Result<&'static Arc<dyn SecretStore>> {
    #[cfg(any(test, feature = "test-support"))]
    {
        Ok(STORE.get_or_init(|| Arc::new(MemoryStore::default())))
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        STORE.get().ok_or_else(|| {
            Error::Store("Platform credential store has not been initialized".into())
        })
    }
}
/// Ephemeral credentials for explicit isolated smoke/test profiles.
/// Production hosts must install their platform store instead.
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<std::collections::HashMap<(String, String), String>>);
impl SecretStore for MemoryStore {
    fn read(&self, s: &str, a: &str) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(&(s.into(), a.into()))
            .cloned()
            .ok_or(Error::NoEntry)
    }
    fn write(&self, s: &str, a: &str, v: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert((s.into(), a.into()), v.into());
        Ok(())
    }
    fn delete(&self, s: &str, a: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .remove(&(s.into(), a.into()))
            .map(|_| ())
            .ok_or(Error::NoEntry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ephemeral_credentials_are_isolated_and_not_shared_by_default() {
        let a = MemoryStore::default();
        let b = MemoryStore::default();
        a.write("service", "account", "secret").unwrap();
        assert_eq!(a.read("service", "account").unwrap(), "secret");
        assert!(matches!(b.read("service", "account"), Err(Error::NoEntry)));
        a.delete("service", "account").unwrap();
        assert!(matches!(a.read("service", "account"), Err(Error::NoEntry)));
    }
}
