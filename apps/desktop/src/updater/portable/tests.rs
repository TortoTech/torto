use super::*;
use rebook_app_core::updates::{UpdateAsset, UpdateRelease};
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("torto-portable-update-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn archive(dir: &Path, extra: Option<&str>) -> PathBuf {
    let path = dir.join("update.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("torto.exe", options).unwrap();
    zip.write_all(&[b"MZ".as_slice(), &[1; 64]].concat())
        .unwrap();
    zip.start_file("torto-portable.json", options).unwrap();
    zip.write_all(br#"{"version":1}"#).unwrap();
    if let Some(name) = extra {
        zip.start_file(name, options).unwrap();
        zip.write_all(b"untrusted").unwrap();
    }
    zip.finish().unwrap();
    path
}
fn update(path: PathBuf) -> DownloadedUpdate {
    let bytes = fs::read(&path).unwrap();
    DownloadedUpdate { installer_path: path, release: UpdateRelease {
        version: "0.10.0".into(), notes: String::new(), macos_signature: None,
        asset: UpdateAsset { name: Platform::WindowsPortableX64.name("0.10.0"), url: "https://github.com/TortoTech/torto/releases/download/v0.10.0/Torto-0.10.0-windows-x86_64-portable.zip".into(), github_asset_id: Some(77), size: bytes.len() as u64, sha256: format!("{:x}", Sha256::digest(bytes)) },
    }}
}
#[test]
fn verified_staging_never_changes_the_program_or_data_before_exit() {
    let dir = Directory::new();
    let exe = dir.0.join("torto.exe");
    fs::write(&exe, b"old-program").unwrap();
    fs::create_dir_all(dir.0.join("Data/config")).unwrap();
    fs::write(
        dir.0.join("Data/config/credentials.json"),
        b"credential-fixture",
    )
    .unwrap();
    let update = update(archive(&dir.0, None));
    let session = stage(&update, &exe).unwrap();
    let job: Job =
        serde_json::from_slice(&fs::read(session.join("request.json")).unwrap()).unwrap();
    assert_eq!(job.target, exe);
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(job.staged).unwrap())),
        job.sha256
    );
    assert_eq!(fs::read(&exe).unwrap(), b"old-program");
    assert_eq!(
        fs::read(dir.0.join("Data/config/credentials.json")).unwrap(),
        b"credential-fixture"
    );
    assert!(!job.backup.exists());
}
#[test]
fn zip_traversal_user_data_and_corrupt_downloads_are_rejected_before_installation() {
    for name in [
        "../torto.exe",
        "Data/config/credentials.json",
        "unexpected.dll",
        "sub/torto.exe",
    ] {
        let dir = Directory::new();
        let path = archive(&dir.0, Some(name));
        assert!(extract_executable(&path, &dir.0.join("new.exe")).is_err());
        assert!(!dir.0.join("new.exe").exists());
    }
    let dir = Directory::new();
    let mut bad = update(archive(&dir.0, None));
    bad.release.asset.sha256 = "0".repeat(64);
    assert!(stage(&bad, &dir.0.join("torto.exe")).is_err());
    assert!(!dir.0.join("Data").exists());
}
#[test]
fn portable_and_installer_metadata_cannot_select_each_others_assets() {
    let release = rebook_app_core::updates::GitHubRelease {
        tag_name: "v0.10.0".into(),
        body: None,
        draft: false,
        prerelease: false,
        assets: [Platform::WindowsX64, Platform::WindowsPortableX64]
            .map(|platform| {
                let name = platform.name("0.10.0");
                rebook_app_core::updates::GitHubAsset {
                    id: 7,
                    browser_download_url: format!(
                        "https://github.com/TortoTech/torto/releases/download/v0.10.0/{name}"
                    ),
                    name,
                    digest: Some(format!("sha256:{}", "a".repeat(64))),
                    size: 100,
                }
            })
            .into(),
    };
    let portable = rebook_app_core::updates::release_from_github_platform(
        release,
        "0.9.0",
        Platform::WindowsPortableX64,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        portable.asset.name,
        Platform::WindowsPortableX64.name("0.10.0")
    );
    assert_ne!(
        Platform::WindowsX64.manifest(),
        Platform::WindowsPortableX64.manifest()
    );
}

#[test]
fn completed_update_cache_is_bounded_without_removing_active_or_recovery_jobs() {
    let dir = Directory::new();
    for index in 0..5 {
        let session = dir.0.join(format!("portable-completed-{index}"));
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("state.txt"), "completed").unwrap();
    }
    for name in ["portable-active", "portable-recovery"] {
        let session = dir.0.join(name);
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("state.txt"),
            if name.ends_with("active") {
                "starting"
            } else {
                "failed"
            },
        )
        .unwrap();
        if name.ends_with("recovery") {
            fs::write(session.join("previous.exe"), b"recoverable").unwrap();
        }
    }
    prune_finished_sessions(&dir.0);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 4);
    assert!(dir.0.join("portable-active").is_dir());
    assert_eq!(
        fs::read(dir.0.join("portable-recovery/previous.exe")).unwrap(),
        b"recoverable"
    );
}
