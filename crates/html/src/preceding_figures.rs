//! Bind front captions before flattening their authored sibling scope.
use super::*;
use rebook_publication::media_references::{self, MediaKind};

fn ignorable(node: Node<'_, '_>) -> bool {
    if !node.is_element() {
        return !node.is_text() || node.text().is_none_or(|text| text.trim().is_empty());
    }
    matches!(node.tag_name().name(), "a" | "span")
        && !node_has_visible_text(node)
        && !has_descendant_image(node)
}

fn caption(node: Node<'_, '_>, styles: &StyleSheet, headings: &HashSet<String>) -> bool {
    if !node.is_element() || !matches!(node.tag_name().name(), "p" | "div" | "figcaption") {
        return false;
    }
    let explicit = has_caption_semantic_attribute(node) || node.has_tag_name("figcaption");
    // Ordinary paragraphs and chapter-sized wrappers must not incur a full
    // descendant/CSS scan. A numbered figure label has a very small prefix set.
    if !explicit
        && !matches!(
            node.descendants()
                .filter(Node::is_text)
                .filter_map(|n| n.text())
                .flat_map(str::chars)
                .find(|ch| !ch.is_whitespace()),
            Some('f' | 'F' | '图' | '圖' | '插' | '▲' | '△' | '◆' | '◇')
        )
    {
        return false;
    }
    if media_semantics::protected(node, headings)
        || !(is_inferred_figure_caption(node, styles) || node.has_tag_name("figcaption"))
        || node.descendants().skip(1).any(|child| {
            child.is_element()
                && is_block_boundary(child.tag_name().name())
                && (!child.has_tag_name("img") || styles.image_establishes_block_layout(child))
        })
    {
        return false;
    }
    let text = node_text(node);
    // Table captions have a separate parser, including image-backed tables.
    if media_references::caption_label(&text).is_some_and(|r| r.key.kind == MediaKind::Table) {
        return false;
    }
    if explicit || structured_figures::numbered_caption_text(&text) {
        return true;
    }
    // A CSS keep-with-next constraint disambiguates a front label even when
    // its longer description contains words such as "shows". Do not rely on
    // publisher class names or broaden the ordinary-prose fallback.
    let properties = styles.cascaded_properties(node);
    ["break-after", "page-break-after"].into_iter().any(|key| {
        properties
            .get(key)
            .is_some_and(|value| matches!(value.as_str(), "avoid" | "avoid-page" | "avoid-column"))
    }) && media_references::caption_label(&text).is_some_and(|r| r.key.kind == MediaKind::Figure)
}

impl ReadingIrParser<'_> {
    pub(super) fn try_parse_preceding_image_figure(
        &mut self,
        nodes: &[Node<'_, '_>],
    ) -> Result<Option<usize>, HtmlError> {
        let Some(&first) = nodes.first() else {
            return Ok(None);
        };
        if !caption(first, &self.styles, &self.heading_fragments) {
            return Ok(None);
        }
        let mut siblings = nodes
            .iter()
            .copied()
            .enumerate()
            .skip(1)
            .filter(|(_, node)| !ignorable(*node))
            .peekable();
        // Only explicit, unnumbered continuation captions may extend the first
        // label. Another numbered caption belongs to a separate figure.
        while siblings.peek().is_some_and(|(_, node)| {
            caption(*node, &self.styles, &self.heading_fragments)
                && (has_caption_semantic_attribute(*node) || node.has_tag_name("figcaption"))
                && media_references::caption_label(&node_text(*node)).is_none()
        }) {
            siblings.next();
        }
        let Some((image_start, image)) = siblings.next() else {
            return Ok(None);
        };
        if !is_captionable_image_container(image, &self.footnote_links) {
            return Ok(None);
        }
        let mut end = image_start + 1;
        for (index, node) in siblings {
            if !is_captionable_image_container(node, &self.footnote_links) {
                break;
            }
            end = index + 1;
        }
        // Parse exactly once in source order, including empty anchor elements.
        // Do not allocate replacement source IDs while discovering the group.
        let start = self.blocks.len();
        for node in nodes[..image_start]
            .iter()
            .copied()
            .filter(Node::is_element)
        {
            if node.has_tag_name("figcaption") {
                self.queue_node_anchors(node);
                self.push_text_block(
                    node,
                    TextBlockKind::Paragraph,
                    self.styles.block_style(node, BlockStyle::default()),
                )?;
            } else {
                self.parse_node(node)?;
            }
        }
        let split = self.blocks.len();
        for node in nodes[image_start..end]
            .iter()
            .copied()
            .filter(Node::is_element)
        {
            self.parse_node(node)?;
        }
        let captions = &self.blocks[start..split];
        let images = &self.blocks[split..];
        if !captions.is_empty()
            && !images.is_empty()
            && captions.iter().all(|b| {
                matches!(b, Block::Text(t)
                if matches!(t.kind, TextBlockKind::Paragraph | TextBlockKind::Caption))
            })
            && images.iter().all(|b| matches!(b, Block::Image(_)))
        {
            for block in &mut self.blocks[start..split] {
                let Block::Text(text) = block else {
                    unreachable!()
                };
                text.kind = TextBlockKind::Caption;
            }
            let parsed = self.blocks.split_off(start);
            self.blocks.extend(nested_media::group_images_and_captions(
                parsed,
                BlockStyle::default(),
            ));
        }
        // Decorative/hidden media and explicit quotations retain their parsed
        // blocks instead of being discarded or forced into a figure.
        Ok(Some(end))
    }
}

#[cfg(test)]
mod tests;
