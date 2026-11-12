//! Prefer usable authored navigation. Optional navigation errors cannot make
//! intact prose unreadable. Mechanical fallbacks retain spine order.
use super::*;
use rebook_publication::TableOfContentsOrigin;

pub(super) fn auxiliary(
    archive: &EpubArchive,
    package: &PackageModel,
    opf: &str,
    base: &PublicationUrl,
) -> rebook_publication::NavigationMetadata {
    use rebook_publication::{Landmark, NavigationMetadata, TextDirection};
    let mut result = NavigationMetadata::default();
    if let Ok(doc) = Document::parse(opf) {
        result.page_progression = doc
            .descendants()
            .find(|n| n.has_tag_name("spine"))
            .and_then(|n| n.attribute("page-progression-direction"))
            .and_then(|dir| match dir {
                "ltr" => Some(TextDirection::Ltr),
                "rtl" => Some(TextDirection::Rtl),
                _ => None,
            });
        for node in doc.descendants().filter(|n| n.has_tag_name("reference")) {
            if let Some(href) = node.attribute("href").and_then(|h| base.resolve(h).ok()) {
                result.landmarks.push(Landmark {
                    kinds: token_attribute(node, "type"),
                    label: node.attribute("title").unwrap_or_default().into(),
                    href,
                });
            }
        }
    }
    let mut nav_landmarks = Vec::new();
    for item in package
        .manifest_order
        .iter()
        .filter_map(|id| package.manifest.get(id))
        .filter(|i| i.properties.iter().any(|p| p == "nav"))
    {
        let Ok(xml) = archive.read_content_xml(&item.href) else {
            continue;
        };
        let Ok(doc) = Document::parse(&xml) else {
            continue;
        };
        for nav in doc.descendants().filter(|n| n.has_tag_name("nav")) {
            let kinds = token_attribute(nav, "type");
            if kinds.iter().any(|k| k == "page-list")
                && result.page_list.is_empty()
                && let Some(list) = direct_child(nav, "ol")
            {
                result.page_list = parse_nav_list(list, &item.href).unwrap_or_default();
            }
            if kinds.iter().any(|k| k == "landmarks") {
                for a in nav.descendants().filter(|n| n.has_tag_name("a")) {
                    if let Some(href) = a.attribute("href").and_then(|h| item.href.resolve(h).ok())
                    {
                        nav_landmarks.push(Landmark {
                            kinds: token_attribute(a, "type"),
                            label: normalized_node_text(a).unwrap_or_default(),
                            href,
                        });
                    }
                }
            }
        }
    }
    if !nav_landmarks.is_empty() {
        result.landmarks = nav_landmarks;
    }
    if result.page_list.is_empty() {
        for item in package
            .manifest_order
            .iter()
            .filter_map(|id| package.manifest.get(id))
            .filter(|i| i.media_type == "application/x-dtbncx+xml")
        {
            let Ok(xml) = archive.read_xml(&item.href) else {
                continue;
            };
            let Ok(doc) = Document::parse(&xml) else {
                continue;
            };
            for target in doc.descendants().filter(|n| n.has_tag_name("pageTarget")) {
                if let Some(href) = target
                    .children()
                    .find(|n| n.has_tag_name("content"))
                    .and_then(|n| n.attribute("src"))
                    .and_then(|h| item.href.resolve(h).ok())
                {
                    result.page_list.push(TocEntry {
                        label: first_descendant_text(target, "text").unwrap_or_default(),
                        href: Some(href),
                        children: Vec::new(),
                    });
                }
            }
            if !result.page_list.is_empty() {
                break;
            }
        }
    }
    result
}

pub(super) fn read(
    archive: &EpubArchive,
    package: &PackageModel,
    spine: &[SpineItem],
    base: &PublicationUrl,
) -> (Vec<TocEntry>, TableOfContentsOrigin) {
    let items = || {
        package
            .manifest_order
            .iter()
            .filter_map(|id| package.manifest.get(id))
    };
    for nav in items().filter(|item| item.properties.iter().any(|p| p == "nav")) {
        if let Ok(xml) = archive.read_content_xml(&nav.href)
            && let Ok(toc) = parse_epub_navigation(&xml, &nav.href)
            && let Some(toc) = usable(toc, archive, package)
        {
            return (toc, TableOfContentsOrigin::Embedded);
        }
    }
    let explicit_ncx = package
        .ncx_id
        .as_ref()
        .and_then(|id| package.manifest.get(id));
    let mut tried = std::collections::HashSet::new();
    for ncx in explicit_ncx
        .into_iter()
        .chain(items().filter(|i| i.media_type == "application/x-dtbncx+xml"))
    {
        if !tried.insert(&ncx.href) {
            continue;
        }
        if let Ok(xml) = archive.read_xml(&ncx.href)
            && let Ok(toc) = parse_ncx(&xml, &ncx.href)
            && let Some(toc) = usable(toc, archive, package)
        {
            return (toc, TableOfContentsOrigin::Embedded);
        }
    }
    // EPUB 2 HTML guides are used only after both structured formats fail.
    if let Ok(xml) = archive.read_xml(base)
        && let Ok(doc) = Document::parse(&xml)
    {
        for reference in doc.descendants().filter(|n| {
            n.has_tag_name("reference") && token_attribute(*n, "type").iter().any(|t| t == "toc")
        }) {
            let Some(href) = attribute_local(reference, "href").and_then(|h| base.resolve(h).ok())
            else {
                continue;
            };
            let Ok(xml) = archive.read_content_xml(&href) else {
                continue;
            };
            let Ok(doc) = Document::parse(&xml) else {
                continue;
            };
            let toc = doc
                .descendants()
                .filter(|n| n.has_tag_name("a"))
                .filter_map(|a| {
                    Some(TocEntry {
                        label: normalized_node_text(a)?,
                        href: attribute_local(a, "href").and_then(|h| href.resolve(h).ok()),
                        children: Vec::new(),
                    })
                })
                .collect();
            if let Some(mut toc) = usable(toc, archive, package) {
                // This is an inferred flat HTML index, so retain only forward
                // reading-order destinations rather than inventing a turn loop.
                let mut previous = 0;
                toc.retain(|entry| {
                    let Some(index) = entry
                        .href
                        .as_ref()
                        .and_then(|h| spine.iter().position(|s| s.href.path() == h.path()))
                    else {
                        return false;
                    };
                    if index < previous {
                        return false;
                    }
                    previous = index;
                    true
                });
                if toc.is_empty() {
                    continue;
                }
                return (toc, TableOfContentsOrigin::Embedded);
            }
        }
    }
    // No eager chapter scan: human-readable file names give a stable, cheap
    // index until authored headings become available on section workers.
    let toc = spine
        .iter()
        .filter(|s| s.linear && archive.entries.contains_key(s.href.path()))
        .map(|s| {
            let label = s.href.path().rsplit('/').next().unwrap_or(s.id.as_str());
            let label = label.rsplit_once('.').map_or(label, |(stem, _)| stem);
            TocEntry {
                label: label.replace(['_', '-'], " "),
                href: Some(s.href.clone()),
                children: Vec::new(),
            }
        })
        .collect();
    (toc, TableOfContentsOrigin::Fallback)
}

fn usable(
    mut toc: Vec<TocEntry>,
    archive: &EpubArchive,
    package: &PackageModel,
) -> Option<Vec<TocEntry>> {
    let aliases = package
        .manifest
        .values()
        .filter_map(|item| {
            resolve_manifest_item(package, &item.id, archive)
                .map(|resolved| (item.href.path(), resolved.href.clone()))
        })
        .collect::<HashMap<_, _>>();
    fn visit(
        toc: &mut Vec<TocEntry>,
        archive: &EpubArchive,
        aliases: &HashMap<&str, PublicationUrl>,
    ) -> bool {
        let mut found = false;
        for entry in toc.iter_mut() {
            entry.href = entry.href.as_ref().and_then(|h| {
                let target = aliases.get(h.path())?;
                if !archive.entries.contains_key(target.path()) {
                    return None;
                }
                if let Some(fragment) = h.fragment() {
                    target
                        .resolve(&format!("#{}", fragment.replace('%', "%25")))
                        .ok()
                } else {
                    Some(target.clone())
                }
            });
            found |= visit(&mut entry.children, archive, aliases) || entry.href.is_some();
        }
        toc.retain(|e| e.href.is_some() || !e.children.is_empty());
        found
    }
    visit(&mut toc, archive, &aliases).then_some(toc)
}

pub(super) fn add_nonspine_targets(
    toc: &[TocEntry],
    package: &PackageModel,
    archive: &EpubArchive,
    spine: &mut Vec<SpineItem>,
) {
    fn visit(
        toc: &[TocEntry],
        package: &PackageModel,
        archive: &EpubArchive,
        spine: &mut Vec<SpineItem>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        for entry in toc {
            if let Some(href) = &entry.href
                && seen.insert(href.path().to_owned())
                && let Some(item) = package
                    .manifest
                    .values()
                    .find(|i| i.href.path() == href.path())
                && let Some(item) = resolve_manifest_item(package, &item.id, archive)
            {
                spine.push(SpineItem {
                    id: SpineItemId::new(item.id.clone()).expect("validated manifest ID"),
                    href: item.href.clone(),
                    media_type: item.media_type.clone(),
                    linear: false,
                    properties: item.properties.clone(),
                });
            }
            visit(&entry.children, package, archive, spine, seen);
        }
    }
    let mut seen = spine.iter().map(|s| s.href.path().to_owned()).collect();
    visit(toc, package, archive, spine, &mut seen);
}
