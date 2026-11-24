//! Source container boundaries shared by focus, AI batching and note numbering.
use rebook_publication::{Block, Inline, InlineRole, LinkRole, SeparatorKind, TextBlockKind};
use std::ops::Range;

pub fn semantic_paragraph_groups(blocks: &[&Block]) -> Vec<Range<usize>> {
    if !blocks.iter().any(|b| matches!(b, Block::Text(t) if t.style.paragraph_group.is_some())) {
        return Vec::new();
    }
    paragraph_groups(blocks, &super::semantic_list_groups(blocks))
}

fn paragraph_groups(blocks: &[&Block], lists: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut claimed = vec![false; blocks.len()];
    for list in lists {
        claimed[list.clone()].fill(true);
    }
    let paragraph = |i: usize| match blocks[i] {
        Block::Text(t)
            if t.kind == TextBlockKind::Paragraph
                && !claimed[i]
                && !super::is_display_formula(t) =>
        {
            t.style.paragraph_group.map(|id| (id, t))
        }
        _ => None,
    };
    let mut groups = Vec::new();
    let mut start = 0;
    while start < blocks.len() {
        let Some((id, first)) = paragraph(start) else {
            start += 1;
            continue;
        };
        let spine = first.source.as_ref().map(|s| &s.start.spine);
        let mut count = usize::from(first.source.is_some());
        let mut end = start + 1;
        while end < blocks.len() {
            let Some((next, text)) = paragraph(end) else {
                break;
            };
            if next != id
                || text
                    .source
                    .as_ref()
                    .is_some_and(|s| Some(&s.start.spine) != spine)
            {
                break;
            }
            count += usize::from(text.source.is_some());
            end += 1;
        }
        if count >= 2 {
            groups.push(start..end);
        }
        start = end;
    }
    groups
}

pub struct FocusGroupScope {
    pub range: Range<usize>,
    pub share_notes_when_split: bool,
}

/// Focus-only scopes may include introductions without changing the underlying
/// paragraph identities, layout, or reference numbering.
pub fn focus_group_scopes(blocks: &[&Block]) -> Vec<FocusGroupScope> {
    let lists = super::semantic_list_groups(blocks);
    let paragraphs = if blocks.iter().any(|b| matches!(b, Block::Text(t) if t.style.paragraph_group.is_some())) {
        paragraph_groups(blocks, &lists)
    } else {
        Vec::new()
    };
    lists
        .into_iter()
        .map(|range| super::semantic_lists::focus_scope(blocks, range))
        .chain(paragraphs.into_iter().map(|mut range| {
            if let Some(intro) = focus_introduction(blocks, range.start, false) {
                range.start = intro;
            }
            FocusGroupScope {
                range,
                share_notes_when_split: false,
            }
        }))
        .collect()
}

pub(super) fn focus_introduction(
    blocks: &[&Block],
    start: usize,
    across_blank_spacing: bool,
) -> Option<usize> {
    let Block::Text(first) = blocks[start] else {
        return None;
    };
    let spine = &first.source.as_ref()?.start.spine;
    let (index, block) = blocks[..start]
        .iter()
        .copied()
        .enumerate()
        .rev()
        .find(|(_, block)| match block {
            Block::Text(t) if t.source.is_none() => false,
            Block::Separator(s) if across_blank_spacing => {
                s.in_quote
                    || s.text.is_some()
                    || s.image.is_some()
                    || !matches!(s.kind, SeparatorKind::Spacing | SeparatorKind::AuthoredSpacing)
            }
            _ => true,
        })?;
    let Block::Text(text) = block else {
        return None;
    };
    if text.kind != TextBlockKind::Paragraph
        || &text.source.as_ref()?.start.spine != spine
        || text.style.paragraph_group.is_some()
        || text.content.iter().any(|inline| matches!(inline, Inline::Image(_)))
        || super::is_display_formula(text)
    {
        return None;
    }
    let last = text.content.iter().rev().find_map(|inline| match inline {
        Inline::Math(_) | Inline::Image(_) => Some('\u{fffc}'),
        _ => inline
            .text_runs()
            .iter()
            .rev()
            .filter(|run| {
                run.style.inline_role == InlineRole::Normal && run.style.link_role == LinkRole::Normal
            })
            .find_map(|run| {
                run.text
                    .trim_end_matches(|ch: char| {
                        ch.is_whitespace() || matches!(ch, '\u{200b}' | '\u{2060}' | '\u{feff}')
                    })
                    .chars()
                    .next_back()
            }),
    });
    matches!(last, Some(':' | '：')).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{SourceAnchor, SourceRange, SpineItemId, TextBlock};
    fn text(node: &str, group: Option<u64>) -> Block {
        let anchor = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: node.into(),
            text_offset: 0,
        };
        Block::Text(TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![],
            source: Some(SourceRange {
                start: anchor.clone(),
                end: anchor,
            }),
            style: rebook_publication::BlockStyle {
                paragraph_group: group,
                ..Default::default()
            },
        })
    }
    #[test]
    fn companions_do_not_split_groups_and_existing_lists_keep_their_introduction() {
        let mut companion = text("translated", Some(1));
        let Block::Text(t) = &mut companion else {
            unreachable!()
        };
        t.source = None;
        let mut item = text("item", None);
        let Block::Text(t) = &mut item else {
            unreachable!()
        };
        t.kind = TextBlockKind::ListItem {
            ordered: false,
            ordinal: 1,
            depth: 0,
            marker_visible: true,
        };
        let blocks = [
            text("a", Some(1)),
            companion,
            text("b", Some(1)),
            text("intro", Some(2)),
            item,
        ];
        let refs = blocks.iter().collect::<Vec<_>>();
        assert_eq!(semantic_paragraph_groups(&refs), vec![0..3]);
    }
}
