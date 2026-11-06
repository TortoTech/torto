use super::*;
use crate::credentials::{MemoryStore, VaultStore};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("torto-credentials-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self) -> PathBuf {
        self.0.join("config/credentials.json")
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Legacy {
    loads: AtomicUsize,
    cleanups: AtomicUsize,
    denied: bool,
}
impl LegacyCredentials for Legacy {
    fn load(&self) -> Result<Migration> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        if self.denied {
            return Err(fail("legacy unavailable"));
        }
        Ok(Migration {
            entries: BTreeMap::from([
                (identity("AI", "one")?, Some("old-key".into())),
                (identity("WebDAV", "device")?, Some("cloud-password".into())),
            ]),
            android_entries: BTreeMap::from([(
                legacy_android_identity("旧😀", "second"),
                "android-key".into(),
            )]),
        })
    }
    fn cleanup(&self) -> Result<()> {
        self.cleanups.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[test]
fn imports_complete_snapshot_before_first_write_and_never_resurrects_deleted_keys() {
    let dir = Directory::new();
    let legacy = Arc::new(Legacy {
        loads: AtomicUsize::new(0),
        cleanups: AtomicUsize::new(0),
        denied: false,
    });
    let store = FileStore::new(dir.file(), Some(legacy.clone()));
    store.write("AI", "one", "replacement").unwrap();
    assert_eq!(store.read("WebDAV", "device").unwrap(), "cloud-password");
    assert_eq!(store.read("旧😀", "second").unwrap(), "android-key");
    store.delete("AI", "one").unwrap();
    let reopened = FileStore::new(dir.file(), Some(legacy.clone()));
    assert!(matches!(reopened.read("AI", "one"), Err(Error::NoEntry)));
    assert_eq!(legacy.loads.load(Ordering::SeqCst), 1);
    assert_eq!(legacy.cleanups.load(Ordering::SeqCst), 1);
    assert!(
        String::from_utf8(fs::read(dir.file()).unwrap())
            .unwrap()
            .contains("cloud-password")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.file()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn unavailable_legacy_storage_never_creates_a_replacement_or_runs_cleanup() {
    let dir = Directory::new();
    let legacy = Arc::new(Legacy {
        loads: AtomicUsize::new(0),
        cleanups: AtomicUsize::new(0),
        denied: true,
    });
    let store = FileStore::new(dir.file(), Some(legacy.clone()));
    assert!(store.write("AI", "one", "new").is_err());
    assert!(store.read("AI", "one").is_err());
    assert_eq!(
        legacy.loads.load(Ordering::SeqCst),
        1,
        "denied migration must not repeatedly reopen the native store"
    );
    store.retry_unlock();
    assert!(store.read("AI", "one").is_err());
    assert_eq!(legacy.loads.load(Ordering::SeqCst), 2);
    assert!(!dir.file().exists());
    assert_eq!(legacy.cleanups.load(Ordering::SeqCst), 0);
}
#[test]
fn encrypted_desktop_snapshot_preserves_all_services_and_tombstones() {
    let dir = Directory::new();
    let native = Arc::new(MemoryStore::default());
    let vault = VaultStore::new(dir.0.join("old.vault"), native);
    vault.write("AI", "provider", "api-value").unwrap();
    vault.write("WebDAV", "device", "password-value").unwrap();
    vault.delete("AI", "removed").unwrap();
    struct Import(VaultStore);
    impl LegacyCredentials for Import {
        fn load(&self) -> Result<Migration> {
            Ok(Migration {
                entries: self.0.migration_snapshot()?,
                ..Default::default()
            })
        }
    }
    let file = FileStore::new(dir.file(), Some(Arc::new(Import(vault))));
    assert_eq!(file.read("AI", "provider").unwrap(), "api-value");
    assert_eq!(file.read("WebDAV", "device").unwrap(), "password-value");
    assert!(matches!(file.read("AI", "removed"), Err(Error::NoEntry)));
}
#[test]
fn corrupt_or_future_files_are_not_overwritten_and_errors_do_not_echo_secrets() {
    let dir = Directory::new();
    fs::create_dir_all(dir.file().parent().unwrap()).unwrap();
    let store = FileStore::new(dir.file(), None);
    for bytes in [
        b"{secret-value".as_slice(),
        br#"{"version":99,"entries":{}}"#,
    ] {
        fs::write(dir.file(), bytes).unwrap();
        let error = store
            .write("AI", "key", "secret-value")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret-value"));
        assert_eq!(fs::read(dir.file()).unwrap(), bytes);
    }
}
#[test]
fn copied_profiles_need_no_os_key_and_parallel_writers_keep_every_account() {
    let dir = Directory::new();
    let file = Arc::new(FileStore::new(dir.file(), None));
    let threads: Vec<_> = (0..8)
        .map(|worker| {
            let file = file.clone();
            std::thread::spawn(move || {
                for index in 0..10 {
                    file.write("AI", &format!("{worker}-{index}"), "value")
                        .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let other = Directory::new();
    fs::create_dir_all(other.file().parent().unwrap()).unwrap();
    fs::copy(dir.file(), other.file()).unwrap();
    let moved = FileStore::new(other.file(), None);
    for worker in 0..8 {
        for index in 0..10 {
            assert_eq!(
                moved.read("AI", &format!("{worker}-{index}")).unwrap(),
                "value"
            );
        }
    }
    assert!(matches!(
        moved.read("other-service", "0-0"),
        Err(Error::NoEntry)
    ));
}
