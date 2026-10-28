//! Static print-page locations never force a reflowed screen to turn a page.
use super::*;

pub(super) fn is_marker(node: Node<'_, '_>) -> bool {
    [attribute_local(node, "type"), attribute_local(node, "role")]
        .into_iter()
        .flatten()
        .flat_map(str::split_ascii_whitespace)
        .any(|role| matches!(role, "pagebreak" | "doc-pagebreak"))
}

// Publishers wrap empty print locations in ordinary flow containers. The
// wrapper must be transparent too, including when a list/table is collected
// inline rather than through Parser::parse_node. Keep visiting its children so
// the location survives without introducing a paragraph break.
pub(super) fn is_empty_wrapper(node: Node<'_, '_>) -> bool {
    node.is_element()
        && matches!(node.tag_name().name(), "p" | "div" | "span")
        && node.descendants().skip(1).any(is_marker)
        && node.descendants().all(|child| {
            if child.is_text() {
                child.text().is_none_or(|text| text.trim().is_empty())
            } else {
                !child.is_element()
                    || matches!(child.tag_name().name(), "p" | "div" | "span" | "a")
                    || is_marker(child)
            }
        })
}

pub(super) fn label(node: Node<'_, '_>) -> String {
    attribute_local(node, "aria-label")
        .or_else(|| attribute_local(node, "title"))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            node.descendants()
                .filter(Node::is_text)
                .filter_map(|n| n.text())
                .collect::<String>()
                .trim()
                .to_owned()
        })
}

pub(super) fn fragment(node: Node<'_, '_>) -> String {
    attribute_local(node, "id")
        .map(str::to_owned)
        .unwrap_or_else(|| format!("torto-page-location-{}", node.range().start))
}

fn source(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(text) => text.source.as_ref(),
        Block::Image(image) => image.source.as_ref(),
        Block::Table(table) => table.source.as_ref(),
        Block::Figure(figure) => figure.source.as_ref(),
        Block::Quote(quote) => quote.source.as_ref(),
        Block::Note(note) => note.source.as_ref(),
        _ => None,
    }
}

pub(super) fn annotate(root: Node<'_, '_>, parser: &mut ReadingIrParser<'_>) {
    for node in root
        .descendants()
        .filter(Node::is_element)
        .filter(|n| is_marker(*n))
    {
        let label = label(node);
        let fragment = fragment(node);
        if let Some(anchor) = parser
            .anchors
            .iter_mut()
            .find(|anchor| anchor.fragment == fragment)
        {
            anchor.page_label = Some(label);
            // A standalone printed number is hidden in unified layout. Its
            // navigation target must resolve to actual content in both modes.
            if let Some(index) = parser.blocks.iter().position(|block| matches!(block,
                Block::Separator(separator) if separator.text.as_ref().and_then(|t| t.source.as_ref()).is_some_and(|s| s.start.node == anchor.source.node)))
            {
                if let Some(next) = parser.blocks[index + 1..].iter().find_map(source) {
                    anchor.source = next.start.clone();
                } else if let Some(previous) = parser.blocks[..index].iter().rev().find_map(source) {
                    anchor.source = previous.end.clone();
                }
            }
        } else {
            let source = parser
                .blocks
                .iter()
                .rev()
                .find_map(source)
                .map(|range| range.end.clone())
                .unwrap_or_else(|| parser.source_range("n0", 0).start);
            parser.anchors.push(SectionAnchor {
                page_label: Some(label),
                fragment,
                source,
            });
        }
    }
}

pub(super) fn has_spacing(styles: &StyleSheet, node: Node<'_, '_>) -> bool {
    let properties = styles.cascaded_properties(node);
    properties.iter().any(|(name, value)| {
        let dimension = name.starts_with("margin")
            || name.starts_with("padding")
            || matches!(
                name.as_str(),
                "width" | "height" | "min-width" | "min-height"
            );
        if dimension {
            value.split_whitespace().any(|part| {
                part.trim_end_matches(|c: char| c.is_alphabetic() || c == '%')
                    .parse::<f32>()
                    .is_ok_and(|number| number.is_finite() && number != 0.0)
            })
        } else {
            (matches!(
                name.as_str(),
                "border"
                    | "border-top"
                    | "border-bottom"
                    | "border-left"
                    | "border-right"
                    | "background"
                    | "background-color"
            )) && !value
                .split_whitespace()
                .any(|part| matches!(part, "none" | "hidden" | "transparent" | "0" | "0px"))
                && value.replace(' ', "") != "rgba(0,0,0,0)"
        }
    })
}
