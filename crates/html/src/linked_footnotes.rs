//! Recognize legacy references that point to notes in a different EPUB document.
use super::*;

struct Link {
    source: String,
    target: PublicationUrl,
    marker: String,
    reference_position: bool,
    marker_style: bool,
}

fn candidate(node: Node<'_, '_>, base: &PublicationUrl) -> Option<Link> {
    if !node.has_tag_name("a") || media_return_link(node) {
        return None;
    }
    let target = base.resolve(attribute_local(node, "href")?).ok()?;
    if target.path() == base.path() || target.fragment().is_none() || target.website_url().is_some()
    {
        return None;
    }
    let source = footnote_reference_fragment(node)?.to_owned();
    let text = node_text(node);
    let marker = normalize_footnote_marker(&text)?;
    let reference_position = link_has_preceding_block_text(node);
    let marker_style = text.trim_start().starts_with(['[', '【', '(', '（'])
        || node.descendants().any(|part| part.has_tag_name("sup"))
        || node
            .ancestors()
            .skip(1)
            .take_while(|part| part.is_element() && !is_block_boundary(part.tag_name().name()))
            .any(|part| part.has_tag_name("sup"))
        || explicit_link_role(node) == Some(LinkRole::FootnoteReference);
    // A normal chapter link following prose should not trigger another read.
    let bare_marker = marker.chars().count() == 1 || marker.bytes().all(|c| c.is_ascii_digit());
    if !marker_style && (reference_position || !bare_marker) {
        return None;
    }
    Some(Link {
        source,
        target,
        marker,
        reference_position,
        marker_style,
    })
}

pub(super) fn classify(
    document: &Document<'_>,
    base: &PublicationUrl,
    load: &dyn Fn(&PublicationUrl) -> Option<String>,
    roles: &mut HashMap<usize, LinkRole>,
) {
    // Do not walk the spine: read only the targets of plausible links in the
    // current document, once each, within a fixed per-section budget.
    const MAX_DOCUMENTS: usize = 12;
    let mut linked = HashMap::<PublicationUrl, HashMap<String, Vec<Link>>>::new();
    for node in document.descendants().filter(|node| node.has_tag_name("a")) {
        let offset = node.range().start;
        if roles.contains_key(&offset) {
            continue;
        }
        let Some(link) = candidate(node, base) else {
            continue;
        };
        let path = link.target.resource_url();
        if !linked.contains_key(&path) {
            if linked.len() >= MAX_DOCUMENTS {
                continue;
            }
            let mut backlinks = HashMap::<String, Vec<Link>>::new();
            if let Some(xml) = load(&path)
                && let Ok(doc) = Document::parse(&xml)
            {
                for reverse in doc.descendants().filter_map(|node| candidate(node, &path)) {
                    if reverse.target.path() == base.path() {
                        backlinks
                            .entry(reverse.source.clone())
                            .or_default()
                            .push(reverse);
                    }
                }
            }
            linked.insert(path.clone(), backlinks);
        }
        if link
            .target
            .fragment()
            .and_then(|id| linked[&path].get(id))
            .is_some_and(|backlinks| {
                backlinks.iter().any(|back| {
                    back.target.fragment() == Some(link.source.as_str())
                        && back.marker.eq_ignore_ascii_case(&link.marker)
                        && back.reference_position != link.reference_position
                        && if link.reference_position {
                            link.marker_style
                        } else {
                            back.marker_style
                        }
                })
            })
        {
            roles.insert(
                offset,
                if link.reference_position {
                    LinkRole::FootnoteReference
                } else {
                    LinkRole::FootnoteBacklink
                },
            );
        }
    }
}
