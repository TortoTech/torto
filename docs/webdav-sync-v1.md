# Torto Desktop WebDAV Sync v1

This protocol is currently desktop-to-desktop. It does not depend on `rebook-service` and is not yet a compatibility contract with `rebook-web`. The historical `Rebook/v1/` remote path is intentionally retained so existing Torto users do not lose access to synchronized data after the product rename.

## Remote layout

```text
Rebook/v1/
├── protocol.json
├── library/devices/<device-id>.json
├── books/<sha256>/manifest.json
├── books/<sha256>/content.<extension>
├── books/<sha256>/cover.bin
├── state/<sha256>/devices/<device-id>.json
└── tmp/
```

`<sha256>` is the lowercase SHA-256 of the exact imported book bytes. A title, filename, author, or format metadata value is never used to match books.

## Ownership and writes

- Book content and manifests are immutable and uploaded with `If-None-Match: *`.
- A device only updates its own files under `library/devices/` and `state/.../devices/`.
- Mutable device files use the last ETag with `If-Match`; creation uses `If-None-Match: *`.
- Redirects to a different origin are rejected so credentials are not forwarded.
- Non-local endpoints must use HTTPS.

## Merge rules

- Reading progress stores a versioned `LocatorV1` and a hybrid logical timestamp. The newest reading event wins; progress is never merged by taking the maximum percentage.
- Highlights have UUID identity, a vector clock, an HLC update timestamp, and an optional deletion timestamp.
- A causally newer highlight replaces an older one. Concurrent versions choose a deterministic HLC winner and retain the visible loser as a conflict copy.
- Highlight deletions remain as tombstones and participate in causal merge.
- Removing a book from one local shelf records a device-local membership tombstone, preventing the same remote book from immediately reappearing on that device. Global remote deletion is intentionally not part of v1.

## Local persistence

Progress, annotations, HLC state, and local book membership are stored transactionally in `sync-v1.sqlite3` under the application data directory. Legacy `highlights.json` records are imported idempotently. The WebDAV password is stored in Windows Credential Manager under the `Rebook WebDAV` service name; it is not serialized with settings.

Local database handles share an in-process access coordinator by canonical file
path, including handles opened independently by reading and synchronization.
The coordinator covers connection open through close, so local writers do not
compete for SQLite's busy timeout and a final WAL checkpoint cannot race another
local open. Different database files remain independent. The registry retains
only weak references and removes expired entries. SQLite's five-second timeout
still applies to competing processes; SQL failures continue to propagate.
Read-then-write operations retain immediate transactions. Registering a missing
book and advancing its HLC use one transaction without reopening the same store.

Regression checks include 100 repetitions of four concurrent workers saving
progress, taking upload snapshots, editing notes and merging remote progress
(40,000 operations), plus 20 delayed-writer repetitions. The delayed-writer check
reproduces `SQLITE_BUSY` on an uncoordinated connection with a 20 ms wait budget;
an independently opened local handle succeeds after the first writer finishes.
Final locators, notes and synchronization revisions are verified. These runs use
temporary databases on macOS; Windows CI has not yet been rerun for this fix.
