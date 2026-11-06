use super::*;
use rebook_publication::{
    BlockStyle, Metadata, PublicationId, Resource, SpineItem, SpineItemId, TextStyle,
};
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

struct Source {
    book: Book,
    original: Section,
    translated: bool,
    calls: AtomicUsize,
}
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut section = self.original.clone();
        if self.translated {
            for b in &mut section.blocks {
                if let Block::Text(t) = b
                    && t.kind.is_heading()
                {
                    t.content = vec![Inline::Text(TextRun {
                        text: "译文标题".into(),
                        style: Default::default(),
                        link: None,
                    })];
                }
            }
        }
        Ok(section)
    }
    fn navigation_section(&self, _: usize) -> Result<Option<Section>, PublicationError> {
        Ok(self.translated.then(|| self.original.clone()))
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Err(PublicationError::ResourceNotFound(href.to_string()))
    }
}
fn source(labels: &[&str], blocks: &[(TextBlockKind, &str)], translated: bool) -> Arc<Source> {
    let id = SpineItemId::new("front").unwrap();
    let href = PublicationUrl::parse("front.xhtml").unwrap();
    let blocks = blocks
        .iter()
        .enumerate()
        .map(|(i, (kind, text))| {
            let start = SourceAnchor {
                spine: id.clone(),
                node: format!("n{i}"),
                text_offset: 0,
            };
            Block::Text(TextBlock {
                kind: *kind,
                content: vec![Inline::Text(TextRun {
                    text: (*text).into(),
                    style: TextStyle::default(),
                    link: None,
                })],
                style: BlockStyle::default(),
                source: Some(SourceRange {
                    end: SourceAnchor {
                        text_offset: text.chars().count() as u64,
                        ..start.clone()
                    },
                    start,
                }),
            })
        })
        .collect();
    Arc::new(Source {
        book: Book {
            id: PublicationId::new("toc-recovery").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![SpineItem {
                id: id.clone(),
                href: href.clone(),
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: vec![],
            }],
            table_of_contents: labels
                .iter()
                .map(|label| TocEntry {
                    label: (*label).into(),
                    href: Some(href.clone()),
                    children: vec![],
                })
                .collect(),
        },
        original: Section {
            id,
            href,
            blocks,
            anchors: vec![],
        },
        translated,
        calls: AtomicUsize::new(0),
    })
}
#[test]
fn ambiguous_toc_prefers_unique_authored_headings_and_survives_translation_resize() {
    for translated in [false, true] {
        let source = source(
            &["Title Page", "Copyright", "Preface", "Contents"],
            &[
                (TextBlockKind::Heading(1), "A Book"),
                (TextBlockKind::Paragraph, "Second Edition"),
                (TextBlockKind::Paragraph, "Preface"),
                (TextBlockKind::Paragraph, "© Example Publisher 2026"),
                (TextBlockKind::Heading(1), "Preface"),
                (TextBlockKind::Paragraph, "Authored introduction."),
                (TextBlockKind::Heading(1), "Contents"),
                (TextBlockKind::Paragraph, "Chapter listing."),
            ],
            translated,
        );
        let before = source.book.clone();
        let original = source.original.clone();
        let mut reader = ReaderSession::open(
            source.clone(),
            LayoutViewport::new(800, 600).unwrap(),
            ReaderStyle::default(),
        )
        .unwrap();
        assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("0"));
        for (id, node) in [("1", "n3"), ("2", "n4"), ("3", "n6")] {
            reader.go_to_toc(id).unwrap();
            assert_eq!(reader.source_anchor_for_toc(id).unwrap().node, node);
            assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some(id));
            assert_eq!(reader.current_reading_unit_anchor().unwrap().node, node);
            let locator = reader.current_locator();
            assert_eq!(
                locator.href.fragment(),
                None,
                "virtual anchors never enter durable locators"
            );
            reader
                .resize(LayoutViewport::new(420, 340).unwrap())
                .unwrap();
            reader.restore_locator(&locator).unwrap();
            assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some(id));
        }
        reader.go_to_toc("2").unwrap();
        reader
            .go_to_adjacent_reading_unit(PageDirection::Next)
            .unwrap();
        assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("3"));
        reader
            .go_to_href(&PublicationUrl::parse("front.xhtml").unwrap())
            .unwrap();
        assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("0"));
        assert_eq!(source.book, before);
        assert_eq!(source.original, original);
        if !translated {
            assert_eq!(
                source.calls.load(Ordering::Relaxed),
                1,
                "no second parse for a plain source"
            );
        }
    }
}
#[test]
fn valid_fragments_and_parent_aliases_are_preserved_and_duplicate_headings_are_not_guessed() {
    let mut source = source(
        &["Title", "Preface", "Contents"],
        &[
            (TextBlockKind::Paragraph, "Title"),
            (TextBlockKind::Heading(1), "Preface"),
            (TextBlockKind::Paragraph, "First prose."),
            (TextBlockKind::Heading(1), "Preface"),
            (TextBlockKind::Paragraph, "Other prose."),
            (TextBlockKind::Heading(1), "Contents"),
            (TextBlockKind::Paragraph, "List."),
        ],
        false,
    );
    let s = Arc::get_mut(&mut source).unwrap();
    s.book.table_of_contents[2].href = Some(PublicationUrl::parse("front.xhtml#authored").unwrap());
    s.original.anchors.push(SectionAnchor {
        page_label: None,
        fragment: "authored".into(),
        source: block_source(&s.original.blocks[0]).unwrap().start.clone(),
    });
    let mut reader = ReaderSession::open(
        source,
        LayoutViewport::new(700, 500).unwrap(),
        ReaderStyle::default(),
    )
    .unwrap();
    assert_eq!(
        reader.source_anchor_for_toc("1").unwrap().node,
        "n0",
        "two matching headings stay unresolved"
    );
    reader.go_to_toc("2").unwrap();
    assert_eq!(
        reader.source_anchor_for_toc("2").unwrap().node,
        "n0",
        "a valid authored anchor wins over a later title"
    );
    let target = reader.repository.get(0).unwrap().toc_targets["2"].clone();
    assert_eq!(target.href.fragment(), Some("authored"));
}
#[test]
fn authored_internal_contents_links_remain_clickable_after_recovery() {
    let mut source = source(
        &["Start", "Contents"],
        &[
            (TextBlockKind::Heading(1), "A book"),
            (TextBlockKind::Paragraph, "Go to contents"),
            (TextBlockKind::Heading(1), "Contents"),
            (TextBlockKind::Paragraph, "Chapter list"),
        ],
        false,
    );
    let raw = Arc::get_mut(&mut source).unwrap();
    let target = PublicationUrl::parse("front.xhtml#contents").unwrap();
    if let Block::Text(t) = &mut raw.original.blocks[1]
        && let Inline::Text(r) = &mut t.content[0]
    {
        r.link = Some(target.clone());
    }
    raw.original.anchors.push(SectionAnchor {
        page_label: None,
        fragment: "contents".into(),
        source: block_source(&raw.original.blocks[2]).unwrap().start.clone(),
    });
    let range = block_source(&raw.original.blocks[1]).unwrap().clone();
    let mut reader = ReaderSession::open(
        source,
        LayoutViewport::new(700, 500).unwrap(),
        ReaderStyle::default(),
    )
    .unwrap();
    let bounds = reader.current_page().source_rects(&[range])[0];
    let link = reader
        .book_link_at_current_spread(
            (bounds.x0 + 3.) as f32,
            ((bounds.y0 + bounds.y1) * 0.5) as f32,
        )
        .unwrap()
        .unwrap();
    assert_eq!(link.0, target);
    reader.go_to_href(&link.0).unwrap();
    assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("1"));
}
#[test]
fn parent_child_file_aliases_and_fixed_page_navigation_are_never_repaired() {
    let mut source = source(
        &["Parent", "Different child title"],
        &[
            (TextBlockKind::Heading(1), "Parent"),
            (TextBlockKind::Paragraph, "Body"),
            (TextBlockKind::Heading(1), "Different child title"),
            (TextBlockKind::Paragraph, "Child body"),
        ],
        false,
    );
    let raw = Arc::get_mut(&mut source).unwrap();
    let child = raw.book.table_of_contents.pop().unwrap();
    raw.book.table_of_contents[0].children.push(child);
    let rows = flatten_toc(&raw.book.table_of_contents);
    let mut section = raw.original.clone();
    let resolved = prepare(raw, 0, &rows, &mut section).unwrap();
    assert!(section.anchors.is_empty());
    assert!(
        resolved
            .targets
            .values()
            .all(|t| t.href.fragment().is_none() && t.active)
    );
    raw.book.metadata.layout = RenditionLayout::PrePaginated;
    let child = raw.book.table_of_contents[0].children.pop().unwrap();
    raw.book.table_of_contents.push(child);
    let rows = flatten_toc(&raw.book.table_of_contents);
    let mut section = raw.original.clone();
    let resolved = prepare(raw, 0, &rows, &mut section).unwrap();
    assert!(section.anchors.is_empty());
    assert!(
        resolved
            .targets
            .values()
            .all(|t| t.href.fragment().is_none() && t.active)
    );
}
#[test]
fn inverted_recoveries_are_rejected_and_forward_backward_units_cannot_loop() {
    for anchored_second in [false, true] {
        let mut source = source(
            &["Start", "First", "Second"],
            &[
                (TextBlockKind::Paragraph, "Introduction"),
                (TextBlockKind::Heading(1), "Second"),
                (TextBlockKind::Paragraph, "Second body"),
                (TextBlockKind::Heading(1), "First"),
                (TextBlockKind::Paragraph, "First body"),
            ],
            false,
        );
        if anchored_second {
            let raw = Arc::get_mut(&mut source).unwrap();
            raw.book.table_of_contents[2].href =
                Some(PublicationUrl::parse("front.xhtml#second").unwrap());
            raw.original.anchors.push(SectionAnchor {
                page_label: None,
                fragment: "second".into(),
                source: block_source(&raw.original.blocks[1]).unwrap().start.clone(),
            });
        }
        let mut reader = ReaderSession::open(
            source,
            LayoutViewport::new(700, 500).unwrap(),
            ReaderStyle::default(),
        )
        .unwrap();
        assert_eq!(
            reader.source_anchor_for_toc("1").unwrap().node,
            "n0",
            "a reversed heading is not recovered"
        );
        if !anchored_second {
            assert_eq!(reader.source_anchor_for_toc("2").unwrap().node, "n0");
        }
        let mut seen = HashSet::new();
        loop {
            assert!(
                seen.insert({
                    let p = reader.current_position();
                    (p.section_index, p.segment_index, p.page_index)
                }),
                "forward navigation revisited a unit"
            );
            assert!(seen.len() < 8);
            if reader
                .go_to_adjacent_reading_unit(PageDirection::Next)
                .unwrap()
                .outcome
                == NavigationOutcome::Boundary
            {
                break;
            }
        }
        seen.clear();
        loop {
            assert!(
                seen.insert({
                    let p = reader.current_position();
                    (p.section_index, p.segment_index, p.page_index)
                }),
                "backward navigation revisited a unit"
            );
            assert!(seen.len() < 8);
            if reader
                .go_to_adjacent_reading_unit(PageDirection::Previous)
                .unwrap()
                .outcome
                == NavigationOutcome::Boundary
            {
                break;
            }
        }
    }
}
#[test]
fn missing_fragment_recovers_only_the_heading() {
    let mut source = source(
        &["Start", "Preface"],
        &[
            (TextBlockKind::Paragraph, "Preface"),
            (TextBlockKind::Heading(1), "Preface"),
            (TextBlockKind::Paragraph, "Actual preface."),
        ],
        false,
    );
    Arc::get_mut(&mut source).unwrap().book.table_of_contents[1].href =
        Some(PublicationUrl::parse("front.xhtml#missing").unwrap());
    let mut reader = ReaderSession::open(
        source,
        LayoutViewport::new(700, 500).unwrap(),
        ReaderStyle::default(),
    )
    .unwrap();
    let mut attempt = reader.try_go_to_toc("1").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while matches!(attempt, NavigationAttempt::Pending) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
        attempt = reader.try_go_to_toc("1").unwrap();
    }
    assert_eq!(reader.source_anchor_for_toc("1").unwrap().node, "n1");
    assert_eq!(reader.snapshot().active_toc_id.as_deref(), Some("1"));
}

#[test]
fn note_source_contexts_reuse_repaired_chapters_without_moving_the_reader() {
    let source = source(&["First", "Second"], &[
        (TextBlockKind::Heading(1), "First"),
        (TextBlockKind::Paragraph, "First highlight"),
        (TextBlockKind::Heading(1), "Second"),
        (TextBlockKind::Paragraph, "Second highlight"),
    ], true);
    let reader = ReaderSession::open(source.clone(), LayoutViewport::new(700, 500).unwrap(), ReaderStyle::default()).unwrap();
    let before = reader.snapshot();
    let calls = source.calls.load(Ordering::Relaxed);
    let anchor = |node: &str| SourceAnchor {spine: source.book.sections[0].id.clone(), node: node.into(), text_offset: 0};
    let contexts = reader.source_contexts(&[anchor("n3"), anchor("n1"), anchor("outdated")]);
    assert_eq!(contexts[0].as_ref().unwrap().toc.as_ref().unwrap().label, "Second");
    assert_eq!(contexts[1].as_ref().unwrap().toc.as_ref().unwrap().label, "First");
    assert!(contexts[0].as_ref().unwrap().node_order > contexts[1].as_ref().unwrap().node_order);
    assert!(contexts[2].as_ref().unwrap().node_order.is_none());
    assert!(contexts[2].as_ref().unwrap().toc.is_none());
    assert_eq!(reader.snapshot(), before);
    assert_eq!(source.calls.load(Ordering::Relaxed), calls, "one batch must reuse the already prepared section");
}
