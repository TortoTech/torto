use super::{
    Face,
    catalog::{Asset, urls},
    store,
};
use futures_util::StreamExt;
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
    mut progress: impl FnMut(u8),
) -> Result<Face, String> {
    let mut last_error = String::new();
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        for url in urls(&asset) {
            match fetch_url(client, &url, asset.size, &mut progress).await {
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
    url: &str,
    size: usize,
    progress: &mut impl FnMut(u8),
) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length != size as u64)
    {
        return Err("字体下载大小不符 / Unexpected download size".into());
    }
    let mut bytes = Vec::with_capacity(size);
    let mut stream = response.bytes_stream();
    let mut previous = 0;
    progress(0);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        if chunk.len() > size.saturating_sub(bytes.len()) {
            return Err("字体下载超出大小限制 / Font download exceeds size limit".into());
        }
        bytes.extend_from_slice(&chunk);
        let percent = (bytes.len() * 100 / size.max(1)).min(99) as u8;
        if percent >= previous + 5 {
            previous = percent;
            progress(percent);
        }
    }
    if bytes.len() != size {
        return Err("字体下载不完整 / Incomplete font download".into());
    }
    Ok(bytes)
}
