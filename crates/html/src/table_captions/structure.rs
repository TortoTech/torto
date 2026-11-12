//! Structural evidence shared by caption association; no publisher class names.
use super::*;
use rebook_publication::media_references::{MediaKind, caption_label};

pub(super) fn next_visible<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    node.next_siblings().skip(1).find(|node| !ignorable(*node))
}

pub(super) fn previous_visible<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    node.prev_siblings().skip(1).find(|node| !ignorable(*node))
}

pub(super) fn leaf<'a, 'input>(mut node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    loop {
        if !node.is_element() {
            return None;
        }
        if ["role", "type"]
            .iter()
            .filter_map(|name| attribute_local(node, name))
            .flat_map(str::split_ascii_whitespace)
            .any(|role| {
                matches!(
                    role,
                    "footnote" | "endnote" | "doc-footnote" | "doc-endnote"
                )
            })
        {
            return None;
        }
        if !matches!(
            node.tag_name().name(),
            "p" | "div" | "caption" | "section" | "aside" | "footer"
        ) {
            return None;
        }
        if !node.descendants().skip(1).any(|child| {
            child.is_element()
                && matches!(
                    child.tag_name().name(),
                    "div"
                        | "p"
                        | "section"
                        | "aside"
                        | "footer"
                        | "table"
                        | "figure"
                        | "blockquote"
                        | "pre"
                        | "ul"
                        | "ol"
                        | "dl"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                )
        }) {
            return node_has_visible_text(node).then_some(node);
        }
        let children: Vec<_> = node
            .children()
            .filter(Node::is_element)
            .filter(|child| !ignorable(*child))
            .collect();
        if children.len() != 1
            || node
                .children()
                .any(|child| child.is_text() && !child.text().unwrap_or_default().trim().is_empty())
        {
            return None;
        }
        node = children[0];
    }
}

pub(super) fn note_prefix(text: &str) -> bool {
    rebook_publication::media_notes::is_annotation(text)
}

/// Multiple unmarked continuations require a dedicated, text-only container
/// whose first paragraph introduces the note. Keep all paragraphs independent.
pub(super) fn note_parts<'a, 'input>(node: Node<'a, 'input>) -> Option<Vec<Node<'a, 'input>>> {
    if ["role", "type"]
        .iter()
        .filter_map(|name| attribute_local(node, name))
        .flat_map(str::split_ascii_whitespace)
        .any(|role| {
            matches!(
                role,
                "footnote" | "endnote" | "doc-footnote" | "doc-endnote"
            )
        })
    {
        return None;
    }
    if let Some(leaf) = leaf(node) {
        return note_prefix(&node_text(leaf)).then_some(vec![leaf]);
    }
    if !matches!(
        node.tag_name().name(),
        "div" | "aside" | "footer" | "section"
    ) || node
        .children()
        .any(|child| child.is_text() && !child.text().unwrap_or_default().trim().is_empty())
    {
        return None;
    }
    let children: Vec<_> = node
        .children()
        .filter(Node::is_element)
        .filter(|child| !ignorable(*child))
        .collect();
    let parts = children.into_iter().map(leaf).collect::<Option<Vec<_>>>()?;
    if parts.is_empty()
        || !note_prefix(&node_text(parts[0]))
        || parts
            .iter()
            .skip(1)
            .any(|part| table_label(&node_text(*part)))
    {
        return None;
    }
    Some(parts)
}

pub(super) fn scoped(parent: Node<'_, '_>) -> bool {
    if !matches!(
        parent.tag_name().name(),
        "div" | "section" | "figure" | "aside" | "footer"
    ) {
        return false;
    }
    let mut carriers = 0;
    let mut numbered = false;
    let mut plain = 0;
    for child in parent.children().filter(|node| !ignorable(*node)) {
        if carrier(child) {
            carriers += 1;
            if carriers > 1 {
                return false;
            }
        } else if let Some(leaf) = leaf(child) {
            let text = node_text(leaf);
            if table_label(&text) || has_table_caption_semantics(leaf) {
                numbered = true;
            } else if !note_prefix(&text) {
                plain += 1;
                // A chapter-sized paragraph container is not a table scope.
                // Stop after the second prose paragraph instead of rescanning
                // its complete sibling list for every parser callback.
                if plain > 1 {
                    return false;
                }
            }
        } else if note_parts(child).is_none() {
            return false;
        }
    }
    carriers == 1 && (plain == 0 || numbered && plain == 1)
}

pub(super) fn number(text: &str) -> Option<String> {
    if !table_label(text) {
        return None;
    }
    caption_label(text)
        .filter(|label| label.key.kind == MediaKind::Table)
        .map(|label| label.key.number)
}

pub(super) fn targets(caption: Node<'_, '_>, media: Node<'_, '_>) -> bool {
    caption
        .descendants()
        .filter(Node::is_element)
        .filter_map(|node| attribute_local(node, "href"))
        .filter_map(|href| href.strip_prefix('#'))
        .filter(|id| !id.is_empty())
        .any(|id| {
            media
                .descendants()
                .filter(Node::is_element)
                .any(|node| attribute_local(node, "id") == Some(id))
        })
}

pub(super) fn previous_number(block: &Block) -> Option<String> {
    let texts: Vec<_> = match block {
        Block::Table(table) => table.before.iter().chain(&table.after).collect(),
        Block::Figure(figure) => figure.text_blocks().collect(),
        _ => return None,
    };
    texts.into_iter().find_map(|text| {
        let value: String = text
            .content
            .iter()
            .flat_map(|inline| inline.text_runs())
            .map(|run| run.text.as_str())
            .collect();
        number(&value)
    })
}

pub(super) fn previous_media(blocks: &[Block]) -> bool {
    // Notes and a caption may intervene, but ordinary prose and headings are
    // hard boundaries. Bound this local look-back independently of book size.
    for block in blocks.iter().rev().take(12) {
        match block {
            Block::Image(_) | Block::Figure(_) | Block::Table(_) => return true,
            Block::Note(note) if note.kind == super::super::NoteBlockKind::Definition => {}
            Block::Text(text)
                if !matches!(
                    text.kind,
                    TextBlockKind::Heading(_) | TextBlockKind::HeadingOrdinal(_)
                ) =>
            {
                let value: String = text
                    .content
                    .iter()
                    .flat_map(|inline| inline.text_runs())
                    .map(|run| run.text.as_str())
                    .collect();
                if text.kind != TextBlockKind::Caption
                    && !table_label(&value)
                    && !super::super::structured_figures::numbered_caption_text(&value)
                    && !rebook_publication::media_notes::is_annotation(&value)
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    false
}

/// A superscript definition belongs to a table only when its marker also
/// occurs in that table's cells. Typography alone is never sufficient.
pub(super) fn marker_note(node: Node<'_, '_>, media: Node<'_, '_>) -> bool {
    let Some(node) = leaf(node) else {
        return false;
    };
    let Some(first) = node
        .descendants()
        .filter(Node::is_text)
        .find(|text| !text.text().unwrap_or_default().trim().is_empty())
    else {
        return false;
    };
    let Some(marker) = first
        .ancestors()
        .take_while(|parent| *parent != node)
        .find(|parent| parent.has_tag_name("sup"))
    else {
        return false;
    };
    let key = node_text(marker);
    let key = key.trim();
    !key.is_empty()
        && key.chars().count() <= 4
        && node_text(node).trim() != key
        && media
            .descendants()
            .filter(|part| part.has_tag_name("sup"))
            .any(|part| node_text(part).trim() == key)
}

pub(super) fn native(media: Node<'_, '_>) -> bool {
    media.descendants().any(|part| part.has_tag_name("table"))
}

pub(super) fn following_carrier<'a, 'input>(caption: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    let next = next_visible(caption)?;
    if carrier(next) {
        return Some(next);
    }
    if standalone_table_label(caption) && standalone_caption_text(next) {
        return next_visible(next).filter(|node| carrier(*node));
    }
    None
}

/// At most two neighboring pairs establish repeated post-table titles. A
/// leading title already attached to the current grid takes precedence.
pub(super) fn suffix_pattern(mut caption: Node<'_, '_>) -> bool {
    let Some(mut key) = number(&node_text(caption)) else {
        return false;
    };
    for _ in 0..2 {
        let Some(media) = next_visible(caption).filter(|node| carrier(*node)) else {
            return false;
        };
        let Some(next) = next_visible(media) else {
            return false;
        };
        let Some(next_key) = number(&node_text(next)) else {
            return false;
        };
        if next_key == key {
            return false;
        }
        if next_visible(next).is_none_or(|node| !carrier(node)) {
            return true;
        }
        key = next_key;
        caption = next;
    }
    false
}
