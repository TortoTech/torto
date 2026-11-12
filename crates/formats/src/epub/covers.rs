//! Ordered, bounded cover hints. An explicitly authored composite cover page
//! takes precedence over filename guesses; random first images are never used.
use super::*;

pub(super) fn candidates(
    archive: &EpubArchive,
    package: &PackageModel,
    xml: &str,
    base: &PublicationUrl,
    allow_names: bool,
) -> Vec<PublicationUrl> {
    let mut result = Vec::new();
    let mut push = |href: PublicationUrl| {
        if result.len() < 32 && archive.entries.contains_key(href.path()) && !result.contains(&href)
        {
            result.push(href);
        }
    };
    for item in package
        .manifest_order
        .iter()
        .filter_map(|id| package.manifest.get(id))
        .filter(|item| item.properties.iter().any(|p| p == "cover-image"))
    {
        push(item.href.clone());
    }
    if let Ok(doc) = Document::parse(xml) {
        for meta in doc
            .descendants()
            .filter(|n| n.has_tag_name("meta") && n.attribute("name") == Some("cover"))
        {
            let Some(value) = meta.attribute("content") else {
                continue;
            };
            if let Some(item) = package.manifest.get(value) {
                push(item.href.clone());
            }
            if let Ok(href) = base.resolve(value) {
                push(href.resource_url());
            }
        }
    }
    if allow_names {
        for item in package
            .manifest_order
            .iter()
            .filter_map(|id| package.manifest.get(id))
        {
            if item.media_type.starts_with("image/")
                && (item.id.eq_ignore_ascii_case("cover") || cover_named(item.href.path()))
            {
                push(item.href.clone());
            }
        }
        let mut names = archive
            .entries
            .keys()
            .filter(|path| cover_named(path))
            .collect::<Vec<_>>();
        names.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        for name in names.into_iter().take(32) {
            if let Ok(href) = PublicationUrl::from_archive_path(name) {
                push(href);
            }
        }
    }
    result
}

fn cover_named(path: &str) -> bool {
    if !guess_media_type(path).starts_with("image/") {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
    matches!(
        stem,
        "cover" | "frontcover" | "front-cover" | "coverimage" | "cover-image" | "封面"
    ) || stem.strip_prefix("cover").is_some_and(|s| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '-' | '_'))
    })
}
