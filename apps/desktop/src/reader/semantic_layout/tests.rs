use crate::reader::*;
use rebook_publication::{
    Book, ImageBlock, Metadata, PublicationError, PublicationId, RasterResource, Resource,
    SpineItem, SpineItemId, TextBlock, TextRun, TextStyle,
};

struct Fixture {
    book: Book,
    section: Section,
}
impl BookSource for Fixture {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        Ok(self.section.clone())
    }
    fn resource(&self, _: &PublicationUrl) -> Result<Resource, PublicationError> {
        unreachable!()
    }
    fn raster_resource(
        &self,
        _: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        Ok(Some(RasterResource {
            width: 80,
            height: 100,
            pixels: vec![255; 80 * 100 * 4].into(),
        }))
    }
}
struct EmptyHighlights;

#[test]
fn formula_copy_uses_latex_and_ordinary_images_still_copy_pixels() {
    let (mut reader, _, _) = fixture();
    let ctx = egui::Context::default();
    let mut image = rebook_reader::ReaderImage {
        formula: Some(r"\frac{a}{b}".into()),
        position: rebook_reader::ReaderPosition {
            section_index: 0,
            segment_index: 0,
            page_index: 0,
        },
        x: 0.0,
        y: 0.0,
        display_width: 20.0,
        display_height: 10.0,
        width: 2,
        height: 1,
        pixels: Arc::from([255_u8; 8]),
    };
    reader.selected_image = Some(SelectedImage::from_reader_image(&image, true).unwrap());
    let mut idle = ctx.run_ui(egui::RawInput::default(), |ui| {
        reader.copy_shortcut(ui.ctx(), false)
    });
    idle.textures_delta.clear();
    assert!(idle.platform_output.commands.is_empty());
    let copy = || egui::RawInput {
        events: vec![egui::Event::Copy],
        ..Default::default()
    };
    let mut result = ctx.run_ui(copy(), |ui| reader.copy_shortcut(ui.ctx(), false));
    result.textures_delta.clear();
    assert!(result.platform_output.commands.iter().any(
        |command| matches!(command, egui::OutputCommand::CopyText(text) if text == r"\frac{a}{b}")
    ));
    assert!(
        !result
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::CopyImage(_)))
    );
    image.formula = None;
    assert!(
        !reader
            .selected_image
            .as_ref()
            .unwrap()
            .matches(&image, true)
    );
    reader.selected_image = Some(SelectedImage::from_reader_image(&image, true).unwrap());
    let mut result = ctx.run_ui(copy(), |ui| reader.copy_shortcut(ui.ctx(), false));
    result.textures_delta.clear();
    assert!(
        result
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::CopyImage(_)))
    );
    let mut blocked = ctx.run_ui(copy(), |ui| reader.copy_shortcut(ui.ctx(), true));
    blocked.textures_delta.clear();
    assert!(blocked.platform_output.commands.is_empty());
    let pixels = reader.selected_image.as_ref().unwrap().color_image();
    reader.image_preview = Some(ImagePreview {
        formula: Some(r"x^2".into()),
        texture: ctx.load_texture(
            "formula-copy-test",
            pixels.clone(),
            egui::TextureOptions::default(),
        ),
        image: pixels,
        source_size: egui::vec2(2.0, 1.0),
        zoom: 1.0,
        pan: egui::Vec2::ZERO,
    });
    let mut result = ctx.run_ui(copy(), |ui| reader.copy_shortcut(ui.ctx(), true));
    result.textures_delta.clear();
    assert!(
        result
            .platform_output
            .commands
            .iter()
            .any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == "x^2"))
    );
    reader.image_preview = None;
    let mut cleanup = ctx.run_ui(egui::RawInput::default(), |_| {});
    cleanup.textures_delta.clear();
}

#[test]
fn image_refocus_reuses_raw_pixels_without_clipboard_conversion() {
    let (mut reader, _, _) = fixture();
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    let image = reader
        .focus_units
        .iter()
        .position(|unit| unit.is_image)
        .unwrap();
    reader.select_focus_unit(image);
    let pixels = reader.selected_image.as_ref().unwrap().pixels.as_ptr();
    reader.sync_focus_selected_image();
    assert_eq!(
        reader.selected_image.as_ref().unwrap().pixels.as_ptr(),
        pixels
    );
}

#[test]
fn background_refresh_keeps_old_pages_and_restores_text_and_image_anchors() {
    let (mut reader, original, target) = fixture();
    let old_page = reader.reader.current_page() as *const _;
    reader.refresh_semantic_layout();
    assert!(reader.semantic_layout.reflow_dirty);
    assert_eq!(reader.reader.current_page() as *const _, old_page);
    let request = reader.reader.prepare_refresh_request(Some(target.clone()));
    let mut prepared = std::thread::spawn(move || request.prepare().unwrap())
        .join()
        .unwrap();
    assert_eq!(reader.reader.current_page() as *const _, old_page);
    assert!(prepared.restore_cached_anchor(&target.start));
    let image = block_source_range(&original.blocks[2]).unwrap();
    assert!(prepared.restore_cached_anchor(&image.start));
    assert!(
        !prepared
            .current_page()
            .image_source_rects(std::slice::from_ref(image))
            .is_empty()
    );
}

#[test]
fn background_reflow_adopts_latest_image_focus_and_rejects_stale_versions() {
    let (mut reader, original, target) = fixture();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    reader.select_focus_unit(1);
    let request = reader.reader.prepare_refresh_request(Some(target));
    let prepared = std::thread::spawn(move || request.prepare().unwrap())
        .join()
        .unwrap();
    let image_index = reader
        .focus_units
        .iter()
        .position(|unit| unit.is_image)
        .unwrap();
    reader.select_focus_unit(image_index);
    let unit = reader.reader.reading_unit_location().index;
    let style = reader.reader.style();
    assert!(reader.adopt_semantic_reflow(&runtime, 0, unit, style, Ok(prepared)));
    let image = block_source_range(&original.blocks[2]).unwrap();
    assert!(
        !reader
            .reader
            .current_page()
            .image_source_rects(std::slice::from_ref(image))
            .is_empty()
    );
    let request = reader.reader.prepare_refresh_request(Some(image.clone()));
    let prepared = std::thread::spawn(move || request.prepare().unwrap())
        .join()
        .unwrap();
    reader.semantic_layout.reflow_version = 2;
    let unit = reader.reader.reading_unit_location().index;
    let style = reader.reader.style();
    assert!(!reader.adopt_semantic_reflow(&runtime, 1, unit, style, Ok(prepared)));
    assert!(reader.semantic_layout.reflow_dirty);
}

#[test]
fn warm_content_scheduling_does_not_reparse_the_book() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counted {
        inner: Arc<dyn BookSource>,
        calls: Arc<AtomicUsize>,
    }
    impl BookSource for Counted {
        fn book(&self) -> &Book {
            self.inner.book()
        }
        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.parse_section(index)
        }
        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            self.inner.resource(href)
        }
    }
    let (reader, original, range) = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let source: Arc<dyn BookSource> = Arc::new(Counted {
        inner: reader.semantic_source.original(),
        calls: calls.clone(),
    });
    let prepared = super::preparation::prepare(
        source.clone(),
        vec![(0, vec![range.clone()])],
        HashMap::new(),
        false,
        true,
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let changed = super::preparation::prepare(
        source,
        vec![(
            0,
            vec![block_source_range(&original.blocks[0]).unwrap().clone()],
        )],
        prepared.originals,
        false,
        true,
    );
    for _ in 0..100 {
        reader
            .translation_source
            .untranslated_prepared(0, &changed.inputs[&0], std::slice::from_ref(&range))
            .unwrap();
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "viewport changes and polling must reuse prepared content"
    );
}

#[test]
#[ignore = "requires TORTO_PERF_BOOK local EPUB; no model requests"]
fn local_translation_planning_performance() {
    let book = rebook_formats::open_file(std::path::PathBuf::from(
        std::env::var_os("TORTO_PERF_BOOK").unwrap(),
    ))
    .unwrap();
    let source = book.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|section| section.id.as_str() == "nav_10")
        .unwrap_or(9);
    let section = source.parse_section(index).unwrap();
    let ranges = section
        .blocks
        .iter()
        .filter_map(block_source_range)
        .take(4)
        .cloned()
        .collect::<Vec<_>>();
    let translations =
        TranslationBookSource::new(source.clone(), crate::plugins::TranslationMode::Replace);
    let started = Instant::now();
    let baseline = translations
        .untranslated_blocks_for_ranges(index, &ranges)
        .unwrap();
    let uncached = started.elapsed();
    let prepared = super::preparation::prepare(
        source.clone(),
        vec![(index, ranges.clone())],
        HashMap::new(),
        false,
        true,
    );
    let started = Instant::now();
    for _ in 0..100 {
        assert_eq!(
            translations
                .untranslated_prepared(index, &prepared.inputs[&index], &ranges)
                .unwrap(),
            baseline
        );
    }
    let indexed = started.elapsed().as_secs_f64() * 1_000.0 / 100.0;
    let cached = RewriteBookSource::new(source);
    let original = cached.parse_section(index).unwrap();
    let started = Instant::now();
    for _ in 0..10 {
        assert_eq!(cached.parse_section(index).unwrap(), original);
    }
    println!(
        "section={index} blocks={} uncached_selection_ms={:.2} prepared_selection_ms={indexed:.4} cached_parse_ms={:.3}",
        section.blocks.len(),
        uncached.as_secs_f64() * 1_000.0,
        started.elapsed().as_secs_f64() * 100.0
    );
}

fn staged_group(original: &Section, range: std::ops::Range<usize>) -> super::Group {
    super::Group {
        index: 0,
        sources: original.blocks[range.clone()]
            .iter()
            .flat_map(crate::plugins::semantic_layout::block_ranges)
            .collect(),
        hash: crate::plugins::semantic_layout::fingerprint(original),
        result: Some(crate::plugins::semantic_layout::empty_recognition(
            &crate::plugins::semantic_layout::scope_section(original, range.clone()),
        )),
        range,
    }
}

#[test]
fn completed_translation_is_invisible_until_its_ai_group_finishes() {
    let (mut reader, original, range) = fixture();
    reader.translation.enabled = true;
    reader.plugin_settings.providers[0].api_key = "fixture".into();
    reader.plugin_settings.providers[0].base_url = "http://127.0.0.1:9".into();
    assert!(reader.plugin_settings.translation_endpoint().is_ok());
    reader.translation_source.set_enabled(true).unwrap();
    reader
        .translation_source
        .set_mode(crate::plugins::TranslationMode::Replace)
        .unwrap();
    let demand = vec![(0, vec![range])];
    reader.stage_translation_batch(
        0,
        vec![crate::plugins::BlockTranslation {
            block_index: 1,
            segment_index: None,
            text: "Translated current paragraph".into(),
        }],
    );
    reader.commit_ready_content(&demand, true);
    assert_eq!(reader.semantic_source.parse_section(0).unwrap(), original);
    reader
        .semantic_layout
        .groups
        .push(staged_group(&original, 1..2));
    reader.commit_ready_content(&demand, true);
    let displayed = reader.semantic_source.parse_section(0).unwrap();
    assert!(
        super::super::block_focus_text(&displayed.blocks[1])
            .contains("Translated current paragraph")
    );
    assert!(reader.semantic_layout.translations.is_empty());
}

#[test]
fn related_group_waits_for_all_members_and_failure_releases_successful_members() {
    let (mut reader, original, range) = fixture();
    reader.translation.enabled = true;
    reader.plugin_settings.providers[0].api_key = "fixture".into();
    reader.plugin_settings.providers[0].base_url = "http://127.0.0.1:9".into();
    assert!(reader.plugin_settings.translation_endpoint().is_ok());
    reader.translation_source.set_enabled(true).unwrap();
    reader
        .translation_source
        .set_mode(crate::plugins::TranslationMode::Replace)
        .unwrap();
    let demand = vec![(0, vec![range])];
    reader
        .semantic_layout
        .groups
        .push(staged_group(&original, 0..2));
    reader.stage_translation_batch(
        0,
        vec![crate::plugins::BlockTranslation {
            block_index: 1,
            segment_index: None,
            text: "Translated member".into(),
        }],
    );
    reader.commit_ready_content(&demand, true);
    assert_eq!(reader.semantic_source.parse_section(0).unwrap(), original);
    reader.semantic_layout.failed.insert((0, 0, None));
    reader.commit_ready_content(&demand, true);
    let displayed = reader.semantic_source.parse_section(0).unwrap();
    assert_eq!(displayed.blocks[0], original.blocks[0]);
    assert!(super::super::block_focus_text(&displayed.blocks[1]).contains("Translated member"));
    assert!(reader.semantic_layout.groups.is_empty());
}

#[test]
fn completed_offscreen_group_is_retained_until_returning() {
    let (mut reader, original, range) = fixture();
    reader
        .semantic_layout
        .groups
        .push(staged_group(&original, 1..2));
    reader.commit_ready_content(&Vec::new(), true);
    assert_eq!(reader.semantic_layout.groups.len(), 1);
    reader.commit_ready_content(&vec![(0, vec![range])], true);
    assert!(reader.semantic_layout.groups.is_empty());
}

#[test]
fn disabling_ai_releases_completed_translation_without_a_barrier() {
    let (mut reader, _, range) = fixture();
    reader.translation.enabled = true;
    reader.translation_source.set_enabled(true).unwrap();
    reader.stage_translation_batch(
        0,
        vec![crate::plugins::BlockTranslation {
            block_index: 1,
            segment_index: None,
            text: "Ready translation".into(),
        }],
    );
    reader.commit_ready_content(&vec![(0, vec![range])], false);
    assert!(
        super::super::block_focus_text(&reader.semantic_source.parse_section(0).unwrap().blocks[1])
            .contains("Ready translation")
    );
}

#[test]
fn credential_or_target_change_discards_staged_old_translation() {
    let (mut reader, _, _) = fixture();
    reader.sync_content_config();
    let stage = |reader: &mut DesktopReader| {
        reader.stage_translation_batch(
            0,
            vec![crate::plugins::BlockTranslation {
                block_index: 1,
                segment_index: None,
                text: "Old result".into(),
            }],
        )
    };
    stage(&mut reader);
    reader.plugin_settings.providers[0].api_key = "changed-fixture-key".into();
    reader.sync_content_config();
    assert!(reader.semantic_layout.translations.is_empty());
    stage(&mut reader);
    reader.plugin_settings.target_language = "French".into();
    reader.sync_content_config();
    assert!(reader.semantic_layout.translations.is_empty());
}

#[test]
fn overlapping_pending_semantic_groups_share_one_barrier() {
    let (mut reader, original, _) = fixture();
    reader
        .semantic_layout
        .originals
        .insert(0, Arc::new(original.clone()));
    reader.stage_semantic_group(staged_group(&original, 0..2));
    reader.stage_semantic_group(staged_group(&original, 1..3));
    assert_eq!(reader.semantic_layout.groups.len(), 1);
    assert_eq!(reader.semantic_layout.groups[0].range, 0..3);
}

#[test]
fn linked_note_demand_does_not_expand_to_all_endnotes() {
    let (_, original, range) = fixture();
    let notes = Block::Note(rebook_publication::NoteBlock {
        kind: rebook_publication::NoteBlockKind::Section,
        blocks: original.blocks[..2].to_vec(),
        source: None,
    });
    assert_eq!(
        super::demanded_sources(&notes, std::slice::from_ref(&range)),
        vec![range]
    );
}

#[test]
fn translation_lookahead_and_standalone_ai_use_their_respective_screen_ranges() {
    let (mut reader, original, _) = fixture();
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    reader.select_focus_unit(0);
    let image = block_source_range(&original.blocks[2]).unwrap();
    let (page_index, bounds) = layout
        .pages
        .iter()
        .enumerate()
        .find_map(|(index, entry)| {
            entry
                .page
                .image_source_rects(std::slice::from_ref(image))
                .first()
                .copied()
                .map(|bounds| (index, bounds))
        })
        .unwrap();
    let image_y = layout.content_y(page_index, ((bounds.y0 + bounds.y1) * 0.5) as f32);
    let padding = reader.scroll_content_padding(1.0);
    reader.scroll_viewport = Some(ScrollViewportState {
        offset_y: image_y + padding,
        size: egui::vec2(800.0, 1.0),
    });
    reader.translation.enabled = false;
    let visible = reader.current_content_request_ranges().unwrap();
    assert!(
        visible
            .iter()
            .flat_map(|(_, ranges)| ranges)
            .any(|range| range.start.node == "image")
    );
    assert!(
        !visible
            .iter()
            .flat_map(|(_, ranges)| ranges)
            .any(|range| range.start.node == "current" || range.start.node == "earlier")
    );
    // Focus is still on the first paragraph: screen visibility, not activation,
    // determines standalone AI's targets.
    assert_eq!(reader.focus_unit_index, 0);
    reader.translation.enabled = true;
    let shared = reader.current_content_request_ranges().unwrap();
    assert_eq!(shared, reader.current_translation_ranges().unwrap());
    assert!(
        shared
            .iter()
            .flat_map(|(_, ranges)| ranges)
            .any(|range| range.start.node == "image")
    );
    assert!(
        shared
            .iter()
            .flat_map(|(_, ranges)| ranges)
            .any(|range| range.start.node == "current")
    );
    reader.translation.enabled = false;
    assert_eq!(reader.current_content_request_ranges().unwrap(), visible);

    let height = layout.page_heights[page_index];
    reader.scroll_viewport = Some(ScrollViewportState {
        offset_y: layout.page_tops[page_index] + reader.scroll_content_padding(height),
        size: egui::vec2(800.0, height),
    });
    let visible = reader.current_content_request_ranges().unwrap();
    assert!(
        visible
            .iter()
            .flat_map(|(_, ranges)| ranges)
            .any(|range| range.start.node == "current")
    );
    assert!(
        visible
            .iter()
            .flat_map(|(_, ranges)| ranges)
            .any(|range| range.start.node == "image")
    );
}

#[test]
fn hidden_linked_notes_join_translation_requests_but_not_standalone_ai() {
    let (reader, mut section, _) = fixture();
    let note_range = SourceRange {
        start: SourceAnchor {
            spine: section.id.clone(),
            node: "note".into(),
            text_offset: 0,
        },
        end: SourceAnchor {
            spine: section.id.clone(),
            node: "note".into(),
            text_offset: 12,
        },
    };
    let Block::Text(body) = &mut section.blocks[0] else {
        panic!()
    };
    body.content.push(Inline::Text(TextRun {
        text: "1".into(),
        link: Some(section.href.resolve("#note").unwrap()),
        style: TextStyle {
            link_role: rebook_publication::LinkRole::FootnoteReference,
            baseline: rebook_publication::TextBaseline::Superscript,
            ..Default::default()
        },
    }));
    body.source.as_mut().unwrap().end.text_offset += 1;
    let visible = body.source.clone().unwrap();
    section
        .blocks
        .push(Block::Note(rebook_publication::NoteBlock {
            kind: rebook_publication::NoteBlockKind::Definition,
            source: Some(note_range.clone()),
            blocks: vec![Block::Text(TextBlock {
                kind: rebook_publication::TextBlockKind::Paragraph,
                source: Some(note_range.clone()),
                style: Default::default(),
                content: vec![Inline::Text(TextRun {
                    text: "Hidden note.".into(),
                    style: Default::default(),
                    link: None,
                })],
            })],
        }));
    section.anchors.push(rebook_publication::SectionAnchor {
        fragment: "note".into(),
        source: note_range.start.clone(),
    });
    let source: Arc<dyn BookSource> = Arc::new(Fixture {
        book: reader.semantic_source.original().book().clone(),
        section,
    });
    let raw = vec![(0, vec![visible])];
    let standalone =
        super::preparation::prepare(source.clone(), raw.clone(), HashMap::new(), false, false);
    assert_eq!(standalone.demand, raw);
    assert!(!standalone.with_translation);
    let shared = super::preparation::prepare(source, raw, standalone.originals, false, true);
    assert!(shared.with_translation);
    assert!(
        shared
            .demand
            .iter()
            .flat_map(|(_, ranges)| ranges)
            .any(|range| range == &note_range)
    );
}

#[test]
fn viewport_sources_include_images_and_exclude_offscreen_paragraphs() {
    let (mut reader, original, _) = fixture();
    let image = block_source_range(&original.blocks[2]).unwrap();
    let layout = reader.current_scroll_layout().unwrap();
    let (page, bounds) = layout
        .pages
        .iter()
        .find_map(|entry| {
            entry
                .page
                .image_source_rects(std::slice::from_ref(image))
                .first()
                .copied()
                .map(|bounds| (&entry.page, bounds))
        })
        .unwrap();
    let y = ((bounds.y0 + bounds.y1) / 2.0) as f32;
    let visible = page.visible_content_sources(y, y + 1.0);
    assert!(visible.contains(image));
    assert!(!visible.iter().any(|range| range.start.node == "earlier"));
}

impl crate::highlights::HighlightRepository for EmptyHighlights {
    fn highlights_for_book(
        &self,
        _: &str,
    ) -> crate::highlights::HighlightResult<Vec<StoredHighlight>> {
        Ok(vec![])
    }
    fn insert_highlight(&self, _: &StoredHighlight) -> crate::highlights::HighlightResult<()> {
        unreachable!()
    }
    fn update_highlight(&self, _: &StoredHighlight) -> crate::highlights::HighlightResult<bool> {
        unreachable!()
    }
    fn remove_highlight(&self, _: &str) -> crate::highlights::HighlightResult<bool> {
        unreachable!()
    }
}

pub(in crate::reader) fn fixture() -> (DesktopReader, Section, SourceRange) {
    fixture_with_numbered_heading(false)
}

fn fixture_with_numbered_heading(numbered_heading: bool) -> (DesktopReader, Section, SourceRange) {
    let spine = SpineItemId::new("chapter").unwrap();
    let href = PublicationUrl::parse("chapter.xhtml").unwrap();
    let text = |node: &str, text: String| {
        let start = SourceAnchor {
            spine: spine.clone(),
            node: node.into(),
            text_offset: 0,
        };
        Block::Text(TextBlock {
            kind: rebook_publication::TextBlockKind::Paragraph,
            source: Some(SourceRange {
                end: SourceAnchor {
                    text_offset: text.chars().count() as u64,
                    ..start.clone()
                },
                start,
            }),
            content: vec![Inline::Text(TextRun {
                text,
                style: TextStyle::default(),
                link: None,
            })],
            style: Default::default(),
        })
    };
    let image_anchor = SourceAnchor {
        spine: spine.clone(),
        node: "image".into(),
        text_offset: 0,
    };
    let mut section=Section {id:spine.clone(),href:href.clone(),anchors:vec![],blocks:vec![
        text("earlier","Earlier paragraphs change height when recognized as a quotation. ".repeat(25)),
        text("current","Current paragraph stays next to its illustration after the preceding content is reflowed. ".repeat(6)),
        Block::Image(ImageBlock {formula_image: false,
formula: None,
href:PublicationUrl::parse("image.png").unwrap(),alt:String::new(),style:Default::default(),source:Some(SourceRange {start:image_anchor.clone(),end:image_anchor}),text_layer:None}),
    ]};
    let target = block_source_range(&section.blocks[1]).unwrap().clone();
    if numbered_heading {
        section.blocks.insert(1, text("section-number", "2".into()));
    }
    let original = Arc::new(Fixture {
        book: Book {
            id: PublicationId::new("semantic-reflow-regression").unwrap(),
            metadata: Metadata {
                languages: vec!["en-US".into()],
                ..Metadata::default()
            },
            cover: None,
            sections: vec![SpineItem {
                id: spine,
                href,
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: vec![],
            }],
            table_of_contents: vec![],
        },
        section: section.clone(),
    });
    let settings = PluginSettings::default();
    let rewrite_source = Arc::new(RewriteBookSource::new(original));
    let translation_source = Arc::new(TranslationBookSource::new(
        rewrite_source.clone(),
        settings.translation_mode,
    ));
    let semantic_source = Arc::new(SemanticLayoutSource::new(
        translation_source.clone(),
        rewrite_source.clone(),
    ));
    let structure_source = Arc::new(ParagraphStructureSource::new(semantic_source.clone()));
    let source: Arc<dyn BookSource> = structure_source.clone();
    let session = ReaderSession::open_with_fonts(
        source.clone(),
        LayoutViewport::new(800, 600).unwrap(),
        ReaderStyle {
            spread: SpreadMode::Scroll,
            typesetting: ReaderTypesetting::unified(),
            ..ReaderStyle::default()
        },
        crate::fonts::embedded_reader_fonts(),
    )
    .unwrap();
    let mut reader = DesktopReader::new(
        session,
        DesktopReaderResources {
            source,
            rewrite_source,
            translation_source,
            semantic_source,
            structure_source,
            pdf_ocr_controller: None,
            pdf_ocr_available: false,
            pdf_ocr_mode: PdfOcrViewMode::Original,
            cover: None,
            format: BookFormat::Epub,
            book_id: "semantic-reflow-regression".into(),
            display_metadata: BookDisplayMetadata {
                id: "semantic-reflow-regression".into(),
                title: "Fixture".into(),
                authors: vec![],
            },
            pdf_metadata_missing: PdfMetadataMissing {
                title: false,
                authors: false,
            },
            highlight_store: HighlightStore::from_repository(EmptyHighlights),
            highlights: vec![],
            progress_store: None,
            restored_source_range: None,
            plugin_settings: settings,
            language: AppLanguage::English,
            reading_mode: ReadingMode::Focus,
            hide_cursor_in_focus_mode: false,
            selection_granularity: SelectionGranularity::Paragraph,
            shortcuts: ShortcutPreferences::default(),
            sync_settings: SyncSettings::new_device(),
            sync_password: String::new(),
            source_path: PathBuf::new(),
        },
    );
    reader
        .semantic_layout
        .originals
        .insert(0, Arc::new(section.clone()));
    reader
        .semantic_layout
        .hashes
        .insert(0, crate::plugins::semantic_layout::fingerprint(&section));
    reader.semantic_layout.inputs.insert(
        0,
        crate::plugins::prepare_translation_inputs(&section, false),
    );
    (reader, section, target)
}

#[test]
fn recognized_numbered_heading_is_visible_but_not_a_focus_stop() {
    let (mut reader, original, target) = fixture_with_numbered_heading(true);
    let layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    assert_eq!(reader.focus_units.len(), 4);
    let recognition = serde_json::from_value(serde_json::json!({
        "fingerprint":crate::plugins::semantic_layout::fingerprint(&original),
        "annotations":[{"SectionHeading":{"source":block_source_range(&original.blocks[1]).unwrap()}}],
        "skipped_groups":0
    })).unwrap();
    assert!(reader.semantic_source.install(0, recognition));
    reader.refresh_semantic_layout_sync();
    let layout = reader.current_scroll_layout().unwrap();
    assert!(
        layout
            .source_baseline(block_source_range(&original.blocks[1]).unwrap())
            .is_some()
    );
    reader.rebuild_focus_units(&layout);
    assert_eq!(reader.focus_units.len(), 3);
    reader.move_focus_unit(PageDirection::Next);
    assert_eq!(reader.focus_anchor.as_ref(), Some(&target.start));
    reader.move_focus_unit(PageDirection::Previous);
    assert_eq!(reader.focus_unit_index, 0);
    assert!(reader.pending_reading_unit_turn.is_none());
}

#[test]
fn focus_navigation_waits_for_units_after_content_refresh() {
    for semantic in [false, true] {
        let (mut reader, _, target) = fixture();
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        reader.select_focus_unit(1);
        assert_eq!(reader.focus_anchor.as_ref(), Some(&target.start));

        if semantic {
            reader.refresh_semantic_layout_sync();
        } else {
            reader.refresh_translation_view();
        }
        assert!(reader.focus_units.is_empty());
        // Rendering may populate the layout cache before the UI rebuilds units.
        let layout = reader.current_scroll_layout().unwrap();
        for direction in [PageDirection::Previous, PageDirection::Next] {
            reader.move_focus_unit(direction);
            assert!(reader.completion.is_none(), "refresh must not end the book");
            assert!(reader.pending_reading_unit_turn.is_none());
            assert!(reader.pending_reading_unit_entry.is_none());
            assert_eq!(reader.focus_anchor.as_ref(), Some(&target.start));
        }
        reader.rebuild_focus_units(&layout);
        reader.move_focus_unit(PageDirection::Next);
        assert_eq!(reader.focus_unit_index, 2);
        assert!(reader.completion.is_none());
        reader.move_focus_unit(PageDirection::Next);
        assert!(
            reader.completion.is_some(),
            "real boundaries must still work"
        );
    }
}

#[test]
fn ai_reflow_moves_the_viewport_instead_of_detaching_text_from_its_image() {
    let (mut reader, original, target) = fixture();
    let mut layout = reader.current_scroll_layout().unwrap();
    reader.rebuild_focus_units(&layout);
    reader.focus_unit_index = reader
        .focus_units
        .iter()
        .position(|u| u.range.start == target.start)
        .unwrap();
    reader.focus_anchor = Some(target.start.clone());
    let height = 600.0;
    let screen_y = 120.0;
    reader.scroll_viewport = Some(ScrollViewportState {
        size: egui::vec2(800.0, height),
        offset_y: layout.source_baseline(&target).unwrap() + height * 0.5 - screen_y,
    });
    for enabled in [true, false] {
        let before = layout.source_baseline(&target).unwrap();
        if enabled {
            let recognition=serde_json::from_value(serde_json::json!({
                "fingerprint":crate::plugins::semantic_layout::fingerprint(&original),
                "annotations":[{"Quote":{"body":[block_source_range(&original.blocks[0]).unwrap()],"attribution":null}}],"skipped_groups":0
            })).unwrap();
            assert!(reader.semantic_source.install(0, recognition));
        } else {
            reader.semantic_source.clear();
        }
        reader.refresh_semantic_layout_sync();
        let fresh = reader.current_scroll_layout().unwrap();
        let after = fresh.source_baseline(&target).unwrap();
        assert!(
            (after - before).abs() > 1.0,
            "fixture must change the active paragraph's document position"
        );
        let displayed = reader.locally_correct_focus_reflow(fresh.clone(), height);
        assert!(
            Arc::ptr_eq(&fresh, &displayed),
            "AI reflow must never translate only the active text"
        );
        reader.rebuild_focus_units(&displayed);
        let offset = reader
            .restore_focus_reflow_anchor(&displayed, height)
            .unwrap();
        assert!(
            (after + height * 0.5 - offset - screen_y).abs() < 0.01,
            "source baseline should keep its screen position by scrolling"
        );
        reader.scroll_viewport = Some(ScrollViewportState {
            size: egui::vec2(800.0, height),
            offset_y: offset,
        });
        layout = displayed;
    }
}

#[test]
#[ignore = "requires TORTO_PERF_BOOK local EPUB; no model requests"]
fn local_reading_unit_resize_performance() {
    let book = rebook_formats::open_file(std::path::PathBuf::from(
        std::env::var_os("TORTO_PERF_BOOK").unwrap(),
    ))
    .unwrap();
    let source = book.source();
    let index = source
        .book()
        .sections
        .iter()
        .position(|section| section.id.as_str() == "nav_11")
        .unwrap_or(11);
    let style = rebook_layout::ReaderStyle {
        spread: rebook_layout::SpreadMode::Scroll,
        typesetting: rebook_layout::ReaderTypesetting::unified(),
        ..Default::default()
    };
    let mut reader = rebook_reader::ReaderSession::open_with_fonts(
        source,
        rebook_layout::LayoutViewport::new(1000, 700).unwrap(),
        style,
        crate::fonts::embedded_reader_fonts(),
    )
    .unwrap();
    reader.go_to_section(index).unwrap();
    for width in [1100, 1600, 900] {
        // Different starting widths force both runs to discard layout caches.
        reader
            .resize(rebook_layout::LayoutViewport::new(width - 1, 800).unwrap())
            .unwrap();
        let started = Instant::now();
        reader
            .resize(rebook_layout::LayoutViewport::new(width, 800).unwrap())
            .unwrap();
        let scoped = reader.current_reading_unit_pages().unwrap();
        let scoped_ms = started.elapsed().as_secs_f64() * 1000.0;
        reader
            .resize(rebook_layout::LayoutViewport::new(width - 1, 800).unwrap())
            .unwrap();
        let started = Instant::now();
        reader
            .resize(rebook_layout::LayoutViewport::new(width, 800).unwrap())
            .unwrap();
        let full = reader.current_section_pages().unwrap();
        let full_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(!scoped.is_empty());
        assert!(scoped.len() < full.len());
        for entry in &scoped {
            let matching = full
                .iter()
                .find(|page| page.position == entry.position)
                .unwrap();
            assert_eq!(
                entry.page.content_sources(),
                matching.page.content_sources()
            );
        }
        println!(
            "section={index} width={width} scoped_pages={} full_pages={} scoped_ms={scoped_ms:.2} full_ms={full_ms:.2}",
            scoped.len(),
            full.len()
        );
    }
}

#[test]
#[ignore = "requires TORTO_PERF_BOOK local EPUB; exercises real UI without model calls"]
fn local_image_resize_ui_performance() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let book = rebook_formats::open_file(std::path::PathBuf::from(
        std::env::var_os("TORTO_PERF_BOOK").unwrap(),
    ))
    .unwrap();
    let (mut reader, _, _) = fixture();
    reader.rewrite_source = Arc::new(RewriteBookSource::new(book.source()));
    reader.translation_source = Arc::new(TranslationBookSource::new(
        reader.rewrite_source.clone(),
        crate::plugins::TranslationMode::Replace,
    ));
    reader.semantic_source = Arc::new(SemanticLayoutSource::new(
        reader.translation_source.clone(),
        reader.rewrite_source.clone(),
    ));
    reader.structure_source = Arc::new(ParagraphStructureSource::new(
        reader.semantic_source.clone(),
    ));
    reader.source = reader.structure_source.clone();
    reader.semantic_layout = Default::default();
    reader.reader = rebook_reader::ReaderSession::open_with_fonts(
        reader.source.clone(),
        rebook_layout::LayoutViewport::new(1200, 800).unwrap(),
        rebook_layout::ReaderStyle {
            spread: rebook_layout::SpreadMode::Scroll,
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            ..Default::default()
        },
        crate::fonts::embedded_reader_fonts(),
    )
    .unwrap();
    let snapshot = reader.reader.go_to_section(11).unwrap().snapshot;
    reader.apply_snapshot(snapshot, super::super::SnapshotEffects::navigation());
    let ctx = egui::Context::default();
    let mut frame = 0;
    for (width, height) in [
        (1200., 800.),
        (2560., 1369.),
        (1200., 800.),
        (2560., 1369.),
        (1200., 800.),
    ] {
        for pass in 0..4 {
            let started = Instant::now();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, height),
                    )),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    reader.ui(ui, None, false);
                },
            );
            let ui_ms = started.elapsed().as_secs_f64() * 1000.;
            output.textures_delta.clear();
            let started = Instant::now();
            let scene = reader.page_scene();
            println!(
                "size={width}x{height} pass={pass} ui_ms={ui_ms:.2} scene_ms={:.2} images={}",
                started.elapsed().as_secs_f64() * 1000.,
                scene.images.len()
            );
            frame += 1;
        }
    }
    let image_index = reader
        .focus_units
        .iter()
        .position(|unit| unit.is_image && unit.text.contains("4.1"))
        .unwrap_or_else(|| {
            reader
                .focus_units
                .iter()
                .position(|unit| unit.is_image)
                .unwrap()
        });
    println!(
        "image_unit={image_index} caption={}",
        reader.focus_units[image_index].text
    );
    reader.focus_unit_index = image_index;
    reader.focus_anchor = Some(reader.focus_units[image_index].range.start.clone());
    reader.focus_target_offset = reader.focus_unit_target_offset(800.);
    for step in 0..15 {
        let started = Instant::now();
        reader.sync_focus_selected_image();
        let select_ms = started.elapsed().as_secs_f64() * 1000.;
        reader.focus_target_offset =
            Some(reader.focus_units[image_index].rect.top() + step as f32 * 30.);
        let started = Instant::now();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200., 800.),
                )),
                time: Some(frame as f64 / 60.),
                ..Default::default()
            },
            |ui| {
                reader.ui(ui, None, false);
            },
        );
        output.textures_delta.clear();
        let ui_ms = started.elapsed().as_secs_f64() * 1000.;
        let started = Instant::now();
        let scene = reader.page_scene();
        println!(
            "image step={step} select_ms={select_ms:.2} ui_ms={ui_ms:.2} scene_ms={:.2} images={}",
            started.elapsed().as_secs_f64() * 1000.,
            scene.images.len()
        );
        frame += 1;
    }
    assert!(reader.reader.cached_segment_count() < reader.reader.location().segment_count);
    for (width, height) in [(2560., 1369.), (1200., 800.), (2560., 1369.), (1200., 800.)] {
        for pass in 0..3 {
            let started = Instant::now();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, height),
                    )),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    reader.ui(ui, None, false);
                },
            );
            let ui_ms = started.elapsed().as_secs_f64() * 1000.;
            output.textures_delta.clear();
            let started = Instant::now();
            let scene = reader.page_scene();
            println!(
                "image resize={width}x{height} pass={pass} ui_ms={ui_ms:.2} scene_ms={:.2} images={} cached_segments={}",
                started.elapsed().as_secs_f64() * 1000.,
                scene.images.len(),
                reader.reader.cached_segment_count()
            );
            assert!(reader.reader.cached_segment_count() < reader.reader.location().segment_count);
            frame += 1;
        }
    }
}

#[test]
fn translation_waits_for_same_block_semantics_and_failure_releases_it() {
    let (mut reader, original, _) = fixture();
    let hash = crate::plugins::semantic_layout::fingerprint(&original);
    assert!(!reader.translation_semantics_ready(0, 0));
    reader.semantic_layout.done.insert((0, 0), hash.clone());
    assert!(reader.translation_semantics_ready(0, 0));
    assert!(!reader.translation_semantics_ready(0, 1));
    // Failed recognition is staged as an empty result, with the same done key.
    reader.stage_semantic_group(staged_group(&original, 0..1));
    assert!(reader.translation_semantics_ready(0, 0));
    reader
        .semantic_layout
        .hashes
        .insert(0, "changed source".into());
    assert!(!reader.translation_semantics_ready(0, 0));
}

#[test]
fn staged_citations_are_in_translation_input_before_display_commit() {
    let (mut reader, mut original, _) = fixture();
    reader.plugin_settings.semantic_layout.enabled = true;
    let Block::Text(block) = &mut original.blocks[0] else {
        panic!()
    };
    let value = "Claim (Smith, 2020).";
    block.content = vec![Inline::Text(TextRun {
        text: value.into(),
        style: Default::default(),
        link: None,
    })];
    let source = block.source.clone().unwrap();
    let hash = crate::plugins::semantic_layout::fingerprint(&original);
    reader.semantic_layout.inputs.insert(
        0,
        crate::plugins::prepare_translation_inputs(&original, false),
    );
    reader
        .semantic_layout
        .originals
        .insert(0, Arc::new(original.clone()));
    reader.semantic_layout.hashes.insert(0, hash.clone());
    let recognition = serde_json::from_value(serde_json::json!({
        "fingerprint":hash, "formulas_checked":true, "skipped_groups":0,
        "annotations":[{"InlineCitations":{"source":source,"spans":[{"start":6,"end":19,"text":"(Smith, 2020)"}]}}]
    })).unwrap();
    let mut group = staged_group(&original, 0..1);
    group.result = Some(recognition);
    reader.stage_semantic_group(group);
    assert!(!reader.translation_semantics_ready(0, 0));
    reader.semantic_layout.done.insert((0, 0), hash);
    let input = reader
        .missing_prepared(0, std::slice::from_ref(&source))
        .unwrap();
    assert!(
        input[0]
            .text
            .contains("<citation id=\"1\">(Smith, 2020)</citation>")
    );
    assert_eq!(input[0].block_index, 0);
    // Input enrichment must not publish layout or translation prematurely.
    assert!(
        !reader
            .semantic_source
            .has_recognition(0, &crate::plugins::semantic_layout::fingerprint(&original))
    );
}
