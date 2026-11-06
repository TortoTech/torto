//! Select the deployment before any settings, credentials, fonts or workers.
use rebook_app_core::environment;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
use rebook_app_core::environment::AppPaths;
#[cfg(any(target_os = "windows", test))]
use serde::Deserialize;
use std::{
    fs::File,
    sync::atomic::{AtomicBool, Ordering},
};
#[cfg(any(target_os = "windows", target_os = "macos", test))]
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

#[cfg(any(target_os = "windows", test))]
pub(crate) const MARKER: &str = "torto-portable.json";
#[cfg(any(target_os = "macos", test))]
pub(crate) const MACOS_DATA: &str = "torto-portable-data";
static PORTABLE: AtomicBool = AtomicBool::new(false);
static CONFIGURED: AtomicBool = AtomicBool::new(false);
#[cfg(any(target_os = "windows", test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u8,
}

pub(crate) struct Profile {
    // Holding this descriptor, rather than deleting a PID file, also survives
    // crashes and prevents two copies from using the same data directory.
    _lock: Option<File>,
}
pub(crate) fn is_portable() -> bool {
    PORTABLE.load(Ordering::Relaxed)
}
pub(crate) fn configured() -> bool {
    CONFIGURED.load(Ordering::Relaxed)
}

pub(crate) fn initialize() -> Result<Profile, Box<dyn std::error::Error>> {
    if crate::smoke::enabled() && !crate::smoke::portable() {
        environment::initialize(crate::smoke::project_dirs().ok_or("Smoke profile unavailable")?)?;
        CONFIGURED.store(true, Ordering::Relaxed);
        return Ok(Profile { _lock: None });
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let executable = std::env::current_exe()?;
        #[cfg(target_os = "windows")]
        let portable = open_portable(&executable)?;
        #[cfg(target_os = "macos")]
        let portable = open_macos_portable(&executable)?;
        if let Some((paths, profile)) = portable {
            crate::smoke::validate_portable_directory(
                paths.data.parent().ok_or("Missing portable directory")?,
            )?;
            environment::initialize(paths)?;
            environment::initialize_portable_machine(machine_scope()?)?;
            PORTABLE.store(true, Ordering::Relaxed);
            CONFIGURED.store(true, Ordering::Relaxed);
            return Ok(profile);
        }
    }
    if crate::smoke::portable() {
        return Err("Portable fixture data directory is missing".into());
    }
    let paths = environment::paths().ok_or("Application profile unavailable")?;
    environment::initialize(paths)?;
    CONFIGURED.store(true, Ordering::Relaxed);
    Ok(Profile { _lock: None })
}

#[cfg(target_os = "windows")]
fn machine_scope() -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::os::windows::process::CommandExt;
    let windows = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::other("Windows directory unavailable"))?;
    let output =
        std::process::Command::new(std::path::PathBuf::from(windows).join("System32/reg.exe"))
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
                "/reg:64",
            ])
            .creation_flags(0x0800_0000)
            .output()?;
    let value = String::from_utf8_lossy(&output.stdout);
    let machine = value
        .lines()
        .find(|line| line.contains("REG_SZ"))
        .and_then(|line| line.split_whitespace().last())
        .filter(|_| output.status.success())
        .ok_or_else(|| io::Error::other("无法读取便携版同步的设备标识"))?;
    Ok(format!(
        "{:x}",
        Sha256::digest(format!(
            "{machine}\n{}\n{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        ))
    ))
}

#[cfg(any(target_os = "windows", test))]
fn open_portable(executable: &Path) -> io::Result<Option<(AppPaths, Profile)>> {
    let root = executable
        .parent()
        .ok_or_else(|| io::Error::other("无法确定便携版目录"))?;
    let marker = match fs::read(root.join(MARKER)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let marker: Marker = serde_json::from_slice(&marker)
        .map_err(|_| io::Error::other("便携版标记无效，未使用系统配置目录"))?;
    if marker.version != 1 {
        return Err(io::Error::other("不支持的便携版格式"));
    }
    open_portable_data(root, root.join("Data")).map(Some)
}

/// Locate the enclosing bundle without depending on the application's name.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn macos_bundle(executable: &Path) -> Option<std::path::PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()?.eq_ignore_ascii_case("app"))
    .then(|| app.to_owned())
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn is_translocated(app: &Path) -> bool {
    if !app.starts_with("/private/var/folders") && !app.starts_with("/var/folders") {
        return false;
    }
    let components: Vec<_> = app
        .components()
        .map(|component| component.as_os_str())
        .collect();
    components.windows(3).any(|parts| {
        parts[0] == "AppTranslocation"
            && parts[2] == "d"
            && parts[1]
                .to_str()
                .is_some_and(|token| uuid::Uuid::parse_str(token).is_ok())
    })
}

#[cfg(any(target_os = "macos", test))]
fn open_macos_portable(executable: &Path) -> io::Result<Option<(AppPaths, Profile)>> {
    let Some(app) = macos_bundle(executable) else {
        return Ok(None);
    };
    // A translocated bundle cannot see its real siblings. Do not create an
    // unrelated default profile and make an existing portable library look lost.
    if is_translocated(&app) {
        return Err(io::Error::other(
            "应用正在 macOS 临时隔离位置运行，请用 Finder 将应用移至最终可写目录后重新打开",
        ));
    }
    let root = app
        .parent()
        .ok_or_else(|| io::Error::other("无法确定应用所在目录"))?;
    let data = root.join(MACOS_DATA);
    match fs::symlink_metadata(&data) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(io::Error::other(
                "torto-portable-data 必须是普通文件夹，不能是文件或符号链接",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    open_portable_data(root, data).map(Some)
}

#[cfg(target_os = "macos")]
fn machine_scope() -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let output = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("无法读取便携版同步的设备标识"));
    }
    let output = String::from_utf8_lossy(&output.stdout);
    let machine = output
        .lines()
        .find_map(|line| {
            let (key, value) = line.trim().split_once(" = ")?;
            (key == r#""IOPlatformUUID""#).then(|| value.trim_matches('"'))
        })
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .ok_or_else(|| io::Error::other("无法读取便携版同步的设备标识"))?;
    let uid = std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()?;
    if !uid.status.success() {
        return Err(io::Error::other("无法读取同步用户标识"));
    }
    let uid = String::from_utf8_lossy(&uid.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|_| io::Error::other("无法读取同步用户标识"))?;
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("macos\n{machine}\n{uid}"))
    ))
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
fn open_portable_data(root: &Path, data: std::path::PathBuf) -> io::Result<(AppPaths, Profile)> {
    fs::create_dir_all(&data)?;
    let lock = lock_portable_data(&data)?;
    let paths = AppPaths {
        data: data.clone(),
        roaming: data.clone(),
        config: data.join("config"),
        cache: data.join("cache"),
    };
    // Do not silently fall back to a system profile on a read-only drive.
    // The updater also needs to replace the program in its parent directory.
    for directory in [
        root,
        data.as_path(),
        &paths.config,
        &paths.cache,
        &data.join("library"),
        &data.join("fonts"),
    ] {
        fs::create_dir_all(directory)?;
        let probe = directory.join(format!(".torto-write-{}", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&probe)?;
        let result = file.write_all(b"Torto").and_then(|_| file.sync_all());
        drop(file);
        let cleanup = fs::remove_file(probe);
        result?;
        cleanup?;
    }
    Ok((paths, Profile { _lock: Some(lock) }))
}

#[cfg(any(target_os = "windows", target_os = "macos", test))]
pub(crate) fn lock_portable_data(data: &Path) -> io::Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(data.join(".instance.lock"))?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::other("该便携书库已在另一个 Torto 窗口中打开")
        }
        std::fs::TryLockError::Error(error) => io::Error::new(error.kind(), "无法锁定便携数据目录"),
    })?;
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mac_fixture(root: &Path, name: &str) -> std::path::PathBuf {
        let executable = root.join(name).join("Contents/MacOS/torto");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        executable
    }
    #[test]
    fn macos_portable_data_follows_renamed_app_and_outer_folder_with_real_credentials() {
        use rebook_app_core::credentials::{FileStore, SecretStore};
        let root = std::env::temp_dir().join(format!("torto-mac-profile-{}", uuid::Uuid::new_v4()));
        let original = root.join("My books 书库 & spaces");
        let exe = mac_fixture(&original, "My Reader.app");
        fs::create_dir_all(original.join(MACOS_DATA)).unwrap();
        let (paths, guard) = open_macos_portable(&exe).unwrap().unwrap();
        assert_eq!(paths.data, original.join(MACOS_DATA));
        assert!(open_macos_portable(&exe).is_err());
        let credentials = FileStore::new(paths.config.join("credentials.json"), None);
        credentials.write("AI", "provider", "portable-key").unwrap();
        fs::write(paths.data.join("library/book.epub"), b"book").unwrap();
        drop(credentials);
        drop(guard);
        let moved = root.join("AppTranslocation"); // an arbitrary user folder name, not Apple's runtime path
        fs::rename(&original, &moved).unwrap();
        fs::rename(moved.join("My Reader.app"), moved.join("Renamed.app")).unwrap();
        let (paths, guard) = open_macos_portable(&moved.join("Renamed.app/Contents/MacOS/torto"))
            .unwrap()
            .unwrap();
        assert_eq!(
            FileStore::new(paths.config.join("credentials.json"), None)
                .read("AI", "provider")
                .unwrap(),
            "portable-key"
        );
        assert_eq!(
            fs::read(paths.data.join("library/book.epub")).unwrap(),
            b"book"
        );
        drop(guard);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn macos_detects_only_a_real_sibling_data_directory_and_does_not_create_one() {
        let root = std::env::temp_dir().join(format!("torto-mac-detect-{}", uuid::Uuid::new_v4()));
        let exe = mac_fixture(&root, "Torto.app");
        fs::create_dir_all(exe.parent().unwrap().join(MACOS_DATA)).unwrap();
        assert!(open_macos_portable(&exe).unwrap().is_none());
        assert!(!root.join(MACOS_DATA).exists());
        assert!(macos_bundle(&root.join("torto")).is_none());
        fs::write(root.join(MACOS_DATA), b"not a directory").unwrap();
        assert!(open_macos_portable(&exe).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn macos_rejects_symlinked_and_read_only_portable_data() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root =
            std::env::temp_dir().join(format!("torto-mac-read-only-{}", uuid::Uuid::new_v4()));
        let exe = mac_fixture(&root, "Torto.app");
        let other = root.join("other");
        fs::create_dir_all(&other).unwrap();
        symlink(&other, root.join(MACOS_DATA)).unwrap();
        assert!(open_macos_portable(&exe).is_err());
        fs::remove_file(root.join(MACOS_DATA)).unwrap();
        fs::create_dir_all(root.join(MACOS_DATA)).unwrap();
        fs::set_permissions(root.join(MACOS_DATA), fs::Permissions::from_mode(0o555)).unwrap();
        let result = open_macos_portable(&exe);
        fs::set_permissions(root.join(MACOS_DATA), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn macos_translocated_paths_fail_before_any_default_profile_can_be_selected() {
        let path = format!(
            "/private/var/folders/xx/yy/T/AppTranslocation/{}/d/Torto.app/Contents/MacOS/torto",
            uuid::Uuid::new_v4()
        );
        assert!(open_macos_portable(Path::new(&path)).is_err());
        assert!(!is_translocated(Path::new(
            "/Users/me/SomeAppTranslocation/Torto.app"
        )));
        assert!(!is_translocated(Path::new(
            "/Users/me/AppTranslocation/Torto.app"
        )));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_machine_scope_is_stable_without_persisting_a_machine_name() {
        let first = machine_scope().unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(first, machine_scope().unwrap());
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
    #[test]
    fn portable_profile_follows_executable_move_and_locks_only_its_own_data() {
        let root = std::env::temp_dir().join(format!("torto-portable-{}", uuid::Uuid::new_v4()));
        let first = root.join("目录 with spaces");
        fs::create_dir_all(&first).unwrap();
        fs::write(first.join(MARKER), br#"{"version":1}"#).unwrap();
        let (paths, guard) = open_portable(&first.join("torto.exe")).unwrap().unwrap();
        fs::write(paths.data.join("library/book.epub"), b"book").unwrap();
        fs::write(paths.config.join("credentials.json"), b"fixture").unwrap();
        assert!(open_portable(&first.join("torto.exe")).is_err());
        drop(guard);
        let moved = root.join("moved");
        fs::rename(&first, &moved).unwrap();
        let (paths, guard) = open_portable(&moved.join("torto.exe")).unwrap().unwrap();
        assert_eq!(
            fs::read(paths.data.join("library/book.epub")).unwrap(),
            b"book"
        );
        assert_eq!(
            fs::read(paths.config.join("credentials.json")).unwrap(),
            b"fixture"
        );
        drop(guard);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn invalid_markers_fail_without_creating_data_or_selecting_a_different_profile() {
        let root = std::env::temp_dir().join(format!("torto-marker-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        assert!(open_portable(&root.join("torto.exe")).unwrap().is_none());
        for marker in [
            "",
            "{}",
            "{\"version\":2}",
            "{\"version\":1,\"data\":\"..\"}",
        ] {
            fs::write(root.join(MARKER), marker).unwrap();
            assert!(open_portable(&root.join("torto.exe")).is_err());
            assert!(!root.join("Data").exists());
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn read_only_portable_root_is_an_error_even_if_its_existing_data_is_writable() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("torto-read-only-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("Data")).unwrap();
        fs::write(root.join(MARKER), br#"{"version":1}"#).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
        let result = open_portable(&root.join("torto.exe"));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
