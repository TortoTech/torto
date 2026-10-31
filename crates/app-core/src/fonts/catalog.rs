use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Deserialize)]
pub struct Catalog {
    pub version: String,
    pub fonts: [Asset; 3],
}
#[derive(Clone, Deserialize)]
pub struct Asset {
    pub id: String,
    #[cfg(test)]
    pub file: String,
    pub family: String,
    pub italic: bool,
    pub github_asset_id: u64,
    pub mirror_url: Option<String>,
    pub size: usize,
    pub sha256: String,
}
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../assets/fonts/manifest.json"))
            .expect("checked-in font manifest")
    })
}
#[derive(Debug)]
pub enum Source {
    GitHubAsset(String),
    Mirror(String),
}
impl Source {
    pub fn url(&self) -> &str {
        match self {
            Self::GitHubAsset(url) | Self::Mirror(url) => url,
        }
    }
}
pub fn sources(asset: &Asset) -> Vec<Source> {
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
