use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Deserialize)]
pub(super) struct Catalog {
    pub(super) version: String,
    pub(super) fonts: [Asset; 3],
}
#[derive(Clone, Deserialize)]
pub(super) struct Asset {
    pub(super) id: String,
    #[cfg(test)]
    pub(super) file: String,
    pub(super) family: String,
    pub(super) italic: bool,
    pub(super) github_asset_id: u64,
    pub(super) mirror_url: Option<String>,
    pub(super) size: usize,
    pub(super) sha256: String,
}
pub(super) fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../assets/fonts/manifest.json"))
            .expect("checked-in font manifest")
    })
}
#[derive(Debug)]
pub(super) enum Source {
    GitHubAsset(String),
    Mirror(String),
}
impl Source {
    pub(super) fn url(&self) -> &str {
        match self {
            Self::GitHubAsset(url) | Self::Mirror(url) => url,
        }
    }
}
pub(super) fn sources(asset: &Asset) -> Vec<Source> {
    // IDs pin immutable font resources independently of application releases.
    // A mirror is added only after its anonymous download has been verified.
    let mut sources = vec![Source::GitHubAsset(
        crate::http::github::LAB.asset_url(asset.github_asset_id),
    )];
    if let Some(url) = &asset.mirror_url {
        sources.push(Source::Mirror(url.clone()));
    }
    sources
}
