use kurbo::{Affine, BezPath, Rect, RoundedRect, Shape};
use peniko::{Color, Fill};
use vello::Scene;

use crate::preferences::FocusStyle;
use crate::reader::{
    DesktopReader, FOCUS_TABLE_BOTTOM_MARGIN, FocusUnit, ScrollViewportState,
    focus_unit_contains_source_range,
};

use rebook_renderer::FOCUS_INACTIVE_OPACITY;

fn clear_content_rect(unit: &FocusUnit) -> egui::Rect {
    let mut rect = unit.rect;
    if unit.is_table {
        // The table focus rectangle includes navigation breathing room, not
        // painted content. Clearing it would expose the next paragraph's glyphs.
        rect.max.y -= FOCUS_TABLE_BOTTOM_MARGIN;
    }
    rect.expand2(egui::vec2(8.0, 2.0))
}

fn clear_unit_rects(unit: &FocusUnit) -> impl Iterator<Item = egui::Rect> {
    std::iter::once(clear_content_rect(unit)).chain(
        unit.heading_rect
            .map(|rect| rect.expand2(egui::vec2(8.0, 2.0))),
    )
}

// Disjoint rectangles describe the complement of the clear regions. Unlike
// even-odd holes, overlapping active/selected blocks never dim their overlap.
fn surrounding_regions(bounds: Rect, clear: impl IntoIterator<Item = Rect>) -> Vec<Rect> {
    let mut regions = vec![bounds];
    for clear in clear {
        let mut next = Vec::new();
        for region in regions {
            let cut = region.intersect(clear);
            if cut.area() <= 0.0 {
                next.push(region);
                continue;
            }
            for remainder in [
                Rect::new(region.x0, region.y0, region.x1, cut.y0),
                Rect::new(region.x0, cut.y1, region.x1, region.y1),
                Rect::new(region.x0, cut.y0, cut.x0, cut.y1),
                Rect::new(cut.x1, cut.y0, region.x1, cut.y1),
            ] {
                if remainder.area() > 0.0 {
                    next.push(remainder);
                }
            }
        }
        regions = next;
    }
    regions
}

impl DesktopReader {
    pub(in crate::reader) fn focus_background_highlight(&self) -> bool {
        self.is_focus_mode() && self.focus_style == FocusStyle::BackgroundHighlight
    }

    pub(super) fn paint_focus_surroundings(
        &self,
        scene: &mut Scene,
        viewport: ScrollViewportState,
        padding: f32,
    ) {
        if !self.is_focus_mode()
            || self.focus_style != FocusStyle::SurroundingDim
            || self.focus_units.get(self.focus_unit_index).is_none()
        {
            return;
        }
        let preview_x = f64::from(self.preview_offset_x(viewport.size.x));
        let bounds = Rect::new(
            -preview_x,
            0.0,
            f64::from(viewport.size.x) - preview_x,
            f64::from(viewport.size.y),
        );
        let translate = |rect: egui::Rect| {
            Rect::new(
                f64::from(rect.left()),
                f64::from(rect.top() + padding - viewport.offset_y),
                f64::from(rect.right()),
                f64::from(rect.bottom() + padding - viewport.offset_y),
            )
        };
        let clear = self
            .focus_action_units()
            .iter()
            .flat_map(clear_unit_rects)
            .map(translate);
        let selected = self
            .selection
            .as_ref()
            .filter(|_| self.focus_selection_anchor.is_none())
            .into_iter()
            .flat_map(|selection| {
                self.focus_units
                    .iter()
                    .filter(move |unit| {
                        selection
                            .ranges
                            .iter()
                            .any(|range| focus_unit_contains_source_range(unit, range))
                    })
                    .flat_map(clear_unit_rects)
            })
            .map(translate)
            .filter(|rect| rect.intersect(bounds).area() > 0.0);
        let mut path = BezPath::new();
        for region in surrounding_regions(bounds, clear.chain(selected)) {
            path.extend(region.path_elements(0.1));
        }
        let paper = self.reader.style().background;
        // Mixing the already-composited page with its own paper is equivalent
        // to 45% content opacity, including images, formulas and annotations.
        // Keep this single interaction layer independent of retained page tiles.
        let opacity = ((1.0 - FOCUS_INACTIVE_OPACITY) * 255.0).round() as u8;
        let paper_color = Color::from_rgba8(paper.red, paper.green, paper.blue, opacity);
        if let Some((rect, color)) = self.focus_list_backdrop() {
            // Fade inactive ink toward its actual backdrop. Mixing it with
            // plain paper would lighten only the inactive part of the card,
            // leaving an apparent extra background on the focused list item.
            let card = RoundedRect::from_rect(translate(rect), 7.0);
            let mut outside = bounds.to_path(0.1);
            outside.extend(card.path_elements(0.1));
            scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &path);
            scene.fill(Fill::EvenOdd, Affine::IDENTITY, paper_color, None, &outside);
            let color = color.to_rgba8();
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(color.r, color.g, color.b, opacity),
                None,
                &card,
            );
            scene.pop_layer();
        } else {
            scene.fill(Fill::NonZero, Affine::IDENTITY, paper_color, None, &path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html_fixture(html: &str) -> (DesktopReader, rebook_publication::Section) {
        use rebook_publication::{PublicationUrl, SpineItem, SpineItemId};
        let descriptor = SpineItem {
            id: SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        };
        let section = rebook_html::parse_section(html, &descriptor, |_| None).unwrap();
        let reader =
            crate::reader::semantic_layout::tests::fixture_with_blocks(section.blocks.clone()).0;
        (reader, section)
    }

    fn table_fixture() -> DesktopReader {
        html_fixture(
            "<html><body><p>Earlier discussion about error rates.</p>
             <div class='pageavoid'><p class='tnum'>TABLE 3.1</p>
             <p class='ttitle'>Character classes used in error rates.</p>
             <table><tr><td>Correct (C)</td><td>All correct characters.</td></tr>
             <tr><td>Incorrect-not-fixed (INF)</td><td>All incorrect characters.</td></tr>
             <tr><td>Incorrect-fixed (IF)</td><td>Characters backspaced during entry.</td></tr>
             <tr><td>Fix (F)</td><td>All backspaces.</td></tr></table></div>
             <p>Consider the classification example shown in the figure. Automating these
             classifications is straightforward because we only need the counts.</p>
             </body></html>",
        )
        .0
    }

    #[test]
    fn surrounding_dim_only_adds_a_backdrop_to_oversized_lists() {
        let (mut reader, _) = html_fixture("<html><body><p>Before.</p><ul><li>First.</li><li>Second.</li></ul><p>After.</p></body></html>");
        reader.focus_style = FocusStyle::SurroundingDim;
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        let list = reader.focus_units.iter().position(|unit| unit.list_group.as_ref().is_some_and(|group| group.share_notes)).unwrap();
        reader.focus_unit_index = list;
        assert!(reader.focus_list_backdrop().is_none());
        assert!(reader.resize_focus_lists(1.0));
        let list = reader.focus_units.iter().position(|unit| unit.list_group.as_ref().is_some_and(|group| group.share_notes && group.split)).unwrap();
        reader.focus_unit_index = list;
        assert!(reader.focus_list_backdrop().is_some());
        assert!(!reader.focus_background_highlight());
        assert!(reader.resize_focus_lists(2000.0));
        assert!(reader.focus_list_backdrop().is_none());
        assert!(reader.resize_focus_lists(1.0));
        let list = reader.focus_units.iter().position(|unit| unit.list_group.as_ref().is_some_and(|group| group.share_notes && group.split)).unwrap();
        reader.focus_unit_index = list;
        std::sync::Arc::make_mut(reader.focus_units[list].list_group.as_mut().unwrap()).share_notes = false;
        assert!(reader.focus_list_backdrop().is_none());
        reader.focus_style = FocusStyle::BackgroundHighlight;
        assert!(reader.focus_background_highlight());
        assert!(reader.focus_list_backdrop().is_none());
    }

    #[test]
    fn extracted_image_text_has_focus_units_and_clear_geometry() {
        use rebook_publication::{Block, ImageText, Inline};
        let (_, mut section) = html_fixture("<html><body><p>Earlier discussion.</p><p>Image <img src='glyph.png'/> example.</p><p>Later discussion.</p></body></html>");
        let Block::Text(inline) = &mut section.blocks[1] else { panic!() };
        let run = inline.content.iter_mut().find_map(|item| match item { Inline::Image(run) => Some(run), _ => None }).unwrap();
        run.image.ocr_text = Some(ImageText { text: "か".into(), literal: true, translated: None });
        let mut standalone_run = run.clone();
        standalone_run.image.ocr_text = Some(ImageText {
            text: "Original text".into(), literal: false, translated: Some("Translated image text".into()),
        });
        let mut standalone = inline.clone();
        standalone.source.as_mut().unwrap().start.node = "standalone-image".into();
        let range = standalone.source.as_mut().unwrap();
        range.end = range.start.clone();
        standalone_run.image.source = Some(range.clone());
        standalone.content = vec![Inline::Image(standalone_run)];
        section.blocks.insert(2, Block::Text(standalone));
        let mut reader = crate::reader::semantic_layout::tests::fixture_with_blocks(section.blocks).0;
        reader.focus_style = FocusStyle::SurroundingDim;
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        for expected in ["か", "Translated image text"] {
            let unit = reader.focus_units.iter().find(|unit| unit.text.contains(expected))
                .unwrap_or_else(|| panic!("missing focus unit for {expected}"));
            let mut found = false;
            for (page_index, entry) in layout.pages.iter().enumerate() {
                if let Some(bounds) = entry.page.source_content_bounds(&unit.paint_ranges) {
                    let point = egui::pos2(bounds.center().x as f32, layout.content_y(page_index, bounds.center().y as f32));
                    assert!(clear_unit_rects(unit).any(|rect| rect.contains(point)), "extracted text stays dimmed");
                    found = true;
                }
            }
            assert!(found, "missing source geometry for {expected}");
        }
    }

    #[test]
    fn text_figure_focus_keeps_generated_list_markers_clear() {
        let (mut reader, _) = html_fixture("<html><body><p>Earlier discussion.</p><figure><div><ol type='a'><li>What are clouds?</li><li>How are clouds formed?</li><li>List three types of cloud.</li></ol></div><figcaption>Figure 4.3</figcaption></figure><p>Later discussion.</p></body></html>");
        reader.focus_style = FocusStyle::SurroundingDim;
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        let unit = reader.focus_units.iter().find(|unit| unit.text.contains("What are clouds?")).unwrap();
        let mut markers = 0;
        for (page_index, entry) in layout.pages.iter().enumerate() {
            // Semantic bounds include marker glyphs while selectable source
            // rectangles intentionally continue to begin after the prefix.
            let bounds = entry.page.source_content_bounds(&unit.paint_ranges).unwrap();
            let selectable = entry.page.source_rects(&unit.paint_ranges);
            let selected_left = selectable.iter().map(|rect| rect.x0).fold(f64::INFINITY, f64::min);
            assert!(bounds.x0 < selected_left - 8.0);
            let at = egui::pos2(bounds.x0 as f32 + 0.5, layout.content_y(page_index, bounds.center().y as f32));
            assert!(clear_unit_rects(unit).any(|rect| rect.contains(at)), "generated marker was dimmed");
            markers += 1;
        }
        assert!(markers > 0);
    }

    #[test]
    fn consecutive_headings_follow_only_the_next_semantic_unit() {
        use rebook_publication::Block;
        for body in [
            "<p>Following prose.</p>",
            "<table><tr><td>Value</td><td>42</td></tr></table>",
            "<p><img src='image.png' alt='Following image'/></p>",
            "<ul><li>First item.</li><li>Second item.</li></ul>",
            "<blockquote><p>Following quotation.</p></blockquote>",
        ] {
            let (mut reader, section) = html_fixture(&format!(
                "<html><body><p>Earlier paragraph.</p><h2>Topic</h2><h3>Subtopic</h3>{body}<p>Later paragraph.</p></body></html>"
            ));
            let layout = reader.current_scroll_layout().unwrap();
            reader.rebuild_focus_units(&layout);
            let ranges = section
                .blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Text(text) if text.kind.is_heading() => text.source.clone(),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(ranges.len(), 2);
            let headings = crate::reader::focus_unit_geometry(&layout, &ranges)
                .unwrap()
                .0;
            assert_eq!(reader.focus_units[0].heading_rect, None);
            assert_eq!(reader.focus_units[1].heading_rect, Some(headings), "{body}");
            assert_eq!(reader.focus_units.last().unwrap().heading_rect, None);
            assert!(
                reader.focus_units[1]
                    .paint_ranges
                    .iter()
                    .all(|range| !ranges.contains(range)),
                "display association must preserve the semantic body's ranges"
            );
        }
    }

    #[test]
    #[ignore = "requires a native GPU; verifies consecutive heading glyphs remain clear"]
    fn consecutive_heading_ink_stays_clear_in_direct_and_retained_rendering() {
        use crate::reader::ReaderFramePlan;
        use rebook_publication::Block;
        let (mut reader, section) = html_fixture(
            "<html><body><p>Earlier paragraph.</p><h2>Topic</h2><h3>Subtopic</h3><p>Following prose belongs to both headings.</p><p>Later paragraph.</p></body></html>",
        );
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        reader.focus_unit_index = 1;
        let ranges = section
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(text) if text.kind.is_heading() => text.source.clone(),
                _ => None,
            })
            .collect::<Vec<_>>();
        let headings = crate::reader::focus_unit_geometry(&layout, &ranges)
            .unwrap()
            .0;
        let padding = reader.scroll_content_padding(600.0);
        let offset_y = headings.top() + padding - 100.0;
        reader.scroll_viewport = Some(ScrollViewportState {
            size: egui::vec2(800.0, 600.0),
            offset_y,
        });
        reader.focus_style = FocusStyle::SurroundingDim;
        let paper = reader.reader.style().background;
        let plan = ReaderFramePlan {
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
            scene_id: reader.scene_id,
            scene_revision: reader.scene_revision,
            background: Color::from_rgb8(paper.red, paper.green, paper.blue),
            content_left: 0.0,
            preview_left: 0.0,
            reflow_pending: false,
            live_resize_preview: false,
        };
        let mut audit = crate::platform::LayerAudit::new([800, 600]);
        let units = std::mem::take(&mut reader.focus_units);
        audit.direct(&reader.page_scene(), plan, 1.0);
        let baseline = audit.pixels();
        reader.focus_units = units;
        audit.direct(&reader.page_scene(), plan, 1.0);
        let dimmed = audit.pixels();
        let headings = headings.translate(egui::vec2(0.0, padding - offset_y));
        let mut checked = 0;
        for y in headings.top().max(0.0) as usize..headings.bottom().min(600.0) as usize {
            for x in headings.left().max(0.0) as usize..headings.right().min(800.0) as usize {
                let before = &baseline[(y * 800 + x) * 4..][..3];
                if !before
                    .iter()
                    .zip([paper.red, paper.green, paper.blue])
                    .any(|(v, p)| v.abs_diff(p) > 40)
                {
                    continue;
                }
                let after = &dimmed[(y * 800 + x) * 4..][..3];
                assert!(
                    before.iter().zip(after).all(|(a, b)| a.abs_diff(*b) <= 2),
                    "heading dimmed at {x},{y}"
                );
                checked += 1;
            }
        }
        assert!(checked > 100);
        let layers = reader.layered_scene().unwrap();
        for _ in 0..64 {
            if !audit.layered(&layers, plan, 1.0).0.pending {
                break;
            }
        }
        audit.parity(&dimmed, "consecutive-heading-dim");
        println!("HEADING_CLEAR checked_ink={checked}");
    }

    #[test]
    fn table_navigation_margin_does_not_clear_the_next_paragraph() {
        let mut reader = table_fixture();
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        let index = reader
            .focus_units
            .iter()
            .position(|unit| unit.is_table)
            .unwrap();
        let table = &reader.focus_units[index];
        let following = &reader.focus_units[index + 1];
        assert!(
            table.rect.bottom() > following.rect.top(),
            "reproduce navigation margin overlap"
        );
        assert!(clear_content_rect(table).bottom() < following.rect.top());
        assert_eq!(
            clear_content_rect(following),
            following.rect.expand2(egui::vec2(8.0, 2.0))
        );
    }

    #[test]
    #[ignore = "requires a native GPU; checks the following line's first ink rows"]
    fn active_table_dims_the_entire_following_line_in_direct_and_retained_rendering() {
        assert_table_following_dim(table_fixture());
    }

    #[test]
    #[ignore = "requires TORTO_SURROUNDINGS_BOOK and a native GPU; reads local Table 3.1 and captions"]
    fn local_book_table_following_line_and_caption_audit() {
        use rebook_publication::{Block, Inline};
        let path = std::env::var("TORTO_SURROUNDINGS_BOOK").unwrap();
        let publication = rebook_formats::open_file(path).unwrap();
        let source = publication.source();
        let mut figures = 0;
        let mut tables = 0;
        let mut parts = 0;
        let mut bold = 0;
        let mut italic = 0;
        let mut target = None;
        for index in 0..source.book().sections.len() {
            let section = source.parse_section(index).unwrap();
            for (index, block) in section.blocks.iter().enumerate() {
                let captions = match block {
                    Block::Figure(figure) => {
                        figures += 1;
                        figure.text_blocks().collect::<Vec<_>>()
                    }
                    Block::Table(table) => {
                        tables += 1;
                        table.before.iter().chain(&table.after).collect()
                    }
                    Block::Text(text)
                        if text.kind == rebook_publication::TextBlockKind::Caption =>
                    {
                        vec![text]
                    }
                    _ => continue,
                };
                for caption in captions {
                    if caption.kind != rebook_publication::TextBlockKind::Caption {
                        continue;
                    }
                    parts += 1;
                    let runs = caption
                        .content
                        .iter()
                        .flat_map(Inline::text_runs)
                        .collect::<Vec<_>>();
                    bold += usize::from(runs.iter().any(|run| run.style.bold));
                    italic += usize::from(
                        runs.iter()
                            .any(|run| run.style.italic || run.style.emphasis),
                    );
                    let text = runs.iter().map(|run| run.text.as_str()).collect::<String>();
                    if text.starts_with("TABLE 3.1 ") {
                        target = Some(section.blocks[index - 1..=index + 1].to_vec());
                    }
                }
            }
        }
        assert!(figures > 5 && tables > 1);
        println!(
            "LOCAL_CAPTIONS figures={figures} tables={tables} caption_parts={parts} authored_bold_parts={bold} authored_italic_parts={italic}"
        );
        assert_table_following_dim(
            crate::reader::semantic_layout::tests::fixture_with_blocks(target.expect("Table 3.1"))
                .0,
        );
    }

    fn assert_table_following_dim(mut reader: DesktopReader) {
        use crate::reader::ReaderFramePlan;
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        let index = reader
            .focus_units
            .iter()
            .position(|unit| unit.is_table)
            .unwrap();
        reader.focus_unit_index = index;
        let padding = reader.scroll_content_padding(600.0);
        let offset_y = reader.focus_units[index].rect.top() + padding - 80.0;
        let following = reader.focus_units[index + 1]
            .rect
            .translate(egui::vec2(0.0, padding - offset_y));
        reader.scroll_viewport = Some(ScrollViewportState {
            size: egui::vec2(800.0, 600.0),
            offset_y,
        });
        reader.focus_style = FocusStyle::SurroundingDim;
        let paper = reader.reader.style().background;
        let plan = ReaderFramePlan {
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
            scene_id: reader.scene_id,
            scene_revision: reader.scene_revision,
            background: Color::from_rgb8(paper.red, paper.green, paper.blue),
            content_left: 0.0,
            preview_left: 0.0,
            reflow_pending: false,
            live_resize_preview: false,
        };
        let mut audit = crate::platform::LayerAudit::new([800, 600]);
        let units = std::mem::take(&mut reader.focus_units);
        audit.direct(&reader.page_scene(), plan, 1.0);
        let baseline = audit.pixels();
        reader.focus_units = units;
        audit.direct(&reader.page_scene(), plan, 1.0);
        let dimmed = audit.pixels();
        if let Some(path) = std::env::var_os("TORTO_SURROUNDINGS_OUTPUT") {
            image::RgbaImage::from_raw(800, 600, dimmed.clone())
                .unwrap()
                .save(path)
                .unwrap();
        }
        let mut first_ink_y = None;
        let mut checked = 0;
        for y in following.top().max(0.0) as usize..following.bottom().min(600.0) as usize {
            if first_ink_y.is_some_and(|first| y >= first + 6) {
                break;
            }
            for x in following.left().max(0.0) as usize..following.right().min(800.0) as usize {
                let before = &baseline[(y * 800 + x) * 4..][..3];
                if !before
                    .iter()
                    .zip([paper.red, paper.green, paper.blue])
                    .any(|(v, p)| v.abs_diff(p) > 40)
                {
                    continue;
                }
                first_ink_y.get_or_insert(y);
                let after = &dimmed[(y * 800 + x) * 4..][..3];
                for ((before, after), paper) in
                    before
                        .iter()
                        .zip(after)
                        .zip([paper.red, paper.green, paper.blue])
                {
                    let expected = (f64::from(*before) * (115.0 / 255.0)
                        + f64::from(paper) * (140.0 / 255.0))
                        .round() as u8;
                    assert!(
                        after.abs_diff(expected) <= 3,
                        "undimmed following glyph at {x},{y}"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 30, "must check actual glyph tops: {checked}");
        let layers = reader.layered_scene().unwrap();
        for _ in 0..64 {
            if !audit.layered(&layers, plan, 1.0).0.pending {
                break;
            }
        }
        audit.parity(&dimmed, "table-following-line-dim");
        println!("TABLE_FOLLOWING_DIM first_ink_y={first_ink_y:?} checked_ink={checked}");
    }

    #[test]
    fn overlapping_active_and_selected_regions_are_never_dimmed_twice() {
        let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
        let regions = surrounding_regions(
            bounds,
            [
                Rect::new(10.0, 10.0, 70.0, 50.0),
                Rect::new(40.0, 30.0, 90.0, 80.0),
            ],
        );
        assert_eq!(regions.iter().map(|r| r.area()).sum::<f64>(), 5700.0);
        for (index, region) in regions.iter().enumerate() {
            assert!(!region.contains(kurbo::Point::new(50.0, 40.0)));
            assert!(
                regions[index + 1..]
                    .iter()
                    .all(|other| region.intersect(*other).area() <= 0.0)
            );
        }
    }

    #[test]
    fn offscreen_blocks_and_tall_blocks_preserve_viewport_clipping() {
        let bounds = Rect::new(-12.0, 0.0, 100.0, 80.0);
        assert_eq!(
            surrounding_regions(bounds, [Rect::new(10.0, 100.0, 90.0, 120.0)]),
            vec![bounds]
        );
        let regions = surrounding_regions(bounds, [Rect::new(10.0, -200.0, 90.0, 300.0)]);
        assert_eq!(
            regions,
            vec![
                Rect::new(-12.0, 0.0, 10.0, 80.0),
                Rect::new(90.0, 0.0, 100.0, 80.0)
            ]
        );
    }

    #[test]
    #[ignore = "requires a native GPU; verifies focus opacity and retained composition"]
    fn surrounding_dim_pixels_match_direct_and_retained_rendering() {
        use crate::reader::ReaderFramePlan;
        let mut audit = crate::platform::LayerAudit::new([800, 600]);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
            let mut style = reader.reader.style();
            crate::reader::apply_theme_colors(&mut style, theme);
            reader.reader.set_style(style).unwrap();
            reader.invalidate_page_scenes();
            let layout = reader.current_scroll_layout().unwrap();
            reader.rebuild_focus_units(&layout);
            reader.focus_unit_index = 1;
            let padding = reader.scroll_content_padding(600.0);
            let offset_y = reader.focus_units[1].rect.top() + padding - 180.0;
            reader.scroll_viewport = Some(ScrollViewportState {
                size: egui::vec2(800.0, 600.0),
                offset_y,
            });
            let paper = reader.reader.style().background;
            let mut plan = ReaderFramePlan {
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
                scene_id: reader.scene_id,
                scene_revision: reader.scene_revision,
                background: Color::from_rgb8(paper.red, paper.green, paper.blue),
                content_left: 0.0,
                preview_left: 0.0,
                reflow_pending: false,
                live_resize_preview: false,
            };
            reader.focus_style = FocusStyle::SurroundingDim;
            // Keep focus-mode padding and layout identical in the neutral frame.
            let units = std::mem::take(&mut reader.focus_units);
            audit.direct(&reader.page_scene(), plan, 1.0);
            let baseline = audit.pixels();
            reader.focus_units = units;
            let direct = reader.page_scene();
            audit.direct(&direct, plan, 1.0);
            let dimmed = audit.pixels();
            let active = reader.focus_units[1]
                .rect
                .expand2(egui::vec2(8.0, 2.0))
                .translate(egui::vec2(0.0, padding - offset_y));
            let mut clear_ink = 0;
            let mut dim_ink = 0;
            for y in 0..600_usize {
                for x in 0..800_usize {
                    let point = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                    // Avoid the antialiased boundary of the clear region.
                    if active.expand(2.0).contains(point) && !active.shrink(2.0).contains(point) {
                        continue;
                    }
                    let before = &baseline[(y * 800 + x) * 4..][..3];
                    let after = &dimmed[(y * 800 + x) * 4..][..3];
                    let ink = before
                        .iter()
                        .zip([paper.red, paper.green, paper.blue])
                        .any(|(v, paper)| v.abs_diff(paper) > 40);
                    if active.contains(point) {
                        assert!(
                            before.iter().zip(after).all(|(a, b)| a.abs_diff(*b) <= 2),
                            "active content changed at {x},{y}"
                        );
                        clear_ink += usize::from(ink);
                    } else {
                        for ((before, after), paper) in
                            before
                                .iter()
                                .zip(after)
                                .zip([paper.red, paper.green, paper.blue])
                        {
                            let expected = (f64::from(*before) * (115.0 / 255.0)
                                + f64::from(paper) * (140.0 / 255.0))
                                .round() as u8;
                            assert!(
                                after.abs_diff(expected) <= 3,
                                "surrounding opacity mismatch at {x},{y}: {after}, expected {expected}, theme={theme:?}"
                            );
                        }
                        dim_ink += usize::from(ink);
                    }
                }
            }
            assert!(
                clear_ink > 100 && dim_ink > 100,
                "must compare real ink in both regions"
            );
            let layers = reader.layered_scene().unwrap();
            let identities: Vec<_> = layers.layers.iter().filter_map(|l| l.identity).collect();
            let mut ready = false;
            for _ in 0..64 {
                if !audit.layered(&layers, plan, 1.0).0.pending {
                    ready = true;
                    break;
                }
            }
            assert!(ready);
            audit.parity(&dimmed, "surrounding-dim");
            reader.select_focus_unit(0);
            plan.scene_revision = reader.scene_revision;
            let moved = reader.layered_scene().unwrap();
            assert_eq!(
                identities,
                moved
                    .layers
                    .iter()
                    .filter_map(|l| l.identity)
                    .collect::<Vec<_>>()
            );
            audit.direct(&reader.page_scene(), plan, 1.0);
            let expected = audit.pixels();
            audit.layered(&moved, plan, 1.0);
            audit.parity(&expected, "surrounding-dim-next-block");
            println!(
                "SURROUNDING_DIM theme={theme:?} active_ink={clear_ink} inactive_ink={dim_ink} retained_layers={}",
                identities.len()
            );
        }
    }

    #[test]
    #[ignore = "requires a native GPU; verifies uniform list backgrounds and inactive ink"]
    fn surrounding_dim_list_background_stays_uniform_when_focus_moves() {
        use crate::reader::ReaderFramePlan;
        let mut audit = crate::platform::LayerAudit::new([800, 600]);
        let items = (0..12).map(|i| format!(
            "<li>Item {i}. This list item contains enough text to wrap onto several lines and make the whole list taller than the reading viewport.</li>"
        )).collect::<String>();
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let (mut reader, _) = html_fixture(&format!("<html><body><p>Before.</p><ul>{items}</ul><p>After.</p></body></html>"));
            reader.focus_style = FocusStyle::SurroundingDim;
            let mut style = reader.reader.style();
            crate::reader::apply_theme_colors(&mut style, theme);
            reader.reader.set_style(style).unwrap();
            reader.invalidate_page_scenes();
            reader.scroll_viewport = Some(ScrollViewportState {
                size: egui::vec2(800.0, 600.0), offset_y: 0.0,
            });
            let layout = reader.current_scroll_layout().unwrap();
            reader.rebuild_focus_units(&layout);
            let indices = reader.focus_units.iter().enumerate().filter_map(|(i, unit)| {
                unit.list_group.as_ref().filter(|group| group.share_notes && group.split).map(|_| i)
            }).take(2).collect::<Vec<_>>();
            assert_eq!(indices.len(), 2, "must exercise a split list");
            reader.focus_unit_index = indices[0];
            let (rect, color) = reader.focus_list_backdrop().unwrap();
            let padding = reader.scroll_content_padding(600.0);
            let offset_y = rect.top() + padding - 60.0;
            reader.scroll_viewport.as_mut().unwrap().offset_y = offset_y;
            let paper = reader.reader.style().background;
            // Compare with the actual highlight-mode group backdrop, sampled
            // away from its active item so the item tint cannot enter the color.
            reader.focus_style = FocusStyle::BackgroundHighlight;
            let reference_plan = ReaderFramePlan {
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
                scene_id: reader.scene_id, scene_revision: reader.scene_revision,
                background: Color::from_rgb8(paper.red, paper.green, paper.blue),
                content_left: 0.0, preview_left: 0.0, reflow_pending: false, live_resize_preview: false,
            };
            audit.direct(&reader.page_scene(), reference_plan, 1.0);
            let reference = audit.pixels();
            let reference_y = (reader.focus_units[indices[1]].rect.center().y + padding - offset_y) as usize;
            let reference_x = (rect.right() - 3.0) as usize;
            let reference_color = reference[(reference_y * 800 + reference_x) * 4..][..3].to_vec();
            reader.focus_style = FocusStyle::SurroundingDim;
            let mut frames = Vec::new();
            for &index in &indices {
                reader.focus_unit_index = index;
                reader.bump_scene_revision();
                assert_eq!(reader.focus_list_backdrop(), Some((rect, color)));
                let plan = ReaderFramePlan {
                    rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0)),
                    scene_id: reader.scene_id, scene_revision: reader.scene_revision,
                    background: Color::from_rgb8(paper.red, paper.green, paper.blue),
                    content_left: 0.0, preview_left: 0.0, reflow_pending: false, live_resize_preview: false,
                };
                audit.direct(&reader.page_scene(), plan, 1.0);
                let pixels = audit.pixels();
                let expected = color.to_rgba8();
                for &item in &indices {
                    let y = (reader.focus_units[item].rect.center().y + padding - offset_y) as usize;
                    let x = (rect.right() - 3.0) as usize;
                    assert!(x < 800 && y < 600);
                    let pixel = &pixels[(y * 800 + x) * 4..][..3];
                    assert!(pixel.iter().zip([expected.r, expected.g, expected.b]).all(|(a,b)| a.abs_diff(b) <= 2),
                        "list background changed for item {item}: {pixel:?}, theme={theme:?}");
                    assert!(pixel.iter().zip(&reference_color).all(|(a,b)| a.abs_diff(*b) <= 2),
                        "list backdrop differs from highlight mode: {pixel:?} vs {reference_color:?}");
                }
                let layers = reader.layered_scene().unwrap();
                let mut ready = false;
                for _ in 0..64 {
                    if !audit.layered(&layers, plan, 1.0).0.pending { ready = true; break; }
                }
                assert!(ready);
                audit.parity(&pixels, "uniform-list-background");
                frames.push(pixels);
            }
            let bg = color.to_rgba8();
            let contrast = |pixels: &[u8], index: usize| -> u64 {
                let rect = reader.focus_units[index].rect.translate(egui::vec2(0.0, padding - offset_y));
                let mut sum = 0;
                for y in (rect.top().ceil().max(0.0) as usize)..(rect.bottom().floor().min(600.0) as usize) {
                    for x in (rect.left().ceil().max(0.0) as usize)..(rect.right().floor().min(800.0) as usize) {
                        sum += pixels[(y * 800 + x) * 4..][..3].iter()
                            .zip([bg.r,bg.g,bg.b]).map(|(a,b)| u64::from(a.abs_diff(b))).sum::<u64>();
                    }
                }
                sum
            };
            assert!(contrast(&frames[0], indices[0]) > contrast(&frames[1], indices[0]) * 3 / 2);
            assert!(contrast(&frames[1], indices[1]) > contrast(&frames[0], indices[1]) * 3 / 2);
        }
    }
}
