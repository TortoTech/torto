//! Local-first reading history. Immutable events are merged by ID, never by summing counters.
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, TimeZone, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::library::LibraryBook;
use crate::preferences::AppLanguage;
use crate::sync::SyncResult;

pub mod period;

pub fn now_ms() -> u64 {
    Utc::now().timestamp_millis().max(0) as u64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    #[default]
    NotStarted,
    Reading,
    Finished,
}
impl Status {
    pub fn label(self, language: AppLanguage) -> &'static str {
        match self {
            Self::NotStarted => language.text("未开始", "Not started"),
            Self::Reading => language.text("在读", "Reading"),
            Self::Finished => language.text("已读完", "Finished"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub device: String,
    pub book: String,
    pub at: u64,
    pub kind: EventKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EventKind {
    Clear,
    Reading {
        session: String,
        start: u64,
        end: u64,
        offset: i32,
        from: f64,
        to: f64,
    },
    Status {
        status: Status,
        finished: Option<String>,
    },
    Metadata {
        title: String,
        authors: String,
        added: u64,
    },
}

pub fn database() -> SyncResult<Connection> {
    let dirs = crate::environment::paths().ok_or("Cannot find statistics directory")?;
    std::fs::create_dir_all(dirs.data_local_dir())?;
    let db = Connection::open(dirs.data_local_dir().join("reading-statistics-v1.sqlite3"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS events(id TEXT PRIMARY KEY, device TEXT NOT NULL, at INTEGER NOT NULL, json TEXT NOT NULL); CREATE INDEX IF NOT EXISTS events_device ON events(device,at); CREATE TABLE IF NOT EXISTS config(key TEXT PRIMARY KEY,value INTEGER NOT NULL);")?;
    Ok(db)
}

pub fn insert(db: &mut Connection, events: &[Event]) -> SyncResult<()> {
    let tx = db.transaction()?;
    for event in events {
        if event.id.len() > 128 || event.book.len() > 128 || event.device.len() > 128 {
            return Err("Invalid statistics identity".into());
        }
        if let EventKind::Reading {
            start,
            end,
            from,
            to,
            offset,
            ..
        } = &event.kind
        {
            if end < start
                || end - start > 60_000
                || !from.is_finite()
                || !to.is_finite()
                || offset.unsigned_abs() > 86400
            {
                return Err("Invalid reading interval".into());
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO events VALUES (?1,?2,?3,?4)",
            params![
                event.id,
                event.device,
                i64::try_from(event.at)?,
                serde_json::to_string(event)?
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}
pub fn events(db: &Connection) -> SyncResult<Vec<Event>> {
    let mut stmt = db.prepare("SELECT json FROM events ORDER BY at,id")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

pub enum Write {
    Event(Event),
    Flush(mpsc::Sender<()>),
}
pub struct Service {
    sender: mpsc::Sender<Write>,
    device: String,
    failed: Arc<AtomicBool>,
}
static SERVICE: OnceLock<Service> = OnceLock::new();
pub fn service() -> &'static Service {
    SERVICE.get_or_init(|| {
        let device = crate::sync::SyncSettings::load_default()
            .map(|s| s.device_id)
            .unwrap_or_else(|_| Uuid::new_v4().to_string());
        let (sender, receiver) = mpsc::channel();
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut db = match database() {
                Ok(db) => db,
                Err(error) => {
                    tracing::error!(%error,"statistics database unavailable");
                    worker_failed.store(true, Ordering::Relaxed);
                    let _ = ready_tx.send(());
                    return;
                }
            };
            let _ = ready_tx.send(());
            for write in receiver {
                let result = match write {
                    Write::Event(event) => insert(&mut db, &[event]),
                    Write::Flush(done) => {
                        let _ = done.send(());
                        Ok(())
                    }
                };
                if let Err(error) = result {
                    worker_failed.store(true, Ordering::Relaxed);
                    tracing::error!(%error,"failed to save reading statistics");
                }
            }
        });
        let _ = ready_rx.recv();
        Service {
            sender,
            device,
            failed,
        }
    })
}
pub fn record(book: &str, kind: EventKind) {
    let service = service();
    let event = Event {
        id: Uuid::new_v4().to_string(),
        device: service.device.clone(),
        book: book.into(),
        at: now_ms(),
        kind,
    };
    if service.sender.send(Write::Event(event)).is_err() {
        service.failed.store(true, Ordering::Relaxed);
    }
}

pub fn register_book(book: &LibraryBook) {
    record(
        &book.id,
        EventKind::Metadata {
            title: book.title.clone(),
            authors: book.authors.join(", "),
            added: book.added_at,
        },
    );
}
pub fn flush() {
    let (tx, rx) = mpsc::channel();
    if service().sender.send(Write::Flush(tx)).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
}

pub struct Tracker {
    fraction: Duration,
    book: String,
    session: String,
    last: Instant,
    activity: Instant,
    eligible: bool,
    pending_ms: u64,
    start: u64,
    from: f64,
    progress: f64,
    offset: i32,
}
impl Tracker {
    pub fn completion_summary(&mut self) -> SyncResult<(u64, Option<String>)> {
        self.save();
        flush();
        if service().failed.load(Ordering::Relaxed) {
            return Err("Reading statistics could not be saved".into());
        }
        let books = aggregate(&events(&database()?)?);
        Ok(books.get(&self.book).map_or((0, None), |book| {
            (
                union_duration(&book.intervals),
                (book.status == Status::Finished)
                    .then(|| book.finished.clone())
                    .flatten(),
            )
        }))
    }
    pub fn mark_finished(&mut self) {
        self.save();
        record(
            &self.book,
            EventKind::Status {
                status: Status::Finished,
                finished: Some(Local::now().format("%Y-%m-%d").to_string()),
            },
        );
    }
    pub fn new(book: &str) -> Self {
        let now = Instant::now();
        Self {
            fraction: Duration::ZERO,
            book: book.into(),
            session: Uuid::new_v4().to_string(),
            last: now,
            activity: now,
            eligible: false,
            pending_ms: 0,
            start: 0,
            from: 0.0,
            progress: 0.0,
            offset: Local::now().offset().local_minus_utc(),
        }
    }
    pub fn tick(&mut self, eligible: bool, activity: bool, progress: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last);
        let idle = Duration::from_secs(300);
        // A long frame gap may be suspension/lock; never charge that gap.
        if self.eligible && eligible && elapsed <= Duration::from_secs(5) {
            let remaining = idle.saturating_sub(self.last.saturating_duration_since(self.activity));
            let accumulated = elapsed.min(remaining) + self.fraction;
            let amount = accumulated.as_millis() as u64;
            self.fraction = accumulated - Duration::from_millis(amount);
            if self.pending_ms == 0 {
                self.start = now_ms().saturating_sub(amount);
                self.from = self.progress;
                self.offset = Local::now().offset().local_minus_utc();
            }
            self.pending_ms += amount;
        }
        if activity {
            self.activity = now;
        }
        let active = eligible && now.saturating_duration_since(self.activity) < idle;
        self.progress = progress.clamp(0.0, 1.0);
        if self.pending_ms >= 15_000 || !active || elapsed > Duration::from_secs(5) {
            self.save();
        }
        if self.eligible && !active {
            self.session = Uuid::new_v4().to_string();
        }
        self.eligible = active;
        self.last = now;
        active
    }
    fn save(&mut self) {
        if self.pending_ms == 0 {
            return;
        }
        record(
            &self.book,
            EventKind::Reading {
                session: self.session.clone(),
                start: self.start,
                end: self.start + self.pending_ms,
                offset: self.offset,
                from: self.from,
                to: self.progress,
            },
        );
        self.pending_ms = 0;
    }
}
impl Drop for Tracker {
    fn drop(&mut self) {
        self.save();
        flush();
    }
}

#[derive(Default)]
pub struct BookStats {
    pub valid_intervals: Vec<(u64, u64, i32)>,
    pub status_declared: bool,
    pub title: String,
    pub authors: String,
    pub added: u64,
    pub started: Option<u64>,
    pub last: Option<u64>,
    pub status: Status,
    pub finished: Option<String>,
    pub progress: f64,
    pub intervals: Vec<(u64, u64, i32)>,
    pub days: BTreeMap<String, u64>,
}

impl BookStats {
    pub fn display_progress(&self) -> f64 {
        if self.status == Status::Finished {
            1.0
        } else {
            self.progress.clamp(0.0, 1.0)
        }
    }
}
pub fn union_duration(intervals: &[(u64, u64, i32)]) -> u64 {
    let mut sorted = intervals.to_vec();
    sorted.sort_unstable();
    let mut end = 0;
    let mut total = 0;
    for (start, next, _) in sorted {
        total += next.saturating_sub(end.max(start));
        end = end.max(next);
    }
    total
}
pub fn daily(intervals: &[(u64, u64, i32)]) -> BTreeMap<String, u64> {
    let mut slices = BTreeMap::<String, Vec<(u64, u64, i32)>>::new();
    let mut sorted = intervals.to_vec();
    sorted.sort_unstable();
    let mut covered_end = 0;
    for (start, end, offset) in sorted {
        let mut start = start.max(covered_end);
        covered_end = covered_end.max(end);
        while start < end {
            let shifted = start as i64 + i64::from(offset) * 1000;
            let next = ((shifted.div_euclid(86_400_000) + 1) * 86_400_000
                - i64::from(offset) * 1000) as u64;
            let stop = end.min(next);
            let day = Utc
                .timestamp_millis_opt(shifted)
                .single()
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            slices.entry(day).or_default().push((start, stop, offset));
            start = stop;
        }
    }
    slices
        .into_iter()
        .map(|(day, parts)| (day, union_duration(&parts)))
        .collect()
}
pub fn aggregate(events: &[Event]) -> BTreeMap<String, BookStats> {
    let events = visible_history(events);
    let mut output = BTreeMap::<String, BookStats>::new();
    let mut sessions = HashMap::<String, u64>::new();
    for event in &events {
        if let EventKind::Reading {
            session,
            start,
            end,
            ..
        } = &event.kind
        {
            *sessions.entry(session.clone()).or_default() += end - start;
        }
    }
    for event in &events {
        let book = output.entry(event.book.clone()).or_default();
        match &event.kind {
            EventKind::Clear => {}
            EventKind::Metadata {
                title,
                authors,
                added,
            } => {
                book.title = title.clone();
                book.authors = authors.clone();
                if book.added == 0 || *added < book.added {
                    book.added = *added;
                }
            }
            EventKind::Status { status, finished } => {
                book.status_declared = true;
                book.status = *status;
                book.finished = finished.clone();
            }
            EventKind::Reading {
                session,
                start,
                end,
                offset,
                to,
                ..
            } => {
                book.intervals.push((*start, *end, *offset));
                book.progress = *to;
                if sessions.get(session).copied().unwrap_or(0) >= 30_000 {
                    book.valid_intervals.push((*start, *end, *offset));
                    book.started = Some(book.started.map_or(*start, |old| old.min(*start)));
                    book.last = Some(book.last.map_or(*end, |old| old.max(*end)));
                    if book.status == Status::NotStarted {
                        book.status = Status::Reading;
                    }
                }
            }
        }
    }
    for book in output.values_mut() {
        book.days = daily(&book.intervals);
    }
    output
}
pub fn visible_history(events: &[Event]) -> Vec<&Event> {
    let mut cleared = HashMap::<&str, (u64, &str)>::new();
    for event in events {
        if matches!(event.kind, EventKind::Clear) {
            let boundary = cleared.entry(&event.book).or_default();
            *boundary = (*boundary).max((event.at, &event.id));
        }
    }
    events
        .iter()
        .filter(|event| {
            matches!(event.kind, EventKind::Metadata { .. })
                || cleared
                    .get(event.book.as_str())
                    .is_none_or(|boundary| (event.at, event.id.as_str()) > *boundary)
        })
        .collect()
}
pub fn duration(ms: u64) -> String {
    format!("{}h {:02}m", ms / 3_600_000, (ms / 60_000) % 60)
}
pub fn date(ms: Option<u64>) -> String {
    ms.and_then(|ms| Local.timestamp_millis_opt(ms as i64).single())
        .map(|d| d.format("%Y/%m/%d").to_string())
        .unwrap_or_else(|| "—".into())
}

pub fn display_day(day: &str) -> String {
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|date| date.format("%Y/%m/%d").to_string())
        .unwrap_or_else(|_| "—".into())
}

pub fn sync_due(webdav: &crate::sync::webdav::WebDavClient, force: bool) -> SyncResult<bool> {
    let last = webdav
        .cache_get("statistics:last-success")?
        .and_then(|value| String::from_utf8(value).ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    Ok(force || Utc::now().timestamp_millis().saturating_sub(last) >= 10 * 60 * 1000)
}

pub async fn sync(webdav: &crate::sync::webdav::WebDavClient, device: &str) -> SyncResult<()> {
    flush();
    sync_with_database(webdav, device, &mut database()?).await
}

pub async fn sync_with_database(
    webdav: &crate::sync::webdav::WebDavClient,
    device: &str,
    db: &mut Connection,
) -> SyncResult<()> {
    use sha2::{Digest, Sha256};
    let files = webdav.list_json_files("statistics/").await?;
    for file in &files {
        if file.contains('/') || file.contains('\\') {
            continue;
        }
        let path = format!("statistics/{file}");
        if let Some(remote) = webdav.get_optional(&path).await? {
            if remote.bytes.len() > 32 * 1024 * 1024 {
                return Err("Statistics shard too large".into());
            }
            let digest = format!("{:x}", Sha256::digest(&remote.bytes));
            let applied = format!("statistics:applied:{file}");
            if webdav.cache_get(&applied)?.as_deref() == Some(digest.as_bytes()) {
                continue;
            }
            let shard: Shard = serde_json::from_slice(&remote.bytes)?;
            if shard.version != 1 || shard.events.len() > 100_000 {
                return Err("Unsupported statistics shard".into());
            }
            insert(db, &shard.events)?;
            webdav.cache_set(&applied, digest.as_bytes())?;
        }
    }
    let months = {
        let mut query = db.prepare(
            "SELECT strftime('%Y-%m', at / 1000, 'unixepoch'), COUNT(*)
            FROM events WHERE device = ?1 GROUP BY 1 ORDER BY 1",
        )?;
        query
            .query_map([device], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (month, revision) in months {
        let file = format!("{device}-{month}.json");
        let key = format!("statistics:uploaded:{file}");
        let revision = revision.to_string();
        if files.contains(&file) && webdav.cache_get(&key)?.as_deref() == Some(revision.as_bytes())
        {
            continue;
        }
        if !files.contains(&file) {
            webdav.invalidate_object(&format!("statistics/{file}"))?;
        }
        let events = {
            let mut query = db.prepare(
                "SELECT json FROM events WHERE device = ?1
                AND strftime('%Y-%m', at / 1000, 'unixepoch') = ?2 ORDER BY at,id",
            )?;
            query
                .query_map(params![device, month], |row| row.get::<_, String>(0))?
                .map(|row| -> SyncResult<Event> { Ok(serde_json::from_str(&row?)?) })
                .collect::<SyncResult<Vec<_>>>()?
        };
        webdav
            .put_mutable_json(&format!("statistics/{file}"), &Shard { version: 1, events })
            .await?;
        // Confirm only the snapshot count; events inserted during the PUT remain pending.
        webdav.cache_set(&key, revision.as_bytes())?;
    }
    webdav.cache_set(
        "statistics:last-success",
        Utc::now().timestamp_millis().to_string().as_bytes(),
    )?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct Shard {
    version: u32,
    events: Vec<Event>,
}

pub fn has_write_failure() -> bool {
    service().failed.load(Ordering::Relaxed)
}
