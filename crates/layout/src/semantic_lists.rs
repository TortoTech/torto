//! Shared list boundaries for focus navigation and semantic footnote numbering.
use super::semantic_paragraphs::{FocusGroupScope, focus_introduction};
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
            } => Some((depth, ordinal, marker_visible, ordered, t.style.list_group, false)),
            TextBlockKind::DefinitionTerm { depth } | TextBlockKind::DefinitionDescription { depth } =>
                Some((depth, 0, false, false, t.style.list_group, true)),
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
        let Some((depth, mut ordinal, _, ordered, container, definition)) = info(index) else {
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
            let Some((next_depth, next_ordinal, marker, next_ordered, next_container, next_definition)) = info(end)
            else {
                break;
            };
            if spine(first) != spine(end)
                || container != next_container
                || next_depth < depth
                || (next_depth == depth && next_ordered != ordered)
                || (next_depth == depth && definition != next_definition)
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

pub(super) fn focus_scope(blocks: &[&Block], mut range: Range<usize>) -> FocusGroupScope {
    let original_start = range.start;
    if matches!(blocks[original_start], Block::Text(t)
        if matches!(t.kind, TextBlockKind::ListItem { .. }))
        && let Some(intro) = focus_introduction(blocks, original_start, true)
    {
        range.start = intro;
    }
    // A separately numbered introduction can share the card, while split
    // steps keep the note labels that appear in their own text.
    let share_notes_when_split = range.start == original_start;
    FocusGroupScope {
        range,
        share_notes_when_split,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn definitions_form_one_list_without_swallowing_a_neighboring_list() {
        let mut blocks=vec![text("intro",TextBlockKind::Paragraph),text("term",TextBlockKind::DefinitionTerm{depth:0}),text("description",TextBlockKind::DefinitionDescription{depth:0}),text("other",item(0))];
        for b in &mut blocks[1..3] { if let Block::Text(t)=b { t.style.list_group=Some(10); } }
        assert_eq!(semantic_list_groups(&blocks.iter().collect::<Vec<_>>()),vec![0..3,3..4]);
    }
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
    fn list_reference_display_numbers_include_introduction_nested_items_and_companions() {
        let mut blocks = vec![text("intro", TextBlockKind::Paragraph), text("first", item(0)),
            text("nested", item(1)), text("last", item(0)), text("next-list", item(0)),
            text("after", TextBlockKind::Paragraph)];
        for (index, block) in blocks.iter_mut().enumerate() {
            let Block::Text(t) = block else { unreachable!() };
            if (1..=4).contains(&index) { t.style.list_group = Some(if index==4 { 20 } else { 10 }); }
            if index==1 { t.content.clear(); } // Citation-only items must also advance the scope.
            t.content.push(Inline::Text(TextRun { text: "(Author, ".into(), link: None,
                style: TextStyle { inline_citation: if index==0 { 7 } else { 1 }, ..Default::default() } }));
            let mut continuation = t.content.last().unwrap().clone();
            if let Inline::Text(run) = &mut continuation { run.text="2020)".into(); run.style.italic=true; }
            t.content.push(continuation);
        }
        let mut companion = blocks[2].clone();
        if let Block::Text(t) = &mut companion { t.source=None; }
        blocks.insert(3, companion);
        let original = blocks.clone();
        crate::number_list_footnotes(&mut blocks);
        let mut notes=Vec::new();
        let mut citations=Vec::new();
        for block in &blocks {
            let Block::Text(t) = block else { unreachable!() };
            notes.push(t.content.iter().find_map(|i| match i {
                Inline::Text(r) if r.style.inline_role==InlineRole::Footnote => Some(r.style.footnote_number), _=>None,
            }).unwrap_or(0));
            let numbers=t.content.iter().filter_map(|i| match i {
                Inline::Text(r) if r.style.inline_citation!=0 => Some(r.style.citation_number), _=>None,
            }).collect::<Vec<_>>();
            assert_eq!(numbers.len(),2);
            assert_eq!(numbers[0],numbers[1],"styled fragments are one reference");
            citations.push(numbers[0]);
        }
        assert_eq!(notes,vec![1,0,2,2,3,1,0]);
        assert_eq!(citations,vec![1,2,3,3,4,1,0]);
        let mut durable=blocks.clone();
        for block in &mut durable { if let Block::Text(t)=block { for inline in &mut t.content {
            if let Inline::Text(r)=inline { r.style.footnote_number=0; r.style.citation_number=0; }
        } } }
        assert_eq!(durable,original,"display numbering must not alter identities or source text");
        let numbered=blocks.clone();
        crate::number_list_footnotes(&mut blocks);
        assert_eq!(blocks,numbered,"reflow must not increment the same scope again");
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

    #[test]
    fn inferred_lists_include_adjacent_prose_without_keyword_requirements() {
        for (intro, start) in [
            ("Alphabetic scripts have the following characteristics.", 0),
            ("A preceding plain paragraph without special wording.", 0),
            ("Use these two corpora:", 0),
            ("练习包括这些项目。", 0),
        ] {
            let mut blocks = vec![
                text("intro", TextBlockKind::Paragraph),
                text("a", item(0)),
                text("b", item(0)),
            ];
            if let Block::Text(t) = &mut blocks[0] {
                if let Inline::Text(run) = &mut t.content[0] {
                    run.text = intro.into();
                }
            }
            for block in &mut blocks[1..] {
                if let Block::Text(t) = block {
                    t.style.authored_list_marker = Some(2);
                    t.style.list_group = Some(10);
                }
            }
            assert_eq!(
                semantic_list_groups(&blocks.iter().collect::<Vec<_>>()),
                vec![start..3]
            );
        }
    }
}
