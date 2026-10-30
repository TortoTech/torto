use super::tests::{gitee_fixture, github_release};
use super::*;

fn asset(bytes: &[u8]) -> UpdateAsset {
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    let mut asset = release_from_github(github_release("v0.2.12", Some(&digest)), "0.2.11")
        .unwrap()
        .unwrap()
        .asset;
    asset.size = bytes.len() as u64;
    asset
}

#[test]
fn github_metadata_selects_the_asset_api_and_binary_request_headers() {
    let bytes = b"verified installer";
    let asset = asset(bytes);
    assert_eq!(asset.github_asset_id, Some(42));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut calls = Vec::new();
    let downloaded = runtime
        .block_on(download_with_fallback(&asset, |url| {
            calls.push(url);
            std::future::ready(Ok(bytes.to_vec()))
        }))
        .unwrap();
    assert_eq!(downloaded, bytes);
    assert_eq!(calls, [github::TORTO.asset_url(42)]);
    let request = installer_request(&installer_client().unwrap(), &calls[0])
        .build()
        .unwrap();
    assert_eq!(request.headers()[ACCEPT], "application/octet-stream");
    assert_eq!(request.headers()["X-GitHub-Api-Version"], "2026-03-10");
    assert!(!request.headers().contains_key("authorization"));
    let direct = installer_request(&installer_client().unwrap(), &asset.url)
        .build()
        .unwrap();
    assert!(!direct.headers().contains_key("X-GitHub-Api-Version"));
}

#[test]
fn api_failures_and_corrupt_content_try_release_links_then_gitee() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let bytes = b"verified installer";
    let asset = asset(bytes);
    for failure in ["403 rate limited", "429 rate limited", "timeout", "corrupt"] {
        let mut calls = Vec::new();
        let downloaded = runtime
            .block_on(download_with_fallback(&asset, |url| {
                calls.push(url);
                let result = if calls.len() == 3 {
                    Ok(bytes.to_vec())
                } else if failure == "corrupt" {
                    Ok(vec![0; bytes.len()])
                } else {
                    Err(failure.to_owned())
                };
                std::future::ready(result)
            }))
            .unwrap();
        assert_eq!(downloaded, bytes);
        assert_eq!(
            calls,
            [
                github::TORTO.asset_url(42),
                asset.url.clone(),
                asset.url.replace("github.com", "gitee.com"),
            ]
        );
    }
    let mut calls = 0;
    assert!(
        runtime
            .block_on(download_with_fallback(&asset, |_| {
                calls += 1;
                std::future::ready(Err("offline".into()))
            }))
            .is_err()
    );
    assert_eq!(
        calls, 3,
        "a failed download must not loop on a rate-limited API"
    );
}

#[test]
fn gitee_only_updates_still_work_without_a_github_asset_lookup() {
    let (release, manifest) = gitee_fixture();
    let gitee = release_from_gitee(release, manifest, "0.2.11")
        .unwrap()
        .unwrap();
    assert_eq!(gitee.asset.github_asset_id, None);
    let digest = format!("sha256:{}", "a".repeat(64));
    let github = release_from_github(github_release("v0.2.12", Some(&digest)), "0.2.11")
        .unwrap()
        .unwrap();
    let selected = select_update(Ok(Some(github)), Ok(Some(gitee.clone())))
        .unwrap()
        .unwrap();
    assert_eq!(selected.asset.github_asset_id, Some(42));
    let selected = select_update(Err("API unavailable".into()), Ok(Some(gitee)))
        .unwrap()
        .unwrap();
    assert_eq!(selected.asset.github_asset_id, None);
}

#[test]
fn api_downloads_retain_repository_and_redirect_trust_checks() {
    let asset = asset(b"verified installer");
    assert!(trusted_download_redirect(
        &reqwest::Url::parse(&github::TORTO.asset_url(42)).unwrap()
    ));
    for url in [
        "https://api.github.com/repos/another/torto/releases/assets/42",
        "https://api.github.com/repos/TortoTech/another/releases/assets/42",
        "https://api.github.com/repos/TortoTech/torto/releases/assets/0",
        "https://api.github.com/repos/TortoTech/torto/releases/assets/42?redirect=evil",
        "https://api.github.com:444/repos/TortoTech/torto/releases/assets/42",
        "https://api.github.com.evil.example/repos/TortoTech/torto/releases/assets/42",
        "http://api.github.com/repos/TortoTech/torto/releases/assets/42",
    ] {
        assert!(
            !trusted_download_redirect(&reqwest::Url::parse(url).unwrap()),
            "{url}"
        );
    }
    assert!(
        validate_asset(&UpdateAsset {
            github_asset_id: Some(0),
            ..asset.clone()
        })
        .is_err()
    );
    assert!(
        validate_asset(&UpdateAsset {
            url: asset.url.replace("github.com", "gitee.com"),
            ..asset
        })
        .is_err()
    );
    let mut invalid = github_release("v0.2.12", Some(&format!("sha256:{}", "a".repeat(64))));
    invalid.assets[0].id = 0;
    assert!(release_from_github(invalid, "0.2.11").is_err());
}

#[test]
#[ignore = "downloads the current public MSI through the GitHub asset API without installing it"]
fn live_github_installer_asset_api_matches_release_digest() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let release = check_github(&crate::http::client(), "0.0.0")
            .await
            .unwrap()
            .unwrap();
        let id = release.asset.github_asset_id.unwrap();
        let bytes = download_installer_bytes(
            &installer_client().unwrap(),
            github::TORTO.asset_url(id),
            &release.asset,
        )
        .await
        .unwrap();
        verify_installer_bytes(&release.asset, &bytes).unwrap();
    });
}
