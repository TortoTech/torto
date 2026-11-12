//! Commit local range unions and deletion tombstones as a single sync revision.
use super::*;

pub(super) fn apply(
    store: &SyncStore,
    book: &str,
    expected: &[StoredHighlight],
    removed: &[String],
    replacement: Option<&StoredHighlight>,
) -> HighlightResult<bool> {
    let mut connection = store.connection()?;
    let transaction = write_transaction(&mut connection)?;
    let mut rows = {
        let mut query = transaction.prepare("SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json, deleted_hlc, origin_device, conflict_of FROM annotations WHERE book_id=?1 AND deleted_hlc IS NULL ORDER BY id")?;
        query
            .query_map([book], read_annotation_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    let current = rows
        .iter()
        .map(|r| StoredHighlight {
            id: r.id.clone(),
            book_id: r.book_id.clone(),
            ranges: r.ranges.clone(),
            quote: r.quote.clone(),
            note: r.note.clone(),
            created_at: r.created_at,
        })
        .collect::<Vec<_>>();
    let mut expected = expected.to_vec();
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    if current != expected {
        return Ok(false);
    }
    if replacement.is_some_and(|r| r.book_id != book) {
        return Err(io::Error::other("Highlight belongs to another book").into());
    }
    let mut changed = false;
    for row in &mut rows {
        if replacement.is_some_and(|r| r.id == row.id) {
            continue;
        }
        if !removed.contains(&row.id) {
            continue;
        }
        row.updated_at = tick(&transaction, &store.device_id, None)?;
        let counter = row.clock.entry(store.device_id.clone()).or_default();
        *counter = counter.saturating_add(1);
        row.deleted_at = Some(row.updated_at.clone());
        row.origin_device.clone_from(&store.device_id);
        write_annotation(&transaction, row)?;
        changed = true;
    }
    if let Some(value) = replacement {
        let existing = rows.iter().find(|r| r.id == value.id);
        if existing.is_none_or(|r| {
            r.ranges != value.ranges || r.quote != value.quote || r.note != value.note
        }) {
            let updated = tick(&transaction, &store.device_id, None)?;
            let mut row = existing.cloned().unwrap_or(AnnotationState {
                id: value.id.clone(),
                book_id: book.to_owned(),
                ranges: vec![],
                quote: String::new(),
                note: None,
                created_at: value.created_at,
                updated_at: updated.clone(),
                clock: Default::default(),
                deleted_at: None,
                origin_device: store.device_id.clone(),
                conflict_of: None,
            });
            row.ranges.clone_from(&value.ranges);
            row.quote.clone_from(&value.quote);
            row.note.clone_from(&value.note);
            row.updated_at = updated;
            let counter = row.clock.entry(store.device_id.clone()).or_default();
            *counter = counter.saturating_add(1);
            row.origin_device.clone_from(&store.device_id);
            write_annotation(&transaction, &row)?;
            changed = true;
        }
    }
    if changed {
        mark_annotation_changed(&transaction, &store.device_id, book)?;
    }
    transaction.commit()?;
    if changed {
        store.wake.changed(super::super::SyncChange::Annotations);
    }
    Ok(true)
}
