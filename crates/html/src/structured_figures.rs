//! Preserve an authored figure's structured body and explicit source paragraphs.
use super::*;
use rebook_publication::media_references::{self, MediaKind};

fn source_text(text: &str) -> bool {
    let text = text.trim_start().to_lowercase();
    [
        "source:",
        "sources:",
        "source：",
        "credit:",
        "credits:",
        "credit：",
        "来源:",
        "来源：",
        "图片来源:",
        "图片来源：",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
}

pub(super) fn annotation_kind(text: &TextBlock) -> TextBlockKind {
    let value: String = text
        .content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|run| run.text.as_str())
        .collect();
    if source_text(&value) {
        TextBlockKind::QuoteAttribution
    } else {
        TextBlockKind::Caption
    }
}

impl ReadingIrParser<'_> {
    pub(super) fn parse_structured_figure(&mut self, node: Node<'_, '_>) -> Result<(), HtmlError> {
        let start = self.blocks.len();
        if node
            .children()
            .any(|child| child.is_text() && child.text().is_some_and(|s| !s.trim().is_empty()))
        {
            self.parse_block_container(node)?;
            self.finish_structured_figure(start, node);
            return Ok(());
        }
        for child in node.children().filter(Node::is_element) {
            let child_start = self.blocks.len();
            self.parse_node(child)?;
            let caption = child.has_tag_name("figcaption");
            let source = source_text(&node_text(child));
            if caption || source {
                for text in self.blocks[child_start..]
                    .iter_mut()
                    .flat_map(Block::text_blocks_mut)
                {
                    text.kind = if source {
                        TextBlockKind::QuoteAttribution
                    } else {
                        annotation_kind(text)
                    };
                }
            }
        }
        self.finish_structured_figure(start, node);
        Ok(())
    }

    fn finish_structured_figure(&mut self, start: usize, node: Node<'_, '_>) {
        let body = self.blocks.split_off(start);
        if body.is_empty() {
            return;
        }
        let source = combined_block_source(&body);
        let caption_position = if matches!(body.first(), Some(Block::Text(text)) if text.kind == TextBlockKind::Caption)
        {
            CaptionPosition::Before
        } else {
            CaptionPosition::After
        };
        self.blocks.push(Block::Figure(FigureBlock {
            body,
            images: Vec::new(),
            captions: Vec::new(),
            caption_position,
            style: self.styles.block_style(node, BlockStyle::default()),
            source,
        }));
    }

    pub(super) fn try_parse_text_figure(
        &mut self,
        nodes: &[Node<'_, '_>],
    ) -> Result<Option<usize>, HtmlError> {
        fn label(node: Node<'_, '_>) -> bool {
            if !node.is_element() || !matches!(node.tag_name().name(), "p" | "figcaption") {
                return false;
            }
            // Reject ordinary prose before allocating/scanning the whole paragraph.
            let initial = node
                .descendants()
                .filter(Node::is_text)
                .filter_map(|node| node.text())
                .flat_map(str::chars)
                .find(|ch| !ch.is_whitespace());
            if !matches!(initial, Some('f' | 'F' | '图' | '圖' | '插')) {
                return false;
            }
            let text = node_text(node);
            let lower = text.to_lowercase();
            media_references::caption_label(&text)
                .is_some_and(|label| label.key.kind == MediaKind::Figure)
                && ![" shows ", " illustrates ", " depicts ", "所示", "展示了"]
                    .iter()
                    .any(|cue| lower.contains(cue))
        }
        fn body(node: Node<'_, '_>) -> bool {
            node.is_element()
                && matches!(
                    node.tag_name().name(),
                    "div" | "aside" | "section" | "ul" | "ol" | "table" | "pre"
                )
                && !is_semantic_footnote_definition(node)
        }
        let Some(&first) = nodes.first() else {
            return Ok(None);
        };
        // A caption is consumed with its following body before that body gets a
        // chance to claim the next figure's caption. After-caption sequences
        // still work when parsing begins at the body.
        let caption_first = label(first);
        if !caption_first && !body(first) {
            return Ok(None);
        }
        let Some((at, next)) = nodes.iter().copied().enumerate().skip(1).find(|(_, node)| {
            if node.is_text() {
                return node.text().is_some_and(|text| !text.trim().is_empty());
            }
            node.is_element()
                && !(matches!(node.tag_name().name(), "a" | "span")
                    && !node_has_visible_text(*node))
        }) else {
            return Ok(None);
        };
        let (content, caption) = if caption_first && body(next) {
            (next, first)
        } else if !caption_first && label(next) {
            (first, next)
        } else {
            return Ok(None);
        };
        if self
            .heading_fragments
            .contains(node_fragment(caption).unwrap_or(""))
        {
            return Ok(None);
        }
        // The adjacent caption qualifies the candidate before scanning its tree.
        if has_descendant_image(content)
            || content.descendants().any(|node| {
                node.is_element()
                    && matches!(
                        node.tag_name().name(),
                        "figure" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    )
            })
        {
            return Ok(None);
        }
        let start = self.blocks.len();
        for node in nodes[..=at].iter().copied().filter(Node::is_element) {
            let child_start = self.blocks.len();
            self.parse_node(node)?;
            if node == caption {
                for text in self.blocks[child_start..]
                    .iter_mut()
                    .flat_map(Block::text_blocks_mut)
                {
                    text.kind = TextBlockKind::Caption;
                }
            }
        }
        self.finish_structured_figure(start, content);
        Ok(Some(at + 1))
    }
}

pub(super) fn attach_sources(blocks: &mut Vec<Block>) {
    let mut result = Vec::with_capacity(blocks.len());
    for mut block in std::mem::take(blocks) {
        match &mut block {
            Block::Figure(figure) => attach_sources(&mut figure.body),
            Block::Note(note) => attach_sources(&mut note.blocks),
            _ => {}
        }
        block = promote_caption_rasters(block);
        // Some publishers put the numbered caption before a separate figure,
        // or put a hidden note definition between an image and its caption.
        // Only cross note definitions; ordinary prose is a hard boundary.
        let previous = result.iter().rposition(|block| {
            !matches!(block,
            Block::Note(note) if note.kind == NoteBlockKind::Definition)
        });
        if matches!(&block, Block::Figure(_))
            && accepts_caption(&block)
            && previous == result.len().checked_sub(1)
            && matches!(result.last(), Some(Block::Text(text)) if numbered_caption(text))
        {
            let Block::Text(mut caption) = result.pop().unwrap() else {
                unreachable!()
            };
            caption.kind = TextBlockKind::Caption;
            block = with_caption(block, caption, true);
        } else if let Block::Text(text) = &block
            && numbered_caption(text)
            && let Some(index) = previous
            && index + 1 < result.len()
            && accepts_caption(&result[index])
        {
            let mut caption = text.clone();
            caption.kind = TextBlockKind::Caption;
            let media = result.remove(index);
            result.insert(index, with_caption(media, caption, false));
            continue;
        }
        if let Block::Text(mut text) = block {
            if annotation_kind(&text) == TextBlockKind::QuoteAttribution {
                if matches!(result.last(), Some(Block::Text(t)) if t.kind == TextBlockKind::Caption)
                {
                    let mut start = result.len() - 1;
                    while start > 0 && matches!(result[start - 1], Block::Image(_)) {
                        start -= 1;
                    }
                    if start < result.len() - 1 {
                        let group = result.split_off(start);
                        result.extend(nested_media::group_images_and_captions(
                            group,
                            BlockStyle::default(),
                        ));
                    }
                }
                if let Some(Block::Figure(figure)) = result.last_mut() {
                    text.kind = TextBlockKind::QuoteAttribution;
                    if let (Some(outer), Some(source)) = (&mut figure.source, &text.source) {
                        outer.end = source.end.clone();
                    }
                    if figure.body.is_empty() {
                        if figure.caption_position == CaptionPosition::Before {
                            figure.caption_position = CaptionPosition::Split(figure.captions.len());
                        }
                        figure.captions.push(text);
                    } else {
                        figure.body.push(Block::Text(text));
                    }
                    continue;
                }
            }
            result.push(Block::Text(text));
        } else {
            result.push(block);
        }
    }
    *blocks = result;
}

fn promote_caption_rasters(block: Block) -> Block {
    let Block::Text(mut caption) = block else {
        return block;
    };
    if !numbered_caption(&caption) {
        return Block::Text(caption);
    }
    let mut images = Vec::new();
    let mut offset = 0;
    for inline in &mut caption.content {
        if let Inline::Image(run) = inline
            && run.intrinsic_sizing
            && matches!(
                run.image.alt.trim().to_ascii_lowercase().as_str(),
                "figure" | "图" | "圖" | "插图"
            )
        {
            let mut image = run.image.clone();
            image.source = caption.source.as_ref().map(|source| {
                let mut start = source.start.clone();
                start.text_offset += offset;
                SourceRange {
                    start: start.clone(),
                    end: start,
                }
            });
            images.push(image);
            // Keep the original inline slot and all following text offsets.
            *inline = Inline::Text(TextRun {
                text: String::new(),
                style: Default::default(),
                link: None,
            });
        }
        offset += inline
            .text_runs()
            .iter()
            .map(|run| run.text.chars().count() as u64)
            .sum::<u64>();
    }
    if images.is_empty() {
        return Block::Text(caption);
    }
    caption.kind = TextBlockKind::Caption;
    Block::Figure(FigureBlock {
        source: caption.source.clone(),
        images,
        captions: vec![caption],
        body: vec![],
        caption_position: CaptionPosition::After,
        style: BlockStyle::default(),
    })
}

fn numbered_caption(text: &TextBlock) -> bool {
    if !matches!(text.kind, TextBlockKind::Paragraph | TextBlockKind::Caption) {
        return false;
    }
    let initial = text
        .content
        .iter()
        .flat_map(Inline::text_runs)
        .flat_map(|run| run.text.chars())
        .find(|ch| !ch.is_whitespace());
    if !matches!(
        initial,
        Some('f' | 'F' | '图' | '圖' | '插' | '▲' | '△' | '◆' | '◇')
    ) {
        return false;
    }
    let value: String = text
        .content
        .iter()
        .flat_map(Inline::text_runs)
        .filter(|run| run.style.link_role != LinkRole::FootnoteReference)
        .map(|run| run.text.as_str())
        .collect();
    let lower = value.to_lowercase();
    media_references::caption_label(&value).is_some_and(|label| label.key.kind == MediaKind::Figure)
        && ![" shows ", " illustrates ", " depicts ", "所示", "展示了"]
            .iter()
            .any(|cue| lower.contains(cue))
}

fn accepts_caption(block: &Block) -> bool {
    match block {
        Block::Image(_) => true,
        Block::Figure(figure) => figure.caption_blocks().next().is_none(),
        _ => false,
    }
}

fn with_caption(block: Block, caption: TextBlock, before: bool) -> Block {
    let mut figure = match block {
        Block::Figure(figure) => figure,
        Block::Image(image) => FigureBlock {
            source: image.source.clone(),
            images: vec![image],
            body: vec![],
            captions: vec![],
            caption_position: CaptionPosition::After,
            style: BlockStyle::default(),
        },
        _ => unreachable!(),
    };
    if let (Some(outer), Some(source)) = (&mut figure.source, &caption.source) {
        if before {
            outer.start = source.start.clone();
        } else {
            outer.end = source.end.clone();
        }
    }
    if before {
        if figure.body.is_empty() {
            figure.caption_position = if figure.captions.is_empty() {
                CaptionPosition::Before
            } else {
                CaptionPosition::Split(1)
            };
            figure.captions.insert(0, caption);
        } else {
            figure.body.insert(0, Block::Text(caption));
            figure.caption_position = CaptionPosition::Before;
        }
    } else if figure.body.is_empty() {
        figure.captions.push(caption);
    } else {
        figure.body.push(Block::Text(caption));
    }
    Block::Figure(figure)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(body: &str) -> Section {
        let descriptor = SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        };
        parse_section(
            &format!("<html><body>{body}</body></html>"),
            &descriptor,
            |_| None,
        )
        .unwrap()
    }
    fn plain(t: &TextBlock) -> String {
        t.content
            .iter()
            .flat_map(Inline::text_runs)
            .map(|r| r.text.as_str())
            .collect()
    }

    #[test]
    fn consecutive_preceding_captions_belong_to_the_following_body() {
        let chinese = parse("<p>插图 1 示例</p><div><p>示例正文。</p></div>");
        assert!(matches!(chinese.blocks.as_slice(), [Block::Figure(_)]));
        let section = parse(
            "<p id='c1'>Figure 4.1 Fruit</p><span id='page'/><div><ul><li><p>stone fruit</p></li></ul></div><p id='c2'>Figure 4.2 Equations</p><div><ol><li><p>Solve x</p></li></ol></div><p id='c3'>Figure 4.3 Clouds</p><div><ol><li><p>What are clouds?</p></li></ol></div><p>Next prose.</p>",
        );
        assert_eq!(section.blocks.len(), 4);
        for (index, (number, own, other)) in [
            ("4.1", "stone fruit", "Solve x"),
            ("4.2", "Solve x", "What are clouds?"),
            ("4.3", "What are clouds?", "stone fruit"),
        ]
        .into_iter()
        .enumerate()
        {
            let Block::Figure(figure) = &section.blocks[index] else {
                panic!("figure {index}")
            };
            assert_eq!(figure.caption_position, CaptionPosition::Before);
            let captions = figure
                .caption_blocks()
                .map(plain)
                .collect::<Vec<_>>()
                .join(" ");
            let body = figure
                .text_blocks()
                .filter(|t| t.kind != TextBlockKind::Caption)
                .map(plain)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(captions.contains(number));
            assert!(body.contains(own));
            assert!(!body.contains(other));
        }
        assert!(section.anchors.iter().any(|a| a.fragment == "page"));
        let after = parse(
            "<div><ul><li><p>First body</p></li></ul></div><p>Figure 1 First</p><div><ol><li><p>Second body</p></li></ol></div><p>Figure 2 Second</p>",
        );
        assert_eq!(after.blocks.len(), 2);
        assert!(after.blocks.iter().all(
            |b| matches!(b, Block::Figure(f) if f.caption_position == CaptionPosition::After)
        ));
    }

    #[test]
    fn text_figure_keeps_lists_counters_anchors_and_outer_annotations() {
        let section = parse(
            "<style>.example{counter-reset:num 5}.alpha{list-style-type:lower-alpha}</style><p>Before.</p><div><p id='intro'>Solve:</p><ol class='example'><li><p>x=2</p></li></ol><ol class='alpha'><li><p>First</p></li><li><p>Second</p></li></ol></div><p id='caption'>Figure 4.3 A nested list</p><p>Source: Authors.</p><p id='after'>After.</p>",
        );
        let [Block::Text(_), Block::Figure(f), Block::Text(after)] = section.blocks.as_slice()
        else {
            panic!("{:?}", section.blocks)
        };
        assert!(f.images.is_empty());
        let texts: Vec<_> = f.text_blocks().collect();
        assert_eq!(
            texts.iter().map(|t| plain(t)).collect::<Vec<_>>(),
            [
                "Solve:",
                "x=2",
                "First",
                "Second",
                "Figure 4.3 A nested list",
                "Source: Authors."
            ]
        );
        assert!(matches!(
            texts[1].kind,
            TextBlockKind::ListItem { ordinal: 6, .. }
        ));
        assert_eq!(
            texts[2].style.list_marker_format,
            Some(rebook_publication::ListMarkerFormat::LowerAlpha)
        );
        assert_eq!(texts[5].kind, TextBlockKind::QuoteAttribution);
        assert_eq!(f.caption_blocks().count(), 2);
        assert_eq!(plain(after), "After.");
        for id in ["intro", "caption", "after"] {
            assert!(section.anchors.iter().any(|a| a.fragment == id));
        }
    }

    #[test]
    fn figure_table_body_is_distinct_from_its_figure_caption() {
        let section = parse(
            "<figure id='f'><table><tr><td id='cell'>A real text cell</td></tr></table><figcaption>Figure 5.1 A tabular example</figcaption><p>Credit: Example.</p></figure><p>Next.</p>",
        );
        let Block::Figure(f) = &section.blocks[0] else {
            panic!("{:?}", section.blocks)
        };
        assert!(matches!(f.body[0], Block::Table(_)));
        assert_eq!(f.text_blocks().count(), 3);
        assert_eq!(f.caption_blocks().count(), 2);
        assert_eq!(
            f.text_blocks().last().unwrap().kind,
            TextBlockKind::QuoteAttribution
        );
    }

    #[test]
    fn sources_attach_only_to_adjacent_media_and_leave_prose_alone() {
        for media in [
            "<figure><img src='a.jpg'/><figcaption>Figure 1 Example</figcaption></figure>",
            "<p><img src='a.jpg'/></p><p>Figure 1 Example</p>",
        ] {
            let section = parse(&format!(
                "{media}<p>Source: A.</p><p>Credits: B.</p><p>Ordinary prose.</p><p>Source: C.</p>"
            ));
            let Block::Figure(f) = &section.blocks[0] else {
                panic!("{:?}", section.blocks)
            };
            assert_eq!(f.captions.len(), 3);
            assert_eq!(f.captions[1].kind, TextBlockKind::QuoteAttribution);
            assert_eq!(section.blocks.len(), 3);
        }
        let section = parse(
            "<div><p>Two paragraphs.</p><p>In a container.</p></div><p>Figure 1 shows a prose reference.</p>",
        );
        assert!(
            section
                .blocks
                .iter()
                .all(|b| !matches!(b, Block::Figure(_)))
        );
    }
}
