//! Media relationships depend on authored structure and resolved CSS, never selector names.
use super::*;

fn protected(node: Node<'_, '_>, headings: &HashSet<String>) -> bool {
    node.descendants().filter(Node::is_element).any(|n| {
        matches!(n.tag_name().name(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
            || node_fragment(n).is_some_and(|id| headings.contains(id))
            || is_semantic_footnote_definition(n)
    })
}

fn text_leaf(node: Node<'_, '_>) -> bool {
    node.is_element()
        && matches!(node.tag_name().name(), "p" | "div" | "figcaption")
        && node_has_visible_text(node)
        && !has_descendant_image(node)
        && !node
            .descendants()
            .skip(1)
            .any(|n| n.is_element() && is_block_boundary(n.tag_name().name()))
}

fn avoids_split(node: Node<'_, '_>, styles: &StyleSheet) -> bool {
    let properties = styles.cascaded_properties(node);
    ["break-inside", "page-break-inside"]
        .into_iter()
        .any(|key| {
            properties.get(key).is_some_and(|value| {
                matches!(value.as_str(), "avoid" | "avoid-page" | "avoid-column")
            })
        })
}

fn links_to_image(caption: Node<'_, '_>, image: Node<'_, '_>) -> bool {
    caption
        .descendants()
        .filter(|n| n.has_tag_name("a"))
        .filter_map(|n| attribute_local(n, "href").and_then(|href| href.strip_prefix('#')))
        .any(|fragment| {
            image
                .descendants()
                .filter(Node::is_element)
                .any(|n| node_fragment(n) == Some(fragment))
        })
}

fn constrained_media_box(image: Node<'_, '_>, styles: &StyleSheet) -> bool {
    let properties = styles.cascaded_properties(image);
    let sized = ["width", "max-width"].into_iter().any(|key| {
        properties
            .get(key)
            .and_then(|value| image_length(value))
            .is_some()
    });
    sized
        && (styles.inherited_text_alignment(image) == Some(TextAlignment::Center)
            || properties
                .get("margin")
                .is_some_and(|margin| margin.split_whitespace().any(|v| v == "auto"))
            || properties.get("margin-left").is_some_and(|v| v == "auto")
                && properties.get("margin-right").is_some_and(|v| v == "auto"))
}

pub(super) fn inferred_figure_caption_sibling(
    siblings: &[Node<'_, '_>],
    image_index: usize,
    footnote_links: &HashMap<usize, LinkRole>,
    styles: &StyleSheet,
    headings: &HashSet<String>,
) -> Option<std::ops::Range<usize>> {
    let image = *siblings.get(image_index)?;
    if !is_captionable_image_container(image, footnote_links) {
        return None;
    }
    let mut after = siblings
        .iter()
        .copied()
        .enumerate()
        .skip(image_index + 1)
        .filter(|(_, n)| !n.is_text() || n.text().is_some_and(|t| !t.trim().is_empty()))
        .filter(|(_, n)| n.is_element() || n.is_text());
    let (first_index, first) = after.next()?;
    if protected(first, headings) {
        return None;
    }
    if is_inferred_figure_caption(first, styles) || first.has_tag_name("figcaption") {
        let mut end = first_index + 1;
        for (index, next) in after {
            if protected(next, headings)
                || !is_inferred_figure_caption(next, styles)
                || !has_caption_semantic_attribute(next)
            {
                break;
            }
            end = index + 1;
        }
        return Some(first_index..end);
    }

    // Explicit links to the adjacent image also establish an association. This
    // covers diagram explanations without a figure tag or pagination constraint.
    let parent = image.parent()?;
    let linked = links_to_image(first, image);
    let isolated = matches!(parent.tag_name().name(), "div" | "aside")
        && (avoids_split(parent, styles) || styles.has_visual_boundary(parent));
    let media_box = constrained_media_box(image, styles);
    if !linked && !isolated && !media_box {
        return None;
    }
    let after = std::iter::once((first_index, first))
        .chain(after)
        .take_while(|(_, n)| text_leaf(*n) && !protected(*n, headings))
        .collect::<Vec<_>>();
    if after.is_empty() {
        return None;
    }
    let parent_style = styles.text_style_for_block(parent, TextBlockKind::Paragraph);
    let body_start = usize::from(
        styles
            .text_style_for_block(first, TextBlockKind::Paragraph)
            .size_scale
            > parent_style.size_scale * 1.04,
    );
    if body_start == after.len() || (!linked && !isolated && body_start == 0) {
        return None;
    }
    let bodies = &after[body_start..];
    let count = bodies
        .iter()
        .take_while(|(_, n)| {
            styles
                .text_style_for_block(*n, TextBlockKind::Paragraph)
                .size_scale
                < parent_style.size_scale * 0.96
        })
        .count();
    if count == 0 {
        return None;
    }
    if after[..body_start + count].iter().any(|(_, n)| {
        n.descendants()
            .any(|d| footnote_links.get(&d.range().start) == Some(&LinkRole::FootnoteBacklink))
    }) {
        return None;
    }
    Some(first_index..after[body_start + count - 1].0 + 1)
}

pub(super) fn is_navigation_breadcrumb(
    node: Node<'_, '_>,
    base: &PublicationUrl,
    roles: &HashMap<usize, LinkRole>,
    navigation_documents: &HashSet<String>,
    ancestor_targets: &HashSet<String>,
    styles: &StyleSheet,
    headings: &HashSet<String>,
) -> bool {
    if navigation_documents.is_empty()
        || ancestor_targets.is_empty()
        || !node.is_element()
        || !matches!(
            node.tag_name().name(),
            "div" | "p" | "nav" | "aside" | "header"
        )
        || has_descendant_image(node)
        || node.descendants().any(|n| {
            n.is_element()
                && matches!(
                    n.tag_name().name(),
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "figure" | "ol" | "ul"
                )
        })
    {
        return false;
    }
    // A visible authored TOC remains content. Only the compact ancestor trail is suppressed.
    if node.ancestors().filter(Node::is_element).any(|n| {
        attribute_local(n, "type").is_some_and(|k| k.split_whitespace().any(|k| k == "toc"))
            || attribute_local(n, "role") == Some("doc-toc")
    }) {
        return false;
    }
    let links = node
        .descendants()
        .filter(|n| n.has_tag_name("a"))
        .collect::<Vec<_>>();
    if links.len() < 2 {
        return false;
    }
    let mut has_navigation = false;
    let mut has_ancestor = false;
    for link in links {
        if roles
            .get(&link.range().start)
            .is_some_and(|role| *role != LinkRole::Normal)
        {
            return false;
        }
        let Some(target) = attribute_local(link, "href").and_then(|href| base.resolve(href).ok())
        else {
            return false;
        };
        let nav = navigation_documents.contains(target.path());
        let ancestor = ancestor_targets.contains(&target.to_string());
        if !nav && !ancestor {
            return false;
        }
        has_navigation |= nav;
        has_ancestor |= ancestor;
    }
    if !has_navigation || !has_ancestor {
        return false;
    }
    // A resource can contain several TOC units. A trail at the beginning of a
    // later unit is just as valid as a chapter-leading trail, but closing reading
    // suggestions after prose must remain visible.
    let before_heading = node
        .next_siblings()
        .skip(1)
        .find(|n| n.is_element() && node_has_visible_text(*n))
        .is_some_and(|n| starts_with_heading(n, headings));
    if !before_heading {
        let mut branch = node;
        while let Some(parent) = branch.parent().filter(Node::is_element) {
            if branch.prev_siblings().skip(1).any(|n| {
                n.is_element()
                    && !matches!(n.tag_name().name(), "style" | "script" | "link" | "meta")
                    && node_has_visible_text(n)
            }) {
                return false;
            }
            if parent.has_tag_name("body") || parent.has_tag_name("section") {
                break;
            }
            branch = parent;
        }
    }
    node.descendants().filter(Node::is_text).all(|text| {
        let value = text.text().unwrap_or_default().trim();
        value.is_empty() || value.chars().all(|c| !c.is_alphanumeric())
            || text.ancestors().take_while(|n| *n != node).any(|n| {
                n.has_tag_name("a") || attribute_local(n, "aria-hidden") == Some("true")
                    // One isolated glyph between known navigation links is a
                    // separator even if its symbol font maps an ASCII letter.
                    || (n.has_tag_name("span") && node_text(n).trim().chars().count() == 1
                        && styles.cascaded_properties(n).get("font-family").is_some_and(|family| {
                            !matches!(family.as_str(), "inherit" | "initial" | "unset")
                                && n.parent().is_none_or(|parent| styles.cascaded_properties(parent).get("font-family") != Some(family))
                        }))
            })
    })
}

fn starts_with_heading(mut node: Node<'_, '_>, headings: &HashSet<String>) -> bool {
    loop {
        if matches!(
            node.tag_name().name(),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        ) || node_fragment(node).is_some_and(|id| headings.contains(id))
        {
            return true;
        }
        let Some(child) = node.children().find(|n| {
            n.is_element() && node_has_visible_text(*n)
                || n.is_text() && n.text().is_some_and(|text| !text.trim().is_empty())
        }) else {
            return false;
        };
        if !child.is_element() {
            return false;
        }
        node = child;
    }
}
