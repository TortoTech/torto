//! Shared HTML/CSS to format-neutral reading IR parser.

use std::collections::{HashMap, HashSet};

use rebook_publication::{
    Block, BlockStyle, ImageBlock, ImageLength, ImageStyle, Inline, MathRun, PublicationUrl, Rgba,
    Section, SectionAnchor, SourceAnchor, SourceRange, SpineItem, SpineItemId, TableBlock,
    TableCell, TableRow, TextAlignment, TextBaseline, TextBlock, TextBlockKind, TextRun, TextStyle,
};
use roxmltree::{Document, Node};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HtmlError {
    #[error("invalid HTML in {resource}: {message}")]
    InvalidDocument { resource: String, message: String },
    #[error(transparent)]
    Publication(#[from] rebook_publication::PublicationError),
}

pub fn parse_section(
    xml: &str,
    descriptor: &SpineItem,
    mut load_stylesheet: impl FnMut(&PublicationUrl) -> Option<String>,
) -> Result<Section, HtmlError> {
    let document = Document::parse(xml).map_err(|error| HtmlError::InvalidDocument {
        resource: descriptor.href.to_string(),
        message: error.to_string(),
    })?;
    let styles = StyleSheet::from_document(&document, &descriptor.href, &mut load_stylesheet);
    let root = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "body")
        .unwrap_or_else(|| document.root_element());
    let mut parser = ReadingIrParser::new(descriptor.id.clone(), descriptor.href.clone(), styles);
    parser.queue_node_anchors(root);
    parser.parse_children(root)?;

    if parser.blocks.is_empty() {
        let style = parser.styles.block_style(root, BlockStyle::default());
        parser.push_text_block(root, TextBlockKind::Paragraph, style)?;
    }

    Ok(Section {
        id: descriptor.id.clone(),
        href: descriptor.href.clone(),
        blocks: parser.blocks,
        anchors: parser.anchors,
    })
}

struct ReadingIrParser {
    section_id: SpineItemId,
    section_href: PublicationUrl,
    next_node: u64,
    blocks: Vec<Block>,
    anchors: Vec<SectionAnchor>,
    pending_anchors: Vec<String>,
    seen_anchors: HashSet<String>,
    styles: StyleSheet,
}

impl ReadingIrParser {
    fn new(section_id: SpineItemId, section_href: PublicationUrl, styles: StyleSheet) -> Self {
        Self {
            section_id,
            section_href,
            next_node: 0,
            blocks: Vec::new(),
            anchors: Vec::new(),
            pending_anchors: Vec::new(),
            seen_anchors: HashSet::new(),
            styles,
        }
    }

    fn parse_children(&mut self, parent: Node<'_, '_>) -> Result<(), HtmlError> {
        for node in parent.children() {
            if node.is_element() {
                self.parse_node(node)?;
            }
        }
        Ok(())
    }

    fn parse_block_container(&mut self, container: Node<'_, '_>) -> Result<(), HtmlError> {
        let mut style = self.styles.block_style(container, BlockStyle::default());
        style.indent = 0.0;
        let text_style = self
            .styles
            .text_style_for_block(container, TextBlockKind::Paragraph);
        let mut collector = InlineCollector::new(false);

        for child in container.children() {
            if child.is_element()
                && is_block_boundary(child.tag_name().name().to_ascii_lowercase().as_str())
            {
                self.push_collected_text_block(
                    TextBlockKind::Paragraph,
                    style,
                    std::mem::replace(&mut collector, InlineCollector::new(false)),
                );
                self.parse_node(child)?;
                continue;
            }
            if child.is_element() && has_descendant_image(child) {
                self.push_collected_text_block(
                    TextBlockKind::Paragraph,
                    style,
                    std::mem::replace(&mut collector, InlineCollector::new(false)),
                );
                self.queue_node_anchors(child);
                self.push_text_block(child, TextBlockKind::Paragraph, style)?;
                continue;
            }
            if child.is_element() {
                self.queue_node_anchors(child);
                self.queue_descendant_anchors(child);
            }
            collect_inline_node(
                child,
                text_style,
                None,
                &self.section_href,
                &self.styles,
                &mut collector,
            );
        }
        self.push_collected_text_block(TextBlockKind::Paragraph, style, collector);
        Ok(())
    }

    fn parse_node(&mut self, node: Node<'_, '_>) -> Result<(), HtmlError> {
        let name = node.tag_name().name().to_ascii_lowercase();
        if matches!(name.as_str(), "script" | "style" | "head" | "nav") {
            return Ok(());
        }
        self.queue_node_anchors(node);
        match name.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name[1..].parse::<u8>().unwrap_or(1);
                let mut style = self.styles.block_style(
                    node,
                    BlockStyle {
                        margin_before: 32.0,
                        margin_after: 8.0,
                        indent: 0.0,
                        line_height: 1.3,
                        ..BlockStyle::default()
                    },
                );
                style.indent = 0.0;
                self.push_text_block(node, TextBlockKind::Heading(level), style)?;
            }
            "p" => {
                let mut style = self.styles.block_style(node, BlockStyle::default());
                style.indent = 0.0;
                if contains_display_math(node) && has_only_math_content(node) {
                    style.align = TextAlignment::Center;
                    style.margin_before = style.margin_before.max(12.0);
                    style.margin_after = style.margin_after.max(12.0);
                }
                self.push_text_block(node, TextBlockKind::Paragraph, style)?;
            }
            "blockquote" => {
                let mut style = self.styles.block_style(node, BlockStyle::default());
                style.indent += 24.0;
                self.push_text_block(node, TextBlockKind::Blockquote, style)?;
            }
            "pre" => {
                let style = self.styles.block_style(
                    node,
                    BlockStyle {
                        line_height: 1.35,
                        ..BlockStyle::default()
                    },
                );
                self.push_text_block(node, TextBlockKind::Preformatted, style)?;
            }
            "table" => self.parse_table(node),
            name if is_generic_block_container(name) => self.parse_block_container(node)?,
            "ul" => self.parse_list(node, false)?,
            "ol" => self.parse_list(node, true)?,
            "img" | "image" => self.push_image(node, None)?,
            "hr" => self.blocks.push(Block::Separator),
            "br" => self.blocks.push(Block::PageBreak),
            _ => self.parse_children(node)?,
        }
        Ok(())
    }

    fn parse_list(&mut self, list: Node<'_, '_>, ordered: bool) -> Result<(), HtmlError> {
        let mut ordinal = 1_u32;
        for item in list
            .children()
            .filter(|node| node.is_element() && node.tag_name().name().eq_ignore_ascii_case("li"))
        {
            self.queue_node_anchors(item);
            let mut style = self.styles.block_style(item, BlockStyle::default());
            style.indent += 24.0;
            self.push_text_block(item, TextBlockKind::ListItem { ordered, ordinal }, style)?;
            ordinal = ordinal.saturating_add(1);
        }
        Ok(())
    }

    fn parse_table(&mut self, table: Node<'_, '_>) {
        let table_node = self.allocate_node();
        let table_source = self.source_range(&table_node, 0);
        self.bind_pending_anchors(&table_source.start);
        let mut rows = Vec::new();
        for row in table.descendants().filter(|node| {
            node.is_element()
                && node.tag_name().name().eq_ignore_ascii_case("tr")
                && node.ancestors().skip(1).find(|ancestor| {
                    ancestor.is_element()
                        && ancestor.tag_name().name().eq_ignore_ascii_case("table")
                }) == Some(table)
        }) {
            let mut cells = Vec::new();
            for cell in row.children().filter(|node| {
                node.is_element()
                    && matches!(
                        node.tag_name().name().to_ascii_lowercase().as_str(),
                        "td" | "th"
                    )
            }) {
                self.queue_node_anchors(cell);
                let header = cell.tag_name().name().eq_ignore_ascii_case("th");
                let mut style = self.styles.block_style(cell, BlockStyle::default());
                style.indent = 0.0;
                style.margin_before = 0.0;
                style.margin_after = 0.0;
                style.line_height = style.line_height.clamp(1.0, 1.5);
                let mut text_style = self
                    .styles
                    .text_style_for_block(cell, TextBlockKind::Paragraph);
                if header {
                    text_style.bold = true;
                }
                let mut collector = InlineCollector::new(false);
                collect_inline(
                    cell,
                    text_style,
                    None,
                    &self.section_href,
                    &self.styles,
                    &mut collector,
                );
                collector.finish();
                let text_len = collector
                    .content
                    .iter()
                    .map(|inline| match inline {
                        Inline::Text(run) => run.text.chars().count() as u64,
                        Inline::Math(_) => 0,
                        Inline::Break => 1,
                    })
                    .sum();
                let node_id = self.allocate_node();
                let source = self.source_range(&node_id, text_len);
                self.bind_pending_anchors(&source.start);
                cells.push(TableCell {
                    text: TextBlock {
                        kind: TextBlockKind::Paragraph,
                        content: collector.content,
                        style,
                        source: Some(source),
                    },
                    column_span: table_span(cell, "colspan"),
                    row_span: table_span(cell, "rowspan"),
                    header,
                });
            }
            if !cells.is_empty() {
                rows.push(TableRow { cells });
            }
        }
        if !rows.is_empty() {
            self.blocks.push(Block::Table(TableBlock {
                rows,
                source: Some(table_source),
            }));
        }
    }

    fn push_text_block(
        &mut self,
        node: Node<'_, '_>,
        kind: TextBlockKind,
        style: BlockStyle,
    ) -> Result<(), HtmlError> {
        let node_id = self.allocate_node();
        self.queue_descendant_anchors(node);
        let mut collector = InlineCollector::new(matches!(kind, TextBlockKind::Preformatted));
        collect_inline(
            node,
            self.styles.text_style_for_block(node, kind),
            None,
            &self.section_href,
            &self.styles,
            &mut collector,
        );
        collector.finish();
        let text_len = collector
            .content
            .iter()
            .map(|inline| match inline {
                Inline::Text(run) => run.text.chars().count() as u64,
                Inline::Math(_) => 0,
                Inline::Break => 1,
            })
            .sum();
        if !collector.content.is_empty() {
            let source = self.source_range(&node_id, text_len);
            self.bind_pending_anchors(&source.start);
            self.blocks.push(Block::Text(TextBlock {
                kind,
                content: collector.content,
                style,
                source: Some(source),
            }));
        }

        let images = node
            .descendants()
            .filter(|descendant| {
                descendant != &node
                    && descendant.is_element()
                    && matches!(
                        descendant.tag_name().name().to_ascii_lowercase().as_str(),
                        "img" | "image"
                    )
            })
            .collect::<Vec<_>>();
        let image_count = images.len();
        for (index, image) in images.into_iter().enumerate() {
            let container_style = (text_len == 0).then_some((
                if index == 0 { style.margin_before } else { 0.0 },
                if index + 1 == image_count {
                    style.margin_after
                } else {
                    0.0
                },
            ));
            self.push_image(image, container_style)?;
        }
        Ok(())
    }

    fn push_collected_text_block(
        &mut self,
        kind: TextBlockKind,
        style: BlockStyle,
        mut collector: InlineCollector,
    ) {
        collector.finish();
        let text_len = collector
            .content
            .iter()
            .map(|inline| match inline {
                Inline::Text(run) => run.text.chars().count() as u64,
                Inline::Math(_) => 0,
                Inline::Break => 1,
            })
            .sum();
        if collector.content.is_empty() {
            return;
        }
        let node_id = self.allocate_node();
        let source = self.source_range(&node_id, text_len);
        self.bind_pending_anchors(&source.start);
        self.blocks.push(Block::Text(TextBlock {
            kind,
            content: collector.content,
            style,
            source: Some(source),
        }));
    }

    fn push_image(
        &mut self,
        node: Node<'_, '_>,
        container_style: Option<(f32, f32)>,
    ) -> Result<(), HtmlError> {
        let Some(src) = attribute_local(node, "src")
            .or_else(|| attribute_local(node, "href"))
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        let href = self.section_href.resolve(src)?.resource_url();
        let node_id = self.allocate_node();
        let source = self.source_range(&node_id, 0);
        self.bind_pending_anchors(&source.start);
        let mut style = self.styles.image_style(node);
        if let Some((margin_before, margin_after)) = container_style {
            style.margin_before = style.margin_before.max(margin_before);
            style.margin_after = style.margin_after.max(margin_after);
        }
        self.blocks.push(Block::Image(ImageBlock {
            href,
            alt: attribute_local(node, "alt").unwrap_or_default().to_owned(),
            style,
            source: Some(source),
            text_layer: None,
        }));
        Ok(())
    }

    fn queue_descendant_anchors(&mut self, node: Node<'_, '_>) {
        for descendant in node.descendants().skip(1).filter(Node::is_element) {
            self.queue_node_anchors(descendant);
        }
    }

    fn queue_node_anchors(&mut self, node: Node<'_, '_>) {
        for fragment in [attribute_local(node, "id"), attribute_local(node, "name")]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|fragment| !fragment.is_empty())
        {
            if self.seen_anchors.insert(fragment.to_owned()) {
                self.pending_anchors.push(fragment.to_owned());
            }
        }
    }

    fn bind_pending_anchors(&mut self, source: &SourceAnchor) {
        self.anchors.extend(
            self.pending_anchors
                .drain(..)
                .map(|fragment| SectionAnchor {
                    fragment,
                    source: source.clone(),
                }),
        );
    }

    fn allocate_node(&mut self) -> String {
        let id = format!("n{}", self.next_node);
        self.next_node = self.next_node.saturating_add(1);
        id
    }

    fn source_range(&self, node: &str, text_len: u64) -> SourceRange {
        SourceRange {
            start: SourceAnchor {
                spine: self.section_id.clone(),
                node: node.to_owned(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: self.section_id.clone(),
                node: node.to_owned(),
                text_offset: text_len,
            },
        }
    }
}

struct InlineCollector {
    content: Vec<Inline>,
    preserve_whitespace: bool,
    last_was_space: bool,
}

impl InlineCollector {
    fn new(preserve_whitespace: bool) -> Self {
        Self {
            content: Vec::new(),
            preserve_whitespace,
            last_was_space: true,
        }
    }

    fn push_text(&mut self, text: &str, style: TextStyle, link: Option<PublicationUrl>) {
        let normalized = if self.preserve_whitespace {
            text.to_owned()
        } else {
            let mut result = String::new();
            for character in text.chars() {
                if character.is_whitespace() {
                    if !self.last_was_space {
                        result.push(' ');
                        self.last_was_space = true;
                    }
                } else {
                    result.push(character);
                    self.last_was_space = false;
                }
            }
            result
        };
        if normalized.is_empty() {
            return;
        }
        if let Some(Inline::Text(previous)) = self.content.last_mut()
            && previous.style == style
            && previous.link == link
        {
            previous.text.push_str(&normalized);
        } else {
            self.content.push(Inline::Text(TextRun {
                text: normalized,
                style,
                link,
            }));
        }
    }

    fn push_break(&mut self) {
        self.content.push(Inline::Break);
        self.last_was_space = true;
    }

    fn push_math(&mut self, latex: &str, display: bool, size_scale: f32) {
        let latex = latex.trim().to_owned();
        if latex.is_empty() {
            return;
        }
        self.content.push(Inline::Math(MathRun {
            latex,
            display,
            size_scale,
        }));
        self.last_was_space = false;
    }

    fn finish(&mut self) {
        if let Some(Inline::Text(run)) = self.content.last_mut() {
            while run.text.ends_with(' ') {
                run.text.pop();
            }
        }
        self.content
            .retain(|inline| !matches!(inline, Inline::Text(run) if run.text.is_empty()));
    }
}

fn collect_inline(
    node: Node<'_, '_>,
    inherited: TextStyle,
    link: Option<&PublicationUrl>,
    base: &PublicationUrl,
    styles: &StyleSheet,
    collector: &mut InlineCollector,
) {
    for child in node.children() {
        collect_inline_node(child, inherited, link, base, styles, collector);
    }
}

fn collect_inline_node(
    node: Node<'_, '_>,
    inherited: TextStyle,
    link: Option<&PublicationUrl>,
    base: &PublicationUrl,
    styles: &StyleSheet,
    collector: &mut InlineCollector,
) {
    if node.is_text() {
        collector.push_text(node.text().unwrap_or_default(), inherited, link.cloned());
        return;
    }
    if !node.is_element() {
        return;
    }
    let name = node.tag_name().name().to_ascii_lowercase();
    if name == "br" {
        collector.push_break();
        return;
    }
    if matches!(name.as_str(), "img" | "script" | "style") {
        return;
    }

    let mut style = inherited;
    match name.as_str() {
        "strong" | "b" => style.bold = true,
        "em" | "i" | "cite" => style.italic = true,
        "u" => style.underline = true,
        "small" => style.size_scale *= 0.85,
        "big" => style.size_scale *= 1.2,
        "sup" => {
            style.baseline = TextBaseline::Superscript;
            style.size_scale *= 0.75;
        }
        "sub" => {
            style.baseline = TextBaseline::Subscript;
            style.size_scale *= 0.75;
        }
        _ => {}
    }
    styles.apply_text_node(node, &mut style, inherited.size_scale);
    if name == "span" {
        let classes = attribute_local(node, "class").unwrap_or_default();
        let is_math = classes
            .split_ascii_whitespace()
            .any(|class| class == "math");
        if is_math {
            let display = classes
                .split_ascii_whitespace()
                .any(|class| class == "math-display");
            let latex = node
                .descendants()
                .filter(Node::is_text)
                .filter_map(|text| text.text())
                .collect::<String>();
            collector.push_math(&latex, display, style.size_scale);
            return;
        }
    }
    if name == "a" {
        let resolved = attribute_local(node, "href").and_then(|href| base.resolve(href).ok());
        collect_inline(
            node,
            style,
            resolved.as_ref().or(link),
            base,
            styles,
            collector,
        );
    } else {
        collect_inline(node, style, link, base, styles, collector);
    }
}

fn is_generic_block_container(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "center"
            | "dd"
            | "dl"
            | "div"
            | "dt"
            | "figcaption"
            | "figure"
            | "footer"
            | "header"
            | "li"
            | "main"
            | "section"
    )
}

fn table_span(node: Node<'_, '_>, name: &str) -> u16 {
    attribute_local(node, name)
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(1)
        .clamp(1, 64)
}

fn has_descendant_image(node: Node<'_, '_>) -> bool {
    node.descendants().skip(1).any(|descendant| {
        descendant.is_element()
            && matches!(
                descendant.tag_name().name().to_ascii_lowercase().as_str(),
                "img" | "image"
            )
    })
}

fn contains_display_math(node: Node<'_, '_>) -> bool {
    node.descendants().skip(1).any(|descendant| {
        descendant.is_element()
            && descendant.tag_name().name().eq_ignore_ascii_case("span")
            && attribute_local(descendant, "class").is_some_and(|classes| {
                classes
                    .split_ascii_whitespace()
                    .any(|class| class == "math-display")
            })
    })
}

fn has_only_math_content(node: Node<'_, '_>) -> bool {
    node.descendants().skip(1).all(|descendant| {
        if descendant.is_text() {
            return descendant.text().is_none_or(|text| text.trim().is_empty())
                || descendant.parent().is_some_and(|parent| {
                    attribute_local(parent, "class").is_some_and(|classes| {
                        classes
                            .split_ascii_whitespace()
                            .any(|class| class == "math-display")
                    })
                });
        }
        !descendant.is_element()
            || (descendant.tag_name().name().eq_ignore_ascii_case("span")
                && attribute_local(descendant, "class").is_some_and(|classes| {
                    classes
                        .split_ascii_whitespace()
                        .any(|class| matches!(class, "math" | "math-display"))
                }))
    })
}

fn is_block_boundary(name: &str) -> bool {
    is_generic_block_container(name)
        || matches!(
            name,
            "blockquote"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "hr"
                | "img"
                | "image"
                | "nav"
                | "ol"
                | "p"
                | "pre"
                | "table"
                | "ul"
        )
}

#[derive(Default)]
struct StyleSheet {
    rules: Vec<StyleRule>,
    next_order: usize,
}

struct StyleRule {
    selector: SimpleSelector,
    specificity: u16,
    order: usize,
    declarations: Vec<(String, String)>,
}

struct SimpleSelector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl StyleSheet {
    fn from_document(
        document: &Document<'_>,
        base: &PublicationUrl,
        load_stylesheet: &mut impl FnMut(&PublicationUrl) -> Option<String>,
    ) -> Self {
        let mut sheet = Self::default();
        for node in document.descendants().filter(Node::is_element) {
            match node.tag_name().name().to_ascii_lowercase().as_str() {
                "style" => {
                    let css = node
                        .descendants()
                        .filter(Node::is_text)
                        .filter_map(|text| text.text())
                        .collect::<String>();
                    sheet.add_css(&css);
                }
                "link"
                    if attribute_local(node, "rel").is_some_and(|rel| {
                        rel.split_ascii_whitespace()
                            .any(|token| token.eq_ignore_ascii_case("stylesheet"))
                    }) =>
                {
                    let Some(href) = attribute_local(node, "href") else {
                        continue;
                    };
                    let Ok(href) = base.resolve(href).map(|url| url.resource_url()) else {
                        continue;
                    };
                    if let Some(css) = load_stylesheet(&href) {
                        sheet.add_css(&css);
                    }
                }
                _ => {}
            }
        }
        sheet
    }

    fn add_css(&mut self, css: &str) {
        let css = strip_css_comments(css);
        let mut cursor = 0;
        while let Some(relative_open) = css[cursor..].find('{') {
            let open = cursor + relative_open;
            let Some(close) = matching_brace(&css, open) else {
                break;
            };
            let prelude = css[cursor..open].trim();
            if !prelude.starts_with('@') {
                let declarations = declarations(&css[open + 1..close]).collect::<Vec<_>>();
                for raw_selector in prelude.split(',') {
                    let Some(selector) = SimpleSelector::parse(raw_selector) else {
                        continue;
                    };
                    self.rules.push(StyleRule {
                        specificity: selector.specificity(),
                        selector,
                        order: self.next_order,
                        declarations: declarations.clone(),
                    });
                }
                self.next_order = self.next_order.saturating_add(1);
            }
            cursor = close.saturating_add(1);
        }
    }

    fn block_style(&self, node: Node<'_, '_>, mut style: BlockStyle) -> BlockStyle {
        let mut ancestors = node
            .ancestors()
            .filter(Node::is_element)
            .collect::<Vec<_>>();
        ancestors.reverse();
        for ancestor in ancestors {
            let inherited_only = ancestor != node;
            let properties = self.cascaded_properties(ancestor);
            apply_block_properties(&mut style, &properties, inherited_only);
        }
        style
    }

    fn text_style_for_block(&self, node: Node<'_, '_>, kind: TextBlockKind) -> TextStyle {
        let mut ancestors = node
            .ancestors()
            .filter(Node::is_element)
            .collect::<Vec<_>>();
        ancestors.reverse();
        let mut style = TextStyle::default();
        for ancestor in ancestors {
            let inherited_size = style.size_scale;
            if ancestor == node {
                apply_semantic_block_style(kind, &mut style, inherited_size);
            }
            self.apply_text_node(ancestor, &mut style, inherited_size);
        }
        style
    }

    fn image_style(&self, node: Node<'_, '_>) -> ImageStyle {
        let mut style = ImageStyle {
            width: attribute_local(node, "width").and_then(image_length),
            height: attribute_local(node, "height").and_then(image_length),
            ..ImageStyle::default()
        };
        let properties = self.cascaded_properties(node);
        if let Some(value) = properties
            .get("width")
            .and_then(|value| image_length(value))
        {
            style.width = Some(value);
        }
        if let Some(value) = properties
            .get("height")
            .and_then(|value| image_length(value))
        {
            style.height = Some(value);
        }
        if let Some(value) = properties
            .get("max-width")
            .and_then(|value| image_length(value))
        {
            style.max_width = Some(value);
        }
        if let Some(value) = properties
            .get("max-height")
            .and_then(|value| image_length(value))
        {
            style.max_height = Some(value);
        }
        if let Some(value) = properties
            .get("margin-top")
            .and_then(|value| css_length(value))
        {
            style.margin_before = value;
        }
        if let Some(value) = properties
            .get("margin-bottom")
            .and_then(|value| css_length(value))
        {
            style.margin_after = value;
        }
        style
    }

    fn apply_text_node(&self, node: Node<'_, '_>, style: &mut TextStyle, inherited_size: f32) {
        let properties = self.cascaded_properties(node);
        apply_text_properties(style, &properties, inherited_size);
    }

    fn cascaded_properties(&self, node: Node<'_, '_>) -> HashMap<String, String> {
        let mut matching = self
            .rules
            .iter()
            .filter(|rule| rule.selector.matches(node))
            .collect::<Vec<_>>();
        matching.sort_by_key(|rule| (rule.specificity, rule.order));

        let mut properties = HashMap::new();
        for rule in matching {
            insert_declarations(&mut properties, rule.declarations.iter().cloned());
        }
        if let Some(inline) = attribute_local(node, "style") {
            insert_declarations(&mut properties, declarations(inline));
        }
        properties
    }
}

impl SimpleSelector {
    fn parse(raw: &str) -> Option<Self> {
        let mut rest = raw.trim();
        if rest.is_empty()
            || rest
                .chars()
                .any(|character| character.is_whitespace() || ">+~[:*".contains(character))
        {
            return None;
        }

        let mut selector = Self {
            tag: None,
            id: None,
            classes: Vec::new(),
        };
        if !rest.starts_with('.') && !rest.starts_with('#') {
            let (tag, tail) = take_css_identifier(rest)?;
            selector.tag = Some(tag.to_ascii_lowercase());
            rest = tail;
        }
        while !rest.is_empty() {
            let (kind, tail) = rest.split_at(1);
            let (value, next) = take_css_identifier(tail)?;
            match kind {
                "." => selector.classes.push(value.to_owned()),
                "#" => selector.id = Some(value.to_owned()),
                _ => return None,
            }
            rest = next;
        }
        Some(selector)
    }

    fn specificity(&self) -> u16 {
        u16::from(self.id.is_some()) * 100
            + u16::try_from(self.classes.len()).unwrap_or(u16::MAX) * 10
            + u16::from(self.tag.is_some())
    }

    fn matches(&self, node: Node<'_, '_>) -> bool {
        if self
            .tag
            .as_deref()
            .is_some_and(|tag| !node.tag_name().name().eq_ignore_ascii_case(tag))
        {
            return false;
        }
        if self
            .id
            .as_deref()
            .is_some_and(|id| attribute_local(node, "id").is_none_or(|candidate| candidate != id))
        {
            return false;
        }
        let classes = attribute_local(node, "class").unwrap_or_default();
        self.classes.iter().all(|class| {
            classes
                .split_ascii_whitespace()
                .any(|candidate| candidate == class)
        })
    }
}

fn apply_semantic_block_style(kind: TextBlockKind, style: &mut TextStyle, inherited_size: f32) {
    match kind {
        TextBlockKind::Heading(level) => {
            style.bold = true;
            style.size_scale = inherited_size
                * match level {
                    1 => 1.5,
                    2 => 1.3,
                    3 => 1.15,
                    _ => 1.05,
                };
        }
        TextBlockKind::Preformatted => style.size_scale = inherited_size * 0.9,
        _ => {}
    }
}

fn apply_block_properties(
    style: &mut BlockStyle,
    properties: &HashMap<String, String>,
    inherited_only: bool,
) {
    // Reading IR flattens nested HTML boxes into blocks. Preserve the start-side
    // offset contributed by every containing box so authored lists keep their
    // visual hierarchy after flattening.
    for property in [
        properties
            .get("margin-inline-start")
            .or_else(|| properties.get("margin-left")),
        properties
            .get("padding-inline-start")
            .or_else(|| properties.get("padding-left")),
    ]
    .into_iter()
    .flatten()
    {
        if let Some((pixels, fraction)) = css_horizontal_length(property) {
            style.margin_start += pixels;
            style.margin_start_fraction += fraction;
        }
    }
    if let Some(value) = properties.get("text-align") {
        style.align = match value.as_str() {
            "center" => TextAlignment::Center,
            "right" | "end" => TextAlignment::End,
            "justify" => TextAlignment::Justify,
            _ => TextAlignment::Start,
        };
    }
    if let Some(value) = properties
        .get("text-indent")
        .and_then(|value| css_length(value))
    {
        style.indent = value;
    }
    if let Some(value) = properties
        .get("line-height")
        .and_then(|value| css_line_height(value))
    {
        style.line_height = value;
    }
    if inherited_only {
        return;
    }
    if let Some(value) = properties
        .get("margin-top")
        .and_then(|value| css_length(value))
    {
        style.margin_before = value;
    }
    if let Some(value) = properties
        .get("margin-bottom")
        .and_then(|value| css_length(value))
    {
        style.margin_after = value;
    }
}

fn apply_text_properties(
    style: &mut TextStyle,
    properties: &HashMap<String, String>,
    inherited_size: f32,
) {
    if let Some(value) = properties
        .get("font-size")
        .and_then(|value| css_scale(value))
    {
        style.size_scale = inherited_size * value;
    }
    if let Some(value) = properties.get("font-weight") {
        style.bold = value == "bold"
            || value == "bolder"
            || value.parse::<u16>().is_ok_and(|weight| weight >= 600);
    }
    if let Some(value) = properties.get("font-style") {
        style.italic = matches!(value.as_str(), "italic" | "oblique");
    }
    if let Some(value) = properties
        .get("text-decoration-line")
        .or_else(|| properties.get("text-decoration"))
    {
        style.underline = value.contains("underline");
    }
    if let Some(color) = properties.get("color").and_then(|value| css_color(value)) {
        style.color = color;
    }
    if let Some(value) = properties.get("vertical-align") {
        style.baseline = match value.trim().to_ascii_lowercase().as_str() {
            "super" => TextBaseline::Superscript,
            "sub" => TextBaseline::Subscript,
            "baseline" => TextBaseline::Normal,
            _ => style.baseline,
        };
    }
}

fn insert_declarations(
    properties: &mut HashMap<String, String>,
    declarations: impl IntoIterator<Item = (String, String)>,
) {
    for (name, value) in declarations {
        if name == "margin" {
            if let Some((top, _, bottom, left)) = box_sides(&value) {
                properties.insert("margin-top".into(), top.to_owned());
                properties.insert("margin-bottom".into(), bottom.to_owned());
                properties.insert("margin-left".into(), left.to_owned());
            }
        } else if name == "padding" {
            if let Some((_, _, _, left)) = box_sides(&value) {
                properties.insert("padding-left".into(), left.to_owned());
            }
        } else {
            properties.insert(name, value);
        }
    }
}

fn take_css_identifier(input: &str) -> Option<(&str, &str)> {
    let end = input
        .char_indices()
        .find_map(|(index, character)| {
            (!character.is_ascii_alphanumeric() && !matches!(character, '-' | '_')).then_some(index)
        })
        .unwrap_or(input.len());
    (end > 0).then(|| input.split_at(end))
}

fn strip_css_comments(css: &str) -> String {
    let mut output = String::with_capacity(css.len());
    let mut remaining = css;
    while let Some(start) = remaining.find("/*") {
        output.push_str(&remaining[..start]);
        let Some(end) = remaining[start + 2..].find("*/") else {
            return output;
        };
        remaining = &remaining[start + end + 4..];
    }
    output.push_str(remaining);
    output
}

fn matching_brace(css: &str, open: usize) -> Option<usize> {
    let mut depth = 0_u32;
    for (relative, character) in css[open..].char_indices() {
        match character {
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + relative);
                }
            }
            _ => {}
        }
    }
    None
}

fn box_sides(value: &str) -> Option<(&str, &str, &str, &str)> {
    let values = value.split_ascii_whitespace().collect::<Vec<_>>();
    match values.as_slice() {
        [all] => Some((all, all, all, all)),
        [vertical, horizontal] => Some((vertical, horizontal, vertical, horizontal)),
        [top, horizontal, bottom] => Some((top, horizontal, bottom, horizontal)),
        [top, right, bottom, left] => Some((top, right, bottom, left)),
        _ => None,
    }
}

fn declarations(style: &str) -> impl Iterator<Item = (String, String)> + '_ {
    style.split(';').filter_map(|declaration| {
        let (name, value) = declaration.split_once(':')?;
        Some((
            name.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_end_matches("!important")
                .trim()
                .to_ascii_lowercase(),
        ))
    })
}

fn css_length(value: &str) -> Option<f32> {
    const BASE_FONT_SIZE: f32 = 16.0;
    let value = value.trim();
    let (number, scale) = if let Some(number) = value.strip_suffix("px") {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix("rem") {
        (number, BASE_FONT_SIZE)
    } else if let Some(number) = value.strip_suffix("em") {
        (number, BASE_FONT_SIZE)
    } else if let Some(number) = value.strip_suffix("pt") {
        (number, 96.0 / 72.0)
    } else {
        (value, 1.0)
    };
    number.trim().parse::<f32>().ok().map(|v| v * scale)
}

fn css_horizontal_length(value: &str) -> Option<(f32, f32)> {
    if let Some(percent) = value.trim().strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| (0.0, value / 100.0));
    }
    css_length(value)
        .filter(|value| value.is_finite())
        .map(|value| (value, 0.0))
}

fn css_scale(value: &str) -> Option<f32> {
    const BASE_FONT_SIZE: f32 = 16.0;
    let value = value.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent.parse::<f32>().ok().map(|number| number / 100.0);
    }
    if let Some(em) = value
        .strip_suffix("rem")
        .or_else(|| value.strip_suffix("em"))
    {
        return em.parse::<f32>().ok();
    }
    if let Some(px) = value.strip_suffix("px") {
        return px.parse::<f32>().ok().map(|number| number / BASE_FONT_SIZE);
    }
    None
}

fn css_line_height(value: &str) -> Option<f32> {
    let value = value.trim();
    let parsed = if let Some(em) = value.strip_suffix("em") {
        em.parse::<f32>().ok()
    } else if let Some(percent) = value.strip_suffix('%') {
        percent.parse::<f32>().ok().map(|number| number / 100.0)
    } else if let Some(px) = value.strip_suffix("px") {
        px.parse::<f32>().ok().map(|number| number / 16.0)
    } else {
        value.parse::<f32>().ok()
    }?;
    (0.8..=4.0).contains(&parsed).then_some(parsed)
}

fn css_color(value: &str) -> Option<Rgba> {
    let hex = value.trim().strip_prefix('#')?;
    let (red, green, blue) = match hex.len() {
        3 => (
            u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?,
            u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?,
        ),
        6 => (
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some(Rgba {
        red,
        green,
        blue,
        alpha: 255,
    })
}

fn image_length(value: &str) -> Option<ImageLength> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") || value.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(percent) = value.strip_suffix('%') {
        let fraction = percent.trim().parse::<f32>().ok()? / 100.0;
        return fraction
            .is_finite()
            .then_some(ImageLength::Fraction(fraction.max(0.0)));
    }
    let pixels = css_length(value)?;
    pixels
        .is_finite()
        .then_some(ImageLength::Pixels(pixels.max(0.0)))
}

fn attribute_local<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name().eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_external_class_styles_and_inline_cascade() {
        let descriptor = SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml">
            <head><link rel="stylesheet" href="styles/book.css"/></head>
            <body><div class="centered"><p class="body" style="text-align:right">
                Hello <span class="emphasis">world</span>
            </p></div><img class="figure" src="images/chart.png" width="640"/></body>
        </html>"#;
        let css = r"
            .centered { text-align: center; }
            p.body {
                font-size: 1.25em;
                line-height: 1.8em;
                margin: 2em 0 1em;
                text-indent: 2em;
            }
            .emphasis { font-weight: bold; color: #123456; }
            img.figure { width: 80%; max-width: 420px; max-height: 60%; }
        ";
        let mut loaded = false;

        let section = parse_section(xml, &descriptor, |href| {
            loaded = true;
            assert_eq!(href.path(), "OPS/styles/book.css");
            Some(css.into())
        })
        .unwrap();

        assert!(loaded);
        let Some(Block::Text(block)) = section.blocks.first() else {
            panic!("expected a text block");
        };
        assert_eq!(block.style.align, TextAlignment::End);
        assert_close(block.style.line_height, 1.8);
        assert_close(block.style.margin_before, 32.0);
        assert_close(block.style.margin_after, 16.0);
        assert_close(block.style.indent, 0.0);
        let Inline::Text(regular) = &block.content[0] else {
            panic!("expected regular text");
        };
        assert_close(regular.style.size_scale, 1.25);
        let Inline::Text(emphasis) = &block.content[1] else {
            panic!("expected emphasized text");
        };
        assert!(emphasis.style.bold);
        assert_eq!(emphasis.style.color.red, 0x12);
        assert_eq!(emphasis.style.color.green, 0x34);
        assert_eq!(emphasis.style.color.blue, 0x56);
        let Some(Block::Image(image)) = section.blocks.get(1) else {
            panic!("expected an image block");
        };
        assert_eq!(image.style.width, Some(ImageLength::Fraction(0.8)));
        assert_eq!(image.style.max_width, Some(ImageLength::Pixels(420.0)));
        assert_eq!(image.style.max_height, Some(ImageLength::Fraction(0.6)));
    }

    #[test]
    fn preserves_superscript_and_subscript_baselines() {
        let descriptor = SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml">
            <head><style>.css-super { vertical-align: super; font-size: 80%; }</style></head>
            <body><p>read.<a href="notes.xhtml#note-4"><sup>4</sup></a>
                H<sub>2</sub>O <span class="css-super">5</span></p></body>
        </html>"#;

        let section = parse_section(xml, &descriptor, |_| unreachable!()).unwrap();
        let [Block::Text(block)] = section.blocks.as_slice() else {
            panic!("expected one text block");
        };
        let styled = block
            .content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) => Some((run.text.trim(), run.style)),
                Inline::Math(_) | Inline::Break => None,
            })
            .collect::<Vec<_>>();

        assert!(styled.iter().any(|(text, style)| {
            *text == "4"
                && style.baseline == TextBaseline::Superscript
                && (style.size_scale - 0.75).abs() < 0.001
        }));
        assert!(styled.iter().any(|(text, style)| {
            *text == "2"
                && style.baseline == TextBaseline::Subscript
                && (style.size_scale - 0.75).abs() < 0.001
        }));
        assert!(styled.iter().any(|(text, style)| {
            *text == "5"
                && style.baseline == TextBaseline::Superscript
                && (style.size_scale - 0.8).abs() < 0.001
        }));
    }

    #[test]
    fn preserves_margins_from_an_image_only_block_container() {
        let descriptor = SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml">
            <head><style>
                p.IMG { margin-top: 25px; margin-bottom: 10px; text-align: center; }
            </style></head>
            <body><p class="IMG"><a id="figure"/><img src="images/chart.png"/></p></body>
        </html>"#;

        let section = parse_section(xml, &descriptor, |_| unreachable!()).unwrap();
        let [Block::Image(image)] = section.blocks.as_slice() else {
            panic!("expected only the image block");
        };

        assert_close(image.style.margin_before, 25.0);
        assert_close(image.style.margin_after, 10.0);
    }

    #[test]
    fn parses_svg_image_href_as_an_image_block() {
        let descriptor = SpineItem {
            id: SpineItemId::new("cover").unwrap(),
            href: PublicationUrl::parse("OPS/titlepage.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml">
            <body><svg xmlns="http://www.w3.org/2000/svg"
                xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 622 910">
                <image width="622" height="910" xlink:href="images/cover.jpeg"/>
            </svg></body>
        </html>"#;

        let section = parse_section(xml, &descriptor, |_| unreachable!()).unwrap();
        let Some(Block::Image(image)) = section.blocks.first() else {
            panic!("expected an SVG image block");
        };
        assert_eq!(image.href.path(), "OPS/images/cover.jpeg");
        assert_eq!(image.style.width, Some(ImageLength::Pixels(622.0)));
        assert_eq!(image.style.height, Some(ImageLength::Pixels(910.0)));
    }

    #[test]
    fn preserves_block_container_and_empty_element_fragment_anchors() {
        let descriptor = SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("OPS/chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
            <a id="before-heading"></a>
            <div id="chapter-start"><h2 id="heading">Heading</h2></div>
            <p>Text <span id="inside-paragraph">target</span></p>
        </body></html>"#;

        let section = parse_section(xml, &descriptor, |_| unreachable!()).unwrap();
        let anchors = section
            .anchors
            .iter()
            .map(|anchor| (anchor.fragment.as_str(), anchor.source.node.as_str()))
            .collect::<HashMap<_, _>>();
        assert_eq!(anchors.get("before-heading"), Some(&"n0"));
        assert_eq!(anchors.get("chapter-start"), Some(&"n0"));
        assert_eq!(anchors.get("heading"), Some(&"n0"));
        assert_eq!(anchors.get("inside-paragraph"), Some(&"n1"));
    }

    #[test]
    fn parses_nested_generic_block_containers_without_flattening_or_duplication() {
        let descriptor = SpineItem {
            id: SpineItemId::new("contents").unwrap(),
            href: PublicationUrl::parse("Text/contents.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
            <div id="title">目录</div>
            <div id="chapter"><a id="chapter-link">第一章</a>
                <div id="item"><a id="item-link">◎故事的力量</a></div>
            </div>
            <div id="mixed">开头<p id="paragraph">正文</p>结尾</div>
        </body></html>"#;

        let section = parse_section(xml, &descriptor, |_| unreachable!()).unwrap();
        let texts = section
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(block) => Some(
                    block
                        .content
                        .iter()
                        .filter_map(|inline| match inline {
                            Inline::Text(run) => Some(run.text.as_str()),
                            Inline::Math(_) | Inline::Break => None,
                        })
                        .collect::<String>(),
                ),
                Block::Table(_) | Block::Image(_) | Block::Separator | Block::PageBreak => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            ["目录", "第一章", "◎故事的力量", "开头", "正文", "结尾"]
        );

        let anchors = section
            .anchors
            .iter()
            .map(|anchor| (anchor.fragment.as_str(), anchor.source.node.as_str()))
            .collect::<HashMap<_, _>>();
        assert_eq!(anchors.get("title"), Some(&"n0"));
        assert_eq!(anchors.get("chapter"), Some(&"n1"));
        assert_eq!(anchors.get("chapter-link"), Some(&"n1"));
        assert_eq!(anchors.get("item"), Some(&"n2"));
        assert_eq!(anchors.get("item-link"), Some(&"n2"));
        assert_eq!(anchors.get("mixed"), Some(&"n3"));
        assert_eq!(anchors.get("paragraph"), Some(&"n4"));
    }

    #[test]
    fn preserves_images_wrapped_by_inline_elements_in_block_containers() {
        let descriptor = SpineItem {
            id: SpineItemId::new("plates").unwrap(),
            href: PublicationUrl::parse("OPS/text/plates.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><div>
            <h3>Plates</h3>
            <div><a id="plate-one"><img src="../images/one.jpeg"/></a></div>
            <div><span><a id="plate-two"><img src="../images/two.jpeg"/></a></span></div>
        </div></body></html>"#;

        let section = parse_section(xml, &descriptor, |_| unreachable!()).unwrap();
        let images = section
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Image(image) => Some(image.href.path()),
                Block::Text(_) | Block::Table(_) | Block::Separator | Block::PageBreak => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(images, ["OPS/images/one.jpeg", "OPS/images/two.jpeg"]);
        assert!(
            section
                .anchors
                .iter()
                .any(|anchor| anchor.fragment == "plate-one")
        );
        assert!(
            section
                .anchors
                .iter()
                .any(|anchor| anchor.fragment == "plate-two")
        );
    }

    #[test]
    fn keeps_definition_list_entries_as_separate_blocks() {
        let descriptor = SpineItem {
            id: SpineItemId::new("contents").unwrap(),
            href: PublicationUrl::parse("OPS/text/contents.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let xml = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><style>
            dl { margin: 1em 0 1em 10%; }
            dt { padding: 0 0 0 .5em; }
            dd { margin: 0 0 .4em 2.75em; }
        </style></head><body><div>
            <h3>目录</h3>
            <dl>
                <dt><a href="chapter.xhtml#one">第一章</a></dt>
                <dd><a href="chapter.xhtml#section">第一节</a></dd>
                <dt><a href="chapter.xhtml#two">第二章</a></dt>
            </dl>
        </div></body></html>"#;

        let section = parse_section(xml, &descriptor, |_| unreachable!()).unwrap();
        let texts = section
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(block) => Some(
                    block
                        .content
                        .iter()
                        .filter_map(|inline| match inline {
                            Inline::Text(run) => Some(run.text.as_str()),
                            Inline::Math(_) | Inline::Break => None,
                        })
                        .collect::<String>(),
                ),
                Block::Table(_) | Block::Image(_) | Block::Separator | Block::PageBreak => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(texts, ["目录", "第一章", "第一节", "第二章"]);
        let styles = section
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(block) => Some(block.style),
                Block::Table(_) | Block::Image(_) | Block::Separator | Block::PageBreak => None,
            })
            .collect::<Vec<_>>();
        assert_close(styles[1].margin_start, 8.0);
        assert_close(styles[1].margin_start_fraction, 0.1);
        assert_close(styles[2].margin_start, 44.0);
        assert_close(styles[2].margin_start_fraction, 0.1);
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
    }
}
