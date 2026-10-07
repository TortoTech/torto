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
        && let Some(progress) = store.load_progress("sidebar-scroll-regression").unwrap()
    {
        session.restore_locator(&progress.locator).unwrap();
    }
    let mut reader = DesktopReader::new(
        session,
        DesktopReaderResources {
            source: structure_source.clone(),
            rewrite_source,
            translation_source,
            semantic_source,
            structure_source,
            pdf_ocr_controller: None,
            pdf_ocr_available: false,
            pdf_ocr_mode: PdfOcrViewMode::Original,
            cover: None,
            format: BookFormat::Epub,
            book_id: "sidebar-scroll-regression".into(),
            display_metadata: BookDisplayMetadata {
                id: "sidebar-scroll-regression".into(),
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
    (reader, section)
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
