use std::sync::mpsc;

use rebook_publication::{
    LocatorV1, PublicationId, PublicationUrl, SourceAnchor, SourceRange, SpineItemId,
};

use super::*;

fn store() -> SyncStore {
    SyncStore::open_at(
        std::env::temp_dir().join(format!("torto-activity-{}.sqlite3", uuid::Uuid::new_v4())),
        "activity-test",
    )
    .unwrap()
}

fn locator(node: &str) -> LocatorV1 {
    let mut locator = LocatorV1::at_start(
        PublicationId::new("book").unwrap(),
        PublicationUrl::parse("chapter.xhtml").unwrap(),
    );
    let anchor = SourceAnchor {
        spine: SpineItemId::new("chapter").unwrap(),
        node: node.into(),
        text_offset: 12,
    };
    locator.source = Some(SourceRange {
        start: anchor.clone(),
        end: anchor,
    });
    locator
}

fn cleanup(store: &SyncStore) {
    let _ = std::fs::remove_file(store.path());
    let _ = std::fs::remove_file(store.path().with_extension("sqlite3-wal"));
    let _ = std::fs::remove_file(store.path().with_extension("sqlite3-shm"));
}

#[test]
fn opening_updates_order_before_the_database_and_stale_snapshots_cannot_undo_it() {
    let store = store();
    let mut activity = ReadingActivity::default();
    activity.apply_snapshot(HashMap::from([("previous".into(), 200)]));
    activity.record(store.clone(), "book".into(), locator("p1"), 300);
    assert_eq!(activity.times()["book"], 300);
    assert!(
        store.load_progress("book").unwrap().is_none(),
        "recording on the UI must not write SQLite"
    );
    activity.apply_snapshot(HashMap::from([("previous".into(), 200)]));
    assert_eq!(activity.times()["book"], 300);
    assert!(activity.optimistic.contains_key("book"));
    activity.apply_snapshot(HashMap::from([("book".into(), 350)]));
    assert_eq!(activity.times()["book"], 350);
    assert!(activity.optimistic.is_empty());
    cleanup(&store);
}

#[test]
fn pending_activity_is_coalesced_in_opening_order_and_removed_books_are_not_retained() {
    let store = store();
    let mut activity = ReadingActivity::default();
    for book in ["a", "b", "a"] {
        activity.record(store.clone(), book.into(), locator("p1"), 300);
    }
    assert_eq!(
        activity
            .pending
            .iter()
            .map(|request| request.book_id.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "a"]
    );
    activity.remove("a");
    assert!(!activity.times().contains_key("a"));
    assert!(!activity.optimistic.contains_key("a"));
    assert_eq!(activity.pending.len(), 1);
    activity.invalidate();
    assert!(!activity.is_pending());
    assert!(!activity.is_ready());
    assert!(activity.times().is_empty());
    cleanup(&store);
}

#[test]
fn busy_activity_write_keeps_ui_polling_free_and_preserves_newer_progress() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let store = store();
    let mut activity = ReadingActivity::default();
    activity.record(store.clone(), "book".into(), locator("old"), 300);
    let newer = locator("precise-new-position");
    store.save_progress("book", &newer).unwrap();
    let mut connection = rusqlite::Connection::open(store.path()).unwrap();
    let lock = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let (woke, wake) = mpsc::channel();
    activity.spawn(&runtime, move || {
        woke.send(()).unwrap();
    });
    for _ in 0..100 {
        assert!(!activity.poll());
    }
    assert!(activity.is_pending());
    assert!(matches!(wake.try_recv(), Err(mpsc::TryRecvError::Empty)));
    lock.commit().unwrap();
    wake.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(activity.poll());
    assert!(!activity.is_pending());
    assert_eq!(store.load_progress("book").unwrap().unwrap().locator, newer);
    activity.apply_snapshot(store.progress_activity_times().unwrap());
    assert!(activity.optimistic.is_empty());
    drop(connection);
    cleanup(&store);
}

#[test]
fn changing_context_discards_old_worker_notifications() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let store = store();
    let mut activity = ReadingActivity::default();
    activity.record(store.clone(), "book".into(), locator("p1"), 300);
    let (woke, wake) = mpsc::channel();
    activity.spawn(&runtime, move || {
        woke.send(()).unwrap();
    });
    activity.invalidate();
    wake.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(!activity.poll());
    assert!(!activity.is_ready());
    assert!(activity.times().is_empty());
    assert!(activity.optimistic.is_empty());
    cleanup(&store);
}
