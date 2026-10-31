use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rebook_publication::{LocatorV1, SourceRange};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::highlights::{HighlightRepository, HighlightResult, StoredHighlight};

use super::SyncResult;
use super::protocol::{
    AnnotationState, ClockOrder, DeviceBookEntry, DeviceBookState, HybridTimestamp,
    PROTOCOL_VERSION, ProgressState, compare_clocks,
};

mod annotation_merge;

const DATABASE_FILE: &str = "sync-v1.sqlite3";
const DATABASE_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug)]
pub struct SyncStore {
    path: PathBuf,
    device_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredProgress {
    pub locator: LocatorV1,
    pub updated_at: HybridTimestamp,
}

impl SyncStore {
    pub fn open_default(device_id: impl Into<String>) -> SyncResult<Self> {
        let project = crate::environment::paths()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定同步数据库目录"))?;
        Self::open_at(project.data_local_dir().join(DATABASE_FILE), device_id)
    }

    pub fn open_at(path: PathBuf, device_id: impl Into<String>) -> SyncResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = Self {
            path,
            device_id: device_id.into(),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn create_annotation(
        &self,
        id: String,
        book_id: String,
        ranges: Vec<SourceRange>,
        quote: String,
        note: Option<String>,
        created_at: u64,
    ) -> SyncResult<AnnotationState> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let updated_at = tick(&transaction, &self.device_id, None)?;
        let annotation = AnnotationState {
            id,
            book_id,
            ranges,
            quote,
            note,
            created_at,
            updated_at,
            clock: BTreeMap::from([(self.device_id.clone(), 1)]),
            deleted_at: None,
            origin_device: self.device_id.clone(),
            conflict_of: None,
        };
        write_annotation(&transaction, &annotation)?;
        mark_reading_changed(&transaction, &self.device_id, &annotation.book_id)?;
        transaction.commit()?;
        Ok(annotation)
    }

    pub fn annotations_for_book(&self, book_id: &str) -> SyncResult<Vec<AnnotationState>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json, \
             deleted_hlc, origin_device, conflict_of FROM annotations \
             WHERE book_id = ?1 AND deleted_hlc IS NULL ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([book_id], read_annotation_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[cfg(test)]
    pub fn annotations_for_device_book(&self, book_id: &str) -> SyncResult<Vec<AnnotationState>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json, \
             deleted_hlc, origin_device, conflict_of FROM annotations \
             WHERE book_id = ?1 AND origin_device = ?2 ORDER BY id",
        )?;
        let rows = statement.query_map(params![book_id, self.device_id], read_annotation_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_annotation(&self, id: &str) -> SyncResult<bool> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let Some(mut annotation) = read_annotation(&transaction, id)? else {
            return Ok(false);
        };
        if annotation.deleted_at.is_some() {
            return Ok(false);
        }
        let updated_at = tick(&transaction, &self.device_id, None)?;
        let counter = annotation.clock.entry(self.device_id.clone()).or_default();
        *counter = counter.saturating_add(1);
        annotation.updated_at = updated_at.clone();
        annotation.deleted_at = Some(updated_at);
        annotation.origin_device.clone_from(&self.device_id);
        write_annotation(&transaction, &annotation)?;
        mark_reading_changed(&transaction, &self.device_id, &annotation.book_id)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn update_annotation_note(&self, id: &str, note: Option<String>) -> SyncResult<bool> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let Some(mut annotation) = read_annotation(&transaction, id)? else {
            return Ok(false);
        };
        if annotation.deleted_at.is_some() {
            return Ok(false);
        }
        if annotation.note == note {
            return Ok(false);
        }
        let updated_at = tick(&transaction, &self.device_id, None)?;
        let counter = annotation.clock.entry(self.device_id.clone()).or_default();
        *counter = counter.saturating_add(1);
        annotation.note = note;
        annotation.updated_at = updated_at;
        annotation.origin_device.clone_from(&self.device_id);
        write_annotation(&transaction, &annotation)?;
        mark_reading_changed(&transaction, &self.device_id, &annotation.book_id)?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn merge_annotations(&self, annotations: &[AnnotationState]) -> SyncResult<usize> {
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = annotation_merge::merge(&transaction, &self.device_id, annotations)?;
        transaction.commit()?;
        Ok(changed)
    }

    fn compact_annotation_conflicts(&self, book_id: &str) -> SyncResult<usize> {
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let removed = annotation_merge::compact(&transaction, &self.device_id, book_id)?;
        transaction.commit()?;
        Ok(removed)
    }

    pub fn save_progress(&self, book_id: &str, locator: &LocatorV1) -> SyncResult<()> {
        self.write_progress(book_id, locator, false)
    }

    /// Refreshes activity without replacing an existing precise locator. The
    /// supplied locator is only used for a book with no progress yet.
    pub fn record_reading_activity(&self, book_id: &str, locator: &LocatorV1) -> SyncResult<()> {
        self.write_progress(book_id, locator, true)
    }

    fn write_progress(
        &self,
        book_id: &str,
        locator: &LocatorV1,
        refresh_activity: bool,
    ) -> SyncResult<()> {
        locator.validate()?;
        let mut connection = self.connection()?;
        let transaction = if refresh_activity {
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?
        } else {
            connection.transaction()?
        };
        if !refresh_activity {
            let current: Option<String> = transaction
                .query_row(
                    "SELECT locator_json FROM progress WHERE book_id = ?1",
                    [book_id],
                    |row| row.get(0),
                )
                .optional()?;
            if current
                .as_deref()
                .and_then(|json| serde_json::from_str::<LocatorV1>(json).ok())
                .is_some_and(|current| same_progress_location(&current, locator))
            {
                return Ok(());
            }
        }
        let locator_json = serde_json::to_string(locator)?;
        let updated_at = tick(&transaction, &self.device_id, None)?;
        let sql = if refresh_activity {
            "INSERT INTO progress(book_id, locator_json, updated_hlc) VALUES (?1, ?2, ?3) \
             ON CONFLICT(book_id) DO UPDATE SET updated_hlc = excluded.updated_hlc"
        } else {
            "INSERT INTO progress(book_id, locator_json, updated_hlc) VALUES (?1, ?2, ?3) \
             ON CONFLICT(book_id) DO UPDATE SET locator_json = excluded.locator_json, \
             updated_hlc = excluded.updated_hlc"
        };
        transaction.execute(
            sql,
            params![book_id, locator_json, serde_json::to_string(&updated_at)?],
        )?;
        mark_reading_changed(&transaction, &self.device_id, book_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn load_progress(&self, book_id: &str) -> SyncResult<Option<StoredProgress>> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT locator_json, updated_hlc FROM progress WHERE book_id = ?1",
                [book_id],
                |row| {
                    let locator: String = row.get(0)?;
                    let updated_at: String = row.get(1)?;
                    Ok((locator, updated_at))
                },
            )
            .optional()?
            .map(|(locator, updated_at)| {
                Ok(StoredProgress {
                    locator: serde_json::from_str(&locator)?,
                    updated_at: HybridTimestamp::parse(&updated_at)?,
                })
            })
            .transpose()
    }

    pub fn progress_activity_times(&self) -> SyncResult<HashMap<String, u64>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT book_id, updated_hlc FROM progress")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut activity_times = HashMap::new();
        for row in rows {
            let (book_id, updated_at) = row?;
            // One unreadable row must not hide every other book's activity:
            // a failure here aborts the whole shelf snapshot upstream.
            match HybridTimestamp::parse(&updated_at) {
                Ok(timestamp) => {
                    activity_times.insert(book_id, timestamp.wall_time_ms);
                }
                Err(error) => {
                    tracing::warn!(%error, %book_id, "skipping unreadable progress activity row");
                }
            }
        }
        Ok(activity_times)
    }

    pub fn merge_progress(&self, progress: &ProgressState) -> SyncResult<bool> {
        progress.locator.validate()?;
        let book_id = progress.locator.publication_id.as_str();
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        tick(
            &transaction,
            &self.device_id,
            Some(progress.updated_at.wall_time_ms),
        )?;
        let current = transaction
            .query_row(
                "SELECT updated_hlc FROM progress WHERE book_id = ?1",
                [book_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            // An unreadable current timestamp compares as absent, so the
            // incoming value overwrites the row and heals it.
            .map(|value| HybridTimestamp::parse(&value).ok())
            .flatten();
        if current
            .as_ref()
            .is_some_and(|value| value >= &progress.updated_at)
        {
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO progress(book_id, locator_json, updated_hlc) VALUES (?1, ?2, ?3) \
             ON CONFLICT(book_id) DO UPDATE SET locator_json = excluded.locator_json, \
             updated_hlc = excluded.updated_hlc",
            params![
                book_id,
                serde_json::to_string(&progress.locator)?,
                serde_json::to_string(&progress.updated_at)?
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn set_book_present(&self, book_id: &str, present: bool) -> SyncResult<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let current = transaction
            .query_row(
                "SELECT present FROM book_membership WHERE book_id = ?1",
                [book_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?;
        if current == Some(present) {
            transaction.commit()?;
            return Ok(());
        }
        let changed_at = tick(&transaction, &self.device_id, None)?;
        transaction.execute(
            "INSERT INTO book_membership(book_id, present, changed_hlc) VALUES (?1, ?2, ?3) \
             ON CONFLICT(book_id) DO UPDATE SET present = excluded.present, \
             changed_hlc = excluded.changed_hlc",
            params![book_id, present, serde_json::to_string(&changed_at)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn membership_entries(
        &self,
        local_book_ids: &[String],
    ) -> SyncResult<Vec<DeviceBookEntry>> {
        for book_id in local_book_ids {
            self.register_book_if_missing(book_id)?;
        }
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT book_id, present, changed_hlc FROM book_membership ORDER BY book_id",
        )?;
        let rows = statement.query_map([], |row| {
            let changed_at: String = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, changed_at))
        })?;
        rows.map(|row| {
            let (book_id, present, changed_at) = row?;
            Ok(DeviceBookEntry {
                book_id,
                present,
                changed_at: HybridTimestamp::parse(&changed_at)?,
            })
        })
        .filter(|entry: &SyncResult<DeviceBookEntry>| {
            entry.as_ref().map_or(true, |entry| {
                !entry.present || local_book_ids.contains(&entry.book_id)
            })
        })
        .collect()
    }

    pub fn is_locally_removed(&self, book_id: &str) -> SyncResult<bool> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT present FROM book_membership WHERE book_id = ?1",
                [book_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            == Some(false))
    }

    pub fn register_book_if_missing(&self, book_id: &str) -> SyncResult<()> {
        let connection = self.connection()?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM book_membership WHERE book_id = ?1)",
            [book_id],
            |row| row.get(0),
        )?;
        if !exists {
            let timestamp = self.tick()?;
            // A removal committed after the existence check must win over this old snapshot.
            connection.execute("INSERT OR IGNORE INTO book_membership(book_id,present,changed_hlc) VALUES (?1,1,?2)",
                params![book_id, serde_json::to_string(&timestamp)?])?;
        }
        Ok(())
    }

    pub fn tick(&self) -> SyncResult<HybridTimestamp> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let timestamp = tick(&transaction, &self.device_id, None)?;
        transaction.commit()?;
        Ok(timestamp)
    }

    pub fn pending_reading(&self, account: &str) -> SyncResult<Vec<(String, u64, u64)>> {
        let connection = self.connection()?;
        let mut query = connection.prepare(
            "SELECT c.book_id, c.revision, c.changed_ms FROM reading_changes c
             LEFT JOIN reading_ack a ON a.account = ?1 AND a.book_id = c.book_id
             WHERE c.device_id = ?2 AND c.revision > COALESCE(a.revision, 0)
             ORDER BY c.changed_ms, c.book_id",
        )?;
        Ok(query
            .query_map(params![account, self.device_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn acknowledge_reading(
        &self,
        account: &str,
        book_id: &str,
        revision: u64,
    ) -> SyncResult<()> {
        self.connection()?.execute(
            "INSERT INTO reading_ack(account, book_id, revision) VALUES (?1, ?2, ?3)
             ON CONFLICT(account, book_id) DO UPDATE SET revision = MAX(revision, excluded.revision)",
            params![account, book_id, i64::try_from(revision)?])?;
        Ok(())
    }

    pub fn reading_snapshot(&self, book_id: &str) -> SyncResult<(u64, DeviceBookState)> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let revision = transaction
            .query_row(
                "SELECT revision FROM reading_changes WHERE device_id = ?1 AND book_id = ?2",
                params![self.device_id, book_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0)
            .max(0) as u64;
        let progress = transaction
            .query_row(
                "SELECT locator_json, updated_hlc FROM progress WHERE book_id = ?1",
                [book_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(locator, timestamp)| -> SyncResult<ProgressState> {
                Ok(ProgressState {
                    locator: serde_json::from_str(&locator)?,
                    updated_at: serde_json::from_str(&timestamp)?,
                })
            })
            .transpose()?
            .filter(|value| value.updated_at.device_id == self.device_id);
        let annotations = {
            let mut query = transaction.prepare(
                "SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json,
                 deleted_hlc, origin_device, conflict_of FROM annotations
                 WHERE book_id = ?1 AND origin_device = ?2 ORDER BY id",
            )?;
            query
                .query_map(params![book_id, self.device_id], read_annotation_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let state = DeviceBookState {
            version: PROTOCOL_VERSION,
            device_id: self.device_id.clone(),
            book_id: book_id.to_owned(),
            updated_at: tick(&transaction, &self.device_id, None)?,
            progress,
            annotations,
        };
        transaction.commit()?;
        Ok((revision, state))
    }

    pub fn cache_get(&self, account: &str, key: &str) -> SyncResult<Option<Vec<u8>>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT value FROM transfer_cache WHERE account = ?1 AND key = ?2",
                params![account, key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn cache_set(&self, account: &str, key: &str, value: &[u8]) -> SyncResult<()> {
        self.connection()?.execute(
            "INSERT INTO transfer_cache(account,key,value) VALUES (?1,?2,?3)
            ON CONFLICT(account,key) DO UPDATE SET value = excluded.value",
            params![account, key, value],
        )?;
        Ok(())
    }

    pub fn cache_remove(&self, account: &str, key: &str) -> SyncResult<()> {
        self.connection()?.execute(
            "DELETE FROM transfer_cache WHERE account = ?1 AND key = ?2",
            params![account, key],
        )?;
        Ok(())
    }

    fn initialize(&self) -> SyncResult<()> {
        let connection = self.connection()?;
        let had_change_table: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'reading_changes')",
            [], |row| row.get(0))?;
        let schema_version =
            connection.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))?;
        if schema_version != DATABASE_SCHEMA_VERSION {
            connection.execute_batch(
                "DROP TABLE IF EXISTS sync_meta;
                 DROP TABLE IF EXISTS progress;
                 DROP TABLE IF EXISTS annotations;
                 DROP TABLE IF EXISTS book_membership;",
            )?;
        }
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS sync_meta(
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS progress(
                book_id TEXT PRIMARY KEY,
                locator_json TEXT NOT NULL,
                updated_hlc TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS annotations(
                id TEXT PRIMARY KEY,
                book_id TEXT NOT NULL,
                ranges_json TEXT NOT NULL,
                quote TEXT NOT NULL,
                note TEXT,
                created_at INTEGER NOT NULL,
                updated_hlc TEXT NOT NULL,
                clock_json TEXT NOT NULL,
                deleted_hlc TEXT,
                origin_device TEXT NOT NULL,
                conflict_of TEXT
             );
             CREATE TABLE IF NOT EXISTS annotation_range_checks(
                annotation_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS annotations_book_id ON annotations(book_id);
             CREATE INDEX IF NOT EXISTS annotations_origin ON annotations(origin_device, book_id);
             CREATE TABLE IF NOT EXISTS book_membership(
                book_id TEXT PRIMARY KEY,
                present INTEGER NOT NULL,
                changed_hlc TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS reading_changes(
                device_id TEXT NOT NULL, book_id TEXT NOT NULL, revision INTEGER NOT NULL,
                changed_ms INTEGER NOT NULL, PRIMARY KEY(device_id, book_id));
             CREATE TABLE IF NOT EXISTS reading_ack(
                account TEXT NOT NULL, book_id TEXT NOT NULL, revision INTEGER NOT NULL,
                PRIMARY KEY(account, book_id));
             CREATE TABLE IF NOT EXISTS transfer_cache(
                account TEXT NOT NULL, key TEXT NOT NULL, value BLOB NOT NULL,
                PRIMARY KEY(account, key));",
        )?;
        // Additive migration: retain existing progress and annotations, and publish them once per account.
        let seed_key = format!("reading-change-seed:{}", self.device_id);
        let seeded: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_meta WHERE key = ?1)",
            [&seed_key],
            |row| row.get(0),
        )?;
        if !had_change_table || !seeded {
            connection.execute("INSERT OR IGNORE INTO reading_changes(device_id,book_id,revision,changed_ms)
            SELECT ?1, book_id, 1, 0 FROM progress WHERE json_extract(updated_hlc, '$.device_id') = ?1
            UNION SELECT ?1, book_id, 1, 0 FROM annotations WHERE origin_device = ?1", [&self.device_id])?;
            connection.execute(
                "INSERT OR REPLACE INTO sync_meta(key,value) VALUES (?1,'1')",
                [&seed_key],
            )?;
        }
        connection.pragma_update(None, "user_version", DATABASE_SCHEMA_VERSION)?;
        Ok(())
    }

    fn connection(&self) -> SyncResult<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }
}

fn mark_reading_changed(
    transaction: &Transaction<'_>,
    device: &str,
    book_id: &str,
) -> SyncResult<()> {
    transaction.execute("INSERT INTO reading_changes(device_id,book_id,revision,changed_ms) VALUES (?1,?2,1,?3)
        ON CONFLICT(device_id,book_id) DO UPDATE SET revision = revision + 1, changed_ms = excluded.changed_ms",
        params![device, book_id, i64::try_from(unix_timestamp_millis())?])?;
    Ok(())
}

impl HighlightRepository for SyncStore {
    fn compact_conflicts(&self, book_id: &str) -> HighlightResult<usize> {
        self.compact_annotation_conflicts(book_id)
    }

    fn highlights_for_book(&self, book_id: &str) -> HighlightResult<Vec<StoredHighlight>> {
        self.annotations_for_book(book_id).map(|annotations| {
            annotations
                .into_iter()
                .map(|annotation| StoredHighlight {
                    id: annotation.id,
                    book_id: annotation.book_id,
                    ranges: annotation.ranges,
                    quote: annotation.quote,
                    note: annotation.note,
                    created_at: annotation.created_at,
                })
                .collect()
        })
    }

    fn insert_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<()> {
        self.create_annotation(
            highlight.id.clone(),
            highlight.book_id.clone(),
            highlight.ranges.clone(),
            highlight.quote.clone(),
            highlight.note.clone(),
            highlight.created_at,
        )?;
        Ok(())
    }

    fn update_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<bool> {
        self.update_annotation_note(&highlight.id, highlight.note.clone())
    }

    fn range_checks(&self, book_id: &str) -> HighlightResult<HashMap<String, String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT c.annotation_id, c.fingerprint FROM annotation_range_checks c
             JOIN annotations a ON a.id = c.annotation_id
             WHERE a.book_id = ?1 AND a.deleted_hlc IS NULL",
        )?;
        let rows = statement.query_map([book_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn save_range_checks(&self, checks: &[(StoredHighlight, String)]) -> HighlightResult<()> {
        if checks.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for (expected, fingerprint) in checks {
            // A sync or edit may have replaced the annotation since it was read.
            transaction.execute(
                "INSERT OR REPLACE INTO annotation_range_checks(annotation_id, fingerprint)
                 SELECT id, ?1 FROM annotations WHERE id = ?2 AND book_id = ?3
                 AND ranges_json = ?4 AND quote = ?5 AND deleted_hlc IS NULL",
                params![
                    fingerprint,
                    expected.id,
                    expected.book_id,
                    serde_json::to_string(&expected.ranges)?,
                    expected.quote
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM annotation_range_checks WHERE annotation_id NOT IN
             (SELECT id FROM annotations WHERE deleted_hlc IS NULL)",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn repair_ranges(
        &self,
        expected: &StoredHighlight,
        ranges: &[SourceRange],
    ) -> HighlightResult<bool> {
        Ok(self.repair_ranges_batch(&[(expected.clone(), ranges.to_vec())])?[0])
    }

    fn repair_ranges_batch(
        &self,
        repairs: &[(StoredHighlight, Vec<SourceRange>)],
    ) -> HighlightResult<Vec<bool>> {
        if repairs.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut repaired = Vec::with_capacity(repairs.len());
        for (expected, ranges) in repairs {
            let Some(mut annotation) = read_annotation(&transaction, &expected.id)? else {
                repaired.push(false);
                continue;
            };
            if annotation.deleted_at.is_some()
                || annotation.book_id != expected.book_id
                || annotation.ranges != expected.ranges
                || annotation.quote != expected.quote
                || annotation.ranges == *ranges
            {
                repaired.push(false);
                continue;
            }
            // Repair only ranges: retain concurrently edited notes and sync history.
            if ranges.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "range repair requires a source range",
                )
                .into());
            }
            annotation.ranges.clone_from(ranges);
            incoming_ranges_are_valid(&annotation)?;
            annotation.updated_at = tick(&transaction, &self.device_id, None)?;
            let counter = annotation.clock.entry(self.device_id.clone()).or_default();
            *counter = counter.saturating_add(1);
            annotation.origin_device.clone_from(&self.device_id);
            write_annotation(&transaction, &annotation)?;
            mark_reading_changed(&transaction, &self.device_id, &annotation.book_id)?;
            repaired.push(true);
        }
        transaction.commit()?;
        Ok(repaired)
    }

    fn remove_highlight(&self, id: &str) -> HighlightResult<bool> {
        self.delete_annotation(id)
    }
}

fn tick(
    transaction: &Transaction<'_>,
    device_id: &str,
    observed_wall_time: Option<u64>,
) -> SyncResult<HybridTimestamp> {
    let previous = transaction
        .query_row("SELECT value FROM sync_meta WHERE key = 'hlc'", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()?
        .map(|value| HybridTimestamp::parse(&value))
        .transpose()?;
    let now = unix_timestamp_millis();
    let previous_wall = previous.as_ref().map_or(0, |value| value.wall_time_ms);
    let wall_time_ms = now.max(previous_wall).max(observed_wall_time.unwrap_or(0));
    let counter = if wall_time_ms == previous_wall {
        previous
            .as_ref()
            .map_or(0, |value| value.counter.saturating_add(1))
    } else {
        0
    };
    let timestamp = HybridTimestamp {
        wall_time_ms,
        counter,
        device_id: device_id.to_owned(),
    };
    transaction.execute(
        "INSERT INTO sync_meta(key, value) VALUES ('hlc', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [serde_json::to_string(&timestamp)?],
    )?;
    Ok(timestamp)
}

fn read_annotation(transaction: &Transaction<'_>, id: &str) -> SyncResult<Option<AnnotationState>> {
    transaction
        .query_row(
            "SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json, \
             deleted_hlc, origin_device, conflict_of FROM annotations WHERE id = ?1",
            [id],
            read_annotation_row,
        )
        .optional()
        .map_err(Into::into)
}

fn read_annotation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AnnotationState> {
    let ranges: String = row.get(2)?;
    let updated_at: String = row.get(6)?;
    let clock: String = row.get(7)?;
    let deleted_at: Option<String> = row.get(8)?;
    Ok(AnnotationState {
        id: row.get(0)?,
        book_id: row.get(1)?,
        ranges: serde_json::from_str(&ranges).map_err(json_error)?,
        quote: row.get(3)?,
        note: row.get(4)?,
        created_at: u64::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
        updated_at: HybridTimestamp::parse(&updated_at).map_err(json_error)?,
        clock: serde_json::from_str(&clock).map_err(json_error)?,
        deleted_at: deleted_at
            .map(|value| HybridTimestamp::parse(&value).map_err(json_error))
            .transpose()?,
        origin_device: row.get(9)?,
        conflict_of: row.get(10)?,
    })
}

fn json_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn write_annotation(transaction: &Transaction<'_>, annotation: &AnnotationState) -> SyncResult<()> {
    transaction.execute(
        "INSERT INTO annotations(id, book_id, ranges_json, quote, note, created_at, updated_hlc, \
         clock_json, deleted_hlc, origin_device, conflict_of) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT(id) DO UPDATE SET book_id = excluded.book_id, \
         ranges_json = excluded.ranges_json, quote = excluded.quote, note = excluded.note, \
         created_at = excluded.created_at, updated_hlc = excluded.updated_hlc, \
         clock_json = excluded.clock_json, deleted_hlc = excluded.deleted_hlc, \
         origin_device = excluded.origin_device, conflict_of = excluded.conflict_of",
        params![
            annotation.id,
            annotation.book_id,
            serde_json::to_string(&annotation.ranges)?,
            annotation.quote,
            annotation.note,
            i64::try_from(annotation.created_at).unwrap_or(i64::MAX),
            serde_json::to_string(&annotation.updated_at)?,
            serde_json::to_string(&annotation.clock)?,
            annotation
                .deleted_at
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            annotation.origin_device,
            annotation.conflict_of,
        ],
    )?;
    Ok(())
}

fn incoming_ranges_are_valid(annotation: &AnnotationState) -> SyncResult<()> {
    if annotation.id.trim().is_empty()
        || annotation.book_id.trim().is_empty()
        || annotation.origin_device.trim().is_empty()
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "远端批注标识无效").into());
    }
    Ok(())
}

fn conflict_id(annotation: &AnnotationState) -> String {
    format!(
        "{}~conflict~{}~{}-{}",
        annotation.id,
        annotation.origin_device,
        annotation.updated_at.wall_time_ms,
        annotation.updated_at.counter
    )
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

// Keep the stored fractions lossless: fallback navigation uses floor(), so
// rounding a value at a segment boundary can restore the previous segment.
fn same_progress_location(a: &LocatorV1, b: &LocatorV1) -> bool {
    // The reader currently restores precise source anchors, but does not use
    // CFI/position alone before falling back to fractions.
    let precise = a.source.is_some();
    let same_fraction = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => a == b || (precise && (a - b).abs() <= 8.0 * f64::EPSILON),
        (None, None) => true,
        _ => false,
    };
    a.version == b.version
        && a.publication_id == b.publication_id
        && a.href == b.href
        && a.position == b.position
        && a.source == b.source
        && a.partial_cfi == b.partial_cfi
        && a.text == b.text
        && same_fraction(a.progression, b.progression)
        && same_fraction(a.total_progression, b.total_progression)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_library_snapshot_preserves_removals_and_defers_new_unuploaded_books() {
        let store = test_store("membership-snapshot");
        store.set_book_present("old", true).unwrap();
        let captured = vec!["old".to_owned()];
        store.set_book_present("old", false).unwrap();
        store.set_book_present("new", true).unwrap();
        let entries = store.membership_entries(&captured).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].book_id, "old");
        assert!(!entries[0].present);
        assert!(store.is_locally_removed("old").unwrap());
        let next = store.membership_entries(&["new".into()]).unwrap();
        assert!(
            next.iter()
                .any(|entry| entry.book_id == "new" && entry.present)
        );
        cleanup(&store);
    }

    #[test]
    fn sync_acknowledges_only_uploaded_revision_and_survives_reopen() {
        let store = test_store("incremental-revisions");
        store.save_progress("book", &locator("book", 0.2)).unwrap();
        let (uploaded_revision, snapshot) = store.reading_snapshot("book").unwrap();
        assert_eq!(
            snapshot.progress.unwrap().locator.total_progression,
            Some(0.2)
        );
        store.save_progress("book", &locator("book", 0.3)).unwrap();
        store
            .acknowledge_reading("account-a", "book", uploaded_revision)
            .unwrap();
        let reopened = SyncStore::open_at(store.path.clone(), store.device_id.clone()).unwrap();
        let pending = reopened.pending_reading("account-a").unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].1 > uploaded_revision);
        reopened
            .acknowledge_reading("account-a", "book", pending[0].1)
            .unwrap();
        reopened
            .save_progress("book", &locator("book", 0.3))
            .unwrap();
        assert!(reopened.pending_reading("account-a").unwrap().is_empty());
        assert_eq!(reopened.pending_reading("account-b").unwrap().len(), 1);
        // A late completion can never replace a newer acknowledgement.
        reopened
            .acknowledge_reading("account-a", "book", uploaded_revision)
            .unwrap();
        assert!(reopened.pending_reading("account-a").unwrap().is_empty());
        cleanup(&store);
    }

    #[test]
    fn sync_cache_is_isolated_by_account_and_additive_migration_preserves_progress() {
        let store = test_store("incremental-migration");
        store.save_progress("book", &locator("book", 0.7)).unwrap();
        let original = store.load_progress("book").unwrap();
        store
            .connection()
            .unwrap()
            .execute_batch(
                "DROP TABLE reading_changes; DROP TABLE reading_ack; DROP TABLE transfer_cache;",
            )
            .unwrap();
        let reopened = SyncStore::open_at(store.path.clone(), store.device_id.clone()).unwrap();
        assert_eq!(reopened.load_progress("book").unwrap(), original);
        assert_eq!(reopened.pending_reading("account-a").unwrap().len(), 1);
        reopened
            .cache_set("account-a", "object:file", b"first")
            .unwrap();
        reopened
            .cache_set("account-b", "object:file", b"second")
            .unwrap();
        assert_eq!(
            reopened
                .cache_get("account-a", "object:file")
                .unwrap()
                .as_deref(),
            Some(b"first".as_slice())
        );
        assert_eq!(
            reopened
                .cache_get("account-b", "object:file")
                .unwrap()
                .as_deref(),
            Some(b"second".as_slice())
        );
        cleanup(&store);
    }

    #[test]
    fn sync_tracks_annotation_edits_and_deletions_in_the_same_snapshot() {
        let store = test_store("incremental-notes");
        store
            .create_annotation(
                "note".into(),
                "book".into(),
                vec![source_range()],
                "quote".into(),
                None,
                1,
            )
            .unwrap();
        let (first, _) = store.reading_snapshot("book").unwrap();
        store.acknowledge_reading("account", "book", first).unwrap();
        store
            .update_annotation_note("note", Some("edited".into()))
            .unwrap();
        let (edited, snapshot) = store.reading_snapshot("book").unwrap();
        assert!(edited > first);
        assert_eq!(snapshot.annotations[0].note.as_deref(), Some("edited"));
        store.delete_annotation("note").unwrap();
        store
            .acknowledge_reading("account", "book", edited)
            .unwrap();
        let (_, snapshot) = store.reading_snapshot("book").unwrap();
        assert!(snapshot.annotations[0].deleted_at.is_some());
        assert_eq!(store.pending_reading("account").unwrap().len(), 1);
        cleanup(&store);
    }
    use rebook_publication::{PublicationId, PublicationUrl, SourceAnchor, SpineItemId};

    #[test]
    fn progress_uses_latest_real_timestamp_instead_of_maximum_percentage() {
        let store = test_store("progress");
        let mut first = locator("book", 0.9);
        store.save_progress("book", &first).unwrap();
        first.total_progression = Some(0.2);
        store.save_progress("book", &first).unwrap();

        assert_eq!(
            store
                .load_progress("book")
                .unwrap()
                .unwrap()
                .locator
                .total_progression,
            Some(0.2)
        );
        cleanup(&store);
    }

    #[test]
    fn progress_activity_times_include_every_book_with_reading_progress() {
        let store = test_store("progress-activity-times");
        store
            .save_progress("first", &locator("first", 0.2))
            .unwrap();
        store
            .save_progress("second", &locator("second", 0.6))
            .unwrap();

        let activity_times = store.progress_activity_times().unwrap();

        assert_eq!(activity_times.len(), 2);
        assert!(activity_times.contains_key("first"));
        assert!(activity_times.contains_key("second"));
        cleanup(&store);
    }

    #[test]
    fn progress_activity_times_recovers_wrapped_and_skips_unreadable_rows() {
        let store = test_store("progress-activity-times-corrupt");
        store.save_progress("good", &locator("good", 0.2)).unwrap();
        store
            .save_progress("wrapped", &locator("wrapped", 0.4))
            .unwrap();
        store
            .save_progress("garbage", &locator("garbage", 0.6))
            .unwrap();
        let before = store.progress_activity_times().unwrap();
        let wrapped_time = before["wrapped"];
        {
            let connection = store.connection().unwrap();
            let stored = connection
                .query_row(
                    "SELECT updated_hlc FROM progress WHERE book_id = 'wrapped'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap();
            // A second encode turns the object into a quoted string.
            connection
                .execute(
                    "UPDATE progress SET updated_hlc = ?1 WHERE book_id = 'wrapped'",
                    [serde_json::to_string(&stored).unwrap()],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE progress SET updated_hlc = 'not-json' WHERE book_id = 'garbage'",
                    [],
                )
                .unwrap();
        }

        let activity_times = store.progress_activity_times().unwrap();

        assert_eq!(activity_times.len(), 2);
        assert_eq!(activity_times["good"], before["good"]);
        assert_eq!(activity_times["wrapped"], wrapped_time);
        assert!(!activity_times.contains_key("garbage"));
        cleanup(&store);
    }

    #[test]
    fn merge_progress_heals_an_unreadable_current_timestamp() {
        let store = test_store("merge-progress-heal");
        store.save_progress("book", &locator("book", 0.2)).unwrap();
        {
            let connection = store.connection().unwrap();
            connection
                .execute(
                    "UPDATE progress SET updated_hlc = 'not-json' WHERE book_id = 'book'",
                    [],
                )
                .unwrap();
        }
        let incoming = ProgressState {
            locator: locator("book", 0.5),
            updated_at: HybridTimestamp {
                wall_time_ms: 1_000,
                counter: 0,
                device_id: "device-b".into(),
            },
        };

        assert!(store.merge_progress(&incoming).unwrap());
        let healed = store.load_progress("book").unwrap().unwrap();
        assert_eq!(healed.locator.total_progression, Some(0.5));
        assert_eq!(healed.updated_at.wall_time_ms, 1_000);
        cleanup(&store);
    }

    #[test]
    fn recording_reading_activity_advances_the_time_without_a_locator_change() {
        let store = test_store("reading-activity-refresh");
        let locator = locator("book", 0.2);
        store.save_progress("book", &locator).unwrap();
        let first = store.progress_activity_times().unwrap()["book"];

        store.save_progress("book", &locator).unwrap();
        assert_eq!(store.progress_activity_times().unwrap()["book"], first);

        std::thread::sleep(std::time::Duration::from_millis(5));
        store.record_reading_activity("book", &locator).unwrap();
        assert!(store.progress_activity_times().unwrap()["book"] > first);
        cleanup(&store);
    }

    #[test]
    fn delayed_reading_activity_preserves_the_complete_latest_locator() {
        let store = test_store("activity-precise-position");
        let initial = locator("book", 0.2);
        store.record_reading_activity("book", &initial).unwrap();
        assert_eq!(
            store.load_progress("book").unwrap().unwrap().locator,
            initial
        );

        let mut precise = locator("book", 0.6);
        let mut range = source_range();
        range.start.text_offset = 17;
        range.end.text_offset = 31;
        precise.source = Some(range);
        store.save_progress("book", &precise).unwrap();
        let previous_time = store.load_progress("book").unwrap().unwrap().updated_at;
        // An earlier opening finishes after the reader has already moved.
        store.record_reading_activity("book", &initial).unwrap();
        let saved = store.load_progress("book").unwrap().unwrap();
        assert_eq!(saved.locator, precise);
        assert!(saved.updated_at > previous_time);
        cleanup(&store);
    }

    #[test]
    fn progress_float_noise_is_ignored_only_with_matching_precise_anchors() {
        let store = test_store("progress-float-noise");
        let mut initial = locator("book", 1.0 / 3.0);
        initial.progression = Some(1.0 / 3.0);
        initial.source = Some(source_range());
        store.save_progress("book", &initial).unwrap();
        let saved = store.load_progress("book").unwrap().unwrap();
        let mut changed = initial.clone();
        changed.progression = initial.progression.map(|p| f64::from_bits(p.to_bits() + 1));
        changed.total_progression = changed.progression;
        store.save_progress("book", &changed).unwrap();
        assert_eq!(store.load_progress("book").unwrap().unwrap(), saved);

        changed.source.as_mut().unwrap().start.text_offset += 1;
        store.save_progress("book", &changed).unwrap();
        assert_eq!(
            store.load_progress("book").unwrap().unwrap().locator,
            changed
        );

        changed.source = None;
        changed.partial_cfi = Some("/4/2".into());
        store.save_progress("book", &changed).unwrap();
        changed.progression = initial.progression;
        store.save_progress("book", &changed).unwrap();
        assert_eq!(
            store.load_progress("book").unwrap().unwrap().locator,
            changed
        );
        assert_eq!((changed.progression.unwrap() * 3.0).floor(), 1.0);
        cleanup(&store);
    }

    #[test]
    fn deleting_an_annotation_keeps_a_tombstone_for_sync() {
        let store = test_store("tombstone");
        let range = source_range();
        store
            .create_annotation(
                "note".into(),
                "book".into(),
                vec![range],
                "quote".into(),
                Some("comment".into()),
                1,
            )
            .unwrap();

        assert!(store.delete_annotation("note").unwrap());
        assert!(store.annotations_for_book("book").unwrap().is_empty());
        let exported = store.annotations_for_device_book("book").unwrap();
        assert_eq!(exported.len(), 1);
        assert!(exported[0].deleted_at.is_some());
        assert_eq!(exported[0].note.as_deref(), Some("comment"));
        cleanup(&store);
    }

    #[test]
    fn updating_an_annotation_note_advances_its_sync_state() {
        let store = test_store("update-note");
        store
            .create_annotation(
                "note".into(),
                "book".into(),
                vec![source_range()],
                "quote".into(),
                Some("before".into()),
                1,
            )
            .unwrap();
        let before = store.annotations_for_book("book").unwrap().remove(0);

        assert!(
            store
                .update_annotation_note("note", Some("after".into()))
                .unwrap()
        );
        let after = store.annotations_for_book("book").unwrap().remove(0);
        assert_eq!(after.note.as_deref(), Some("after"));
        assert!(after.clock["device-a"] > before.clock["device-a"]);
        assert!(after.updated_at >= before.updated_at);
        cleanup(&store);
    }

    #[test]
    fn batch_range_repairs_commit_together_and_roll_back_invalid_input() {
        let store = test_store("batch-range-repair");
        let a =
            StoredHighlight::with_note("book".into(), vec![source_range()], "first".into(), None);
        let b =
            StoredHighlight::with_note("book".into(), vec![source_range()], "second".into(), None);
        store.insert_highlight(&a).unwrap();
        store.insert_highlight(&b).unwrap();
        let mut repaired = source_range();
        repaired.end.text_offset = 10;
        assert!(
            store
                .repair_ranges_batch(&[
                    (a.clone(), vec![repaired.clone()]),
                    (b.clone(), Vec::new()),
                ])
                .is_err()
        );
        assert!(
            store
                .highlights_for_book("book")
                .unwrap()
                .iter()
                .all(|h| h.ranges == vec![source_range()])
        );
        assert_eq!(
            store
                .repair_ranges_batch(&[
                    (a.clone(), vec![repaired.clone()]),
                    (b, vec![repaired.clone()]),
                ])
                .unwrap(),
            vec![true, true]
        );
        assert!(
            store
                .highlights_for_book("book")
                .unwrap()
                .iter()
                .all(|h| h.ranges == vec![repaired.clone()])
        );
        assert_eq!(
            store
                .repair_ranges_batch(&[(a, vec![source_range()])])
                .unwrap(),
            vec![false]
        );
        cleanup(&store);
    }

    #[test]
    fn range_repair_preserves_new_notes_and_rejects_stale_ranges_and_checks() {
        let store = test_store("range-repair");
        let old =
            StoredHighlight::with_note("book".into(), vec![source_range()], "quote".into(), None);
        store.insert_highlight(&old).unwrap();
        store
            .update_annotation_note(&old.id, Some("new note".into()))
            .unwrap();
        let before = store.annotations_for_book("book").unwrap().remove(0);
        let mut repaired = source_range();
        repaired.end.text_offset = 10;
        assert!(store.repair_ranges(&old, &[repaired.clone()]).unwrap());
        let after = store.annotations_for_book("book").unwrap().remove(0);
        assert_eq!(after.note.as_deref(), Some("new note"));
        assert_eq!(after.ranges, vec![repaired]);
        assert!(after.clock["device-a"] > before.clock["device-a"]);
        assert!(!store.repair_ranges(&old, &[source_range()]).unwrap());
        store
            .save_range_checks(&[(old.clone(), "stale".into())])
            .unwrap();
        assert!(store.range_checks("book").unwrap().is_empty());
        let current = store.highlights_for_book("book").unwrap().remove(0);
        store
            .save_range_checks(&[(current.clone(), "current".into())])
            .unwrap();
        let reopened = SyncStore::open_at(store.path.clone(), "device-a").unwrap();
        assert_eq!(reopened.range_checks("book").unwrap()[&old.id], "current");
        store.delete_annotation(&old.id).unwrap();
        assert!(!store.repair_ranges(&current, &[source_range()]).unwrap());
        assert!(store.range_checks("book").unwrap().is_empty());
        cleanup(&store);
    }

    fn locator(book_id: &str, progress: f64) -> LocatorV1 {
        let mut locator = LocatorV1::at_start(
            PublicationId::new(book_id).unwrap(),
            PublicationUrl::parse("chapter.xhtml").unwrap(),
        );
        locator.total_progression = Some(progress);
        locator
    }

    fn source_range() -> SourceRange {
        SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: "p".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: "p".into(),
                text_offset: 5,
            },
        }
    }

    #[test]
    fn bundled_sqlite_keeps_persistent_wal_transactions_without_unused_extensions() {
        let store = test_store("sqlite-build");
        {
            let mut connection = store.connection().unwrap();
            for option in [
                "ENABLE_FTS3",
                "ENABLE_FTS4",
                "ENABLE_FTS5",
                "ENABLE_RTREE",
                "ENABLE_DBSTAT_VTAB",
            ] {
                let enabled: bool = connection
                    .query_row("SELECT sqlite_compileoption_used(?1)", [option], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert!(!enabled, "unused SQLite extension is enabled: {option}");
            }
            let journal: String = connection
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "wal");
            assert!(
                connection
                    .pragma_query_value::<bool, _>(None, "foreign_keys", |row| row.get(0))
                    .unwrap()
            );
            connection
                .execute_batch(
                    "CREATE TABLE build_parent(id TEXT PRIMARY KEY);
                 CREATE TABLE build_child(parent TEXT REFERENCES build_parent(id));",
                )
                .unwrap();
            assert!(
                connection
                    .execute("INSERT INTO build_child VALUES ('missing')", [])
                    .is_err()
            );
            let transaction = connection.transaction().unwrap();
            transaction
                .execute("INSERT INTO build_parent VALUES ('committed')", [])
                .unwrap();
            transaction.commit().unwrap();
            let transaction = connection.transaction().unwrap();
            transaction
                .execute("INSERT INTO build_parent VALUES ('rolled-back')", [])
                .unwrap();
            transaction.rollback().unwrap();
        }
        {
            let connection = store.connection().unwrap();
            let values: Vec<String> = connection
                .prepare("SELECT id FROM build_parent ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(values, ["committed"]);
            let integrity: String = connection
                .pragma_query_value(None, "integrity_check", |row| row.get(0))
                .unwrap();
            assert_eq!(integrity, "ok");
        }
        cleanup(&store);
    }

    fn test_store(name: &str) -> SyncStore {
        let path = std::env::temp_dir().join(format!(
            "rebook-sync-{name}-{}-{}.sqlite3",
            std::process::id(),
            unix_timestamp_millis()
        ));
        SyncStore::open_at(path, "device-a").unwrap()
    }

    fn cleanup(store: &SyncStore) {
        let _ = std::fs::remove_file(store.path());
        let _ = std::fs::remove_file(store.path().with_extension("sqlite3-wal"));
        let _ = std::fs::remove_file(store.path().with_extension("sqlite3-shm"));
    }
}
