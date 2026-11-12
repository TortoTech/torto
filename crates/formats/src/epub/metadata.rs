use super::*;
use rebook_publication::{Contributor, MetadataDetails, MetadataTitle};

pub(super) fn details(metadata: Option<Node<'_, '_>>) -> (Vec<String>, MetadataDetails) {
    let Some(metadata) = metadata else {
        return (Vec::new(), MetadataDetails::default());
    };
    let metas = metadata
        .children()
        .filter(|n| n.has_tag_name("meta"))
        .collect::<Vec<_>>();
    let refined = |node: Node<'_, '_>, property: &str| -> Option<String> {
        let id = node.attribute("id")?;
        metas
            .iter()
            .find(|n| {
                n.attribute("property") == Some(property)
                    && n.attribute("refines").and_then(|s| s.strip_prefix('#')) == Some(id)
            })
            .and_then(|n| normalized_node_text(*n))
    };
    let legacy = |name| {
        metas
            .iter()
            .find(|n| n.attribute("name") == Some(name))
            .and_then(|n| n.attribute("content"))
            .map(str::to_owned)
    };
    let mut authors = Vec::new();
    let mut contributors = Vec::new();
    for node in metadata
        .children()
        .filter(|n| matches!(n.tag_name().name(), "creator" | "contributor"))
    {
        let Some(name) = normalized_node_text(node) else {
            continue;
        };
        let mut roles = node
            .attribute("id")
            .map(|id| {
                metas
                    .iter()
                    .filter(|n| {
                        n.attribute("property") == Some("role")
                            && n.attribute("refines").and_then(|s| s.strip_prefix('#')) == Some(id)
                    })
                    .filter_map(|n| normalized_node_text(*n))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if roles.is_empty()
            && let Some(role) = attribute_local(node, "role")
        {
            roles.push(role.into());
        }
        let author = roles
            .iter()
            .any(|role| role.rsplit(['/', ':']).next() == Some("aut"))
            || roles.is_empty() && node.has_tag_name("creator");
        if author && !authors.contains(&name) {
            authors.push(name.clone());
        }
        if roles.is_empty() && node.has_tag_name("creator") {
            roles.push("aut".into());
        }
        contributors.push(Contributor {
            name,
            roles,
            sort_as: refined(node, "file-as")
                .or_else(|| attribute_local(node, "file-as").map(str::to_owned)),
        });
    }
    let titles = metadata
        .children()
        .filter(|n| n.has_tag_name("title"))
        .filter_map(|n| {
            Some(MetadataTitle {
                value: normalized_node_text(n)?,
                kind: refined(n, "title-type"),
                language: attribute_local(n, "lang").map(str::to_owned),
            })
        })
        .collect();
    let series = metas.iter().find(|n| {
        n.attribute("property") == Some("belongs-to-collection")
            && refined(**n, "collection-type").as_deref() == Some("series")
    });
    let details = MetadataDetails {
        titles,
        contributors,
        identifiers: descendant_texts(metadata, "identifier"),
        publisher: first_descendant_text(metadata, "publisher"),
        description: first_descendant_text(metadata, "description"),
        subjects: descendant_texts(metadata, "subject"),
        published: first_descendant_text(metadata, "date"),
        series: series
            .and_then(|n| normalized_node_text(*n))
            .or_else(|| legacy("calibre:series")),
        series_position: series
            .and_then(|n| refined(*n, "group-position"))
            .or_else(|| legacy("calibre:series_index")),
    };
    (authors, details)
}
