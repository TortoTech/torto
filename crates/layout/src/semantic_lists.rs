//! Shared list boundaries for focus navigation and semantic footnote numbering.
use rebook_publication::{Block, Inline, TextBlockKind};
use std::ops::Range;

pub fn semantic_list_groups(blocks: &[&Block]) -> Vec<Range<usize>> {
    let info = |index: usize| match blocks[index] {
        Block::Text(t) => match t.kind {
            TextBlockKind::ListItem {
                depth,
                ordinal,
                marker_visible,
                ordered,
            } => Some((depth, ordinal, marker_visible, ordered, t.style.list_group)),
            _ => None,
        },
        _ => None,
    };
    let spine = |index: usize| match blocks[index] {
        Block::Text(t) => t.source.as_ref().map(|r| &r.start.spine),
        _ => None,
    };
    let companion = |index: usize| matches!(blocks[index], Block::Text(t) if t.source.is_none());
    let mut result = Vec::new();
    let mut index = 0;
    while index < blocks.len() {
        let Some((depth, mut ordinal, _, ordered, container)) = info(index) else {
            index += 1;
            continue;
        };
        let first = index;
        let mut end = first + 1;
        while end < blocks.len() {
            if companion(end) {
                end += 1;
                continue;
            }
            let Some((next_depth, next_ordinal, marker, next_ordered, next_container)) = info(end)
            else {
                break;
            };
            if spine(first) != spine(end)
                || container != next_container
                || next_depth < depth
                || (next_depth == depth && next_ordered != ordered)
                || (container.is_none()
                    && ordered
                    && next_depth == depth
                    && marker
                    && next_ordinal < ordinal)
            {
                break;
            }
            if next_depth == depth {
                ordinal = next_ordinal;
            }
            end += 1;
        }
        let mut previous = first.checked_sub(1);
        while previous.is_some_and(companion) {
            previous = previous.and_then(|i| i.checked_sub(1));
        }
        let start = previous.filter(|&i| {
            spine(i) == spine(first) && matches!(blocks[i], Block::Text(t)
                if t.kind == TextBlockKind::Paragraph
                    && !super::is_display_formula(t)
                    && !t.content.iter().any(|i|matches!(i, Inline::Image(_)))
                    && t.content.iter().any(|i|matches!(i, Inline::Text(r) if !r.text.trim().is_empty())))
        }).unwrap_or(first);
        result.push(start..end);
        index = end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{
        InlineRole, SourceAnchor, SourceRange, SpineItemId, TextBlock, TextRun, TextStyle,
    };
    fn text(node: &str, kind: TextBlockKind) -> Block {
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: node.into(),
            text_offset: 0,
        };
        Block::Text(TextBlock {
            kind,
            source: Some(SourceRange {
                start: anchor.clone(),
                end: SourceAnchor {
                    text_offset: 4,
                    ..anchor
                },
            }),
            style: Default::default(),
            content: vec![Inline::Text(TextRun {
                text: "Note".into(),
                link: None,
                style: TextStyle {
                    inline_role: InlineRole::Footnote,
                    ..Default::default()
                },
            })],
        })
    }
    fn item(depth: u8) -> TextBlockKind {
        TextBlockKind::ListItem {
            depth,
            ordinal: 1,
            ordered: false,
            marker_visible: true,
        }
    }

    #[test]
    fn inferred_lists_and_bilingual_notes_share_one_scope() {
        let mut blocks = vec![
            text("intro", TextBlockKind::Paragraph),
            text("a", item(0)),
            text("nested", item(1)),
            text("b", item(0)),
            text("after", TextBlockKind::Paragraph),
        ];
        let mut companion = blocks[1].clone();
        if let Block::Text(t) = &mut companion {
            t.source = None;
        }
        blocks.insert(2, companion);
        assert_eq!(
            semantic_list_groups(&blocks.iter().collect::<Vec<_>>()),
            vec![0..5]
        );
        crate::number_list_footnotes(&mut blocks);
        let numbers = blocks
            .iter()
            .map(|b| match b {
                Block::Text(t) => match &t.content[0] {
                    Inline::Text(r) => r.style.footnote_number,
                    _ => 0,
                },
                _ => 0,
            })
            .collect::<Vec<_>>();
        assert_eq!(numbers, vec![1, 2, 2, 3, 4, 0]);
    }

    #[test]
    fn authored_containers_and_non_prose_boundaries_are_preserved() {
        for kind in [
            TextBlockKind::Heading(2),
            TextBlockKind::Caption,
            TextBlockKind::Blockquote,
            TextBlockKind::Preformatted,
        ] {
            let mut blocks = vec![text("before", kind), text("a", item(0)), text("b", item(0))];
            for (i, b) in blocks.iter_mut().enumerate().skip(1) {
                if let Block::Text(t) = b {
                    t.style.list_group = Some(i as u64);
                }
            }
            assert_eq!(
                semantic_list_groups(&blocks.iter().collect::<Vec<_>>()),
                vec![1..2, 2..3]
            );
        }
    }
}
