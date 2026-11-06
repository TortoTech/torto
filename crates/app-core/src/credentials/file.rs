//! One portable JSON format for all hosts. Native stores are read only during migration.
use super::{Error, Result, SecretStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
const MAX_SECRET: usize = 128 * 1024;

/// A snapshot is imported in one transaction before any new writes. Android's
/// old preferences used irreversible account hashes; retain those only until
/// their first read/write/delete through the normal service/account API.
#[derive(Default)]
pub struct Migration {
    pub entries: BTreeMap<String, Option<String>>,
    pub android_entries: BTreeMap<String, String>,
}
pub trait LegacyCredentials: Send + Sync {
    fn load(&self) -> Result<Migration>;
    /// Called only after the complete snapshot is durably saved and read back.
    fn cleanup(&self) -> Result<()> {
        Ok(())
    }
    fn retry(&self) {}
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    version: u8,
    entries: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    android_entries: BTreeMap<String, String>,
}
pub struct FileStore {
    path: PathBuf,
    legacy: Option<Arc<dyn LegacyCredentials>>,
    access: Mutex<Option<String>>,
}
fn fail(message: &str) -> Error {
    Error::Store(message.into())
}
fn io_error(_: std::io::Error) -> Error {
    fail("无法读写本地凭据文件")
}
pub fn identity(service: &str, account: &str) -> Result<String> {
    if service.is_empty() || account.is_empty() || service.len() + account.len() > 4096 {
        return Err(fail("凭据标识无效"));
    }
    serde_json::to_string(&(service, account)).map_err(|_| fail("无法编码凭据标识"))
}
pub fn legacy_android_identity(service: &str, account: &str) -> String {
    // The old Kotlin implementation counted UTF-16 code units, not UTF-8 bytes.
    format!(
        "{:x}",
        Sha256::digest(format!(
            "{}:{service}{account}",
            service.encode_utf16().count()
        ))
    )
}
impl FileStore {
    pub fn new(path: PathBuf, legacy: Option<Arc<dyn LegacyCredentials>>) -> Self {
        Self {
            path,
            legacy,
            access: Mutex::new(None),
        }
    }
    fn transaction<T>(&self, action: impl FnOnce(&mut Payload) -> Result<(T, bool)>) -> Result<T> {
        let mut blocked = self
            .access
            .lock()
            .map_err(|_| fail("本地凭据访问锁已损坏"))?;
        if let Some(message) = blocked.as_ref() {
            return Err(fail(message));
        }
        let parent = self.path.parent().ok_or_else(|| fail("凭据文件路径无效"))?;
        fs::create_dir_all(parent).map_err(io_error)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(self.path.with_extension("lock"))
            .map_err(io_error)?;
        #[cfg(target_os = "android")]
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
            |error| {
                if error == rustix::io::Errno::WOULDBLOCK {
                    fail("另一 Torto 实例正在访问凭据，请稍后重试")
                } else {
                    fail("本地凭据文件锁不可用")
                }
            },
        )?;
        #[cfg(not(target_os = "android"))]
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => fail("另一 Torto 实例正在访问凭据，请稍后重试"),
            std::fs::TryLockError::Error(_) => fail("本地凭据文件锁不可用"),
        })?;
        let existing = self.read_bytes()?;
        let mut payload = match existing {
            Some(bytes) => serde_json::from_slice::<Payload>(&bytes)
                .map_err(|_| fail("本地凭据文件已损坏，未覆盖原文件"))?,
            None => {
                let migration = match self.legacy.as_ref().map(|source| source.load()).transpose() {
                    Ok(migration) => migration.unwrap_or_default(),
                    Err(error) => {
                        let message = match error {
                            Error::Store(message) => message,
                            Error::NoEntry => "旧凭据迁移未完成".into(),
                        };
                        *blocked = Some(message.clone());
                        return Err(fail(&message));
                    }
                };
                let payload = Payload {
                    version: 1,
                    entries: migration.entries,
                    android_entries: migration.android_entries,
                };
                self.save(&payload)?;
                if let Some(source) = &self.legacy {
                    if source.cleanup().is_err() {
                        tracing::warn!(
                            "Legacy credential cleanup failed after verified local migration"
                        );
                    }
                }
                payload
            }
        };
        Self::validate(&payload)?;
        let (result, changed) = action(&mut payload)?;
        if changed {
            self.save(&payload)?;
        }
        Ok(result)
    }
    fn read_bytes(&self) -> Result<Option<Vec<u8>>> {
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(error)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > MAX_BYTES {
            return Err(fail("本地凭据文件过大"));
        }
        Ok(Some(bytes))
    }
    fn validate(payload: &Payload) -> Result<()> {
        if payload.version != 1
            || payload.entries.len() + payload.android_entries.len() > MAX_ENTRIES
        {
            return Err(fail("不支持的本地凭据格式或条目过多"));
        }
        if payload
            .entries
            .values()
            .filter_map(Option::as_ref)
            .chain(payload.android_entries.values())
            .any(|value| value.len() > MAX_SECRET)
        {
            return Err(fail("凭据内容过长"));
        }
        if payload
            .android_entries
            .keys()
            .any(|id| id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(fail("旧凭据标识无效"));
        }
        Ok(())
    }
    fn save(&self, payload: &Payload) -> Result<()> {
        Self::validate(payload)?;
        let bytes = serde_json::to_vec_pretty(payload).map_err(|_| fail("无法编码本地凭据"))?;
        if bytes.len() > MAX_BYTES {
            return Err(fail("本地凭据文件过大"));
        }
        // Create the atomic temporary file with restricted permissions before
        // writing secrets; never briefly expose it under the default umask.
        atomicwrites::AtomicFile::new(&self.path, atomicwrites::AllowOverwrite)
            .write(|file| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(fs::Permissions::from_mode(0o600))?;
                }
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .map_err(|_| fail("无法保存本地凭据文件"))?;
        if self.read_bytes()?.as_deref() != Some(bytes.as_slice()) {
            return Err(fail("本地凭据写入校验失败"));
        }
        Ok(())
    }
}
impl SecretStore for FileStore {
    fn read(&self, service: &str, account: &str) -> Result<String> {
        let id = identity(service, account)?;
        let old_id = legacy_android_identity(service, account);
        self.transaction(|payload| {
            if let Some(value) = payload.entries.get(&id) {
                return Ok((value.clone(), false));
            }
            if let Some(value) = payload.android_entries.remove(&old_id) {
                payload.entries.insert(id, Some(value.clone()));
                return Ok((Some(value), true));
            }
            Ok((None, false))
        })?
        .ok_or(Error::NoEntry)
    }
    fn write(&self, service: &str, account: &str, value: &str) -> Result<()> {
        if value.len() > MAX_SECRET {
            return Err(fail("凭据内容过长"));
        }
        let id = identity(service, account)?;
        self.transaction(|payload| {
            let old = payload
                .android_entries
                .remove(&legacy_android_identity(service, account));
            let changed =
                old.is_some() || payload.entries.get(&id).and_then(Option::as_deref) != Some(value);
            payload.entries.insert(id, Some(value.into()));
            Ok(((), changed))
        })
    }
    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let id = identity(service, account)?;
        self.transaction(|payload| {
            let old = payload
                .android_entries
                .remove(&legacy_android_identity(service, account));
            let changed = payload.entries.remove(&id).is_some() || old.is_some();
            Ok(((), changed))
        })
    }
    fn retry_unlock(&self) {
        if let Ok(mut blocked) = self.access.lock() {
            *blocked = None;
        }
        if let Some(source) = &self.legacy {
            source.retry();
        }
    }
}

#[cfg(test)]
mod tests;
