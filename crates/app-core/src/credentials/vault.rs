//! One OS-protected master key; an authenticated, atomically replaced local vault.
//! No native storage is accessed until the first credential operation.
use super::{Error, Result, SecretStore};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const SERVICE: &str = "Torto Credential Vault";
const ACCOUNT: &str = "desktop-master-v1";
const HEADER: &[u8] = b"TORTOVLT\x01";
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
const MAX_SECRET: usize = 128 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Payload {
    // None records absence/deletion, preventing an old item from resurrecting.
    entries: BTreeMap<String, Option<String>>,
}
#[derive(Default)]
struct State {
    key: Option<aead::LessSafeKey>,
    digest: Option<blake3::Hash>,
    payload: Payload,
    blocked: Option<String>,
}
pub struct VaultStore {
    path: PathBuf,
    native: Arc<dyn SecretStore>,
    state: Mutex<State>,
}
fn fail(message: &str) -> Error {
    Error::Store(message.into())
}
fn io_error(_: std::io::Error) -> Error {
    fail("无法读写安全凭据库")
}
fn identity(service: &str, account: &str) -> Result<String> {
    if service.len() + account.len() > 4096 {
        return Err(fail("凭据标识过长"));
    }
    serde_json::to_string(&(service, account)).map_err(|_| fail("无法编码凭据标识"))
}
impl VaultStore {
    pub fn new(path: PathBuf, native: Arc<dyn SecretStore>) -> Self {
        Self {
            path,
            native,
            state: Mutex::new(State::default()),
        }
    }
    fn transaction<T>(&self, action: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut state = self.state.lock().map_err(|_| fail("安全存储状态不可用"))?;
        if let Some(error) = &state.blocked {
            return Err(Error::Store(error.clone()));
        }
        let result = (|| {
            let parent = self
                .path
                .parent()
                .ok_or_else(|| fail("安全凭据库路径无效"))?;
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
            // Never wait for another process (which might be showing a keychain dialog).
            lock.try_lock()
                .map_err(|_| fail("另一 Torto 实例正在访问安全存储，请稍后重试"))?;
            self.load(&mut state)?;
            action(&mut state)
        })();
        // Denials and corrupt storage are not retried by background/settings reloads.
        // A deliberate retry clears this latch; failures never become missing keys.
        if let Err(Error::Store(message)) = &result {
            state.blocked = Some(message.clone());
        }
        result
    }
    fn read_bytes(&self) -> Result<Option<Vec<u8>>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(e)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > MAX_BYTES {
            return Err(fail("安全凭据库超过容量限制"));
        }
        Ok(Some(bytes))
    }
    fn ensure_key(&self, state: &mut State, create: bool) -> Result<()> {
        if state.key.is_none() {
            let encoded = match self.native.read(SERVICE, ACCOUNT) {
                Ok(encoded) => encoded,
                Err(Error::NoEntry) if create => {
                    let mut key = [0u8; 32];
                    SystemRandom::new()
                        .fill(&mut key)
                        .map_err(|_| fail("无法生成安全存储密钥"))?;
                    let encoded = STANDARD.encode(key);
                    self.native.write(SERVICE, ACCOUNT, &encoded)?;
                    // Verify durability before creating a vault or removing legacy data.
                    if self.native.read(SERVICE, ACCOUNT)? != encoded {
                        return Err(fail("安全存储密钥校验失败"));
                    }
                    encoded
                }
                Err(Error::NoEntry) => {
                    return Err(fail("凭据库主密钥丢失，请恢复原系统凭据；未覆盖现有数据"));
                }
                Err(_) => return Err(fail("无法解锁系统安全存储，请在设置中重试")),
            };
            let key = STANDARD
                .decode(encoded)
                .map_err(|_| fail("安全存储密钥格式无效"))?;
            if key.len() != 32 {
                return Err(fail("安全存储密钥长度无效"));
            }
            state.key = Some(aead::LessSafeKey::new(
                aead::UnboundKey::new(&aead::AES_256_GCM, &key)
                    .map_err(|_| fail("无法初始化安全存储"))?,
            ));
        }
        Ok(())
    }
    fn load(&self, state: &mut State) -> Result<()> {
        let bytes = self.read_bytes()?;
        if bytes.is_none() && state.digest.is_none() {
            return Ok(());
        }
        self.ensure_key(state, false)?;
        let digest = bytes.as_ref().map(|v| blake3::hash(v));
        if digest == state.digest {
            return Ok(());
        }
        let bytes = bytes.ok_or_else(|| fail("安全凭据库已被移除，请恢复文件或重启应用"))?;
        if bytes.len() < HEADER.len() + 12 + aead::AES_256_GCM.tag_len()
            || !bytes.starts_with(HEADER)
        {
            return Err(fail("安全凭据库格式无效；未覆盖现有数据"));
        }
        let nonce: [u8; 12] = bytes[HEADER.len()..HEADER.len() + 12].try_into().unwrap();
        let mut encrypted = bytes[HEADER.len() + 12..].to_vec();
        let plaintext = state
            .key
            .as_ref()
            .unwrap()
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(HEADER),
                &mut encrypted,
            )
            .map_err(|_| fail("安全凭据库完整性校验失败；未覆盖现有数据"))?;
        let payload: Payload =
            serde_json::from_slice(plaintext).map_err(|_| fail("安全凭据库内容无效"))?;
        if payload.entries.len() > MAX_ENTRIES {
            return Err(fail("安全凭据条目过多"));
        }
        state.payload = payload;
        state.digest = digest;
        Ok(())
    }
    fn save(&self, state: &mut State, payload: Payload) -> Result<()> {
        if payload.entries.len() > MAX_ENTRIES {
            return Err(fail("安全凭据条目过多"));
        }
        self.ensure_key(state, self.read_bytes()?.is_none())?;
        let mut encrypted = serde_json::to_vec(&payload).map_err(|_| fail("无法编码安全凭据"))?;
        if encrypted.len() + HEADER.len() + 12 + aead::AES_256_GCM.tag_len() > MAX_BYTES {
            return Err(fail("安全凭据库超过容量限制"));
        }
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| fail("无法生成安全存储随机数"))?;
        state
            .key
            .as_ref()
            .unwrap()
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(HEADER),
                &mut encrypted,
            )
            .map_err(|_| fail("无法加密安全凭据"))?;
        let bytes = [HEADER, &nonce, &encrypted].concat();
        crate::persistence::write_bytes_atomic(&self.path, &bytes).map_err(io_error)?;
        if self.read_bytes()?.as_deref() != Some(bytes.as_slice()) {
            return Err(fail("安全凭据写入校验失败"));
        }
        state.digest = Some(blake3::hash(&bytes));
        state.payload = payload;
        Ok(())
    }
    fn cleanup(&self, service: &str, account: &str) {
        // A failed cleanup does not invalidate a verified migration; the vault entry
        // (including tombstones) is authoritative from this point onward.
        if let Err(Error::Store(_)) = self.native.delete(service, account) {
            tracing::warn!("legacy credential cleanup failed after verified vault write");
        }
    }
}
impl SecretStore for VaultStore {
    fn read(&self, service: &str, account: &str) -> Result<String> {
        let id = identity(service, account)?;
        self.transaction(|state| {
            if let Some(value) = state.payload.entries.get(&id) {
                return value.clone().ok_or(Error::NoEntry);
            }
            let value = match self.native.read(service, account) {
                Ok(value) => Some(value),
                Err(Error::NoEntry) => None,
                Err(_) => return Err(fail("无法读取旧凭据，请在设置中重试")),
            };
            let mut payload = state.payload.clone();
            payload.entries.insert(id, value.clone());
            if value.is_some() || state.digest.is_some() {
                self.save(state, payload)?;
            } else {
                // No credential exists yet: cache the miss without creating a
                // master key or prompting users who only read local books.
                if payload.entries.len() > MAX_ENTRIES {
                    return Err(fail("安全凭据条目过多"));
                }
                state.payload = payload;
            }
            if value.is_some() {
                self.cleanup(service, account);
            }
            value.ok_or(Error::NoEntry)
        })
    }
    fn write(&self, service: &str, account: &str, value: &str) -> Result<()> {
        if value.len() > MAX_SECRET {
            return Err(fail("凭据内容过长"));
        }
        let id = identity(service, account)?;
        self.transaction(|state| {
            if state
                .payload
                .entries
                .get(&id)
                .is_some_and(|v| v.as_deref() == Some(value))
            {
                return Ok(());
            }
            let mut payload = state.payload.clone();
            payload.entries.insert(id, Some(value.into()));
            self.save(state, payload)?;
            self.cleanup(service, account);
            Ok(())
        })
    }
    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let id = identity(service, account)?;
        self.transaction(|state| {
            if state.payload.entries.get(&id) == Some(&None) {
                return Err(Error::NoEntry);
            }
            let mut payload = state.payload.clone();
            payload.entries.insert(id, None);
            self.save(state, payload)?;
            self.cleanup(service, account);
            Ok(())
        })
    }
    fn retry_unlock(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.blocked = None;
        }
    }
}

#[cfg(test)]
mod tests;
