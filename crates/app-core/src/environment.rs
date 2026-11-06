//! One application environment per process. Hosts supply paths before starting workers.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    pub data: PathBuf,
    pub roaming: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}
impl AppPaths {
    pub fn data_dir(&self) -> &Path {
        &self.roaming
    }
    pub fn data_local_dir(&self) -> &Path {
        &self.data
    }
    pub fn config_dir(&self) -> &Path {
        &self.config
    }
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }
}
static PATHS: OnceLock<AppPaths> = OnceLock::new();
static PORTABLE_MACHINE: OnceLock<String> = OnceLock::new();
/// A host-supplied opaque machine/user hash. Never persisted in cloud payloads.
pub fn initialize_portable_machine(scope: String) -> Result<(), &'static str> {
    if scope.len() != 64 || !scope.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err("Invalid portable machine scope"); }
    let current = PORTABLE_MACHINE.get_or_init(|| scope.clone());
    if current == &scope { Ok(()) } else { Err("Portable machine already configured") }
}
pub fn portable_machine() -> Option<&'static str> { PORTABLE_MACHINE.get().map(String::as_str) }
pub fn initialize(paths: AppPaths) -> Result<(), &'static str> {
    let current = PATHS.get_or_init(|| paths.clone());
    if current == &paths {
        Ok(())
    } else {
        Err("Core already initialized for a different profile")
    }
}
pub fn paths() -> Option<AppPaths> {
    if let Some(paths) = PATHS.get() {
        return Some(paths.clone());
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        let root = std::env::temp_dir().join(format!("torto-core-tests-{}", std::process::id()));
        return Some(AppPaths {
            data: root.join("data"),
            roaming: root.join("data"),
            config: root.join("config"),
            cache: root.join("cache"),
        });
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        directories::ProjectDirs::from("com", "Rebook", "Rebook").map(|dirs| AppPaths {
            data: dirs.data_local_dir().to_owned(),
            roaming: dirs.data_dir().to_owned(),
            config: dirs.config_dir().to_owned(),
            cache: dirs.cache_dir().to_owned(),
        })
    }
}
