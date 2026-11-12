//! Regression coverage for the reader's left sidebar and its highlight list.
//!
//! Books whose chapters are reading units inside one spine file (a single
//! concatenated HTML document, as the importer and PDF OCR produce) keep one
//! spine index for the whole book, so a jump from the highlight list changes
//! the reading unit without changing the section.

use std::sync::Arc;

use crate::reader::*;
use rebook_publication::{
    Block, BlockStyle, Book, BookSource, Inline, Metadata, PublicationError, PublicationId,
    PublicationUrl, RasterResource, Resource, Section, SectionAnchor, SourceAnchor, SourceRange,
    SpineItem, SpineItemId, TextBlock, TextBlockKind, TextRun, TextStyle, TocEntry,
};

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);
const CHAPTERS: usize = 15;
const PARAGRAPHS_PER_CHAPTER: usize = 20;
const CHAPTER: &str = "chapter";

#[test]
fn issue75_programmatic_scroll_after_content_growth_does_not_acquire_rebound_velocity() {
    let ctx = egui::Context::default();
    let run = |height, target: Option<f32>| {
        let mut observed = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            },
            |ui| {
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("focus-issue75")
                    .auto_shrink([false, false])
                    .scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR);
                if let Some(target) = target {
                    area = area.vertical_scroll_offset(target);
                }
                let result = area.show_viewport(ui, |ui, viewport| {
                    ui.set_height(height);
                    viewport.min.y
                });
                observed = Some((
                    result.inner,
                    result.state.unclamped_offset().y,
                    result.state.velocity().y,
                    result.id,
                ));
            },
        );
        output.textures_delta.clear();
        observed.unwrap()
    };
    run(2_000.0, Some(0.0));
    run(2_000.0, None);
    let target = 15_000.0;
    let (presented, saved, velocity, _) = run(20_000.0, Some(target));
    assert!((presented - target).abs() < 0.01);
    assert!(
        (saved - target).abs() < 0.01,
        "explicit jump drifted to {saved}, velocity={velocity}"
    );
    assert_eq!(velocity, 0.0);
    for _ in 0..8 {
        let (presented, saved, velocity, _) = run(20_000.0, None);
        assert!((presented - target).abs() < 0.01 && (saved - target).abs() < 0.01);
        assert_eq!(velocity, 0.0);
    }
    let (_, _, _, id) = run(2_000.0, Some(0.0));
    let mut state = egui::scroll_area::State::load(&ctx, id).unwrap();
    state.set_offset(egui::vec2(0.0, target));
    state.store(&ctx, id);
    let (presented, saved, velocity, _) = run(20_000.0, None);
    assert_eq!((presented, saved, velocity), (target, target, 0.0));
    // Real out-of-range requests still obey the new content's bounds.
    let (_, saved, velocity, _) = run(2_000.0, Some(target));
    assert!(saved < 2_000.0);
    assert_eq!(velocity, 0.0);
}

#[test]
fn issue75_shelf_resume_after_a_short_book_keeps_the_saved_paragraph_visible() {
    let (reader, section) = chapter_book(ReadingMode::Focus);
    let target = range_at(&section, "p219");
    let mut locator = reader.reader.current_locator();
    locator.source = Some(target.clone());
    let mut short_book = reader.source.book().clone();
    short_book.table_of_contents.clear();
    let mut short_section = section;
    short_section.blocks.truncate(1);
    short_section.anchors.clear();
    drop(reader);
    let ctx = egui::Context::default();
    let mut short = source_reader(
        Arc::new(SingleSpineBook {
            book: short_book,
            section: short_section,
        }),
        ReadingMode::Focus,
        None,
        None,
    );
    for _ in 0..4 {
        issue75_frame(&ctx, &mut short);
    }
    drop(short);
    let root = std::env::temp_dir().join(format!("torto-issue75-resume-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let store =
        crate::sync::SyncStore::open_at(root.join("sync.sqlite3"), "issue75-resume").unwrap();
    store
        .save_progress("sidebar-scroll-regression", &locator)
        .unwrap();
    let (mut resumed, _) =
        chapter_book_with_progress(ReadingMode::Focus, Some(store.clone()), Some(target));
    for _ in 0..12 {
        issue75_frame(&ctx, &mut resumed);
        assert_eq!(shown_node(&resumed).as_deref(), Some("p219"));
        assert_focus_visible(&resumed);
    }
    drop(resumed);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn issue75_previous_chapter_keeps_its_last_focus_unit_visible_after_entry() {
    let (mut reader, _) = chapter_book(ReadingMode::Focus);
    let ctx = egui::Context::default();
    for _ in 0..4 {
        issue75_frame(&ctx, &mut reader);
    }
    toc_jump(&mut reader, "Chapter 5");
    for _ in 0..4 {
        issue75_frame(&ctx, &mut reader);
    }
    reader.go_to_adjacent_section(PageDirection::Previous);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while reader.pending_reading_unit_turn.is_some() {
        assert!(std::time::Instant::now() < deadline);
        reader.retry_pending_reading_unit_turn();
        std::thread::yield_now();
    }
    for _ in 0..10 {
        issue75_frame(&ctx, &mut reader);
        assert_eq!(
            shown_node(&reader).as_deref(),
            Some("p99"),
            "{}",
            state_line(&reader)
        );
        let unit = &reader.focus_units[reader.focus_unit_index];
        let viewport = reader.scroll_viewport.unwrap();
        let center = unit.rect.center().y + reader.scroll_content_padding(viewport.size.y)
            - viewport.offset_y;
        assert!(
            (0.0..viewport.size.y).contains(&center),
            "focus is outside the viewport: {}",
            state_line(&reader)
        );
    }
}

#[test]
fn focus_section_round_trip_restores_each_units_paragraph_and_screen_position() {
    let (mut reader, section) = chapter_book(ReadingMode::Focus);
    let ctx = egui::Context::default();
    for _ in 0..4 { issue75_frame(&ctx, &mut reader); }
    click_highlight(&mut reader, range_at(&section, "p107"));
    for _ in 0..4 { issue75_frame(&ctx, &mut reader); }
    assert_eq!(shown_node(&reader).as_deref(), Some("p107"));
    let original = reader.scroll_viewport.unwrap().offset_y;
    let turn = |reader: &mut DesktopReader, direction| {
        reader.go_to_adjacent_section(direction);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while reader.pending_reading_unit_turn.is_some() {
            assert!(std::time::Instant::now() < deadline);
            reader.retry_pending_reading_unit_turn();
            std::thread::yield_now();
        }
        for _ in 0..4 { issue75_frame(&ctx, reader); }
    };
    turn(&mut reader, PageDirection::Next);
    reader.highlights.clear();
    click_highlight(&mut reader, range_at(&section, "p132"));
    for _ in 0..4 { issue75_frame(&ctx, &mut reader); }
    let next = reader.scroll_viewport.unwrap().offset_y;
    turn(&mut reader, PageDirection::Previous);
    assert_eq!(shown_node(&reader).as_deref(), Some("p107"));
    assert!((reader.scroll_viewport.unwrap().offset_y - original).abs() < 0.1);
    assert_focus_visible(&reader);
    turn(&mut reader, PageDirection::Next);
    assert_eq!(shown_node(&reader).as_deref(), Some("p132"));
    assert!((reader.scroll_viewport.unwrap().offset_y - next).abs() < 0.1);
    assert_focus_visible(&reader);
}

#[test]
#[ignore = "requires TORTO_ISSUE75_BOOK; local book only, no AI or personal settings"]
fn issue75_local_algorithm_previous_chapter_and_shelf_resume_keep_focus_visible() {
    let opened = rebook_formats::open_file(std::path::PathBuf::from(
        std::env::var_os("TORTO_ISSUE75_BOOK").unwrap(),
    ))
    .unwrap();
    let source = opened.source();
    let mut reader = source_reader(source.clone(), ReadingMode::Focus, None, None);
    let ctx = egui::Context::default();
    ctx.set_pixels_per_point(2.0);
    let chapter = reader
        .reader
        .toc_items()
        .iter()
        .find(|item| item.label.starts_with("1.3 "))
        .unwrap()
        .label
        .clone();
    toc_jump(&mut reader, &chapter);
    for _ in 0..5 {
        issue75_frame(&ctx, &mut reader);
    }
    reader.go_to_adjacent_section(PageDirection::Previous);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while reader.pending_reading_unit_turn.is_some() {
        assert!(std::time::Instant::now() < deadline);
        reader.retry_pending_reading_unit_turn();
        std::thread::yield_now();
    }
    for _ in 0..12 {
        issue75_frame(&ctx, &mut reader);
        assert_eq!(
            reader.focus_unit_index + 1,
            reader.focus_units.len(),
            "previous chapter did not select its last block: {}",
            state_line(&reader)
        );
        assert_focus_visible(&reader);
    }
    assert!(reader.current_chapter_title().contains("1.2"));
    let target = reader.progress_source_range().unwrap();
    eprintln!(
        "#75 previous chapter: {}, node={}",
        reader.current_chapter_title(),
        target.start.node
    );
    let mut locator = reader.reader.current_locator();
    locator.source = Some(target.clone());
    let root = std::env::temp_dir().join(format!("torto-issue75-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let store = crate::sync::SyncStore::open_at(root.join("sync.sqlite3"), "issue75").unwrap();
    store
        .save_progress(&source.book().id.to_string(), &locator)
        .unwrap();
    // Reuse the same GUI context, as the shelf does, but first display a much
    // shorter reading unit so the old ScrollArea bounds cannot fit the resume.
    let short = reader
        .reader
        .toc_items()
        .iter()
        .find(|item| item.label.starts_with("1.3 "))
        .unwrap()
        .label
        .clone();
    toc_jump(&mut reader, &short);
    for _ in 0..5 {
        issue75_frame(&ctx, &mut reader);
    }
    drop(reader);
    let mut resumed = source_reader(
        source,
        ReadingMode::Focus,
        Some(store.clone()),
        Some(target.clone()),
    );
    for _ in 0..12 {
        issue75_frame(&ctx, &mut resumed);
        assert!(
            resumed.focus_units[resumed.focus_unit_index]
                .paint_ranges
                .iter()
                .any(|r| source_range_contains_anchor(r, &target.start)),
            "resume changed the focus: {}",
            state_line(&resumed)
        );
        assert_focus_visible(&resumed);
    }
    eprintln!(
        "#75 shelf resume: node={} remained visible",
        target.start.node
    );
    drop(resumed);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

fn issue75_frame(ctx: &egui::Context, reader: &mut DesktopReader) {
    frame(ctx, reader);
    for _ in 0..4 {
        if reader.semantic_layout.pending_viewport.is_none() {
            return;
        }
        super::semantic_layout::tests::finish_test_viewport_reflow(reader);
        frame(ctx, reader);
    }
    assert!(
        reader.semantic_layout.pending_viewport.is_none(),
        "viewport failed to settle"
    );
}

fn assert_focus_visible(reader: &DesktopReader) {
    let viewport = reader.scroll_viewport.unwrap();
    let rect = reader.focus_units[reader.focus_unit_index].rect;
    let top = rect.top() + reader.scroll_content_padding(viewport.size.y) - viewport.offset_y;
    let bottom = top + rect.height();
    assert!(
        top < viewport.size.y && bottom > 0.0,
        "focus scrolled outside the viewport: {}",
        state_line(reader)
    );
}

#[test]
fn search_jump_clears_a_previous_toc_override_and_pending_navigation() {
    let (mut reader, section) = chapter_book(ReadingMode::Focus);
    toc_jump(&mut reader, "Chapter 1");
    let range = range_at(&section, "p100");
    reader.go_to_search_result(&crate::plugins::BookSearchResult {
        section_index: 0, section_title: "Chapter 5".into(), excerpt: "Paragraph".into(),
        excerpt_match: 0..9, matched_text: "Paragraph".into(), block_kind: "Paragraph".into(), range: range.clone(),
    });
    assert!(reader.focus_toc_override.is_none());
    assert!(reader.pending_toc_navigation.is_none());
    assert_eq!(reader.focus_anchor, Some(range.start));
    assert_eq!(reader.current_chapter_title(), "Chapter 5");
}

struct EmptyHighlights;

impl crate::highlights::HighlightRepository for EmptyHighlights {
    fn highlights_for_book(
        &self,
        _: &str,
    ) -> crate::highlights::HighlightResult<Vec<StoredHighlight>> {
        Ok(Vec::new())
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

struct SingleSpineBook {
    book: Book,
    section: Section,
}

impl BookSource for SingleSpineBook {
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

fn paragraph(spine: &SpineItemId, index: usize) -> Block {
    let text = format!(
        "Paragraph {index}. {}",
        "A sentence that wraps a few times across the reading column. ".repeat(3)
    );
    let start = SourceAnchor {
        spine: spine.clone(),
        node: format!("p{index}"),
        text_offset: 0,
    };
    Block::Text(TextBlock {
        kind: TextBlockKind::Paragraph,
        source: Some(SourceRange {
            start: start.clone(),
            end: SourceAnchor {
                text_offset: text.chars().count() as u64,
                ..start
            },
        }),
        content: vec![Inline::Text(TextRun {
            text,
            style: TextStyle::default(),
            link: None,
        })],
        style: BlockStyle::default(),
    })
}

/// One spine file holding `CHAPTERS` chapters, each pointed at by a TOC
/// fragment, exactly like a book whose chapters live in a shared resource.
fn chapter_book(reading_mode: ReadingMode) -> (DesktopReader, Section) {
    chapter_book_with_progress(reading_mode, None, None)
}

#[allow(
    clippy::too_many_lines,
    reason = "the fixture keeps the book, its section, and the reader wiring in one readable place"
)]
fn chapter_book_with_progress(
    reading_mode: ReadingMode,
    progress_store: Option<crate::sync::SyncStore>,
    restored_source_range: Option<SourceRange>,
) -> (DesktopReader, Section) {
    let spine = SpineItemId::new(CHAPTER).unwrap();
    let href = PublicationUrl::parse("chapter.xhtml").unwrap();
    let blocks = (0..CHAPTERS * PARAGRAPHS_PER_CHAPTER)
        .map(|index| paragraph(&spine, index))
        .collect::<Vec<_>>();
    let chapter_start = |chapter: usize| {
        block_source_range(&blocks[chapter * PARAGRAPHS_PER_CHAPTER])
            .unwrap()
            .start
            .clone()
    };
    let anchors = (0..CHAPTERS)
        .map(|chapter| SectionAnchor {
            page_label: None,
            fragment: format!("nav_point_{chapter}"),
            source: chapter_start(chapter),
        })
        .collect();
    let section = Section {
        id: spine.clone(),
        href: href.clone(),
        blocks,
        anchors,
    };
    let book = Book {
        id: PublicationId::new("sidebar-scroll-regression").unwrap(),
        metadata: Metadata {
            languages: vec!["en-US".into()],
            ..Metadata::default()
        },
        cover: None,
        sections: vec![SpineItem {
            id: spine,
            href: href.clone(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        }],
        table_of_contents: (0..CHAPTERS)
            .map(|chapter| TocEntry {
                label: format!("Chapter {chapter}"),
                href: Some(
                    PublicationUrl::parse(&format!("chapter.xhtml#nav_point_{chapter}")).unwrap(),
                ),
                children: vec![],
            })
            .collect(),
    };
    let source: Arc<dyn BookSource> = Arc::new(SingleSpineBook {
        book,
        section: section.clone(),
    });
    let reader = source_reader(source, reading_mode, progress_store, restored_source_range);
    (reader, section)
}

fn source_reader(
    source: Arc<dyn BookSource>,
    reading_mode: ReadingMode,
    progress_store: Option<crate::sync::SyncStore>,
    restored_source_range: Option<SourceRange>,
) -> DesktopReader {
    let book_id = source.book().id.to_string();
    let settings = PluginSettings::default().with_test_model();
    let rewrite_source = Arc::new(RewriteBookSource::new(source.clone()));
    let translation_source = Arc::new(TranslationBookSource::new(
        rewrite_source.clone(),
        settings.translation_mode,
    ));
    let semantic_source = Arc::new(SemanticLayoutSource::new(
        translation_source.clone(),
        rewrite_source.clone(),
    ));
    let structure_source = Arc::new(ParagraphStructureSource::new(semantic_source.clone()));
    let mut session = rebook_reader::ReaderSession::open_with_fonts(
        structure_source.clone(),
        rebook_layout::LayoutViewport::new(800, 600).unwrap(),
        rebook_layout::ReaderStyle {
            spread: rebook_layout::SpreadMode::Scroll,
            typesetting: rebook_layout::ReaderTypesetting::unified(),
            ..rebook_layout::ReaderStyle::default()
        },
        crate::fonts::embedded_reader_fonts(),
    )
    .unwrap();
    if let Some(store) = &progress_store
        && let Some(progress) = store.load_progress(&book_id).unwrap()
    {
        session.restore_locator(&progress.locator).unwrap();
    }
    let mut reader = DesktopReader::new(
        session,
        DesktopReaderResources {
            source: structure_source.clone(),
            media_source: None,
            rewrite_source,
            translation_source,
            semantic_source,
            structure_source,
            pdf_ocr_controller: None,
            pdf_ocr_available: false,
            pdf_ocr_mode: PdfOcrViewMode::Original,
            cover: None,
            format: BookFormat::Epub,
            book_id: book_id.clone(),
            display_metadata: BookDisplayMetadata {
                id: book_id,
                title: "Fixture".into(),
                authors: vec![],
            },
            pdf_metadata_missing: PdfMetadataMissing {
                title: false,
                authors: false,
            },
            highlight_store: HighlightStore::from_repository(EmptyHighlights),
            highlights: vec![],
            progress_store,
            restored_source_range,
            plugin_settings: settings,
            language: AppLanguage::English,
            reading_mode,
            focus_style: crate::preferences::FocusStyle::default(),
            hide_cursor_in_focus_mode: false,
            selection_granularity: SelectionGranularity::Free,
            shortcuts: ShortcutPreferences::default(),
            sync_settings: SyncSettings::new_device(),
            sync_password: String::new(),
            source_path: std::path::PathBuf::new(),
        },
    );
    reader.ui.sidebar_open = true;
    reader.ui.sidebar_pinned = reading_mode == ReadingMode::Classic;
    reader.ui.sidebar_motion = Motion::settled(1.0);
    reader
}

fn frame(ctx: &egui::Context, reader: &mut DesktopReader) {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            ..Default::default()
        },
        |root| {
            reader.ui(root, None, false);
        },
    );
    output.textures_delta.clear();
    // The desktop shell builds the Vello scene right after the UI pass.
    drop(reader.page_scene());
}

fn frames(ctx: &egui::Context, reader: &mut DesktopReader, count: usize) {
    for _ in 0..count {
        frame(ctx, reader);
    }
}

/// Retracts the sidebar the way the toggle does, including its motion.
fn close_sidebar(ctx: &egui::Context, reader: &mut DesktopReader) {
    reader.set_sidebar_open(false);
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        frame(ctx, reader);
        if !reader.ui.sidebar_motion.is_animating() {
            break;
        }
    }
    frames(ctx, reader, 4);
}

/// Nodes of the pages the current layout shows at the current scroll offset.
fn visible_nodes(reader: &DesktopReader) -> Vec<String> {
    let Some(viewport) = reader.scroll_viewport else {
        return Vec::new();
    };
    let Some(layout) = reader.scroll_section.as_ref() else {
        return Vec::new();
    };
    layout
        .visible_pages(viewport)
        .into_iter()
        .flat_map(|position| {
            layout
                .pages
                .iter()
                .find(|entry| entry.position == position)
                .map(|entry| entry.page.content_sources())
                .unwrap_or_default()
        })
        .map(|range| range.start.node.clone())
        .collect()
}

/// The paragraph the reader shows, as the focus unit's first node.
fn shown_node(reader: &DesktopReader) -> Option<String> {
    reader
        .focus_units
        .get(reader.focus_unit_index)
        .map(|unit| unit.range.start.node.clone())
}

fn jump_to(reader: &mut DesktopReader, anchor: &SourceAnchor) {
    let result = reader.reader.go_to_source(anchor).unwrap();
    reader.apply_snapshot(result.snapshot, SnapshotEffects::navigation());
}

fn click_highlight(reader: &mut DesktopReader, range: SourceRange) {
    reader.highlights.push(StoredHighlight {
        id: "highlight".into(),
        book_id: reader.book_id.clone(),
        ranges: vec![range],
        quote: "quote".into(),
        note: None,
        created_at: 0,
    });
    reader.go_to_highlight("highlight");
}

fn anchor_at(section: &Section, node: &str) -> SourceAnchor {
    section
        .blocks
        .iter()
        .find_map(|block| {
            block_source_range(block)
                .filter(|range| range.start.node == node)
                .map(|range| range.start.clone())
        })
        .unwrap_or_else(|| panic!("no block with node {node}"))
}

fn range_at(section: &Section, node: &str) -> SourceRange {
    section
        .blocks
        .iter()
        .find_map(|block| {
            block_source_range(block)
                .filter(|range| range.start.node == node)
                .cloned()
        })
        .unwrap_or_else(|| panic!("no block with node {node}"))
}

/// Navigates through the table of contents, waiting for the background layout.
fn toc_jump(reader: &mut DesktopReader, label: &str) {
    let item = reader
        .reader
        .toc_items()
        .iter()
        .find(|item| item.label == label)
        .cloned()
        .unwrap_or_else(|| panic!("no table of contents entry labelled {label}"));
    reader.go_to_toc(&item.id, item.target.as_ref().expect("navigable entry"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while reader.pending_toc_navigation.is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "toc navigation stalled"
        );
        reader.retry_pending_toc_navigation();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn focus_highlight_jump_lands_on_the_highlight_and_stays_there() {
    let (mut reader, section) = chapter_book(ReadingMode::Focus);
    let scratch = std::env::temp_dir().join(format!("torto-sidebar-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let store =
        crate::sync::SyncStore::open_at(scratch.join("sync.sqlite3"), "sidebar-test").unwrap();
    reader.progress_store = Some(store.clone());
    let ctx = egui::Context::default();
    frames(&ctx, &mut reader, 4);

    // Reading the first chapter, reached the way the table of contents does.
    toc_jump(&mut reader, "Chapter 1");
    frames(&ctx, &mut reader, 6);
    assert_eq!(shown_node(&reader).as_deref(), Some("p20"));

    // Clicking a highlight from a much later chapter.
    let target = range_at(&section, "p205");
    click_highlight(&mut reader, target.clone());
    frames(&ctx, &mut reader, 4);
    assert_eq!(
        shown_node(&reader).as_deref(),
        Some("p205"),
        "the highlight jump did not move the reader to the highlight's paragraph: {}",
        state_line(&reader)
    );

    close_sidebar(&ctx, &mut reader);
    assert_eq!(
        shown_node(&reader).as_deref(),
        Some("p205"),
        "retracting the sidebar returned to the paragraph that was read before: {}",
        state_line(&reader)
    );
    assert!(
        reader
            .progress_source_range()
            .is_some_and(|range| range.start.node == "p205"),
        "the reading position did not follow the highlight: {}",
        state_line(&reader)
    );
    let saved = store.load_progress(&reader.book_id).unwrap().unwrap();
    assert_eq!(
        saved
            .locator
            .source
            .as_ref()
            .map(|range| range.start.node.as_str()),
        Some("p205"),
        "the saved position still points at the paragraph left behind"
    );
}

#[test]
fn opening_book_keeps_its_precise_resume_anchor() {
    let (reader, section) = chapter_book(ReadingMode::Focus);
    let target = range_at(&section, "p1");
    let mut locator = reader.reader.current_locator();
    locator.source = Some(target.clone());
    let path = std::env::temp_dir().join(format!("torto-resume-{}.sqlite3", uuid::Uuid::new_v4()));
    let store = crate::sync::SyncStore::open_at(path.clone(), "resume-test").unwrap();
    store
        .save_progress("sidebar-scroll-regression", &locator)
        .unwrap();
    drop(reader);
    let (resumed, _) =
        chapter_book_with_progress(ReadingMode::Focus, Some(store.clone()), Some(target));
    assert_eq!(
        resumed.reader.current_locator().source.unwrap().start.node,
        "p0"
    );
    assert_eq!(resumed.progress_locator().source.unwrap().start.node, "p1");
    assert_eq!(
        store
            .load_progress("sidebar-scroll-regression")
            .unwrap()
            .unwrap()
            .locator,
        locator
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite3-wal"));
    let _ = std::fs::remove_file(path.with_extension("sqlite3-shm"));
}

#[test]
fn classic_highlight_jump_keeps_the_page_when_the_sidebar_is_retracted() {
    let (mut reader, section) = chapter_book(ReadingMode::Classic);
    let ctx = egui::Context::default();
    frames(&ctx, &mut reader, 4);

    let reading = anchor_at(&section, "p25");
    jump_to(&mut reader, &reading);
    frames(&ctx, &mut reader, 4);
    let early = visible_nodes(&reader);
    assert!(early.contains(&"p25".to_owned()), "{early:?}");

    let target = range_at(&section, "p205");
    click_highlight(&mut reader, target);
    frames(&ctx, &mut reader, 4);
    let jumped = visible_nodes(&reader);
    assert!(jumped.contains(&"p205".to_owned()), "{jumped:?}");
    let narrow = reader
        .canvas_size
        .expect("canvas while the sidebar is open");

    close_sidebar(&ctx, &mut reader);
    let wide = reader.canvas_size.expect("canvas after the sidebar closed");
    assert!(
        wide.0 > narrow.0,
        "the sidebar freed no width: {narrow:?} -> {wide:?}"
    );
    let after_close = visible_nodes(&reader);
    assert!(
        after_close.contains(&"p205".to_owned()),
        "closing the sidebar moved the page away from the highlight: {after_close:?}"
    );
}

/// Replays the two gestures on a real library book.
///
/// `TORTO_SIDEBAR_BOOK` is the book path and `TORTO_SIDEBAR_FROM` /
/// `TORTO_SIDEBAR_TO` are block node ids (for example `n925` -> `n1687`).
#[test]
#[ignore = "requires TORTO_SIDEBAR_BOOK; reads a real library book"]
fn real_book_sidebar_close_keeps_the_highlight_page() {
    let path = std::path::PathBuf::from(std::env::var("TORTO_SIDEBAR_BOOK").unwrap());
    let from = std::env::var("TORTO_SIDEBAR_FROM").unwrap_or_else(|_| "n925".into());
    let to = std::env::var("TORTO_SIDEBAR_TO").unwrap_or_else(|_| "n1687".into());
    let scratch = std::env::temp_dir().join(format!("torto-sidebar-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let store =
        crate::sync::SyncStore::open_at(scratch.join("sync.sqlite3"), "diagnostic").unwrap();
    let mut reader = open_reader(
        &path,
        crate::fonts::embedded_reader_fonts(),
        None,
        None,
        store,
    )
    .unwrap();
    reader.progress_store = None;
    println!(
        "mode={:?} spread={:?} pinned={} sections={}",
        reader.reading_mode,
        reader.reader.style().spread,
        reader.ui.sidebar_pinned,
        reader.source.book().sections.len(),
    );
    // Keep the book's own reading mode; the report is about the left sidebar.
    reader.ui.sidebar_open = true;
    reader.ui.sidebar_motion = Motion::settled(1.0);

    let sections = (0..reader.source.book().sections.len())
        .filter_map(|index| reader.source.parse_section(index).ok())
        .collect::<Vec<_>>();
    let range_of = |node: &str| {
        sections
            .iter()
            .flat_map(|section| section.blocks.iter())
            .find_map(|block| {
                block_source_range(block)
                    .filter(|range| range.start.node == node)
                    .cloned()
            })
            .unwrap_or_else(|| panic!("no block with node {node}"))
    };
    let ctx = egui::Context::default();
    frames(&ctx, &mut reader, 4);

    let reading = range_of(&from);
    jump_to(&mut reader, &reading.start);
    frames(&ctx, &mut reader, 4);
    println!("after reading {from}: {}", state_line(&reader));

    let target = range_of(&to);
    click_highlight(&mut reader, target.clone());
    frames(&ctx, &mut reader, 4);
    println!("after highlight {to}: {}", state_line(&reader));

    close_sidebar(&ctx, &mut reader);
    println!("after sidebar close: {}", state_line(&reader));
    assert_eq!(
        shown_node(&reader).as_deref(),
        Some(to.as_str()),
        "the highlight paragraph is not displayed: {}",
        state_line(&reader)
    );
    assert!(
        reader
            .progress_source_range()
            .is_some_and(|range| range.start.node == to),
        "the reading position did not follow the highlight: {}",
        state_line(&reader)
    );
}

#[test]
fn reading_position_changes_own_the_toc_keyboard_focus() {
    let (mut reader, section) = chapter_book(ReadingMode::Classic);
    let ctx = egui::Context::default();
    frames(&ctx, &mut reader, 4);

    toc_jump(&mut reader, "Chapter 1");
    frames(&ctx, &mut reader, 4);
    let toc_id = |label: &str| {
        reader
            .reader
            .toc_items()
            .iter()
            .find(|item| item.label == label)
            .map(|item| item.id.clone())
            .unwrap_or_else(|| panic!("no table of contents entry labelled {label}"))
    };
    let chapter_one = toc_id("Chapter 1");
    let chapter_two = toc_id("Chapter 2");
    assert_eq!(
        reader.snapshot.active_toc_id.as_deref(),
        Some(chapter_one.as_str()),
        "the fixture did not start on chapter one: {}",
        state_line(&reader)
    );

    // Keyboard focus resting on a later row stays usable while the reading
    // position moves inside the chapter being read.
    reader.ui.toc_keyboard_row = Some(8);
    reader.ui.last_auto_scrolled_toc_keyboard_row = Some(8);
    jump_to(&mut reader, &anchor_at(&section, "p25"));
    assert_eq!(
        reader.snapshot.active_toc_id.as_deref(),
        Some(chapter_one.as_str()),
        "the jump left chapter one: {}",
        state_line(&reader)
    );
    assert_eq!(
        reader.ui.toc_keyboard_row,
        Some(8),
        "a position change inside the chapter dropped the keyboard focus"
    );
    assert_eq!(
        reader.ui.last_auto_scrolled_toc_keyboard_row,
        Some(8),
        "a position change inside the chapter reset the focus scroll marker"
    );

    // Crossing into the next chapter drops the stale focus, so the sidebar
    // lights only the selected highlight.
    jump_to(&mut reader, &anchor_at(&section, "p45"));
    assert_eq!(
        reader.snapshot.active_toc_id.as_deref(),
        Some(chapter_two.as_str()),
        "the jump did not reach chapter two: {}",
        state_line(&reader)
    );
    assert_eq!(
        reader.ui.toc_keyboard_row, None,
        "the keyboard focus outlived the move to the next chapter"
    );
    assert_eq!(
        reader.ui.last_auto_scrolled_toc_keyboard_row, None,
        "the focus scroll marker outlived the move to the next chapter"
    );
}

#[test]
fn starting_a_toc_jump_clears_the_focus_while_layout_catches_up() {
    let (mut reader, _) = chapter_book(ReadingMode::Classic);
    let ctx = egui::Context::default();
    frames(&ctx, &mut reader, 4);

    toc_jump(&mut reader, "Chapter 1");
    frames(&ctx, &mut reader, 4);

    // Focus sits far down the list. Re-entering the chapter already being read
    // keeps the selected highlight where it is, so only the jump start itself
    // can clear the focus before the layout hands back its snapshot.
    reader.ui.toc_keyboard_row = Some(8);
    reader.ui.last_auto_scrolled_toc_keyboard_row = Some(8);
    let item = reader
        .reader
        .toc_items()
        .iter()
        .find(|item| item.label == "Chapter 1")
        .cloned()
        .unwrap_or_else(|| panic!("no table of contents entry labelled Chapter 1"));
    reader.go_to_toc(&item.id, item.target.as_ref().expect("navigable entry"));
    assert_eq!(
        reader.ui.toc_keyboard_row, None,
        "the focused row stayed lit while the navigation was in flight"
    );
    assert_eq!(
        reader.ui.last_auto_scrolled_toc_keyboard_row, None,
        "the focus scroll marker stayed lit while the navigation was in flight"
    );
}

fn state_line(reader: &DesktopReader) -> String {
    let visible = visible_nodes(reader);
    format!(
        "shown={:?} unit={:?} location={:?} layout_unit={:?} offset={:?} progress={:?} visible={:?}",
        shown_node(reader),
        reader
            .focus_units
            .get(reader.focus_unit_index)
            .map(|unit| format!("{}..{}", unit.range.start.node, unit.range.end.node)),
        reader.reader.location(),
        reader
            .scroll_section
            .as_ref()
            .map(|layout| layout.reading_unit_index),
        reader.scroll_viewport.map(|viewport| viewport.offset_y),
        reader
            .progress_source_range()
            .map(|range| range.start.node.clone()),
        visible.first().zip(visible.last()),
    )
}
