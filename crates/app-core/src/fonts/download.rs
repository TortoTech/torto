use super::{
    Face,
    catalog::{Asset, Source, sources},
    store,
};
use std::{path::PathBuf, time::Duration};

pub fn client() -> Result<reqwest::Client, String> {
    crate::http::download_builder()
        .user_agent("Torto-fonts/1")
        .build()
        .map_err(|error| error.to_string())
}
pub async fn fetch(
    client: &reqwest::Client,
    root: PathBuf,
    asset: Asset,
    progress: impl FnMut(u8),
) -> Result<Face, String> {
    let sources = sources(&asset);
    fetch_sources(client, root, asset, &sources, progress).await
}
pub async fn fetch_sources(
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
pub async fn fetch_url(
    client: &reqwest::Client,
    source: &Source,
    size: usize,
    progress: &mut impl FnMut(u8),
) -> Result<Vec<u8>, String> {
    let request = if matches!(source, Source::GitHubAsset(_)) {
        // Without this Accept header the asset API returns JSON metadata, not
        // font bytes. reqwest follows its 302 response to the binary CDN too.
        crate::http::github::asset_request(client, source.url())
    } else {
        client.get(source.url())
    };
    crate::http::fetch_sized_body(request, size as u64, 40 * 1024 * 1024, progress).await
}
