use super::{
    Face,
    catalog::{Asset, Source, sources},
    store,
};
use std::{path::PathBuf, time::Duration};

pub(super) fn client() -> Result<reqwest::Client, String> {
    crate::http::builder()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .user_agent("Torto-fonts/1")
        .build()
        .map_err(|error| error.to_string())
}
pub(super) async fn fetch(
    client: &reqwest::Client,
    root: PathBuf,
    asset: Asset,
    progress: impl FnMut(u8),
) -> Result<Face, String> {
    let sources = sources(&asset);
    fetch_sources(client, root, asset, &sources, progress).await
}
pub(super) async fn fetch_sources(
    client: &reqwest::Client,
    root: PathBuf,
    asset: Asset,
    sources: &[Source],
    mut progress: impl FnMut(u8),
) -> Result<Face, String> {
    let mut last_error = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        for source in sources {
            match fetch_url(client, source, asset.size, &mut progress).await {
                Ok(bytes) => {
                    let root = root.clone();
                    let asset = asset.clone();
                    match tokio::task::spawn_blocking(move || store::save(&root, &asset, bytes))
                        .await
                    {
                        Ok(Ok(face)) => return Ok(face),
                        Ok(Err(error)) => last_error = error,
                        Err(error) => last_error = error.to_string(),
                    }
                }
                Err(error) => last_error = error,
            }
        }
    }
    Err(last_error)
}
pub(super) async fn fetch_url(
    client: &reqwest::Client,
    source: &Source,
    size: usize,
    progress: &mut impl FnMut(u8),
) -> Result<Vec<u8>, String> {
    let mut request = client.get(source.url());
    if matches!(source, Source::GitHubAsset(_)) {
        // Without this Accept header the asset API returns JSON metadata, not
        // font bytes. reqwest follows its 302 response to the binary CDN too.
        request = request
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .header("X-GitHub-Api-Version", "2026-03-10");
    }
    let response = request
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    crate::http::read_sized_body(response, size as u64, 40 * 1024 * 1024, progress).await
}
