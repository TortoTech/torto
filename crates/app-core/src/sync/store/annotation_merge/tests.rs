use super::*;
use rebook_publication::{SourceAnchor, SpineItemId};

struct Database(SyncStore);

impl Database {
    fn new(device: &str) -> Self {
        Self(
            SyncStore::open_at(
                std::env::temp_dir()
                    .join(format!("torto-annotation-{}.sqlite3", uuid::Uuid::new_v4())),
                device,
            )
            .unwrap(),
        )
    }

    fn create(&self, id: &str, note: Option<&str>) -> AnnotationState {
        let start = SourceAnchor {
            spine: SpineItemId::new("chapter").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        self.0
            .create_annotation(
                id.into(),
                "book".into(),
                vec![SourceRange {
                    end: SourceAnchor {
                        text_offset: 15,
                        ..start.clone()
                    },
                    start,
                }],
                "Selected prose.".into(),
                note.map(str::to_owned),
                7,
            )
            .unwrap()
    }

    fn seed(&self, records: &[AnnotationState]) {
        let mut connection = self.0.connection().unwrap();
        let transaction = connection.transaction().unwrap();
        for value in records {
            write_annotation(&transaction, value).unwrap();
        }
        transaction.commit().unwrap();
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0.path);
    }
}

#[test]
fn identical_parallel_edits_join_histories_without_creating_new_edits() {
    let a = Database::new("a");
    let b = Database::new("b");
    let left = a.create("root", Some("Same note"));
    let right = b.create("root", Some("Same note"));
    let (revision, _) = a.0.reading_snapshot("book").unwrap();
    a.0.acknowledge_reading("account", "book", revision)
        .unwrap();
    a.0.merge_annotations(std::slice::from_ref(&right)).unwrap();
    b.0.merge_annotations(std::slice::from_ref(&left)).unwrap();
    for _ in 0..3 {
        let left = a.0.annotations_for_book("book").unwrap();
        let right = b.0.annotations_for_book("book").unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        assert_eq!(
            left[0].clock,
            BTreeMap::from([("a".into(), 1), ("b".into(), 1)])
        );
        assert_eq!(left[0].clock, right[0].clock);
        assert_eq!(a.0.merge_annotations(&right).unwrap(), 0);
        assert_eq!(b.0.merge_annotations(&left).unwrap(), 0);
    }
    assert_eq!(a.0.pending_reading("account").unwrap().len(), 1);
    assert_eq!(a.0.reading_snapshot("book").unwrap().1.annotations.len(), 1);
}

#[test]
fn genuine_note_conflicts_keep_both_notes_and_replayed_branches_are_idempotent() {
    let db = Database::new("a");
    let first = db.create("root", Some("First note"));
    let mut second = first.clone();
    second.note = Some("Second note".into());
    second.clock = BTreeMap::from([("b".into(), 1)]);
    second.origin_device = "b".into();
    second.updated_at.wall_time_ms += 100;
    db.0.merge_annotations(std::slice::from_ref(&second))
        .unwrap();
    let rows = db.0.annotations_for_book("book").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r.note.as_deref() == Some("First note")));
    assert!(
        rows.iter()
            .any(|r| r.note.as_deref() == Some("Second note"))
    );
    assert_eq!(
        db.0.reading_snapshot("book").unwrap().1.annotations.len(),
        2
    );
    for _ in 0..3 {
        assert_eq!(
            db.0.merge_annotations(&[first.clone(), second.clone()])
                .unwrap(),
            0
        );
        assert_eq!(db.0.annotations_for_book("book").unwrap().len(), 2);
    }
}

#[test]
fn matching_existing_alternatives_are_reused_across_different_conflict_versions() {
    let db = Database::new("a");
    let first = db.create("root", Some("First note"));
    let mut second = first.clone();
    second.note = Some("Second note".into());
    second.clock = BTreeMap::from([("b".into(), 1)]);
    second.updated_at.wall_time_ms += 100;
    db.0.merge_annotations(&[second]).unwrap();
    let mut third = first.clone();
    third.clock = BTreeMap::from([("c".into(), 1)]);
    third.origin_device = "c".into();
    third.updated_at.wall_time_ms -= 1;
    db.0.merge_annotations(&[third]).unwrap();
    assert_eq!(db.0.annotations_for_book("book").unwrap().len(), 2);
    assert_eq!(db.0.compact_annotation_conflicts("book").unwrap(), 0);
}

#[test]
fn historical_compaction_removes_only_identical_related_copies_and_preserves_payloads() {
    let db = Database::new("a");
    let first = db.create("first", None);
    let mut second = db.create("second", Some("Original note"));
    second.ranges[0].start.node = "p2".into();
    second.ranges[0].end.node = "p2".into();
    let mut records = vec![first.clone(), second.clone()];
    for root in [&first, &second] {
        for i in 1..=4 {
            let mut copy = root.clone();
            copy.id = format!("{}~conflict~{i}", root.id);
            copy.conflict_of = Some(root.id.clone());
            copy.clock.insert("peer".into(), i);
            records.push(copy);
        }
    }
    let mut independent = first.clone();
    independent.id = "independent".into();
    let mut distinct_note = first.clone();
    distinct_note.id = "distinct-note".into();
    distinct_note.conflict_of = Some(first.id.clone());
    distinct_note.note = Some("A real alternative".into());
    let mut distinct_range = first.clone();
    distinct_range.id = "distinct-range".into();
    distinct_range.conflict_of = Some(first.id.clone());
    distinct_range.ranges[0].end.text_offset = 10;
    records.extend([independent, distinct_note, distinct_range]);
    db.seed(&records);
    assert_eq!(db.0.compact_annotation_conflicts("book").unwrap(), 8);
    assert_eq!(db.0.annotations_for_book("book").unwrap().len(), 5);
    assert_eq!(db.0.compact_annotation_conflicts("book").unwrap(), 0);
    let snapshot = db.0.reading_snapshot("book").unwrap().1;
    let deleted: Vec<_> = snapshot
        .annotations
        .iter()
        .filter(|r| r.deleted_at.is_some())
        .collect();
    assert_eq!(deleted.len(), 8);
    for copy in deleted {
        let old = records.iter().find(|old| old.id == copy.id).unwrap();
        assert!(same_content(copy, old));
        assert_eq!(copy.created_at, old.created_at);
        assert_eq!(compare_clocks(&copy.clock, &old.clock), ClockOrder::After);
    }
    assert_eq!(
        db.0.annotations_for_book("book")
            .unwrap()
            .iter()
            .filter(|r| r.conflict_of.is_none())
            .count(),
        3
    );
}

#[test]
fn deletion_marks_sync_to_other_devices_and_stale_repairs_cannot_resurrect_copies() {
    let local = Database::new("a");
    let peer = Database::new("b");
    let root = local.create("root", None);
    let mut copy = root.clone();
    copy.id = "copy".into();
    copy.conflict_of = Some(root.id.clone());
    copy.clock.insert("b".into(), 1);
    local.seed(&[copy.clone()]);
    peer.seed(&[root, copy.clone()]);
    assert_eq!(local.0.compact_annotation_conflicts("book").unwrap(), 1);
    let snapshot = local.0.reading_snapshot("book").unwrap().1;
    peer.0.merge_annotations(&snapshot.annotations).unwrap();
    assert_eq!(peer.0.annotations_for_book("book").unwrap().len(), 1);
    copy.clock.insert("b".into(), 10);
    copy.updated_at.wall_time_ms += 100_000;
    local.0.merge_annotations(&[copy.clone()]).unwrap();
    assert_eq!(local.0.annotations_for_book("book").unwrap().len(), 1);
    let snapshot = local.0.reading_snapshot("book").unwrap().1;
    let tombstone = snapshot
        .annotations
        .iter()
        .find(|r| r.id == "copy")
        .unwrap();
    assert!(tombstone.deleted_at.is_some());
    assert_eq!(
        compare_clocks(&tombstone.clock, &copy.clock),
        ClockOrder::After
    );
    peer.0.merge_annotations(&snapshot.annotations).unwrap();
    assert_eq!(peer.0.annotations_for_book("book").unwrap().len(), 1);
}

#[test]
fn identical_concurrent_deletes_do_not_create_an_active_conflict() {
    let db = Database::new("a");
    let mut remote = db.create("root", None);
    db.0.delete_annotation("root").unwrap();
    remote.clock = BTreeMap::from([("b".into(), 20)]);
    remote.updated_at.wall_time_ms += 100_000;
    db.0.merge_annotations(&[remote]).unwrap();
    assert!(db.0.annotations_for_book("book").unwrap().is_empty());
    assert_eq!(
        db.0.reading_snapshot("book").unwrap().1.annotations.len(),
        1
    );
}

#[test]
fn an_annotation_id_cannot_replace_annotations_in_another_book() {
    let db = Database::new("a");
    let saved = db.create("root", None);
    let mut other = saved.clone();
    other.book_id = "other-book".into();
    other.clock.insert("peer".into(), 1);
    assert!(db.0.merge_annotations(&[other]).is_err());
    assert_eq!(db.0.annotations_for_book("book").unwrap(), vec![saved]);
    assert!(db.0.annotations_for_book("other-book").unwrap().is_empty());
}
