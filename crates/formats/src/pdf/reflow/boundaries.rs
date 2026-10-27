//! Physical chapter pages can start with the preceding chapter's final lines.
use super::*;

fn key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn text(block: &TextBlock) -> String {
    block
        .content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(run) => Some(run.text.as_str()),
            _ => None,
        })
        .collect()
}

fn heading_index(stored: &StoredSection, expected: &HashSet<String>) -> Option<usize> {
    for (index, block) in stored.section.blocks.iter().enumerate() {
        let Block::Text(heading) = block else {
            continue;
        };
        if !matches!(heading.kind, TextBlockKind::Heading(_)) {
            continue;
        }
        let ordinal = index
            .checked_sub(1)
            .and_then(|i| match &stored.section.blocks[i] {
                Block::Text(t) if matches!(t.kind, TextBlockKind::HeadingOrdinal(_)) => Some(t),
                _ => None,
            });
        let title = key(&text(heading));
        let paired = ordinal.map(|ordinal| key(&format!("{} {}", text(ordinal), text(heading))));
        if expected.contains(&title)
            || paired
                .as_ref()
                .is_some_and(|label| expected.contains(label))
        {
            return Some(index - usize::from(ordinal.is_some()));
        }
    }
    None
}

pub(super) fn heading_source(stored: &StoredSection, label: &str) -> Option<SourceAnchor> {
    let index = heading_index(stored, &HashSet::from([key(label)]))?;
    match &stored.section.blocks[index] {
        Block::Text(t) => t.source.as_ref().map(|source| source.start.clone()),
        _ => None,
    }
}

/// Require a recovered heading matching this page's top-level outline. A page
/// number or a similarly styled paragraph alone cannot move any body content.
pub(super) fn chapter_prefix_len(stored: &StoredSection, labels: &[(String, u8)]) -> usize {
    let expected = labels
        .iter()
        .filter(|(_, level)| *level == 1)
        .map(|(label, _)| key(label))
        .collect::<HashSet<_>>();
    heading_index(stored, &expected)
        .filter(|index| {
            !stored.section.blocks[..*index]
                .iter()
                .any(|block| matches!(block, Block::Text(t) if t.kind.is_heading()))
        })
        .unwrap_or(0)
}

fn nodes(block: &Block, out: &mut HashSet<String>) {
    let mut add = |source: &Option<SourceRange>| {
        if let Some(source) = source {
            out.insert(source.start.node.clone());
            out.insert(source.end.node.clone());
        }
    };
    match block {
        Block::Text(t) => add(&t.source),
        Block::Image(i) => add(&i.source),
        Block::Table(t) => {
            add(&t.source);
            for t in t.text_blocks() {
                add(&t.source);
            }
        }
        Block::Figure(f) => {
            add(&f.source);
            for i in &f.images {
                add(&i.source);
            }
            for t in f.text_blocks() {
                add(&t.source);
            }
        }
        Block::Quote(q) => {
            add(&q.source);
            for t in q.body.iter().chain(q.attribution.iter()) {
                add(&t.source);
            }
        }
        Block::Note(n) => {
            add(&n.source);
            for block in &n.blocks {
                nodes(block, out);
            }
        }
        Block::Separator(s) => {
            if let Some(t) = &s.text {
                add(&t.source);
            }
            if let Some(i) = &s.image {
                add(&i.source);
            }
        }
        Block::LineBreak | Block::PageBreak => {}
    }
}

/// Partition before joining paragraphs so provenance and anchors follow their
/// actual owning blocks. Page anchors can move; outline anchors stay on titles.
pub(super) fn take_prefix(stored: &mut StoredSection, count: usize) -> StoredSection {
    let blocks = stored.section.blocks.drain(..count).collect::<Vec<_>>();
    let mut owned = HashSet::new();
    for block in &blocks {
        nodes(block, &mut owned);
    }
    let mut prefix = StoredSection {
        section: Section {
            blocks,
            anchors: Vec::new(),
            id: stored.section.id.clone(),
            href: stored.section.href.clone(),
        },
        provenance: Vec::new(),
    };
    stored.provenance.retain(|slice| {
        if owned.contains(&slice.source.start.node) {
            prefix.provenance.push(slice.clone());
            false
        } else {
            true
        }
    });
    stored.section.anchors.retain(|anchor| {
        if owned.contains(&anchor.source.node) {
            prefix.section.anchors.push(anchor.clone());
            false
        } else {
            true
        }
    });
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section() -> StoredSection {
        StoredSection {
            section: Section {
                id: spine_id(1).unwrap(),
                href: section_href(1).unwrap(),
                blocks: Vec::new(),
                anchors: Vec::new(),
            },
            provenance: Vec::new(),
        }
    }

    fn add(stored: &mut StoredSection, node: &str, value: &str, kind: TextBlockKind) {
        let start = SourceAnchor {
            spine: stored.section.id.clone(),
            node: node.into(),
            text_offset: 0,
        };
        let end = SourceAnchor {
            text_offset: value.chars().count() as u64,
            ..start.clone()
        };
        let source = SourceRange {
            start: start.clone(),
            end,
        };
        stored.section.blocks.push(Block::Text(TextBlock {
            kind,
            content: vec![Inline::Text(TextRun {
                text: value.into(),
                style: TextStyle::default(),
                link: None,
            })],
            style: BlockStyle::default(),
            source: Some(source.clone()),
        }));
        stored.provenance.push(SourceSlice {
            source,
            page: 16,
            rect: [0.0, 0.0, 20.0, 10.0],
            original_glyph: 0,
        });
        stored.section.anchors.push(SectionAnchor {
            page_label: None,
            fragment: format!("anchor-{node}"),
            source: start,
        });
    }

    #[test]
    fn chapter_split_keeps_prefix_provenance_and_anchors_with_its_previous_owner() {
        let mut stored = section();
        add(
            &mut stored,
            "tail",
            "The preceding chapter's final sentence.",
            TextBlockKind::Paragraph,
        );
        add(
            &mut stored,
            "ordinal",
            "2",
            TextBlockKind::HeadingOrdinal(1),
        );
        add(
            &mut stored,
            "title",
            "The next chapter",
            TextBlockKind::Heading(1),
        );
        add(
            &mut stored,
            "body",
            "The new chapter begins here.",
            TextBlockKind::Paragraph,
        );
        let note_href = stored.section.href.resolve("#note").unwrap();
        if let Block::Text(t) = &mut stored.section.blocks[0]
            && let Inline::Text(r) = &mut t.content[0]
        {
            r.link = Some(note_href.clone());
        }
        let count = chapter_prefix_len(&stored, &[("2 The next chapter".into(), 1)]);
        assert_eq!(count, 1);
        assert_eq!(
            heading_source(&stored, "2 The next chapter").unwrap().node,
            "ordinal"
        );
        let mut prefix = take_prefix(&mut stored, count);
        prefix.section.href = section_href(0).unwrap();
        relocate(&mut prefix, spine_id(0).unwrap(), section_href(0).unwrap());
        assert_eq!(prefix.section.blocks.len(), 1);
        assert_eq!(prefix.provenance.len(), 1);
        assert_eq!(
            prefix.provenance[0].source.start.spine,
            spine_id(0).unwrap()
        );
        assert_eq!(prefix.section.anchors[0].fragment, "anchor-tail");
        assert_eq!(stored.section.anchors.len(), 3);
        assert!(
            stored
                .section
                .anchors
                .iter()
                .all(|a| a.source.node != "tail")
        );
        assert_eq!(stored.provenance.len(), 3);
        let Block::Text(t) = &prefix.section.blocks[0] else {
            panic!()
        };
        let Inline::Text(r) = &t.content[0] else {
            panic!()
        };
        assert_eq!(r.link, Some(note_href));
    }

    #[test]
    fn prefix_requires_matching_top_level_heading_and_respects_prior_heading_boundaries() {
        let mut stored = section();
        add(
            &mut stored,
            "tail",
            "The next chapter",
            TextBlockKind::Paragraph,
        );
        add(
            &mut stored,
            "title",
            "Unrelated title",
            TextBlockKind::Heading(1),
        );
        assert_eq!(
            chapter_prefix_len(&stored, &[("The next chapter".into(), 1)]),
            0
        );
        assert!(heading_source(&stored, "The next chapter").is_none());
        assert_eq!(
            chapter_prefix_len(&stored, &[("Unrelated title".into(), 2)]),
            0
        );
        add(
            &mut stored,
            "later",
            "The next chapter",
            TextBlockKind::Heading(1),
        );
        assert_eq!(
            chapter_prefix_len(&stored, &[("The next chapter".into(), 1)]),
            0
        );
        assert_eq!(
            heading_source(&stored, "The next chapter").unwrap().node,
            "later"
        );
    }
}
