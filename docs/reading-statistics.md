# Reading statistics

The shelf's Statistics action opens the overview. Each book's context menu also
opens Reading details, where its status and completion date can be edited.

## Counting rules

- The reader counts foreground reading, with a configurable 2/5/10 minute idle
  limit (default 5). Keyboard, click and scroll events renew activity; pointer
  motion alone does not. Settings/modal interactions and typing in the assistant
  pause counting. A frame gap over five seconds is not charged (suspension).
- Time is checkpointed every fifteen seconds on a dedicated database writer.
  Pausing or closing the reader checkpoints the remainder. An abrupt crash can
  lose up to the current checkpoint interval.
- Sessions under thirty seconds retain time, but do not establish a reading
  start date or count as a reading day. Rereading keeps accumulating time.
- Completion is explicit, never inferred from a percentage alone. Reopening a
  finished book does not undo completion. Completion dates can be backfilled.
- Percentages are current reading positions, not measured coverage. Existing
  progress and added dates are reused; historical durations are not fabricated.
- Original, translated and OCR views share the source book identity. Different
  files are separate books even when their titles match.
- Per-book durations and personal durations union overlapping intervals. The
  personal total can therefore be lower than the sum of per-book durations.
- Intervals store the local UTC offset and are split at local midnight. For
  overlapping intervals with different offsets, the first interval in stable
  chronological order owns the overlap's day allocation.
- Removing a book keeps history. Clear statistics is a separate confirmed
  operation. It leaves the book, resume position and annotations intact.

## Persistence and synchronization

`reading-statistics-v1.sqlite3` lives beside the existing local reading database.
Its append-only events contain book metadata, reading intervals, status edits
and clear markers. The database uses WAL, a single background writer and UUID
event identities. UI summaries are derived from the events.

The existing WebDAV sync additionally merges `statistics/<device>-<YYYY-MM>.json`
shards. Each shard has its own schema version. Remote events are merged before
publishing local device shards, including restoring that device's own history.
Repeated downloads use INSERT OR IGNORE by event ID. Status edits resolve by
timestamp and event ID; clear markers suppress older events without deleting
the synchronization evidence. Metadata survives a clear marker.

Only the current device's shards are published. Completed months are currently
republished along with the current month; remote ETag-based download and upload
skipping can be added if histories grow large. Statistics are local until the
user enables/runs the existing cloud sync.

## Limits

Foreground/idle time estimates engagement, not eye attention. Historical start
dates before this feature are unknown. Page counts, speed estimates, reading
goals and explicit rereading cycles are not part of this version. Overview and
book detail aggregate on entry/refresh rather than querying on each frame.
