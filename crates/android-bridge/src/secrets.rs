use crate::{Result, error};
use rebook_app_core::credentials;
use std::sync::Arc;

/// Implemented by the Android host with Android Keystore. Called only from
/// service workers; the callback must not retain an Activity or a View.
#[uniffi::export(with_foreign)]
pub trait SecureStorage: Send + Sync {
    fn read(&self, service: String, account: String) -> Result<Option<String>>;
    fn write(&self, service: String, account: String, value: String) -> Result<()>;
    fn delete(&self, service: String, account: String) -> Result<()>;
}
struct Adapter(Arc<dyn SecureStorage>);
impl credentials::SecretStore for Adapter {
    fn read(&self, s: &str, a: &str) -> credentials::Result<String> {
        self.0
            .read(s.into(), a.into())
            .map_err(|e| credentials::Error::Store(e.to_string()))?
            .ok_or(credentials::Error::NoEntry)
    }
    fn write(&self, s: &str, a: &str, v: &str) -> credentials::Result<()> {
        self.0
            .write(s.into(), a.into(), v.into())
            .map_err(|e| credentials::Error::Store(e.to_string()))
    }
    fn delete(&self, s: &str, a: &str) -> credentials::Result<()> {
        self.0
            .delete(s.into(), a.into())
            .map_err(|e| credentials::Error::Store(e.to_string()))
    }
}
#[uniffi::export]
pub fn initialize_secure_storage(storage: Arc<dyn SecureStorage>) -> Result<()> {
    credentials::install(Arc::new(Adapter(storage))).map_err(error)
}
