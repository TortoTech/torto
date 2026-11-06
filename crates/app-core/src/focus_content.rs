//! Source-owned focus membership; hosts add geometry without inventing reading steps.
use crate::reading_text::{block_focus_text, block_source_range, focus_block_paint_ranges};
use rebook_publication::{Block, Inline, LinkRole, SourceRange, TextBlockKind};

pub fn is_footnote_definition(block: &Block) -> bool {
    block.is_footnote_definition()
        || matches!(block, Block::Note(_))
        || block.text_blocks().any(|text| {
            text.content.iter().any(|inline| {
                matches!(inline, Inline::Text(run)
            if run.link.is_some() && run.style.link_role == LinkRole::FootnoteBacklink)
            })
        })
}

/// The same exclusions apply to navigation, regardless of the host's focus style.
pub fn is_body(block: &Block, unified: bool) -> bool {
    if is_footnote_definition(block) || block_source_range(block).is_none() {
        return false;
    }
    match block {
        Block::Text(text) => !text.kind.is_heading() && !block_focus_text(block).trim().is_empty(),
        Block::Quote(_) => !block_focus_text(block).trim().is_empty(),
        Block::Table(table) => {
            !block_focus_text(block).trim().is_empty()
                && !(unified
                    && table
                        .visualization
                        .as_ref()
                        .is_some_and(|chart| chart.active && chart.href.is_none()))
        }
        // A scanned/PDF page is the backdrop of its extracted paragraphs.
        Block::Image(image) => image.text_layer.is_none(),
        Block::Figure(_) => true,
        Block::Note(_) | Block::Separator(_) | Block::LineBreak | Block::PageBreak => false,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub key: SourceRange,
    pub parts: usize,
}

#[derive(Clone, Debug)]
pub struct Unit {
    pub ranges: Vec<SourceRange>,
    /// Display-only association; never put headings into the body's source anchor.
    pub headings: Vec<SourceRange>,
    pub leading: bool,
    pub group: Option<Group>,
    index: usize,
}

/// Flat parts retain their complete source identity across logical-page cuts.
/// Nested list descendants and an image's immediate caption are one part; a host
/// can then combine a whole semantic group only when it fits its usable viewport.
pub fn units(blocks: &[Block], unified: bool) -> Vec<Unit> {
    units_in_ranges(blocks, unified, None)
}
pub fn units_in_ranges(
    blocks: &[Block],
    unified: bool,
    allowed: Option<&[SourceRange]>,
) -> Vec<Unit> {
    let refs = blocks.iter().collect::<Vec<_>>();
    let scopes = rebook_layout::semantic_list_groups(&refs)
        .into_iter()
        .chain(rebook_layout::semantic_paragraph_groups(&refs))
        .collect::<Vec<_>>();
    let mut units: Vec<Unit> = Vec::new();
    let mut headings = Vec::new();
    let mut root: Option<(usize, u8)> = None;
    for (index, block) in blocks.iter().enumerate() {
        if allowed.is_some_and(|allowed| {
            block_source_range(block).is_some_and(|source| {
                !allowed.iter().any(|visible| {
                    source == visible
                        || crate::reading_text::source_range_contains_anchor(source, &visible.start)
                        || crate::reading_text::source_range_contains_anchor(visible, &source.start)
                })
            })
        }) {
            headings.clear();
            root = None;
            continue;
        }
        if is_footnote_definition(block) {
            headings.clear();
            root = None;
            continue;
        }
        if let Block::Text(text) = block {
            if text.kind.is_heading() {
                root = None;
                headings.extend(text.source.iter().cloned());
                continue;
            }
            // A bilingual companion must not break the authored list tree.
            if text.source.is_none() {
                continue;
            }
        }
        if !is_body(block, unified) {
            if !matches!(block, Block::Text(text) if matches!(text.kind, TextBlockKind::ListItem {depth, ..} if depth > 0))
            {
                root = None;
            }
            continue;
        }
        let range = block_source_range(block).expect("focus body has a source");
        let ranges = focus_block_paint_ranges(block, range);
        let caption_image = matches!(block, Block::Text(text) if text.kind == TextBlockKind::Caption)
            && index.checked_sub(1).is_some_and(|previous| {
                matches!(&blocks[previous], Block::Image(image)
                if image.text_layer.is_none() && image.source.as_ref().is_some_and(|source|
                    units.last().is_some_and(|unit| unit.ranges.first() == Some(source))))
            });
        let depth = if let Block::Text(text) = block {
            if let TextBlockKind::ListItem { depth, .. } = text.kind {
                Some(depth)
            } else {
                None
            }
        } else {
            None
        };
        let descendant = depth.and_then(|depth| {
            root.filter(|(at, root_depth)| {
                depth > *root_depth && units[*at].ranges[0].start.spine == range.start.spine
            })
            .map(|(at, _)| at)
        });
        if caption_image || descendant.is_some() {
            let at = descendant.unwrap_or(units.len() - 1);
            units[at].ranges.extend(ranges);
            if caption_image {
                units[at].headings.append(&mut headings);
                root = None;
            }
            continue;
        }
        let at = units.len();
        units.push(Unit {
            ranges,
            headings: std::mem::take(&mut headings),
            leading: at == 0,
            group: None,
            index,
        });
        root = depth.map(|depth| (at, depth));
    }
    let mut index = 0;
    while index < units.len() {
        let scope = scopes
            .iter()
            .find(|scope| scope.contains(&units[index].index));
        let Some(scope) = scope else {
            index += 1;
            continue;
        };
        let mut end = index + 1;
        while end < units.len() && scope.contains(&units[end].index) {
            end += 1;
        }
        let mut key = units[index].ranges[0].clone();
        key.end = units[end - 1].ranges.last().unwrap().end.clone();
        let group = Group {
            key,
            parts: end - index,
        };
        for unit in &mut units[index..end] {
            unit.group = Some(group.clone());
        }
        index = end;
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{PublicationUrl, SpineItem, SpineItemId};
    fn parse(body: &str) -> Vec<Block> {
        let item = SpineItem {
            id: SpineItemId::new("body").unwrap(),
            href: PublicationUrl::parse("body.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        };
        rebook_html::parse_section(&format!("<html><body>{body}</body></html>"), &item, |_| {
            None
        })
        .unwrap()
        .blocks
    }
    #[test]
    fn headings_are_display_only_followers_of_each_semantic_body_kind() {
        for body in [
            "<p>Following prose.</p>",
            "<blockquote><p>Following quote.</p></blockquote>",
            "<p><img src='x.png' alt='Following image'/></p>",
            "<table><tr><td>Following table.</td></tr></table>",
            "<ul><li>Following item.</li><li>Next item.</li></ul>",
        ] {
            let blocks = parse(&format!(
                "<p>Earlier.</p><h2>Topic</h2><h3>Subtopic</h3>{body}<p>Later.</p>"
            ));
            let units = units(&blocks, true);
            assert!(units.len() >= 3, "{body}");
            assert!(units[0].headings.is_empty());
            assert_eq!(units[1].headings.len(), 2, "{body}");
            assert!(
                units[1]
                    .ranges
                    .iter()
                    .all(|range| !units[1].headings.contains(range))
            );
            assert!(units.last().unwrap().headings.is_empty());
            assert!(units.iter().skip(1).all(|u| !u.leading));
        }
    }
    #[test]
    fn empty_companion_backlink_fixed_page_and_pending_chart_are_not_stops() {
        let mut blocks = parse(
            "<h2>Title</h2><p>Companion</p><p>Actual body.</p><p>   </p><p><a href='#body' role='doc-backlink'>Back</a> Note definition.</p><p><img src='x.png' alt='Page'/></p><table><tr><td>Value</td></tr></table>",
        );
        let companion = blocks
            .iter_mut()
            .find_map(|b| {
                if let Block::Text(t) = b {
                    text_block(t, "Companion").then_some(t)
                } else {
                    None
                }
            })
            .unwrap();
        companion.source = None;
        for block in &mut blocks {
            if let Block::Image(image) = block {
                image.text_layer = Some(rebook_publication::FixedPageTextLayer {
                    width: 100.0,
                    height: 100.0,
                    text: "Actual body.".into(),
                    spans: vec![],
                    replacement: None,
                });
            }
            if let Block::Table(table) = block {
                table.visualization = Some(rebook_publication::TableVisualization {
                    root: table.source.clone().unwrap(),
                    href: None,
                    active: true,
                });
            }
        }
        let units = units(&blocks, true);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].headings.len(), 1);
        assert!(units[0].leading);
        assert!(
            blocks.iter().filter(|b| is_footnote_definition(b)).count() > 0,
            "fixture must contain a legacy backlink definition"
        );
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::Table(_)) && is_body(b, false)),
            "pending charts only suppress the hidden unified table"
        );
    }
    fn text_block(text: &rebook_publication::TextBlock, value: &str) -> bool {
        crate::reading_text::text_block_focus_text(text) == value
    }
    #[test]
    fn nested_lists_containers_and_adjacent_image_captions_keep_body_identity() {
        let blocks = parse(
            "<h2>Title</h2><ul><li>Parent<ul><li>Child</li><li>Other child</li></ul></li><li>Last root</li></ul><p>After.</p>",
        );
        let parts = units(&blocks, true);
        assert_eq!(parts.len(), 3);
        assert_eq!(
            parts[0].ranges.len(),
            3,
            "descendants remain with their root even for a tall list"
        );
        assert_eq!(parts[0].group, parts[1].group);
        assert_eq!(parts[0].group.as_ref().unwrap().parts, 2);
        assert!(parts[2].group.is_none());
        let blocks = parse(
            "<p>Before.</p><h2>Title</h2><div><p>One.</p><p>Two.</p><p>Three.</p></div><p>After.</p>",
        );
        let parts = units(&blocks, true);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[1].group.as_ref().unwrap().parts, 3);
        assert_eq!(parts[1].group, parts[3].group);
        assert!(parts[0].group.is_none() && parts[4].group.is_none());
        let mut blocks =
            parse("<p><img src='x.png' alt='Diagram'/></p><p>Figure 1. Caption.</p><p>After.</p>");
        for block in &mut blocks {
            if let Block::Text(t) = block
                && text_block(t, "Figure 1. Caption.")
            {
                t.kind = TextBlockKind::Caption;
            }
        }
        let parts = units(&blocks, true);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].ranges.len(), 2);
    }
    #[test]
    fn reading_unit_ranges_do_not_inherit_another_units_headings() {
        let blocks = parse(
            "<h2>Earlier title</h2><p>Earlier body.</p><h2>Current title</h2><p>Current body.</p>",
        );
        let allowed = blocks
            .iter()
            .skip(2)
            .filter_map(block_source_range)
            .cloned()
            .collect::<Vec<_>>();
        let parts = units_in_ranges(&blocks, true, Some(&allowed));
        assert_eq!(parts.len(), 1);
        assert!(parts[0].leading);
        assert_eq!(parts[0].headings, vec![allowed[0].clone()]);
    }
    #[test]
    fn table_titles_notes_and_figure_credits_are_paint_members_not_extra_steps() {
        for media in [
            "<p>Table 1. Results</p><table><tr><td>Value</td><td>42</td></tr></table><footer><p>Source: Table author.</p><p>Unmarked credit continuation.</p><p>Note: Rounded values.</p></footer>",
            "<figure><img src='x.png' alt='Diagram'/><figcaption>Figure 1. Results</figcaption><p>Source: Image author.</p></figure>",
            "<div><p>Figure 1. Results</p><img src='x.png' alt='Diagram'/><p>Source: Image author.</p></div>",
        ] {
            let blocks = parse(&format!(
                "<p>Before.</p><h2>Media topic</h2>{media}<p>After.</p>"
            ));
            let parts = units(&blocks, true);
            assert_eq!(
                parts.len(),
                3,
                "{media}: titles, notes and credits are not independent stops"
            );
            let unit = &parts[1];
            assert_eq!(unit.headings.len(), 1);
            let media = blocks
                .iter()
                .find(|b| matches!(b, Block::Table(_) | Block::Figure(_)))
                .unwrap();
            let related = media
                .text_blocks()
                .filter_map(|text| text.source.as_ref())
                .collect::<Vec<_>>();
            assert!(related.len() >= 2);
            assert!(
                related.iter().all(|range| unit.ranges.contains(range)),
                "all captions, source and note paragraphs participate in the same focus mask"
            );
            let after = parts.last().unwrap().ranges.first().unwrap();
            assert!(
                !unit.ranges.contains(after),
                "the following prose is not part of the media group"
            );
        }
    }
}
