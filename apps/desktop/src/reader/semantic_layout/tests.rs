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

fn fixture() -> (DesktopReader, Section, SourceRange) {
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
        Block::Image(ImageBlock {href:PublicationUrl::parse("image.png").unwrap(),alt:String::new(),style:Default::default(),source:Some(SourceRange {start:image_anchor.clone(),end:image_anchor}),text_layer:None}),
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
    let reader = DesktopReader::new(
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
    reader.refresh_semantic_layout();
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
            reader.refresh_semantic_layout();
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
        reader.refresh_semantic_layout();
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
