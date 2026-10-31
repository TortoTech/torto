use super::*;
use crate::credentials::MemoryStore;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Default)]
struct Native {
    inner: MemoryStore,
    reads: AtomicUsize,
    writes: AtomicUsize,
    deletes: AtomicUsize,
    deny: AtomicBool,
    fail_save: Mutex<Option<PathBuf>>,
}
impl SecretStore for Native {
    fn read(&self, service: &str, account: &str) -> Result<String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.deny.load(Ordering::SeqCst) {
            return Err(fail("denied"));
        }
        if service == "legacy"
            && let Some(path) = self.fail_save.lock().unwrap().take()
        {
            fs::create_dir(path).unwrap();
        }
        self.inner.read(service, account)
    }
    fn write(&self, s: &str, a: &str, value: &str) -> Result<()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.write(s, a, value)
    }
    fn delete(&self, s: &str, a: &str) -> Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(s, a)
    }
}
struct Fixture {
    root: PathBuf,
    native: Arc<Native>,
    vault: VaultStore,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("torto-vault-{}", uuid::Uuid::new_v4()));
        let native = Arc::new(Native::default());
        let vault = VaultStore::new(root.join("credentials.vault"), native.clone());
        Self {
            root,
            native,
            vault,
        }
    }
    fn reopen(&self) -> VaultStore {
        VaultStore::new(self.vault.path.clone(), self.native.clone())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn migration_is_durable_encrypted_and_only_master_remains_in_native_store() {
    let f = Fixture::new();
    for (account, secret) in [
        ("provider", "api-secret-中文"),
        ("webdav", "password-secret"),
    ] {
        f.native.inner.write("legacy", account, secret).unwrap();
        assert_eq!(f.vault.read("legacy", account).unwrap(), secret);
        assert!(matches!(
            f.native.inner.read("legacy", account),
            Err(Error::NoEntry)
        ));
        assert_eq!(f.reopen().read("legacy", account).unwrap(), secret);
        let bytes = fs::read(&f.vault.path).unwrap();
        assert!(!bytes.windows(secret.len()).any(|b| b == secret.as_bytes()));
    }
    assert_eq!(
        STANDARD
            .decode(f.native.inner.read(SERVICE, ACCOUNT).unwrap())
            .unwrap()
            .len(),
        32
    );
    let reads = f.native.reads.load(Ordering::SeqCst);
    let writes = f.native.writes.load(Ordering::SeqCst);
    for _ in 0..10 {
        assert_eq!(
            f.vault.read("legacy", "provider").unwrap(),
            "api-secret-中文"
        );
        f.vault
            .write("legacy", "provider", "api-secret-中文")
            .unwrap();
    }
    assert_eq!(f.native.reads.load(Ordering::SeqCst), reads);
    assert_eq!(f.native.writes.load(Ordering::SeqCst), writes);
}
#[test]
fn deleted_and_absent_entries_never_fall_back_to_legacy_again() {
    let f = Fixture::new();
    assert!(matches!(
        f.vault.read("legacy", "missing"),
        Err(Error::NoEntry)
    ));
    let reads = f.native.reads.load(Ordering::SeqCst);
    assert!(matches!(
        f.vault.read("legacy", "missing"),
        Err(Error::NoEntry)
    ));
    assert_eq!(f.native.reads.load(Ordering::SeqCst), reads);
    f.vault.write("legacy", "provider", "new").unwrap();
    f.vault.delete("legacy", "provider").unwrap();
    f.native.inner.write("legacy", "provider", "stale").unwrap();
    assert!(matches!(
        f.reopen().read("legacy", "provider"),
        Err(Error::NoEntry)
    ));
    // Tuple encoding cannot confuse a service/account boundary.
    f.vault.write("ab", "c", "one").unwrap();
    f.vault.write("a", "bc", "two").unwrap();
    assert_eq!(f.vault.read("ab", "c").unwrap(), "one");
}
#[test]
fn independent_instances_merge_changes_and_nonce_changes_each_time() {
    let f = Fixture::new();
    f.vault.write("a", "x", "first").unwrap();
    let before = fs::read(&f.vault.path).unwrap();
    let other = f.reopen();
    other.write("a", "y", "second").unwrap();
    f.vault.write("a", "x", "third").unwrap();
    f.vault.write("a", "x", "first").unwrap();
    let after = fs::read(&f.vault.path).unwrap();
    assert_ne!(
        &before[HEADER.len()..HEADER.len() + 12],
        &after[HEADER.len()..HEADER.len() + 12]
    );
    assert_eq!(other.read("a", "x").unwrap(), "first");
    assert_eq!(f.vault.read("a", "y").unwrap(), "second");
}
#[test]
fn corruption_or_missing_master_never_overwrites_files_or_imports_legacy() {
    let f = Fixture::new();
    f.vault.write("a", "x", "first").unwrap();
    let mut bytes = fs::read(&f.vault.path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&f.vault.path, &bytes).unwrap();
    let reads = f.native.reads.load(Ordering::SeqCst);
    assert!(f.reopen().read("legacy", "provider").is_err());
    assert_eq!(f.native.reads.load(Ordering::SeqCst), reads + 1); // master only
    assert_eq!(fs::read(&f.vault.path).unwrap(), bytes);
    f.native.inner.delete(SERVICE, ACCOUNT).unwrap();
    assert!(f.reopen().write("a", "x", "replacement").is_err());
    assert_eq!(fs::read(&f.vault.path).unwrap(), bytes);
    assert!(matches!(
        f.native.inner.read(SERVICE, ACCOUNT),
        Err(Error::NoEntry)
    ));
}
#[test]
fn failed_migration_keeps_old_secret_and_denial_needs_explicit_retry() {
    let f = Fixture::new();
    f.native
        .inner
        .write("legacy", "provider", "original")
        .unwrap();
    *f.native.fail_save.lock().unwrap() = Some(f.vault.path.clone());
    assert!(f.vault.read("legacy", "provider").is_err());
    assert_eq!(
        f.native.inner.read("legacy", "provider").unwrap(),
        "original"
    );
    assert_eq!(f.native.deletes.load(Ordering::SeqCst), 0);
    let f = Fixture::new();
    f.native.deny.store(true, Ordering::SeqCst);
    assert!(f.vault.read("a", "x").is_err());
    let reads = f.native.reads.load(Ordering::SeqCst);
    for _ in 0..10 {
        assert!(f.vault.read("other", "key").is_err());
    }
    assert_eq!(f.native.reads.load(Ordering::SeqCst), reads);
    assert!(f.vault.write("other", "key", "value").is_err());
    f.native.deny.store(false, Ordering::SeqCst);
    f.vault.retry_unlock();
    f.vault.write("other", "key", "value").unwrap();
    assert_eq!(f.vault.read("other", "key").unwrap(), "value");
}
#[test]
fn long_tokens_bypass_windows_blob_limit_and_file_lock_never_waits() {
    let f = Fixture::new();
    let token = "long-token".repeat(1000);
    f.vault.write("provider", "key", &token).unwrap();
    assert_eq!(f.reopen().read("provider", "key").unwrap(), token);
    // Native storage holds only a 44-character base64 master key.
    assert_eq!(f.native.inner.read(SERVICE, ACCOUNT).unwrap().len(), 44);
    let lock = File::options()
        .read(true)
        .write(true)
        .open(f.vault.path.with_extension("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(f.reopen().write("provider", "key", "blocked").is_err());
    drop(lock);
    assert_eq!(f.vault.read("provider", "key").unwrap(), token);
}

#[test]
fn empty_profiles_do_not_create_master_keys_or_vault_files() {
    let f = Fixture::new();
    assert_eq!(f.native.reads.load(Ordering::SeqCst), 0);
    assert!(matches!(
        f.vault.read("provider", "key"),
        Err(Error::NoEntry)
    ));
    assert!(matches!(
        f.vault.delete("provider", "key"),
        Err(Error::NoEntry)
    ));
    assert!(!f.vault.path.exists());
    assert_eq!(f.native.writes.load(Ordering::SeqCst), 0);
    assert!(matches!(
        f.native.inner.read(SERVICE, ACCOUNT),
        Err(Error::NoEntry)
    ));
}

#[test]
fn negative_cache_is_bounded_before_any_master_key_is_created() {
    let f = Fixture::new();
    for index in 0..MAX_ENTRIES {
        assert!(matches!(
            f.vault.read("missing", &index.to_string()),
            Err(Error::NoEntry)
        ));
    }
    assert!(matches!(
        f.vault.read("missing", "overflow"),
        Err(Error::Store(_))
    ));
    assert_eq!(
        f.vault.state.lock().unwrap().payload.entries.len(),
        MAX_ENTRIES
    );
    assert_eq!(f.native.writes.load(Ordering::SeqCst), 0);
}
