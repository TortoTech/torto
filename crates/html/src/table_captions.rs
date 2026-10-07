//! Associate authored table captions with either grids or image carriers.
use super::{
    Block, BlockStyle, CaptionPosition, FigureBlock, HtmlError, InlineCollector,
    InlineParseContext, Node, ReadingIrParser, TableRow, TextAlignment, TextBlock, TextBlockKind,
    attribute_local, collect_table_cell_inline, has_descendant_image, node_has_visible_text,
    node_text, starts_with_caption_identifier,
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

fn standalone_table_label(node: Node<'_, '_>) -> bool {
    let text = node_text(node).trim().to_ascii_lowercase();
    let Some(number) = text
        .strip_prefix("table")
        .or_else(|| text.strip_prefix("表格"))
        .or_else(|| text.strip_prefix('表'))
    else {
        return false;
    };
    table_label(&text)
        && number.chars().any(|c| c.is_ascii_digit())
        && number
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_whitespace() || ".-–()（）:：".contains(c))
}

fn standalone_caption_text(node: Node<'_, '_>) -> bool {
    matches!(node.tag_name().name(), "p" | "div")
        && !node_text(node).trim().is_empty()
        && !node.descendants().skip(1).any(|child| {
            child.is_element()
                && matches!(
                    child.tag_name().name(),
                    "p" | "div" | "table" | "figure" | "img" | "image" | "br"
                )
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
    /// Mark the authored title without moving it: book-mode rendering and
    /// translation segment identities still use the complete original grid.
    pub(super) fn mark_table_title_row(&self, table: Node<'_, '_>, rows: &mut [TableRow]) {
        let Some(first) = rows.first().filter(|row| row.cells.len() == 1) else {
            return;
        };
        let title = &first.cells[0];
        let Some(columns) = rows.get(1).filter(|row| row.cells.len() > 1) else {
            return;
        };
        let width = grid_width(&rows[1..]);
        if title.row_span != 1 || width < 2 || usize::from(title.column_span) < width {
            return;
        }
        let text: String = title
            .text
            .content
            .iter()
            .flat_map(|inline| inline.text_runs())
            .map(|run| run.text.as_str())
            .collect();
        if !text.chars().any(char::is_alphabetic) || text.chars().count() > 1024 {
            return;
        }
        let lower = text.trim().to_lowercase();
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
            return;
        }
        let mut source_rows = table.descendants().filter(|node| {
            node.has_tag_name("tr")
                && node
                    .children()
                    .any(|cell| cell.has_tag_name("td") || cell.has_tag_name("th"))
                && node
                    .ancestors()
                    .skip(1)
                    .find(|parent| parent.has_tag_name("table"))
                    == Some(table)
        });
        let Some(row) = source_rows.next() else {
            return;
        };
        let scoped = table.parent().is_some_and(semantic_table_container);
        let named = row.descendants().filter(Node::is_element).any(|node| {
            has_table_caption_semantics(node)
                || scoped
                    && attribute_local(node, "class").is_some_and(|classes| {
                        classes
                            .split_ascii_whitespace()
                            .any(|class| class.eq_ignore_ascii_case("table"))
                    })
        });
        let labels = columns.cells.iter().all(|cell| {
            let text: String = cell
                .text
                .content
                .iter()
                .flat_map(|inline| inline.text_runs())
                .map(|run| run.text.as_str())
                .collect();
            !text.trim().is_empty()
                && text.chars().count() <= 120
                && text.chars().any(char::is_alphabetic)
        });
        let header = row
            .parent()
            .is_some_and(|parent| parent.has_tag_name("thead"));
        // The cell can inherit justification while its actual title paragraph
        // is centered. Ignore empty wrappers and use a paragraph covering all
        // visible cell text, without changing the authored grid presentation.
        let content_alignment = |cell: Node<'_, '_>, fallback| {
            let visible = node_text(cell);
            cell.descendants()
                .skip(1)
                .filter(|node| node.has_tag_name("p") || node.has_tag_name("div"))
                .find_map(|node| {
                    let content = node_text(node);
                    content
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .eq(visible.chars().filter(|c| !c.is_whitespace()))
                        .then(|| self.styles.declared_text_alignment(node))
                        .flatten()
                })
                .or(fallback)
        };
        let source_cell = row
            .children()
            .find(|node| node.has_tag_name("td") || node.has_tag_name("th"));
        let title_alignment =
            source_cell.and_then(|cell| content_alignment(cell, title.authored_alignment));
        let distinct_centered = title_alignment == Some(TextAlignment::Center)
            && source_rows.next().is_some_and(|row| {
                row.children()
                    .filter(|node| node.has_tag_name("td") || node.has_tag_name("th"))
                    .zip(&columns.cells)
                    .all(|(node, cell)| {
                        content_alignment(node, cell.authored_alignment)
                            != Some(TextAlignment::Center)
                    })
            });
        // Repeated spanning rows usually denote data groups, not one table title.
        let repeated_groups = rows
            .iter()
            .skip(2)
            .any(|row| row.cells.len() == 1 && usize::from(row.cells[0].column_span) >= width);
        if named || labels && (header || distinct_centered) && !repeated_groups {
            rows[0].cells[0].text.kind = TextBlockKind::Caption;
        }
    }

    pub(super) fn parse_table_annotation(
        &mut self,
        node: Node<'_, '_>,
        is_note: bool,
    ) -> Vec<TextBlock> {
        self.parse_table_annotation_nodes(&[node], is_note)
    }

    fn parse_table_annotation_nodes(
        &mut self,
        nodes: &[Node<'_, '_>],
        is_note: bool,
    ) -> Vec<TextBlock> {
        let node = nodes[0];
        let start = self.blocks.len();
        let style = self.styles.block_style(node, BlockStyle::default());
        let mut collector = InlineCollector::new(false);
        for (index, node) in nodes.iter().copied().enumerate() {
            self.queue_node_anchors(node);
            self.queue_descendant_anchors(node);
            if index > 0 {
                collector.push_text(
                    " ",
                    self.styles
                        .text_style_for_block(node, TextBlockKind::Caption),
                    None,
                );
            }
            collect_table_cell_inline(
                node,
                self.styles
                    .text_style_for_block(node, TextBlockKind::Caption),
                None,
                &InlineParseContext::new(&self.section_href, &self.styles, &self.footnote_links)
                    .with_inline_images(true),
                &mut collector,
            );
        }
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
            } else if before_nodes.len() == 1
                && standalone_table_label(before_nodes[0].0)
                && standalone_caption_text(node)
                && nodes[index + 1..]
                    .iter()
                    .copied()
                    .find(|next| !ignorable(*next))
                    .is_some_and(carrier)
            {
                // A separate number, one title paragraph and its media form a
                // caption group independently of publisher-specific classes.
                before_nodes.push((node, false));
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
        let joined_title = (before_nodes.len() == 2
            && !before_nodes[0].1
            && !before_nodes[1].1
            && standalone_table_label(before_nodes[0].0)
            && standalone_caption_text(before_nodes[1].0))
        .then(|| (before_nodes[0].0, before_nodes[1].0));
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
            } else if let Some((_, note)) = before_nodes
                .iter()
                .chain(&after_nodes)
                .find(|(candidate, _)| *candidate == node)
            {
                if joined_title.is_some_and(|(_, title)| node == title) {
                    continue;
                }
                let texts = if let Some((label, title)) =
                    joined_title.filter(|(label, _)| *label == node)
                {
                    for spacer in nodes
                        .iter()
                        .copied()
                        .skip_while(|node| *node != label)
                        .skip(1)
                        .take_while(|node| *node != title)
                    {
                        self.queue_node_anchors(spacer);
                        self.queue_descendant_anchors(spacer);
                    }
                    // One semantic caption, with a normal wrapping space rather
                    // than the paragraph break used by the source document.
                    self.parse_table_annotation_nodes(&[label, title], false)
                } else {
                    self.parse_table_annotation(node, *note)
                };
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

// Compute the remaining grid's width with row spans. Publishers sometimes
// overstate the title's colspan (e.g. 3 over a two-column table); it must not
// manufacture an extra data column when deciding whether it covers the grid.
fn grid_width(rows: &[TableRow]) -> usize {
    let mut occupied = Vec::<u16>::new();
    let mut width = 0;
    for row in rows {
        let mut column = 0;
        for cell in &row.cells {
            while occupied.get(column).copied().unwrap_or(0) > 0 {
                column += 1;
            }
            let end = column + usize::from(cell.column_span.max(1));
            occupied.resize(occupied.len().max(end), 0);
            occupied[column..end].fill(cell.row_span.max(1));
            column = end;
            width = width.max(end);
        }
        for span in &mut occupied {
            *span = span.saturating_sub(1);
        }
    }
    width
}
