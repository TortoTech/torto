//! Recover block boundaries hidden inside paragraph/link wrappers in EPUB XHTML.
use super::*;

pub(super) fn contains_blocks(node: Node<'_, '_>) -> bool {
    node.descendants().skip(1).any(|child| {
        child.is_element()
            && !matches!(child.tag_name().name(), "img" | "image" | "br")
            && is_block_boundary(child.tag_name().name())
    })
}

pub(super) fn ancestor_link(node: Node<'_, '_>, base: &PublicationUrl) -> Option<PublicationUrl> {
    node.ancestors()
        .filter(|n| n.is_element() && n.has_tag_name("a"))
        .find_map(|n| attribute_local(n, "href"))
        .and_then(|href| base.resolve(href).ok())
}

impl ReadingIrParser<'_> {
    pub(super) fn try_parse_nested_media(&mut self, node: Node<'_, '_>) -> Result<bool, HtmlError> {
        if !has_descendant_image(node) || !contains_blocks(node) {
            return Ok(false);
        }
        let first = self.blocks.len();
        let previous = self.inside_nested_media;
        self.inside_nested_media = true;
        let result = self.parse_block_container(node);
        self.inside_nested_media = previous;
        result?;
        let parsed = self.blocks.split_off(first);
        self.blocks.extend(group_images_and_captions(
            parsed,
            self.styles.block_style(node, BlockStyle::default()),
        ));
        Ok(true)
    }
}

fn is_caption(block: &Block) -> bool {
    matches!(block,Block::Text(text) if text.kind==TextBlockKind::Caption)
}

fn group_images_and_captions(blocks: Vec<Block>, style: BlockStyle) -> Vec<Block> {
    let mut pending = blocks.into_iter().peekable();
    let mut result = Vec::new();
    while let Some(block) = pending.next() {
        if matches!(block, Block::Image(_)) || is_caption(&block) {
            let before = is_caption(&block);
            let mut group = vec![block];
            while pending.peek().is_some_and(|b| {
                if before {
                    is_caption(b)
                } else {
                    matches!(b, Block::Image(_))
                }
            }) {
                group.push(pending.next().unwrap());
            }
            let first_part = group.len();
            while pending.peek().is_some_and(|b| {
                if before {
                    matches!(b, Block::Image(_))
                } else {
                    is_caption(b)
                }
            }) {
                group.push(pending.next().unwrap());
            }
            // With captions on both sides, retain them as separate source blocks
            // instead of assigning an arbitrary before/after order to the group.
            if group.len() == first_part || (before && pending.peek().is_some_and(is_caption)) {
                result.extend(group);
                continue;
            }
            let source = combined_block_source(&group);
            let mut images = Vec::new();
            let mut captions = Vec::new();
            for block in group {
                match block {
                    Block::Image(image) => images.push(image),
                    Block::Text(text) => captions.push(text),
                    _ => unreachable!(),
                }
            }
            result.push(Block::Figure(FigureBlock {
                images,
                captions,
                style,
                caption_position: if before {
                    CaptionPosition::Before
                } else {
                    CaptionPosition::After
                },
                source,
            }));
        } else {
            result.push(block);
        }
    }
    result
}

#[cfg(test)]
mod tests;
