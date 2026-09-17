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

const DATABASE_FILE: &str = "sync-v1.sqlite3";
const DATABASE_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug)]
pub(crate) struct SyncStore {
    path: PathBuf,
    device_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoredProgress {
    pub locator: LocatorV1,
    pub updated_at: HybridTimestamp,
}

impl SyncStore {
    pub(crate) fn open_default(device_id: impl Into<String>) -> SyncResult<Self> {
        let project = crate::smoke::project_dirs()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定同步数据库目录"))?;
        Self::open_at(project.data_local_dir().join(DATABASE_FILE), device_id)
    }

    pub(crate) fn open_at(path: PathBuf, device_id: impl Into<String>) -> SyncResult<Self> {
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

    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub(crate) fn create_annotation(
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

    pub(crate) fn annotations_for_book(&self, book_id: &str) -> SyncResult<Vec<AnnotationState>> {
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
    pub(crate) fn annotations_for_device_book(
        &self,
        book_id: &str,
    ) -> SyncResult<Vec<AnnotationState>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, book_id, ranges_json, quote, note, created_at, updated_hlc, clock_json, \
             deleted_hlc, origin_device, conflict_of FROM annotations \
             WHERE book_id = ?1 AND origin_device = ?2 ORDER BY id",
        )?;
        let rows = statement.query_map(params![book_id, self.device_id], read_annotation_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn delete_annotation(&self, id: &str) -> SyncResult<bool> {
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

    pub(crate) fn update_annotation_note(
        &self,
        id: &str,
        note: Option<String>,
    ) -> SyncResult<bool> {
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

    pub(crate) fn merge_annotations(&self, annotations: &[AnnotationState]) -> SyncResult<usize> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut changed = 0;
        for incoming in annotations {
            incoming_ranges_are_valid(incoming)?;
            tick(
                &transaction,
                &self.device_id,
                Some(incoming.updated_at.wall_time_ms),
            )?;
            match read_annotation(&transaction, &incoming.id)? {
                None => {
                    write_annotation(&transaction, incoming)?;
                    changed += 1;
                }
                Some(current) => match compare_clocks(&current.clock, &incoming.clock) {
                    ClockOrder::Before => {
                        write_annotation(&transaction, incoming)?;
                        changed += 1;
                    }
                    ClockOrder::After | ClockOrder::Equal => {}
                    ClockOrder::Concurrent => {
                        let (winner, loser) = if incoming.updated_at > current.updated_at {
                            (incoming, &current)
                        } else {
                            (&current, incoming)
                        };
                        if loser.deleted_at.is_none() && loser.conflict_of.is_none() {
                            let mut conflict = loser.clone();
                            conflict.id = conflict_id(loser);
                            conflict.conflict_of = Some(incoming.id.clone());
                            if read_annotation(&transaction, &conflict.id)?.is_none() {
                                write_annotation(&transaction, &conflict)?;
                            }
                        }
                        write_annotation(&transaction, winner)?;
                        changed += 1;
                    }
                },
            }
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub(crate) fn save_progress(&self, book_id: &str, locator: &LocatorV1) -> SyncResult<()> {
        locator.validate()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let locator_json = serde_json::to_string(locator)?;
        let current: Option<String> = transaction
            .query_row(
                "SELECT locator_json FROM progress WHERE book_id = ?1",
                [book_id],
                |row| row.get(0),
            )
            .optional()?;
        if current.as_deref() == Some(locator_json.as_str()) {
            return Ok(());
        }
        let updated_at = tick(&transaction, &self.device_id, None)?;
        transaction.execute(
            "INSERT INTO progress(book_id, locator_json, updated_hlc) VALUES (?1, ?2, ?3) \
             ON CONFLICT(book_id) DO UPDATE SET locator_json = excluded.locator_json, \
             updated_hlc = excluded.updated_hlc",
            params![book_id, locator_json, serde_json::to_string(&updated_at)?],
        )?;
        mark_reading_changed(&transaction, &self.device_id, book_id)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn load_progress(&self, book_id: &str) -> SyncResult<Option<StoredProgress>> {
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
                    updated_at: serde_json::from_str(&updated_at)?,
                })
            })
            .transpose()
    }

    pub(crate) fn progress_activity_times(&self) -> SyncResult<HashMap<String, u64>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT book_id, updated_hlc FROM progress")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut activity_times = HashMap::new();
        for row in rows {
            let (book_id, updated_at) = row?;
            let updated_at: HybridTimestamp = serde_json::from_str(&updated_at)?;
            activity_times.insert(book_id, updated_at.wall_time_ms);
        }
        Ok(activity_times)
    }

    pub(crate) fn merge_progress(&self, progress: &ProgressState) -> SyncResult<bool> {
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
            .map(|value| serde_json::from_str::<HybridTimestamp>(&value))
            .transpose()?;
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

    pub(crate) fn set_book_present(&self, book_id: &str, present: bool) -> SyncResult<()> {
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

    pub(crate) fn membership_entries(
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
                changed_at: serde_json::from_str(&changed_at)?,
            })
        })
        .filter(|entry: &SyncResult<DeviceBookEntry>| {
            entry.as_ref().map_or(true, |entry| {
                !entry.present || local_book_ids.contains(&entry.book_id)
            })
        })
        .collect()
    }

    pub(crate) fn is_locally_removed(&self, book_id: &str) -> SyncResult<bool> {
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

    pub(crate) fn register_book_if_missing(&self, book_id: &str) -> SyncResult<()> {
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

    pub(crate) fn tick(&self) -> SyncResult<HybridTimestamp> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let timestamp = tick(&transaction, &self.device_id, None)?;
        transaction.commit()?;
        Ok(timestamp)
    }

    pub(crate) fn pending_reading(&self, account: &str) -> SyncResult<Vec<(String, u64, u64)>> {
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

    pub(crate) fn acknowledge_reading(
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

    pub(crate) fn reading_snapshot(&self, book_id: &str) -> SyncResult<(u64, DeviceBookState)> {
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

    pub(crate) fn cache_get(&self, account: &str, key: &str) -> SyncResult<Option<Vec<u8>>> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT value FROM transfer_cache WHERE account = ?1 AND key = ?2",
                params![account, key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn cache_set(&self, account: &str, key: &str, value: &[u8]) -> SyncResult<()> {
        self.connection()?.execute(
            "INSERT INTO transfer_cache(account,key,value) VALUES (?1,?2,?3)
            ON CONFLICT(account,key) DO UPDATE SET value = excluded.value",
            params![account, key, value],
        )?;
        Ok(())
    }

    pub(crate) fn cache_remove(&self, account: &str, key: &str) -> SyncResult<()> {
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
        .map(|value| serde_json::from_str::<HybridTimestamp>(&value))
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
        updated_at: serde_json::from_str(&updated_at).map_err(json_error)?,
        clock: serde_json::from_str(&clock).map_err(json_error)?,
        deleted_at: deleted_at
            .map(|value| serde_json::from_str(&value).map_err(json_error))
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
