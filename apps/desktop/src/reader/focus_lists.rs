use super::*;

#[derive(Clone)]
pub(super) struct ListGroup {
    pub range: SourceRange,
    pub rect: egui::Rect,
    pub split: bool,
    pub paint_ranges: Vec<SourceRange>,
    parts: Vec<FocusUnit>,
}

fn must_split(height: f32, viewport: f32, was_split: bool) -> bool {
    // Match the reader's usable viewport, leaving room around the rounded card.
    height > (viewport - 32.0).max(1.0) + if was_split { -1.0 } else { 1.0 }
}

fn emit(group: ListGroup, output: &mut Vec<FocusUnit>) {
    let group = Arc::new(group);
    let notes = group
        .parts
        .iter()
        .flat_map(|p| p.footnotes.clone())
        .collect::<Vec<_>>();
    let mut parts = if group.split {
        group.parts.clone()
    } else {
        let mut whole = group.parts[0].clone();
        for part in group.parts.iter().skip(1) {
            merge_focus_list_descendant(&mut whole, part.clone());
        }
        whole.rect = group.rect;
        whole.rectangular_activation_rect = Some(group.rect);
        vec![whole]
    };
    for part in &mut parts {
        part.list_group = Some(Arc::clone(&group));
        part.rectangular_activation = true;
        part.structured_activation = true; // normal green, not quote blue
        part.footnotes.clone_from(&notes);
    }
    output.extend(parts);
}

pub(super) fn group(
    units: Vec<FocusUnit>,
    blocks: &[Block],
    layout: &ScrollSectionLayout,
    height: f32,
) -> Vec<FocusUnit> {
    let refs: Vec<_> = blocks.iter().collect();
    let scopes = rebook_layout::semantic_list_groups(&refs);
    let mut membership = HashMap::new();
    for (id, scope) in scopes.iter().enumerate() {
        for block in &blocks[scope.clone()] {
            if let Some(range) = block_source_range(block) {
                membership.insert((range.start.spine.clone(), range.start.node.clone()), id);
            }
        }
    }
    let mut output = Vec::new();
    let mut units = units.into_iter().peekable();
    while let Some(unit) = units.next() {
        let key = |u: &FocusUnit| (u.range.start.spine.clone(), u.range.start.node.clone());
        let Some(&id) = membership.get(&key(&unit)) else {
            output.push(unit);
            continue;
        };
        let mut parts = vec![unit];
        while units
            .peek()
            .is_some_and(|u| membership.get(&key(u)) == Some(&id))
        {
            parts.push(units.next().unwrap());
        }
        for part in &mut parts {
            let bounds = focus_block_activation_geometry(layout, &part.paint_ranges)
                .or_else(|| focus_unit_geometry(layout, &part.paint_ranges).map(|(r, _)| r))
                .unwrap_or(part.rect);
            part.rect = bounds;
            part.rectangular_activation_rect = Some(bounds.expand2(egui::vec2(8.0, 4.0)));
        }
        let rect = parts
            .iter()
            .filter_map(|p| p.rectangular_activation_rect)
            .reduce(|a, b| a.union(b))
            .unwrap();
        for part in &mut parts {
            let r = part.rectangular_activation_rect.as_mut().unwrap();
            r.min.x = rect.min.x;
            r.max.x = rect.max.x;
        }
        let mut range = parts[0].range.clone();
        range.end = parts.last().unwrap().range.end.clone();
        let paint_ranges = parts.iter().flat_map(|p| p.paint_ranges.clone()).collect();
        emit(
            ListGroup {
                range,
                rect,
                split: must_split(rect.height(), height, false),
                parts,
                paint_ranges,
            },
            &mut output,
        );
    }
    output
}

impl DesktopReader {
    pub(super) fn resize_focus_lists(&mut self, height: f32) -> bool {
        if !self.focus_units.iter().any(|u| {
            u.list_group
                .as_ref()
                .is_some_and(|g| must_split(g.rect.height(), height, g.split) != g.split)
        }) {
            return false;
        }
        let anchor = self.focus_anchor.clone();
        let mut output = Vec::new();
        let mut last: Option<Arc<ListGroup>> = None;
        for unit in std::mem::take(&mut self.focus_units) {
            if let Some(group) = &unit.list_group {
                if last
                    .as_ref()
                    .is_some_and(|previous| Arc::ptr_eq(previous, group))
                {
                    continue;
                }
                let mut next = group.as_ref().clone();
                next.split = must_split(next.rect.height(), height, next.split);
                emit(next, &mut output);
                last = Some(Arc::clone(group));
            } else {
                last = None;
                output.push(unit);
            }
        }
        self.focus_unit_index = resolved_focus_unit_index(
            &output,
            anchor.as_ref(),
            None,
            snapshot_position(&self.snapshot),
        );
        self.focus_units = output;
        self.focus_overflow_origin = None;
        self.sync_focus_chat_session();
        self.bump_scene_revision();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reader(html: &str, height: f32) -> DesktopReader {
        let (base, _, _) = super::super::semantic_layout::tests::fixture();
        let section =
            rebook_html::parse_section(html, &base.source.book().sections[0], |_| None).unwrap();
        let (mut reader, _, _) =
            super::super::semantic_layout::tests::fixture_with_blocks(section.blocks);
        reader.scroll_viewport = Some(ScrollViewportState {
            size: egui::vec2(800.0, height),
            offset_y: 0.0,
        });
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        reader
    }

    #[test]
    fn list_groups_include_only_adjacent_prose_and_keep_nested_items_together() {
        let html = "<html><body><p>Before.</p><p>Introduction.</p><ul><li>First.<ul><li>Nested A.</li><li>Nested B.</li></ul></li><li>Second.</li></ul><p>After.</p></body></html>";
        let mut r = reader(html, 2000.0);
        assert_eq!(r.focus_units.len(), 3);
        let group = r.focus_units[1].list_group.as_ref().unwrap();
        assert!(!group.split);
        assert_eq!(group.parts.len(), 3);
        assert!(group.parts[1].text.contains("Nested B"));
        assert!(r.focus_units[1].structured_activation);
        assert!(r.focus_units[1].clipboard_text.contains("Introduction"));
        let anchor = group.parts[2].range.start.clone();
        r.focus_anchor = Some(anchor.clone());
        r.focus_unit_index = 1;
        assert!(r.resize_focus_lists(80.0));
        assert_eq!(r.focus_units.len(), 5);
        assert_eq!(r.focus_units[r.focus_unit_index].range.start, anchor);
        assert_eq!(r.focus_units[0].text, "Before.");
        assert_eq!(r.focus_units[1].text, "Introduction.");
        assert!(r.focus_units[2].text.contains("Nested B"));
        assert_eq!(r.focus_units[3].text, "Second.");
        assert!(r.resize_focus_lists(2000.0));
        assert_eq!(r.focus_unit_index, 1);
        assert_eq!(r.focus_anchor, Some(anchor));
    }

    #[test]
    fn captions_quotes_tables_images_and_headings_are_not_list_introductions() {
        for preceding in [
            "<h2>Heading</h2>",
            "<blockquote>Quotation</blockquote>",
            "<table><tr><td>Cell</td></tr></table>",
            "<p><img src='image.png'/></p>",
            "<figure><img src='image.png'/><figcaption>Caption</figcaption></figure>",
            "<p>Mixed prose <img src='image.png'/></p>",
        ] {
            let html = format!(
                "<html><body><p>Earlier paragraph.</p>{preceding}<ul><li>One.</li><li>Two.</li></ul><p>After.</p></body></html>"
            );
            let r = reader(&html, 2000.0);
            let list = r
                .focus_units
                .iter()
                .find(|u| u.list_group.is_some())
                .unwrap();
            assert_eq!(
                list.list_group.as_ref().unwrap().parts.len(),
                2,
                "{preceding}"
            );
            assert!(!list.text.contains("Earlier"), "{preceding}");
        }
        let r = reader(
            "<html><body><p>Intro.</p><ul><li>A.</li><li>B.</li></ul><ol><li>C.</li><li>D.</li></ol></body></html>",
            2000.0,
        );
        assert_eq!(r.focus_units.len(), 2);
        assert_eq!(r.focus_units[0].list_group.as_ref().unwrap().parts.len(), 3);
        assert_eq!(r.focus_units[1].list_group.as_ref().unwrap().parts.len(), 2);
    }

    #[test]
    fn tall_item_uses_existing_overflow_navigation_and_reverse_entry() {
        let html = format!(
            "<html><body><p>Intro.</p><ul><li>{}</li><li>Last.</li></ul><p>After.</p></body></html>",
            "Long content in one item. ".repeat(250)
        );
        let mut r = reader(&html, 600.0);
        assert!(r.focus_units[0].list_group.as_ref().unwrap().split);
        assert_eq!(
            r.focus_units.len(),
            4,
            "{:?}",
            r.focus_units
                .iter()
                .map(|u| u.text.chars().take(40).collect::<String>())
                .collect::<Vec<_>>()
        );
        r.select_focus_unit(1);
        let height = 600.0;
        let rect = r.focus_units[1].rect;
        let (top, bottom) =
            oversized_focus_unit_scroll_bounds(rect, height, r.scroll_content_padding(height))
                .unwrap();
        r.ui.focus_scroll_motion = None;
        r.focus_target_offset = Some(top);
        let mut steps = 0;
        while r.scroll_within_tall_focus_unit(PageDirection::Next) {
            steps += 1;
            assert!(steps < 100);
            let target = r.ui.focus_scroll_motion.take().unwrap().target;
            r.scroll_viewport.as_mut().unwrap().offset_y = target;
            r.focus_target_offset = Some(target);
            assert_eq!(r.focus_unit_index, 1);
        }
        assert!(steps > 1);
        assert!((r.focus_target_offset.unwrap() - bottom).abs() < 1.0);
        r.move_focus_unit(PageDirection::Next);
        assert_eq!(r.focus_unit_index, 2);
        r.move_focus_unit(PageDirection::Previous);
        assert_eq!(r.focus_unit_index, 1);
        assert!((r.ui.focus_scroll_motion.unwrap().target - bottom).abs() < 1.0);
    }
}
