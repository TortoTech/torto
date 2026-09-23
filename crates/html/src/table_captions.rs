//! Associate authored table captions with either grids or image carriers.
use super::{
    Block, BlockStyle, CaptionPosition, FigureBlock, HtmlError, InlineCollector,
    InlineParseContext, Node, ReadingIrParser, TextBlock, TextBlockKind, attribute_local,
    collect_table_cell_inline, has_descendant_image, node_has_visible_text, node_text,
    starts_with_caption_identifier,
};

#[cfg(test)]
mod tests;

fn semantic_table_container(node: Node<'_, '_>) -> bool {
    attribute_local(node, "class").is_some_and(|value| {
        value.split_ascii_whitespace().any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "table" | "tablegroup" | "table-group" | "table-container"
            )
        })
    })
}

fn table_label(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("table")
        .filter(|rest| {
            rest.chars().next().is_some_and(|character| {
                character.is_whitespace() || character.is_ascii_digit() || character == '.'
            })
        })
        .map(|rest| &text[text.len() - rest.len()..])
        .or_else(|| text.strip_prefix("表格"))
        .or_else(|| text.strip_prefix('表'));
    rest.is_some_and(|rest| {
        let rest = rest.trim_start().trim_start_matches('.').trim_start();
        starts_with_caption_identifier(rest)
            // A prose reference is not a title just because it starts with Table N.
            && ![" shows ", " summarizes ", " lists ", " presents ", " illustrates ", "展示了", "概括了", "所示", "列出了"].iter().any(|cue| lower.contains(cue))
    })
}

fn has_table_caption_semantics(node: Node<'_, '_>) -> bool {
    attribute_local(node, "class").is_some_and(|classes| {
        classes.split_ascii_whitespace().any(|class| {
            matches!(
                class.to_ascii_lowercase().replace(['-', '_'], "").as_str(),
                "tablecaption" | "tabletitle" | "tcaption" | "tabcaption"
            )
        })
    })
}

fn annotation(node: Node<'_, '_>, scoped: bool) -> Option<bool> {
    if !matches!(node.tag_name().name(), "p" | "div" | "caption")
        || node.descendants().skip(1).any(|child| {
            child.is_element()
                && matches!(child.tag_name().name(), "table" | "div" | "p" | "figure")
        })
    {
        return None;
    }
    let text = node_text(node);
    if text.trim().is_empty() {
        return None;
    }
    if table_label(&text) || has_table_caption_semantics(node) {
        return Some(false);
    }
    if !scoped {
        return None;
    }
    let lower = text.trim_start().to_ascii_lowercase();
    if [
        "note:",
        "notes:",
        "source:",
        "sources:",
        "注：",
        "注:",
        "来源：",
        "来源:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return Some(true);
    }
    let named = attribute_local(node, "class").is_some_and(|classes| {
        classes.split_ascii_whitespace().any(|class| {
            matches!(
                class.to_ascii_lowercase().as_str(),
                "title" | "caption" | "table-caption" | "tablecaption" | "table-title"
            )
        })
    });
    named.then_some(false)
}

// Only transparent wrappers may be traversed. Never steal a caption from a
// neighboring table group or infer a grid from a layout table's cell contents.
fn carrier(node: Node<'_, '_>) -> bool {
    if node.has_tag_name("table") {
        return true;
    }
    if matches!(node.tag_name().name(), "img" | "image") {
        return true;
    }
    if !matches!(
        node.tag_name().name(),
        "div" | "p" | "figure" | "html" | "body"
    ) {
        return false;
    }
    let elements = node
        .children()
        .filter(Node::is_element)
        .filter(|child| !ignorable(*child))
        .collect::<Vec<_>>();
    !node
        .children()
        .any(|child| child.is_text() && !child.text().unwrap_or_default().trim().is_empty())
        && elements.len() == 1
        && carrier(elements[0])
}

fn ignorable(node: Node<'_, '_>) -> bool {
    if node.has_tag_name("head") {
        return true;
    }
    if !node.is_element() {
        return !node.is_text() || node.text().unwrap_or_default().trim().is_empty();
    }
    matches!(node.tag_name().name(), "a" | "p" | "span")
        && !node_has_visible_text(node)
        && !has_descendant_image(node)
}

impl ReadingIrParser<'_> {
    pub(super) fn parse_table_annotation(
        &mut self,
        node: Node<'_, '_>,
        is_note: bool,
    ) -> Vec<TextBlock> {
        self.queue_node_anchors(node);
        self.queue_descendant_anchors(node);
        let start = self.blocks.len();
        let style = self.styles.block_style(node, BlockStyle::default());
        let mut collector = InlineCollector::new(false);
        collect_table_cell_inline(
            node,
            self.styles
                .text_style_for_block(node, TextBlockKind::Caption),
            None,
            &InlineParseContext::new(&self.section_href, &self.styles, &self.footnote_links)
                .with_inline_images(true),
            &mut collector,
        );
        self.push_collected_text_block(TextBlockKind::Caption, style, collector);
        self.blocks
            .split_off(start)
            .into_iter()
            .filter_map(|block| match block {
                Block::Text(mut text) => {
                    // Paragraph here denotes an explicit note rather than the title.
                    if is_note {
                        text.kind = TextBlockKind::Paragraph;
                    }
                    Some(text)
                }
                _ => None,
            })
            .collect()
    }

    pub(super) fn try_parse_table_caption_group(
        &mut self,
        nodes: &[Node<'_, '_>],
    ) -> Result<Option<usize>, HtmlError> {
        let scoped = nodes[0].parent().is_some_and(semantic_table_container);
        let mut index = 0;
        let mut before_nodes = Vec::new();
        while index < nodes.len() {
            let node = nodes[index];
            if ignorable(node) {
                index += 1;
                continue;
            }
            if let Some(note) = annotation(node, scoped) {
                before_nodes.push((node, note));
                index += 1;
            } else {
                break;
            }
        }
        if index == nodes.len() || !carrier(nodes[index]) {
            return Ok(None);
        }
        if !scoped
            && !before_nodes.is_empty()
            && nodes[0]
                .prev_siblings()
                .skip(1)
                .find(|node| !ignorable(*node))
                .is_some_and(carrier)
        {
            return Ok(None);
        }
        let media = nodes[index];
        index += 1;
        let mut after_nodes = Vec::new();
        let mut end = index;
        while index < nodes.len() {
            let node = nodes[index];
            if ignorable(node) {
                index += 1;
                continue;
            }
            if let Some(note) = annotation(node, scoped) {
                after_nodes.push((node, note));
                index += 1;
                end = index;
            } else {
                break;
            }
        }
        // Between two unscoped carriers, a label could belong to either one.
        if !scoped && index < nodes.len() && carrier(nodes[index]) {
            after_nodes.clear();
            end = nodes.iter().position(|node| *node == media).unwrap() + 1;
        }
        if before_nodes.is_empty() && after_nodes.is_empty() {
            return Ok(None);
        }
        // Do not infer image tables from a generic caption class alone.
        if !media.descendants().any(|node| node.has_tag_name("table"))
            && !before_nodes.iter().chain(&after_nodes).any(|(node, _)| {
                table_label(&node_text(*node)) || has_table_caption_semantics(*node)
            })
        {
            return Ok(None);
        }
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut parsed = Vec::new();
        let mut passed_media = false;
        for node in nodes[..end].iter().copied() {
            if ignorable(node) {
                if node.is_element() {
                    self.queue_node_anchors(node);
                    self.queue_descendant_anchors(node);
                }
            } else if node == media {
                let start = self.blocks.len();
                self.parse_node(media)?;
                parsed = self.blocks.split_off(start);
                passed_media = true;
            } else if let Some(note) = annotation(node, scoped) {
                let texts = self.parse_table_annotation(node, note);
                if passed_media {
                    after.extend(texts);
                } else {
                    before.extend(texts);
                }
            }
        }
        self.push_table_caption_group(parsed, before, after);
        Ok(Some(end))
    }

    fn push_table_caption_group(
        &mut self,
        mut parsed: Vec<Block>,
        mut before: Vec<TextBlock>,
        after: Vec<TextBlock>,
    ) {
        if parsed.len() == 1 && matches!(parsed[0], Block::Table(_)) {
            let Block::Table(mut table) = parsed.remove(0) else {
                unreachable!()
            };
            before.append(&mut table.before);
            table.before = before;
            table.after.extend(after);
            self.blocks.push(Block::Table(table));
        } else if parsed.len() == 1
            && matches!(parsed[0], Block::Image(_))
            && (before.is_empty() || after.is_empty())
        {
            let Block::Image(image) = parsed.remove(0) else {
                unreachable!()
            };
            let position = if before.is_empty() {
                CaptionPosition::After
            } else {
                CaptionPosition::Before
            };
            before.extend(after);
            self.blocks.push(Block::Figure(FigureBlock {
                source: image.source.clone(),
                style: BlockStyle::default(),
                images: vec![image],
                captions: before,
                caption_position: position,
            }));
        } else {
            // Preserve every parsed block if the carrier resolves unexpectedly.
            self.blocks.extend(before.into_iter().map(Block::Text));
            self.blocks.extend(parsed);
            self.blocks.extend(after.into_iter().map(Block::Text));
        }
    }
}
