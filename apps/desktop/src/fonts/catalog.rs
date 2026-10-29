use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Deserialize)]
pub(super) struct Catalog {
    pub(super) version: String,
    source_revision: String,
    mirror_revision: String,
    pub(super) fonts: [Asset; 3],
}
#[derive(Clone, Deserialize)]
pub(super) struct Asset {
    pub(super) id: String,
    pub(super) file: String,
    pub(super) family: String,
    pub(super) italic: bool,
    fallback_url: Option<String>,
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
pub(super) fn urls(asset: &Asset) -> [String; 2] {
    // Immutable, already-published Torto resources, independent of app releases.
    let revision = &catalog().source_revision;
    [
        format!(
            "https://raw.githubusercontent.com/TortoTech/torto/{revision}/assets/fonts/{}",
            asset.file
        ),
        asset.fallback_url.clone().unwrap_or_else(|| {
            format!(
                "https://gitee.com/TortoTech/torto/raw/{}/assets/fonts/{}",
                catalog().mirror_revision,
                asset.file
            )
        }),
    ]
}
