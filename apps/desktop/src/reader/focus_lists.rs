use super::*;

#[derive(Clone)]
pub(super) struct ListGroup {
    pub range: SourceRange,
    pub rect: egui::Rect,
    pub split: bool,
    pub paint_ranges: Vec<SourceRange>,
    pub footnotes: Vec<FocusFootnote>,
    pub share_notes: bool,
    parts: Vec<FocusUnit>,
}

fn must_split(height: f32, viewport: f32, was_split: bool) -> bool {
    // Match the reader's usable viewport, leaving room around the rounded card.
    height > (viewport - 32.0).max(1.0) + if was_split { -1.0 } else { 1.0 }
}

fn emit(group: ListGroup, output: &mut Vec<FocusUnit>) {
    let group = Arc::new(group);
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
    let mut scopes = rebook_layout::semantic_list_groups(&refs);
    let list_scopes = scopes.len();
    scopes.extend(rebook_layout::semantic_paragraph_groups(&refs));
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
        let footnotes = parts.iter().flat_map(|p| p.footnotes.iter().cloned()).collect();
        emit(
            ListGroup {
                range,
                rect,
                split: must_split(rect.height(), height, false),
                parts,
                paint_ranges,
                footnotes,
                share_notes: id < list_scopes,
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
    fn paragraph_container_focus_includes_all_members_and_titles_but_not_adjacent_prose() {
        let mut r=reader("<html><body><p>Before.</p><h2>Title</h2><h3>Subtitle</h3><div><p>One.</p><p style='text-align:center'>Two.</p><p style='text-align:right'>Three.</p></div><p>After.</p></body></html>",2000.0);
        assert_eq!(r.focus_units.len(),3);
        let u=&r.focus_units[1];
        assert_eq!(u.list_group.as_ref().unwrap().parts.len(),3);
        assert!(u.heading_rect.is_some());
        assert!(u.clipboard_text.contains("One.") && u.clipboard_text.contains("Two.") && u.clipboard_text.contains("Three."));
        assert!(!u.clipboard_text.contains("Before.") && !u.clipboard_text.contains("After."));
        assert!(u.structured_activation && u.rectangular_activation);
        assert_eq!(u.paint_ranges.len(),3);
        r.focus_anchor=Some(u.list_group.as_ref().unwrap().parts[2].range.start.clone());
        assert!(r.resize_focus_lists(50.0));
        assert_eq!(r.focus_units.len(),5);
        assert!(r.resize_focus_lists(2000.0));
        assert_eq!(r.focus_units.len(),3);
    }

    #[test]
    #[ignore = "requires TORTO_PARAGRAPH_BOOK; reads actual local EPUB without changing it"]
    fn local_book_paragraph_containers_keep_both_recognition_examples_together() {
        let path=std::env::var("TORTO_PARAGRAPH_BOOK").unwrap();
        let publication=rebook_formats::open_file(&path).unwrap();
        let source=publication.source();
        let index=source.book().sections.iter().position(|s|s.href.path().ends_with("B9780123735911500140.htm")).unwrap();
        let section=source.parse_section(index).unwrap();
        let refs=section.blocks.iter().collect::<Vec<_>>();
        let groups=rebook_layout::semantic_paragraph_groups(&refs);
        let mut found=Vec::new();
        for group in groups {
            let value=section.blocks[group.clone()].iter().map(|b|if let Block::Text(t)=b {text_block_focus_text(t)}else{String::new()}).collect::<Vec<_>>().join("\n");
            if value.contains("One hay I may") || value.contains("One day I mayed") {
                assert_eq!(group.len(),3);
                let (mut r,_,_)=super::super::semantic_layout::tests::fixture_with_blocks(section.blocks[group.clone()].to_vec());
                r.scroll_viewport=Some(ScrollViewportState {size:egui::vec2(800.0,2000.0),offset_y:0.0});
                let layout=r.current_scroll_layout().unwrap(); r.rebuild_focus_units(&layout);
                assert_eq!(r.focus_units.len(),1);
                assert_eq!(r.focus_units[0].list_group.as_ref().unwrap().parts.len(),3);
                found.push(value);
            }
        }
        assert_eq!(found.len(),2);
        println!("actual section {index}: both three-paragraph examples form separate focus groups");
    }

    #[test]
    fn split_paragraph_groups_keep_independent_note_numbers_and_popups() {
        let r=reader("<html><body><p>Before.</p><div><p>One.<a href='#note-one'><sup>1</sup></a></p><p>Two.<a href='#note-two'><sup>2</sup></a></p><p>Three.</p></div><p>After.</p><aside id='note-one' role='doc-footnote'><p>First note.</p></aside><aside id='note-two' role='doc-footnote'><p>Second note.</p></aside></body></html>",50.0);
        let parts:Vec<_>=r.focus_units.iter().filter(|u|u.list_group.is_some()).collect();
        assert_eq!(parts.len(),3);
        assert_eq!(parts[0].popup_footnotes().len(),1);
        assert_eq!(parts[0].popup_footnotes()[0].number,1);
        assert_eq!(parts[1].popup_footnotes().len(),1);
        assert_eq!(parts[1].popup_footnotes()[0].number,1);
        assert!(parts[2].popup_footnotes().is_empty());
    }

    #[test]
    fn list_reference_numbers_match_body_popup_and_click_targets_after_splitting() {
        let (base, _, _) = super::super::semantic_layout::tests::fixture();
        for list in [
            "<ul><li>First <em>Author 2020</em><a href='#n1'><sup>40</sup></a><ul><li>Nested <em>Author 2020</em><a href='#n2'><sup>41</sup></a></li></ul></li><li>Last <em>Author 2020</em><a href='#n3'><sup>42</sup></a></li></ul>",
            "<dl><dt>Term <em>Author 2020</em><a href='#n1'><sup>40</sup></a></dt><dd>Description <em>Author 2020</em><a href='#n2'><sup>41</sup></a></dd><dt>Other term <em>Author 2020</em><a href='#n3'><sup>42</sup></a></dt></dl>",
        ] {
            let html=format!("<html><body><p>Introduction <em>Author 2020</em><a href='#n0'><sup>39</sup></a></p>{list}<ol><li>Separate <em>Author 2020</em><a href='#n4'><sup>43</sup></a></li></ol><p>After.</p></body></html>");
            let html=html.replace("<a href='#n1'><sup>40</sup></a>",""); // A citation-only item also needs scope numbering.
            let mut section=rebook_html::parse_section(&html,&base.source.book().sections[0], |_|None).unwrap();
            for block in &mut section.blocks {
                if let Block::Text(text)=block { for inline in &mut text.content {
                    if let Inline::Text(run)=inline && run.text.contains("2020") { run.style.inline_citation=1; }
                } }
            }
            let groups=rebook_layout::semantic_list_groups(&section.blocks.iter().collect::<Vec<_>>());
            assert_eq!(groups.len(),2);
            let mut r=super::super::semantic_layout::tests::fixture_with_blocks(section.blocks.clone()).0;
            r.scroll_viewport=Some(ScrollViewportState {size:egui::vec2(800.0,2000.0),offset_y:0.0});
            let layout=r.current_scroll_layout().unwrap();
            r.rebuild_focus_units(&layout);
            let unit=r.focus_units.iter().find(|u|u.text.contains("Introduction")).unwrap();
            let group_range=unit.list_group.as_ref().unwrap().range.clone();
            let notes=unit.popup_footnotes().to_vec();
            assert_eq!(notes.iter().filter(|n|n.citation.is_some()).map(|n|n.number).collect::<Vec<_>>(),vec![1,2,3,4]);
            assert_eq!(notes.iter().filter(|n|n.citation.is_none()).map(|n|n.number).collect::<Vec<_>>(),vec![1,2,3]);
            assert!(notes.iter().filter(|n|n.citation.is_none()).all(|n|n.owner.is_some()),"later items retain their return targets");
            assert!(notes.iter().filter_map(|n|n.citation.as_ref()).all(|(_,id)|*id==1),"source citation IDs remain local");
            assert_eq!(notes.iter().filter_map(|n|n.stable_id).collect::<HashSet<_>>().len(),7);
            let mut labels=Vec::new();
            let mut marker_numbers=Vec::new();
            let mut engine=rebook_layout::LayoutEngine::with_fonts(crate::fonts::embedded_reader_fonts().iter().cloned());
            let mut style=r.reader.style().clone();
            style.focus_footnote_icons=true;
            let painted=engine.layout_blocks(r.source.as_ref(),&section.blocks,LayoutViewport::new(800,600).unwrap(),&style).unwrap();
            for page in &painted.pages { for item in &page.items {
                if let rebook_layout::PageItem::Text(text)=item { for reference in text.citations.iter() {
                    if !reference.owner.as_ref().is_some_and(|owner|unit.paint_ranges.contains(owner)) { continue; }
                    let label=text.text[reference.range.clone()].replace('\u{2060}',"");
                    if reference.number & 0x2000_0000 != 0 { marker_numbers.push(label); }
                    else { labels.push(label); assert_eq!(reference.number,1); }
                } }
            } }
            assert_eq!(labels,vec!["[1]","[2]","[3]","[4]"]);
            assert_eq!(marker_numbers,vec!["1","2","3"]);
            let classic=r.resolve_semantic_scope_footnotes(&section.blocks,groups[0].end-1);
            assert!(classic==notes,"classic popup uses the same scope as focus mode");
            assert!(r.resize_focus_lists(60.0));
            let parts=r.focus_units.iter().filter(|u|u.list_group.as_ref().is_some_and(|g|g.range==group_range)).collect::<Vec<_>>();
            assert!(parts.len()>1,"the list must actually split for this check");
            for part in parts {
                assert!(part.popup_footnotes()==notes);
            }
            let second=r.resolve_semantic_scope_footnotes(&section.blocks,groups[1].start);
            assert_eq!(second.iter().map(|n|n.number).collect::<Vec<_>>(),vec![1,1]);
        }
    }

    #[test]
    #[ignore = "requires TORTO_ALIGNMENT_BOOK; validates the actual Beauty EPUB's centered group"]
    fn local_beauty_group_keeps_important_center_alignment_through_shaping() {
        let path=std::env::var("TORTO_ALIGNMENT_BOOK").unwrap();
        let publication=rebook_formats::open_file(&path).unwrap();
        let source=publication.source();
        let index=source.book().sections.iter().position(|s|s.href.path().ends_with("042-045_Sustainable_Products.xhtml")).unwrap();
        let section=source.parse_section(index).unwrap();
        let refs=section.blocks.iter().collect::<Vec<_>>();
        let groups=rebook_layout::semantic_paragraph_groups(&refs);
        let group=groups.iter().find(|g|section.blocks[(*g).clone()].iter().any(|b|
            matches!(b,Block::Text(t) if text_block_focus_text(t).contains("Natural ingredients are not necessarily safer for wildlife")))).unwrap();
        assert_eq!(group.len(),2);
        let selected=&section.blocks[group.clone()];
        for block in selected {
            let Block::Text(t)=block else {panic!()};
            assert_eq!(t.style.authored_alignment,Some(rebook_publication::TextAlignment::Center));
        }
        let mut style=ReaderStyle::default();
        style.typesetting=rebook_layout::ReaderTypesetting::unified();
        style.spread=rebook_layout::SpreadMode::Single;
        let fonts=crate::fonts::embedded_reader_fonts();
        let mut engine=rebook_layout::LayoutEngine::with_fonts(fonts.iter().cloned());
        for width in [800,1100] {
            let layout=engine.layout_blocks(source.as_ref(),selected,LayoutViewport::new(width,600).unwrap(),&style).unwrap();
            let page=&layout.pages[0];
            let placement=page.items.iter().find_map(|item|match item {
                rebook_layout::PageItem::Text(t) if t.text.trim()=="End-of-life" =>Some(t),_=>None,
            }).unwrap();
            let display=rebook_renderer::DisplayListCompiler.compile(page);
            let bounds=display.source_content_bounds(&[placement.source.clone().unwrap()]).unwrap();
            let expected=f64::from(placement.origin_x+placement.available_width/2.0);
            assert!(bounds.width()<f64::from(placement.available_width)*0.5);
            assert!((bounds.center().x-expected).abs()<3.0,"width={width}: {bounds:?} expected center {expected}");
            println!("Beauty section {index}; width={width}; title center {:.2}; column center {expected:.2}; both members centered",bounds.center().x);
        }
    }

    #[test]
    #[ignore = "requires TORTO_ALIGNMENT_BOOK; validates the actual nested moisturizer container"]
    fn local_beauty_moisturizer_outer_container_is_one_complete_group() {
        let publication = rebook_formats::open_file(std::env::var("TORTO_ALIGNMENT_BOOK").unwrap()).unwrap();
        let source = publication.source();
        let index = source.book().sections.iter().position(|s| s.href.path().ends_with("060-061_Need_to_Moisturize.xhtml")).unwrap();
        let section = source.parse_section(index).unwrap();
        let refs = section.blocks.iter().collect::<Vec<_>>();
        let groups = rebook_layout::semantic_paragraph_groups(&refs);
        let group = groups.iter().find(|g| section.blocks[g.start..g.end].iter().any(|b|
            matches!(b, Block::Text(t) if text_block_focus_text(t).starts_with("HOW DO I CHOOSE A MOISTURIZER?")))).unwrap();
        assert_eq!(group.len(), 14);
        let blocks = &section.blocks[group.clone()];
        assert!(matches!(blocks.last(), Some(Block::Text(t)) if text_block_focus_text(t).contains("Moisturizer texture is adjusted")));
        let (mut reader, _, _) = super::super::semantic_layout::tests::fixture_with_blocks(blocks.to_vec());
        reader.scroll_viewport = Some(ScrollViewportState {size: egui::vec2(800.0, 4000.0), offset_y: 0.0});
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        assert_eq!(reader.focus_units.len(), 1);
        assert_eq!(reader.focus_units[0].list_group.as_ref().unwrap().parts.len(), 14);
        assert!(reader.resize_focus_lists(100.0));
        assert_eq!(reader.focus_units.len(), 14);
        let shared = reader.focus_units[0].list_group.as_ref().unwrap();
        assert!(reader.focus_units.iter().all(|u| u.list_group.as_ref().is_some_and(|g| std::sync::Arc::ptr_eq(g, shared))));
        println!("Beauty section {index}: all 14 paragraphs form one outer group; short viewport preserves the shared group across 14 steps");
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
    fn paragraph_encoded_lists_include_plain_introductions_in_activation_and_copy() {
        let r = reader(
            "<html><body><p>Earlier prose.</p><p>Alphabetic scripts have the following characteristics.</p><p>◆ Each written symbol corresponds to a consonant or a vowel.</p><p>◆ The alphabet consists of a limited number of characters.</p><p>◆ Characters are written in a linear sequence.</p><p>After.</p></body></html>",
            2000.0,
        );
        assert_eq!(r.focus_units.len(), 3);
        let unit = &r.focus_units[1];
        let group = unit.list_group.as_ref().unwrap();
        assert_eq!(group.parts.len(), 4);
        assert!(!group.split);
        assert!(unit.clipboard_text.contains("following characteristics."));
        assert!(!unit.clipboard_text.contains("Earlier prose"));
        assert!(!unit.clipboard_text.contains("After."));
        assert!(unit.paint_ranges.iter().any(|range| range == &group.parts[0].range));
    }

    #[test]
    #[ignore = "requires TORTO_LIST_BOOK and optionally TORTO_LIST_PNG"]
    fn local_book_alphabetic_list_introduction_and_markers() {
        use anyrender::ImageRenderer;
        let publication = rebook_formats::open_file(std::env::var("TORTO_LIST_BOOK").unwrap()).unwrap();
        let source = publication.source();
        for index in 0..source.book().sections.len() {
            let section = source.parse_section(index).unwrap();
            let text = |block: &Block| match block {
                Block::Text(t) => t.content.iter().filter_map(|i| match i {
                    Inline::Text(run) => Some(run.text.as_str()), _ => None,
                }).collect::<String>(),
                _ => String::new(),
            };
            let Some(intro) = section.blocks.iter().position(|b| text(b).contains("Alphabetic scripts have the following characteristics")) else { continue };
            let groups = rebook_layout::semantic_list_groups(&section.blocks.iter().collect::<Vec<_>>());
            let group = groups.iter().find(|r| r.start == intro).expect("introduction shares the list scope");
            assert_eq!(group.len(), 4);
            let mut style = ReaderStyle::default();
            style.typesetting = rebook_layout::ReaderTypesetting::unified();
            style.spread = rebook_layout::SpreadMode::Single;
            let fonts = crate::fonts::embedded_reader_fonts();
            let mut engine = rebook_layout::LayoutEngine::with_fonts(fonts.iter().cloned());
            let layout = engine.layout_blocks(source.as_ref(), &section.blocks[group.clone()], LayoutViewport::new(1100, 600).unwrap(), &style).unwrap();
            let mut bullets = 0;
            for page in &layout.pages {
                let display = rebook_renderer::DisplayListCompiler.compile(page);
                for item in &page.items {
                    if let rebook_layout::PageItem::Text(t) = item {
                        assert!(t.inline_images.is_empty(), "authored bullet images must not render");
                        if t.text.starts_with("•\u{00a0}") { bullets += 1; }
                    }
                }
                // Body selection still points to the exact authored offsets,
                // despite replacing an image marker and hiding its whitespace.
                for region in 0..display.text_region_count() {
                    let value = display.text_region_text(region).unwrap();
                    if let Some(at) = value.find("Each written symbol") {
                        let range = display.text_region_source_range(region, at..at + "Each written symbol".len()).unwrap();
                        let Block::Text(original) = &section.blocks[intro + 1] else { panic!() };
                        let original_text = text(&section.blocks[intro + 1]);
                        let offset = original_text[..original_text.find("Each written symbol").unwrap()].chars().count() as u64;
                        assert_eq!(range.start.text_offset, original.source.as_ref().unwrap().start.text_offset + offset);
                    }
                }
            }
            assert_eq!(bullets, 3);
            if let Ok(path) = std::env::var("TORTO_LIST_PNG") {
                let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
                let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(1100, 600);
                let mut pixels = Vec::new();
                renderer.render_to_vec(|scene| display.paint(scene), &mut pixels);
                image::RgbaImage::from_raw(1100, 600, pixels).unwrap().save(path).unwrap();
            }
            println!("section={index}; introduction plus three items; unified bullets; authored offsets verified");
            return;
        }
        panic!("sample introduction missing");
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
