//! Resolve annotation versions without duplicating identical user content.
use super::*;
use std::collections::BTreeSet;

fn same_content(left: &AnnotationState, right: &AnnotationState) -> bool {
    left.book_id == right.book_id
        && left.ranges == right.ranges
        && left.quote == right.quote
        && left.note == right.note
}

fn join_clock(target: &mut AnnotationState, other: &AnnotationState) -> bool {
    let mut changed = false;
    for (device, value) in &other.clock {
        let current = target.clock.entry(device.clone()).or_default();
        if *current < *value {
            *current = *value;
            changed = true;
        }
    }
    changed
}

fn advance_local(
    transaction: &Transaction<'_>,
    device: &str,
    annotation: &mut AnnotationState,
) -> SyncResult<()> {
    annotation.updated_at = tick(
        transaction,
        device,
        Some(annotation.updated_at.wall_time_ms),
    )?;
    let counter = annotation.clock.entry(device.to_owned()).or_default();
    *counter = counter.saturating_add(1);
    annotation.origin_device = device.to_owned();
    Ok(())
}

fn publish(
    transaction: &Transaction<'_>,
    device: &str,
    annotation: &mut AnnotationState,
) -> SyncResult<()> {
    // Joining histories is not a new edit. Incrementing a device's clock here
    // would make simultaneous, identical merges diverge again on every sync.
    annotation.origin_device = device.to_owned();
    write_annotation(transaction, annotation)?;
    mark_reading_changed(transaction, device, &annotation.book_id)
}

pub(super) fn merge(
    transaction: &Transaction<'_>,
    device: &str,
    incoming: &[AnnotationState],
) -> SyncResult<usize> {
    let mut changed = 0;
    let mut books = BTreeSet::new();
    for incoming in incoming {
        incoming_ranges_are_valid(incoming)?;
        tick(transaction, device, Some(incoming.updated_at.wall_time_ms))?;
        books.insert(incoming.book_id.clone());
        let Some(current) = read_annotation(transaction, &incoming.id)? else {
            write_annotation(transaction, incoming)?;
            changed += 1;
            continue;
        };
        if current.book_id != incoming.book_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Annotation ID belongs to a different book",
            )
            .into());
        }
        match compare_clocks(&current.clock, &incoming.clock) {
            ClockOrder::Before => {
                write_annotation(transaction, incoming)?;
                changed += 1;
            }
            ClockOrder::After | ClockOrder::Equal => {}
            ClockOrder::Concurrent => {
                let equal_content = same_content(&current, incoming);
                let (winner, loser) = if equal_content && current.deleted_at.is_some() {
                    (&current, incoming)
                } else if equal_content && incoming.deleted_at.is_some()
                    || incoming.updated_at > current.updated_at
                {
                    (incoming, &current)
                } else {
                    (&current, incoming)
                };
                if !equal_content && loser.deleted_at.is_none() && loser.conflict_of.is_none() {
                    retain_conflict(transaction, device, loser, &incoming.id)?;
                }
                let mut resolved = winner.clone();
                // A resolved version observes both histories, so re-fetching an
                // older branch cannot generate the same conflict again.
                join_clock(&mut resolved, loser);
                publish(transaction, device, &mut resolved)?;
                changed += 1;
            }
        }
    }
    for book in books {
        changed += compact(transaction, device, &book)?;
    }
    Ok(changed)
}

fn retain_conflict(
    transaction: &Transaction<'_>,
    device: &str,
    loser: &AnnotationState,
    parent: &str,
) -> SyncResult<()> {
    let members = {
        let mut query = transaction.prepare(
            "SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json,
             deleted_hlc, origin_device, conflict_of FROM annotations WHERE book_id = ?1 AND conflict_of = ?2 ORDER BY id",
        )?;
        query
            .query_map(params![loser.book_id, parent], read_annotation_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    if let Some(mut existing) = members.into_iter().find(|value| same_content(value, loser)) {
        // Reuse an existing alternative, including a tombstone for an
        // alternative the user already discarded. Never multiply its IDs.
        if join_clock(&mut existing, loser) {
            publish(transaction, device, &mut existing)?;
        }
        return Ok(());
    }
    let mut copy = loser.clone();
    copy.id = conflict_id(loser);
    copy.conflict_of = Some(parent.to_owned());
    if read_annotation(transaction, &copy.id)?.is_none() {
        publish(transaction, device, &mut copy)?;
    }
    Ok(())
}

pub(super) fn compact(
    transaction: &Transaction<'_>,
    device: &str,
    book: &str,
) -> SyncResult<usize> {
    let members = {
        let mut query = transaction.prepare(
            "SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json,
             deleted_hlc, origin_device, conflict_of FROM annotations
             WHERE book_id = ?1 AND deleted_hlc IS NULL ORDER BY conflict_of IS NOT NULL, id",
        )?;
        query
            .query_map([book], read_annotation_row)?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut kept: HashMap<(String, String), AnnotationState> = HashMap::new();
    let mut removed = 0;
    for mut annotation in members {
        // Independent user-created highlights are not duplicates. Only copies
        // with a shared conflict parent may be compacted.
        let family = annotation
            .conflict_of
            .as_ref()
            .unwrap_or(&annotation.id)
            .clone();
        let content =
            serde_json::to_string(&(&annotation.ranges, &annotation.quote, &annotation.note))?;
        let key = (family, content);
        if let Some(survivor) = kept.get_mut(&key) {
            if join_clock(survivor, &annotation) {
                publish(transaction, device, survivor)?;
            }
            join_clock(&mut annotation, survivor);
            advance_local(transaction, device, &mut annotation)?;
            annotation.deleted_at = Some(annotation.updated_at.clone());
            write_annotation(transaction, &annotation)?;
            removed += 1;
        } else {
            kept.insert(key, annotation);
        }
    }
    if removed > 0 {
        mark_reading_changed(transaction, device, book)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests;
