use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Deserialize)]
pub struct Catalog {
    pub version: String,
    pub fonts: Vec<Asset>,
}

#[derive(Clone, Deserialize)]
pub struct Asset {
    pub id: String,
    #[serde(default)]
    pub file: String,
    pub family: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub preload: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub github_asset_id: Option<u64>,
    #[serde(default)]
    pub mirror_url: Option<String>,
    #[serde(default)]
    pub size: usize,
    #[serde(default)]
    pub sha256: String,
}

impl Asset {
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.family)
    }
}

pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../assets/fonts/manifest.json"))
            .expect("checked-in font manifest")
    })
}

pub fn display_name(family: &str) -> &str {
    if let Some(asset) = catalog().fonts.iter().find(|asset| asset.family == family) {
        return asset.label();
    }
    match family {
        "STSong" => "华文宋体",
        "STKaiti" => "华文楷体",
        "STFangsong" => "华文仿宋",
        "STHeiti" => "华文黑体",
        "STXihei" => "华文细黑",
        "STZhongsong" => "华文中宋",
        "STXingkai" => "华文行楷",
        "STXinwei" => "华文新魏",
        "STHupo" => "华文琥珀",
        "STLiti" => "华文隶书",
        "STCaiyun" => "华文彩云",
        _ => family,
    }
}

pub fn group_for_family(family: &str) -> Option<&'static str> {
    catalog()
        .fonts
        .iter()
        .find(|asset| asset.family == family)
        .map(|asset| asset.group.as_str())
}

pub fn indices_for_family(family: &str) -> Vec<usize> {
    catalog()
        .fonts
        .iter()
        .enumerate()
        .filter_map(|(index, asset)| (asset.family == family).then_some(index))
        .collect()
}

pub fn preload_indices() -> Vec<usize> {
    // Match desktop: small Latin faces first, larger CJK face last.
    let fonts = &catalog().fonts;
    let mut latin = Vec::new();
    let mut other = Vec::new();
    for (index, asset) in fonts.iter().enumerate() {
        if !asset.preload {
            continue;
        }
        if asset.group == "latin" {
            latin.push(index);
        } else {
            other.push(index);
        }
    }
    latin.extend(other);
    latin
}

pub fn cjk_families_in_group(group: &str) -> Vec<(&'static str, &'static str)> {
    let mut seen = Vec::new();
    for asset in &catalog().fonts {
        if asset.group == group && !seen.iter().any(|(family, _)| *family == asset.family) {
            seen.push((asset.family.as_str(), asset.label()));
        }
    }
    seen
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
    let mut sources = Vec::new();
    if let Some(asset_id) = asset.github_asset_id {
        sources.push(Source::GitHubAsset(
            crate::http::github::LAB.asset_url(asset_id),
        ));
    }
    if let Some(url) = &asset.mirror_url {
        sources.push(Source::Mirror(url.clone()));
    }
    sources
}
