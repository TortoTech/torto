//! Verified release metadata and binary transport shared by desktop and mobile.
use crate::{http::github, persistence::write_bytes_atomic, preferences::AppLanguage};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
pub const GITEE_RELEASE_URL: &str =
    "https://gitee.com/api/v5/repos/TortoTech/torto/releases/latest";
pub const RELEASE_DOWNLOAD_PREFIX: &str = "/TortoTech/torto/releases/download/";
pub const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;
const CHINESE_RELEASE_NOTES_SUMMARY: &str = "中文更新说明";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    WindowsX64,
    AndroidArm64,
    AndroidX64,
}
impl Platform {
    pub const ALL: [Self; 3] = [Self::WindowsX64, Self::AndroidArm64, Self::AndroidX64];
    pub fn suffix(self) -> &'static str {
        match self {
            Self::WindowsX64 => "-x86_64.msi",
            Self::AndroidArm64 => "-android-arm64-v8a.apk",
            Self::AndroidX64 => "-android-x86_64.apk",
        }
    }
    pub fn name(self, version: &str) -> String {
        format!("Torto-{version}{}", self.suffix())
    }
    pub fn manifest(self) -> &'static str {
        match self {
            Self::WindowsX64 => "torto-update.json",
            _ => "torto-android-update.json",
        }
    }
}
#[derive(Clone, Copy)]
pub enum UpdateSource {
    Automatic,
    Github,
    Gitee,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdateRelease {
    pub version: String,
    pub notes: String,
    pub asset: UpdateAsset,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpdateAsset {
    pub name: String,
    pub url: String,
    /// Supplied by GitHub's release metadata; a Gitee-only check has no ID.
    pub github_asset_id: Option<u64>,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DownloadedUpdate {
    pub release: UpdateRelease,
    pub installer_path: PathBuf,
}

pub fn localized_release_notes(markdown: &str, language: AppLanguage) -> &str {
    let Some((english, details)) = markdown.split_once("<details>") else {
        return markdown;
    };
    let details = details.trim_start();
    let Some(summary_and_rest) = details.strip_prefix("<summary>") else {
        return markdown;
    };
    let Some((summary, chinese_and_rest)) = summary_and_rest.split_once("</summary>") else {
        return markdown;
    };
    if summary.trim() != CHINESE_RELEASE_NOTES_SUMMARY {
        return markdown;
    }
    let Some((chinese, _)) = chinese_and_rest.split_once("</details>") else {
        return markdown;
    };

    let localized = match language.resolved() {
        AppLanguage::SimplifiedChinese => chinese.trim(),
        AppLanguage::English => english.trim(),
        AppLanguage::System => unreachable!(),
    };
    if localized.is_empty() {
        markdown
    } else {
        localized
    }
}

pub async fn check_for_update_version(current: &str) -> Result<Option<UpdateRelease>, String> {
    check_release(current, Platform::WindowsX64, UpdateSource::Automatic).await
}
pub async fn check_release(
    current: &str,
    platform: Platform,
    source: UpdateSource,
) -> Result<Option<UpdateRelease>, String> {
    let client = crate::http::builder()
        .timeout(Duration::from_secs(6))
        .connect_timeout(Duration::from_secs(4))
        .build()
        .map_err(|error| error.to_string())?;
    match source {
        UpdateSource::Github => check_github_platform(&client, current, platform).await,
        UpdateSource::Gitee => check_gitee_platform(&client, current, platform).await,
        UpdateSource::Automatic => {
            let (a, b) = futures_util::future::join(
                check_github_platform(&client, current, platform),
                check_gitee_platform(&client, current, platform),
            )
            .await;
            select_update(a, b)
        }
    }
}

async fn read_metadata<T:serde::de::DeserializeOwned>(mut response:reqwest::Response)->Result<T,String>{
    let mut bytes=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|e|e.to_string())?{if bytes.len()+chunk.len()>2*1024*1024{return Err("Release metadata exceeds 2 MiB".into());}bytes.extend_from_slice(&chunk);}
    serde_json::from_slice(&bytes).map_err(|e|e.to_string())
}
pub async fn check_github_platform(
    client: &reqwest::Client,
    current: &str,
    platform: Platform,
) -> Result<Option<UpdateRelease>, String> {
    let response = github::json_request(client, &github::TORTO.latest_release_url())
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let release:GitHubRelease=read_metadata(response).await?;
    release_from_github_platform(release, current, platform)
}

pub async fn check_gitee_platform(
    client: &reqwest::Client,
    current: &str,
    platform: Platform,
) -> Result<Option<UpdateRelease>, String> {
    let response = client
        .get(GITEE_RELEASE_URL)
        .header(USER_AGENT, format!("Torto/{}", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        ;
    let release:GiteeRelease=read_metadata(response).await?;
    if release.prerelease
        || AppVersion::parse(release.tag_name.trim_start_matches('v'))?
            <= AppVersion::parse(current)?
    {
        return Ok(None);
    }
    // The sync job uploads this marker last. No marker means an incomplete mirror.
    let marker_url = release
        .assets
        .iter()
        .find(|asset| asset.name == platform.manifest())
        .ok_or_else(|| "The Gitee release has not finished synchronization".to_string())?
        .browser_download_url
        .clone();
    validate_release_url(
        &marker_url,
        "gitee.com",
        &release.tag_name,
        platform.manifest(),
    )?;
    let mut response = client
        .get(&marker_url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len() + chunk.len() > 16 * 1024 {
            return Err("The Gitee update manifest is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let manifest =
        serde_json::from_slice::<GiteeManifest>(&bytes).map_err(|error| error.to_string())?;
    release_from_gitee_platform(release, manifest, current, platform)
}

pub fn select_update(
    github: Result<Option<UpdateRelease>, String>,
    gitee: Result<Option<UpdateRelease>, String>,
) -> Result<Option<UpdateRelease>, String> {
    match (github, gitee) {
        (Ok(Some(first)), Ok(Some(second))) => {
            if first.version == second.version
                && (first.asset.name != second.asset.name
                    || first.asset.size != second.asset.size
                    || first.asset.sha256 != second.asset.sha256)
            {
                return Err("GitHub and Gitee disagree about the installer checksum".into());
            }
            if AppVersion::parse(&second.version)? > AppVersion::parse(&first.version)? {
                Ok(Some(second))
            } else {
                Ok(Some(first))
            }
        }
        (Ok(Some(update)), _) | (_, Ok(Some(update))) => Ok(Some(update)),
        (Ok(None), _) | (_, Ok(None)) => Ok(None),
        (Err(first), Err(second)) => Err(format!("GitHub: {first}\nGitee: {second}")),
    }
}

pub async fn download_update(release: UpdateRelease) -> Result<DownloadedUpdate, String> {
    download_update_progress(release, std::sync::Arc::new(|_| {})).await
}
pub async fn download_update_progress(
    release: UpdateRelease,
    progress: std::sync::Arc<dyn Fn(u8) + Send + Sync>,
) -> Result<DownloadedUpdate, String> {
    validate_asset(&release.asset)?;
    AppVersion::parse(&release.version)?;
    if !Platform::ALL.iter().any(|p|release.asset.name==p.name(&release.version)){return Err("Installer version does not match release".into());}
    let project=crate::environment::paths().ok_or_else(||"Unable to resolve the local update directory".to_owned())?;
    let path=project.cache_dir().join("updates").join(&release.version).join(&release.asset.name);
    let cached=DownloadedUpdate{release:release.clone(),installer_path:path.clone()};
    if verify_installer_file(&cached).is_ok(){progress(100);return Ok(cached);}
    let client = installer_client()?;
    let bytes = download_with_fallback(&release.asset, |url| {
        let client = &client;
        let asset = &release.asset;
        let progress = progress.clone();
        async move {
            crate::http::fetch_sized_body(
                installer_request(client, &url),
                asset.size,
                MAX_INSTALLER_BYTES,
                move |p| progress(p),
            )
            .await
        }
    })
    .await?;
    write_bytes_atomic(&path, &bytes).map_err(|error| error.to_string())?;
    Ok(DownloadedUpdate {
        release,
        installer_path: path,
    })
}

pub fn installer_client() -> Result<reqwest::Client, String> {
    crate::http::download_builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 8 || !trusted_download_redirect(attempt.url()) {
                attempt.error("Untrusted installer redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

pub fn trusted_download_redirect(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && (github::TORTO.is_asset_url(url)
            || matches!(
                url.host_str(),
                Some(
                    "github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                        | "gitee.com"
                        | "foruda.gitee.com"
                )
            ))
}

pub async fn download_with_fallback<F, Fut>(
    asset: &UpdateAsset,
    mut fetch: F,
) -> Result<Vec<u8>, String>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    validate_asset(asset)?;
    let mut alternative = reqwest::Url::parse(&asset.url).map_err(|error| error.to_string())?;
    let host = if alternative.host_str() == Some("github.com") {
        "gitee.com"
    } else {
        "github.com"
    };
    alternative
        .set_host(Some(host))
        .map_err(|error| error.to_string())?;
    let mut errors = Vec::new();
    let mut urls = Vec::with_capacity(3);
    if let Some(id) = asset.github_asset_id {
        urls.push(github::TORTO.asset_url(id));
    }
    urls.extend([asset.url.clone(), alternative.to_string()]);
    for url in urls {
        let result = fetch(url).await.and_then(|bytes| {
            verify_installer_bytes(asset, &bytes)?;
            Ok(bytes)
        });
        match result {
            Ok(bytes) => return Ok(bytes),
            Err(error) => errors.push(error),
        }
    }
    Err(errors.join("\n"))
}

pub async fn download_installer_bytes(
    client: &reqwest::Client,
    url: String,
    asset: &UpdateAsset,
) -> Result<Vec<u8>, String> {
    crate::http::fetch_sized_body(
        installer_request(client, &url),
        asset.size,
        MAX_INSTALLER_BYTES,
        |_| {},
    )
    .await
}

pub fn installer_request(client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    if reqwest::Url::parse(url).is_ok_and(|url| github::TORTO.is_asset_url(&url)) {
        github::asset_request(client, url)
    } else {
        client
            .get(url)
            .header(USER_AGENT, concat!("Torto/", env!("CARGO_PKG_VERSION")))
            .header(ACCEPT, "application/octet-stream")
    }
}

pub fn verify_installer_file(update: &DownloadedUpdate) -> Result<(), String> {
    use std::io::Read;
    let file=std::fs::File::open(&update.installer_path).map_err(|e|e.to_string())?;
    if file.metadata().map_err(|e|e.to_string())?.len()!=update.release.asset.size{return Err("Cached installer size mismatch".into());}
    let mut bytes=Vec::new();file.take(MAX_INSTALLER_BYTES+1).read_to_end(&mut bytes).map_err(|e|e.to_string())?;
    verify_installer_bytes(&update.release.asset, &bytes)
}

pub fn verify_installer_bytes(asset: &UpdateAsset, bytes: &[u8]) -> Result<(), String> {
    crate::http::verify_sized_sha256(bytes, asset.size, MAX_INSTALLER_BYTES, &asset.sha256)
}

pub fn release_from_github(
    release: GitHubRelease,
    current: &str,
) -> Result<Option<UpdateRelease>, String> {
    release_from_github_platform(release, current, Platform::WindowsX64)
}
pub fn release_from_github_platform(
    release: GitHubRelease,
    current_version: &str,
    platform: Platform,
) -> Result<Option<UpdateRelease>, String> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let current = AppVersion::parse(current_version)?;
    let latest = AppVersion::parse(release.tag_name.trim_start_matches('v'))?;
    if latest <= current {
        return Ok(None);
    }
    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name.ends_with(platform.suffix()))
        .ok_or_else(|| {
            "The latest release does not include an installer for this platform".to_string()
        })?;
    validate_release_url(
        &asset.browser_download_url,
        "github.com",
        &release.tag_name,
        &asset.name,
    )?;
    let digest = asset
        .digest
        .and_then(|digest| digest.strip_prefix("sha256:").map(str::to_owned))
        .ok_or_else(|| "The installer does not include a SHA-256 digest".to_string())?
        .to_ascii_lowercase();
    let update = UpdateRelease {
        version: latest.to_string(),
        notes: release
            .body
            .filter(|body| !body.trim().is_empty())
            .unwrap_or_else(|| "This release does not include release notes.".into()),
        asset: UpdateAsset {
            name: asset.name,
            url: asset.browser_download_url,
            github_asset_id: Some(asset.id),
            sha256: digest,
            size: asset.size,
        },
    };
    validate_asset(&update.asset)?;
    if update.asset.name != platform.name(&latest.to_string()) {
        return Err("The installer does not match the release version".into());
    }
    Ok(Some(update))
}

pub fn release_from_gitee(
    release: GiteeRelease,
    manifest: GiteeManifest,
    current: &str,
) -> Result<Option<UpdateRelease>, String> {
    release_from_gitee_platform(release, manifest, current, Platform::WindowsX64)
}
pub fn release_from_gitee_platform(
    release: GiteeRelease,
    manifest: GiteeManifest,
    current: &str,
    platform: Platform,
) -> Result<Option<UpdateRelease>, String> {
    let version = AppVersion::parse(release.tag_name.trim_start_matches('v'))?;
    if release.prerelease || version <= AppVersion::parse(current)? {
        return Ok(None);
    }
    if manifest.tag != release.tag_name
        || manifest.version != version.to_string()
        || manifest.asset.name != platform.name(&version.to_string())
    {
        return Err("The Gitee update manifest does not match its release".into());
    }
    let attachment = release
        .assets
        .iter()
        .find(|asset| asset.name == manifest.asset.name)
        .ok_or_else(|| "The Gitee installer has not been uploaded".to_string())?;
    validate_release_url(
        &attachment.browser_download_url,
        "gitee.com",
        &release.tag_name,
        &attachment.name,
    )?;
    let asset = UpdateAsset {
        name: manifest.asset.name,
        url: attachment.browser_download_url.clone(),
        github_asset_id: None,
        sha256: manifest.asset.sha256,
        size: manifest.asset.size,
    };
    validate_asset(&asset)?;
    Ok(Some(UpdateRelease {
        version: version.to_string(),
        notes: release
            .body
            .filter(|body| !body.trim().is_empty())
            .unwrap_or_else(|| "This release does not include release notes.".into()),
        asset,
    }))
}

pub fn validate_release_url(value: &str, host: &str, tag: &str, name: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|error| error.to_string())?;
    if url.scheme() != "https"
        || url.host_str() != Some(host)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != format!("{RELEASE_DOWNLOAD_PREFIX}{tag}/{name}")
    {
        return Err("The release download URL is not trusted".into());
    }
    Ok(())
}

pub fn validate_asset(asset: &UpdateAsset) -> Result<(), String> {
    if asset.name.contains(['/', '\\'])
        || !Platform::ALL
            .iter()
            .any(|p| asset.name.ends_with(p.suffix()))
    {
        return Err("The installer has an invalid file name".into());
    }
    if asset.size == 0 || asset.size > MAX_INSTALLER_BYTES {
        return Err("The installer has an invalid file size".into());
    }
    if asset.sha256.len() != 64 || !asset.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The installer has an invalid SHA-256 digest".into());
    }
    let url = reqwest::Url::parse(&asset.url).map_err(|error| error.to_string())?;
    if asset
        .github_asset_id
        .is_some_and(|id| id == 0 || url.host_str() != Some("github.com"))
    {
        return Err("The GitHub installer asset ID is invalid".into());
    }
    if !matches!(url.host_str(), Some("github.com" | "gitee.com")) {
        return Err("The installer download URL is not trusted".into());
    }
    let suffix = url
        .path()
        .strip_prefix(RELEASE_DOWNLOAD_PREFIX)
        .ok_or_else(|| "The installer URL has an invalid repository".to_string())?;
    let (tag, _) = suffix
        .split_once('/')
        .ok_or_else(|| "Missing release tag".to_string())?;
    let version = AppVersion::parse(tag.trim_start_matches('v'))?;
    if !Platform::ALL
        .iter()
        .any(|p| asset.name == p.name(&version.to_string()))
    {
        return Err("The installer file name does not match the URL version".into());
    }
    validate_release_url(
        &asset.url,
        url.host_str().unwrap_or_default(),
        tag,
        &asset.name,
    )
}

#[derive(Deserialize)]
pub struct GiteeRelease {
    pub tag_name: String,
    pub body: Option<String>,
    pub prerelease: bool,
    pub assets: Vec<GiteeAsset>,
}

#[derive(Deserialize)]
pub struct GiteeAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Deserialize)]
pub struct GiteeManifest {
    pub version: String,
    pub tag: String,
    pub asset: ManifestAsset,
}

#[derive(Deserialize)]
pub struct ManifestAsset {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
pub struct GitHubAsset {
    pub id: u64,
    pub name: String,
    pub browser_download_url: String,
    pub digest: Option<String>,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AppVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl AppVersion {
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut parts = value.split('.');
        let major = parse_version_part(parts.next(), value)?;
        let minor = parse_version_part(parts.next(), value)?;
        let patch = parse_version_part(parts.next(), value)?;
        if parts.next().is_some() {
            return Err(format!("Unsupported application version: {value}"));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for AppVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub fn parse_version_part(part: Option<&str>, full: &str) -> Result<u64, String> {
    let part = part.ok_or_else(|| format!("Unsupported application version: {full}"))?;
    if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("Unsupported application version: {full}"));
    }
    part.parse::<u64>()
        .map_err(|_| format!("Unsupported application version: {full}"))
}

pub async fn check_github(
    client: &reqwest::Client,
    current: &str,
) -> Result<Option<UpdateRelease>, String> {
    check_github_platform(client, current, Platform::WindowsX64).await
}
pub async fn check_gitee(
    client: &reqwest::Client,
    current: &str,
) -> Result<Option<UpdateRelease>, String> {
    check_gitee_platform(client, current, Platform::WindowsX64).await
}
#[cfg(test)]
mod mobile_tests {
    use super::*;
    #[test]
    fn android_releases_require_the_exact_abi_name_and_digest() {
        let json = serde_json::json!({"tag_name":"v0.10.0","body":"notes","draft":false,"prerelease":false,"assets":[{"id":72,"name":"Torto-0.10.0-android-arm64-v8a.apk","size":100,"digest":format!("sha256:{}","a".repeat(64)),"browser_download_url":"https://github.com/TortoTech/torto/releases/download/v0.10.0/Torto-0.10.0-android-arm64-v8a.apk"}]});
        let release = release_from_github_platform(
            serde_json::from_value(json.clone()).unwrap(),
            "0.9.0",
            Platform::AndroidArm64,
        )
        .unwrap()
        .unwrap();
        assert_eq!(release.asset.github_asset_id, Some(72));
        assert!(validate_asset(&release.asset).is_ok());
        assert!(
            release_from_github_platform(
                serde_json::from_value(json.clone()).unwrap(),
                "0.9.0",
                Platform::AndroidX64
            )
            .is_err()
        );
        let mut invalid = json;
        invalid["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(
            release_from_github_platform(
                serde_json::from_value(invalid).unwrap(),
                "0.9.0",
                Platform::AndroidArm64
            )
            .is_err()
        );
        let mut bad = release.asset;
        bad.name = "../malware.apk".into();
        assert!(validate_asset(&bad).is_err());
    }
    #[test]
    fn android_gitee_manifest_is_distinct_from_the_windows_marker() {
        assert_eq!(
            Platform::AndroidArm64.manifest(),
            "torto-android-update.json"
        );
        let r=GiteeRelease{tag_name:"v0.10.0".into(),body:None,prerelease:false,assets:vec![GiteeAsset{name:"Torto-0.10.0-android-arm64-v8a.apk".into(),browser_download_url:"https://gitee.com/TortoTech/torto/releases/download/v0.10.0/Torto-0.10.0-android-arm64-v8a.apk".into()}]};
        let m = GiteeManifest {
            version: "0.10.0".into(),
            tag: "v0.10.0".into(),
            asset: ManifestAsset {
                name: "Torto-0.10.0-android-arm64-v8a.apk".into(),
                sha256: "a".repeat(64),
                size: 100,
            },
        };
        assert!(
            release_from_gitee_platform(r, m, "0.9.0", Platform::AndroidArm64)
                .unwrap()
                .is_some()
        );
    }
}
