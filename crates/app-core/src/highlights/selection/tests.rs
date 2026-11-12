use super::*;
use crate::sync::SyncStore;
use rebook_publication::*;

struct Source {
    book: Book,
    section: Section,
}
impl BookSource for Source {
    fn book(&self) -> &Book {
        &self.book
    }
    fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
        Ok(self.section.clone())
    }
    fn resource(&self, _: &PublicationUrl) -> Result<Resource, PublicationError> {
        unreachable!()
    }
}
fn source() -> Source {
    let item = SpineItem {
        id: SpineItemId::new("chapter").unwrap(),
        href: PublicationUrl::parse("chapter.xhtml").unwrap(),
        media_type: "application/xhtml+xml".into(),
        linear: true,
        properties: vec![],
    };
    let mut section=rebook_html::parse_section("<html><body><p>0123456789abcdefghijklmnopqrstuv</p><p>甲乙丙丁戊己庚辛壬癸子丑寅卯辰巳午未申酉</p></body></html>",&item,|_|None).unwrap();
    for (block, name) in section.blocks.iter_mut().zip(["p2", "p10"]) {
        let text = block.text_blocks_mut().next().unwrap();
        let range = text.source.as_mut().unwrap();
        range.start.node = name.into();
        range.end.node = name.into();
    }
    Source {
        book: Book {
            id: PublicationId::new("book").unwrap(),
            metadata: Metadata::default(),
            cover: None,
            sections: vec![item],
            table_of_contents: vec![],
        },
        section,
    }
}
fn range(node: &str, start: u64, end: u64) -> SourceRange {
    let anchor = SourceAnchor {
        spine: SpineItemId::new("chapter").unwrap(),
        node: node.into(),
        text_offset: start,
    };
    SourceRange {
        start: anchor.clone(),
        end: SourceAnchor {
            text_offset: end,
            ..anchor
        },
    }
}
fn highlight(id: &str, ranges: Vec<SourceRange>, at: u64, note: Option<&str>) -> StoredHighlight {
    StoredHighlight {
        id: id.into(),
        book_id: "book".into(),
        ranges,
        quote: id.into(),
        note: note.map(str::to_owned),
        created_at: at,
    }
}
struct Db {
    root: std::path::PathBuf,
    store: SyncStore,
}
impl Db {
    fn new(device: &str) -> Self {
        let root = std::env::temp_dir().join(format!("torto-selection-{}", Uuid::new_v4()));
        Self {
            store: SyncStore::open_at(root.join("store.sqlite"), device).unwrap(),
            root,
        }
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn overlap_union_is_atomic_keeps_old_id_and_sync_tombstones_prevent_resurrection() {
    let db = Db::new("a");
    let source = source();
    let store = HighlightStore::from_repository(db.store.clone());
    store
        .save_selection(
            &source,
            highlight("old", vec![range("p2", 2, 8)], 1, None),
            SelectionAction::Add,
        )
        .unwrap();
    store
        .save_selection(
            &source,
            highlight("second", vec![range("p2", 12, 18)], 2, None),
            SelectionAction::Add,
        )
        .unwrap();
    let stale = db.store.annotations_for_book("book").unwrap();
    let rows = store
        .save_selection(
            &source,
            highlight("bridge", vec![range("p2", 6, 14)], 3, None),
            SelectionAction::Add,
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "old");
    assert_eq!(rows[0].created_at, 1);
    assert_eq!(rows[0].ranges, vec![range("p2", 2, 18)]);
    assert_eq!(rows[0].quote, "23456789abcdefgh");
    let snapshot = db.store.reading_snapshot("book").unwrap().1;
    assert!(
        snapshot
            .annotations
            .iter()
            .any(|a| a.id == "second" && a.deleted_at.is_some())
    );
    let peer = Db::new("b");
    peer.store.merge_annotations(&snapshot.annotations).unwrap();
    peer.store.merge_annotations(&stale).unwrap();
    let peer_rows = peer.store.annotations_for_book("book").unwrap();
    assert_eq!(peer_rows.len(), 1);
    assert_eq!(peer_rows[0].ranges, rows[0].ranges);
    let revision = db.store.reading_snapshot("book").unwrap().0;
    assert_eq!(
        store
            .save_selection(&source, rows[0].clone(), SelectionAction::Add)
            .unwrap(),
        rows
    );
    assert_eq!(
        db.store.reading_snapshot("book").unwrap().0,
        revision,
        "saving an existing highlight is idempotent"
    );
    assert!(
        store
            .save_selection(&source, rows[0].clone(), SelectionAction::Remove)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .save_selection(&source, rows[0].clone(), SelectionAction::Remove)
            .unwrap()
            .is_empty(),
        "stale Remove never creates a new highlight"
    );
}

#[test]
fn overlaps_close_transitively_but_adjacent_and_different_nodes_do_not_overlap() {
    let db = Db::new("a");
    let source = source();
    let store = HighlightStore::from_repository(db.store.clone());
    for (id, start, end, at) in [("a", 0, 5, 3), ("b", 4, 10, 2), ("c", 9, 15, 1)] {
        db.store
            .insert_highlight(&highlight(id, vec![range("p2", start, end)], at, None))
            .unwrap();
    }
    let rows = store
        .save_selection(
            &source,
            highlight("d", vec![range("p2", 14, 20)], 4, None),
            SelectionAction::Add,
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ranges, vec![range("p2", 0, 20)]);
    let rows = store
        .save_selection(
            &source,
            highlight("adjacent", vec![range("p2", 20, 25)], 5, None),
            SelectionAction::Add,
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    let rows = store
        .save_selection(
            &source,
            highlight("other-node", vec![range("p10", 0, 10)], 6, None),
            SelectionAction::Add,
        )
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert!(same_ranges(
        &[range("p2", 0, 10), range("p2", 10, 20)],
        &[range("p2", 0, 20)]
    ));
}

#[test]
fn cross_paragraph_union_keeps_source_order_and_unicode_offsets() {
    let db = Db::new("a");
    let source = source();
    let store = HighlightStore::from_repository(db.store.clone());
    store
        .save_selection(
            &source,
            highlight("old", vec![range("p2", 2, 12)], 1, None),
            SelectionAction::Add,
        )
        .unwrap();
    let rows = store
        .save_selection(
            &source,
            highlight("new", vec![range("p2", 8, 32), range("p10", 0, 5)], 2, None),
            SelectionAction::Add,
        )
        .unwrap();
    assert_eq!(rows[0].ranges, vec![range("p2", 2, 32), range("p10", 0, 5)]);
    assert_eq!(
        rows[0].quote,
        "23456789abcdefghijklmnopqrstuv\n\n甲乙丙丁戊"
    );
}

#[test]
fn note_records_and_concurrent_edits_are_never_lost() {
    let db = Db::new("a");
    let source = source();
    let store = HighlightStore::from_repository(db.store.clone());
    let a = highlight("mark", vec![range("p2", 0, 5)], 1, None);
    store
        .save_selection(&source, a.clone(), SelectionAction::Add)
        .unwrap();
    for (id, text) in [("note1", "First thought"), ("note2", "Second thought")] {
        store
            .save_selection(
                &source,
                highlight(id, a.ranges.clone(), 2, Some(text)),
                SelectionAction::Add,
            )
            .unwrap();
    }
    let before = db.store.highlights_for_book("book").unwrap();
    db.store
        .update_annotation_note("note1", Some("Changed on another connection".into()))
        .unwrap();
    assert!(
        !db.store
            .apply_selection_edit("book", &before, &["note1".into()], None)
            .unwrap(),
        "a stale editor cannot remove a concurrent note edit"
    );
    let rows = store
        .save_selection(&source, a, SelectionAction::Remove)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|a| a.note.is_some()));
    assert!(
        rows.iter()
            .any(|a| a.note.as_deref() == Some("Changed on another connection"))
    );
}

#[test]
fn generated_math_does_not_shift_source_offsets_when_expanding_a_highlight() {
    let db = Db::new("a");
    let mut source = source();
    let text = source.section.blocks[0].text_blocks_mut().next().unwrap();
    let original = crate::search::text_block_text(text);
    let run = |s: &str| {
        Inline::Text(TextRun {
            text: s.into(),
            style: TextStyle::default(),
            link: None,
        })
    };
    text.content = vec![
        run(&original[..16]),
        Inline::Math(MathRun {
            original: None,
            latex: "x^2".into(),
            display: false,
            size_scale: 1.0,
        }),
        run(&original[16..]),
    ];
    let store = HighlightStore::from_repository(db.store.clone());
    store
        .save_selection(
            &source,
            highlight("left", vec![range("p2", 12, 17)], 1, None),
            SelectionAction::Add,
        )
        .unwrap();
    let rows = store
        .save_selection(
            &source,
            highlight("right", vec![range("p2", 16, 22)], 2, None),
            SelectionAction::Add,
        )
        .unwrap();
    assert_eq!(rows[0].ranges, vec![range("p2", 12, 22)]);
    assert_eq!(rows[0].quote, "cdefghijkl");
}
