//! Local-first reading history. Immutable events are merged by ID, never by summing counters.
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate, TimeZone, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::library::LibraryBook;
use crate::preferences::AppLanguage;
use crate::sync::SyncResult;

fn now_ms() -> u64 {
    Utc::now().timestamp_millis().max(0) as u64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Status {
    #[default]
    NotStarted,
    Reading,
    Finished,
}
impl Status {
    fn label(self, language: AppLanguage) -> &'static str {
        match self {
            Self::NotStarted => language.text("未开始", "Not started"),
            Self::Reading => language.text("在读", "Reading"),
            Self::Finished => language.text("已读完", "Finished"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Event {
    id: String,
    device: String,
    book: String,
    at: u64,
    kind: EventKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum EventKind {
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

fn database() -> SyncResult<Connection> {
    let dirs = directories::ProjectDirs::from("com", "Rebook", "Rebook")
        .ok_or("Cannot find statistics directory")?;
    std::fs::create_dir_all(dirs.data_local_dir())?;
    let db = Connection::open(dirs.data_local_dir().join("reading-statistics-v1.sqlite3"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS events(id TEXT PRIMARY KEY, device TEXT NOT NULL, at INTEGER NOT NULL, json TEXT NOT NULL); CREATE INDEX IF NOT EXISTS events_device ON events(device,at); CREATE TABLE IF NOT EXISTS config(key TEXT PRIMARY KEY,value INTEGER NOT NULL);")?;
    Ok(db)
}

fn insert(db: &mut Connection, events: &[Event]) -> SyncResult<()> {
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
fn events(db: &Connection) -> SyncResult<Vec<Event>> {
    let mut stmt = db.prepare("SELECT json FROM events ORDER BY at,id")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

enum Write {
    Event(Event),
    Config(bool, u64),
    Flush(mpsc::Sender<()>),
}
struct Service {
    sender: mpsc::Sender<Write>,
    device: String,
    enabled: AtomicBool,
    idle: AtomicU64,
    failed: Arc<AtomicBool>,
}
static SERVICE: OnceLock<Service> = OnceLock::new();
fn service() -> &'static Service {
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
                    let _ = ready_tx.send((true, 300));
                    return;
                }
            };
            let enabled = db
                .query_row("SELECT value FROM config WHERE key='enabled'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(1)
                != 0;
            let idle = db
                .query_row("SELECT value FROM config WHERE key='idle'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap_or(300)
                .clamp(120, 600) as u64;
            let _ = ready_tx.send((enabled, idle));
            for write in receiver {
                let result = match write {
                    Write::Event(event) => insert(&mut db, &[event]),
                    Write::Config(enabled, idle) => (|| {
                        db.execute(
                            "INSERT OR REPLACE INTO config VALUES ('enabled',?1)",
                            [i64::from(enabled)],
                        )?;
                        db.execute(
                            "INSERT OR REPLACE INTO config VALUES ('idle',?1)",
                            [idle as i64],
                        )?;
                        Ok(())
                    })(),
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
        let (enabled, idle) = ready_rx.recv().unwrap_or((true, 300));
        Service {
            sender,
            device,
            enabled: AtomicBool::new(enabled),
            idle: AtomicU64::new(idle),
            failed,
        }
    })
}
fn record(book: &str, kind: EventKind) {
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

pub(crate) fn register_book(book: &LibraryBook) {
    record(
        &book.id,
        EventKind::Metadata {
            title: book.title.clone(),
            authors: book.authors.join(", "),
            added: book.added_at,
        },
    );
}
fn flush() {
    let (tx, rx) = mpsc::channel();
    if service().sender.send(Write::Flush(tx)).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
}

pub(crate) struct Tracker {
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
    pub(crate) fn mark_finished(&mut self) {
        self.save();
        record(
            &self.book,
            EventKind::Status {
                status: Status::Finished,
                finished: Some(Local::now().format("%Y-%m-%d").to_string()),
            },
        );
    }
    pub(crate) fn toggle_timer(&mut self) {
        self.save();
        let enabled = !service().enabled.load(Ordering::Relaxed);
        service().enabled.store(enabled, Ordering::Relaxed);
        self.activity = Instant::now();
        let _ = service().sender.send(Write::Config(
            enabled,
            service().idle.load(Ordering::Relaxed),
        ));
    }
    pub(crate) fn new(book: &str) -> Self {
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
    pub(crate) fn tick(&mut self, eligible: bool, activity: bool, progress: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last);
        let idle = Duration::from_secs(service().idle.load(Ordering::Relaxed));
        // A long frame gap may be suspension/lock; never charge that gap.
        if self.eligible
            && eligible
            && elapsed <= Duration::from_secs(5)
            && service().enabled.load(Ordering::Relaxed)
        {
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
        let active = eligible
            && now.saturating_duration_since(self.activity) < idle
            && service().enabled.load(Ordering::Relaxed);
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
struct BookStats {
    valid_intervals: Vec<(u64, u64, i32)>,
    status_declared: bool,
    title: String,
    authors: String,
    added: u64,
    started: Option<u64>,
    last: Option<u64>,
    status: Status,
    finished: Option<String>,
    progress: f64,
    intervals: Vec<(u64, u64, i32)>,
    days: BTreeMap<String, u64>,
}
fn union_duration(intervals: &[(u64, u64, i32)]) -> u64 {
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
fn daily(intervals: &[(u64, u64, i32)]) -> BTreeMap<String, u64> {
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
fn aggregate(events: &[Event]) -> BTreeMap<String, BookStats> {
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
fn visible_history(events: &[Event]) -> Vec<&Event> {
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
fn duration(ms: u64) -> String {
    format!("{}h {:02}m", ms / 3_600_000, (ms / 60_000) % 60)
}
fn date(ms: Option<u64>) -> String {
    ms.and_then(|ms| Local.timestamp_millis_opt(ms as i64).single())
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

#[derive(Default)]
pub(crate) struct Page {
    settings_open: bool,
    status_draft: Option<Status>,
    detail_key: Option<String>,
    clear_confirm: bool,
    progress: HashMap<String, f64>,
    annotations: HashMap<String, (usize, usize)>,
    covers: HashMap<String, Vec<u8>>,
    textures: HashMap<String, egui::TextureHandle>,
    history: Vec<Event>,
    pub(crate) open: bool,
    selected: Option<String>,
    books: BTreeMap<String, BookStats>,
    error: Option<String>,
    days: i64,
    query: String,
    status_filter: Option<Status>,
    finish_date: String,
    custom_start: String,
    custom_end: String,
    custom: bool,
}
impl Page {
    pub(crate) fn badge(&self, id: &str, language: AppLanguage) -> String {
        self.books.get(id).map_or_else(
            || Status::NotStarted.label(language).into(),
            |book| {
                if book.status == Status::Reading {
                    format!(
                        "{} · {:.1}%",
                        book.status.label(language),
                        book.progress * 100.0
                    )
                } else {
                    book.status.label(language).into()
                }
            },
        )
    }
    pub(crate) fn show_book(
        &mut self,
        book: &LibraryBook,
        library: &[LibraryBook],
        store: Option<&crate::sync::SyncStore>,
    ) {
        self.open(library, store);
        self.selected = Some(book.id.clone());
        self.finish_date = Local::now().format("%Y-%m-%d").to_string();
    }
    pub(crate) fn open(&mut self, library: &[LibraryBook], store: Option<&crate::sync::SyncStore>) {
        self.open = true;
        for book in library {
            if let Some(bytes) = &book.cover_bytes {
                self.covers.insert(book.id.clone(), bytes.clone());
            }
            if let Some(progress) = store.and_then(|s| s.load_progress(&book.id).ok().flatten()) {
                self.progress.insert(
                    book.id.clone(),
                    progress.locator.total_progression.unwrap_or(0.0),
                );
            }
            if let Some(annotations) = store.and_then(|s| s.annotations_for_book(&book.id).ok()) {
                let alive = annotations
                    .iter()
                    .filter(|a| a.deleted_at.is_none())
                    .collect::<Vec<_>>();
                self.annotations.insert(
                    book.id.clone(),
                    (
                        alive.len(),
                        alive
                            .iter()
                            .filter(|a| a.note.as_ref().is_some_and(|n| !n.is_empty()))
                            .count(),
                    ),
                );
            }
        }
        match database().and_then(|db| events(&db)) {
            Ok(existing) => {
                let known = aggregate(&existing);
                for book in library {
                    if known.get(&book.id).is_none_or(|old| {
                        old.title != book.title || old.authors != book.authors.join(", ")
                    }) {
                        record(
                            &book.id,
                            EventKind::Metadata {
                                title: book.title.clone(),
                                authors: book.authors.join(", "),
                                added: book.added_at,
                            },
                        );
                    }
                }
                flush();
                self.reload();
                for book in library {
                    let entry = self.books.entry(book.id.clone()).or_default();
                    if let Some(progress) =
                        store.and_then(|s| s.load_progress(&book.id).ok().flatten())
                    {
                        entry.progress = progress.locator.total_progression.unwrap_or(0.0);
                        if entry.status == Status::NotStarted
                            && entry.started.is_none()
                            && !entry.status_declared
                            && entry.progress > 0.0
                        {
                            entry.status = Status::Reading;
                        }
                    }
                }
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    fn reload(&mut self) {
        match database().and_then(|db| events(&db)) {
            Ok(events) => {
                self.books = aggregate(&events);
                self.history = events;
                for (id, value) in &self.progress {
                    if let Some(book) = self.books.get_mut(id) {
                        book.progress = *value;
                    }
                }
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub(crate) fn ui(&mut self, root: &mut egui::Ui, language: AppLanguage, blocked: bool) {
        use crate::ui::{Icon, dialog_action_button, icon_button, palette};
        if !blocked
            && root
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            if self.clear_confirm {
                self.clear_confirm = false;
            } else if self.selected.take().is_none() {
                self.open = false;
            }
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette().background)
                    .inner_margin(egui::Margin::symmetric(32, 24)),
            )
            .show(root, |ui| {
                ui.add_enabled_ui(!blocked, |ui| {
                    ui.horizontal(|ui| {
                        let back = if self.selected.is_some() {
                            language.text("返回概览", "Back to overview")
                        } else {
                            language.text("返回书架", "Back to library")
                        };
                        if dialog_action_button(ui, back, false).clicked() {
                            if self.selected.take().is_none() {
                                self.open = false;
                            }
                            self.clear_confirm = false;
                        }
                        ui.separator();
                        ui.label(
                            egui::RichText::new(language.text("阅读统计", "Reading statistics"))
                                .size(22.0)
                                .strong(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, Icon::Settings)
                                .on_hover_text(language.text("统计设置", "Statistics settings"))
                                .clicked()
                            {
                                self.settings_open = !self.settings_open;
                            }
                            if dialog_action_button(ui, language.text("刷新", "Refresh"), false)
                                .clicked()
                            {
                                flush();
                                self.reload();
                            }
                        });
                    });
                    ui.add_space(20.0);
                    if let Some(error) = &self.error {
                        ui.colored_label(palette().error_text, error);
                    }
                    if service().failed.load(Ordering::Relaxed) {
                        ui.colored_label(
                            palette().error_text,
                            language.text(
                                "部分统计保存失败，请检查磁盘。",
                                "Some statistics could not be saved. Check disk space.",
                            ),
                        );
                    }
                    if self.settings_open {
                        card().show(ui, |ui| {
                            ui.set_width((ui.available_width()).min(1050.0));
                            ui.label(
                                egui::RichText::new(
                                    language.text("统计设置", "Statistics settings"),
                                )
                                .strong(),
                            );
                            let mut enabled = service().enabled.load(Ordering::Relaxed);
                            let mut idle = service().idle.load(Ordering::Relaxed);
                            let mut changed = ui
                                .checkbox(
                                    &mut enabled,
                                    language.text("记录阅读时间", "Track reading time"),
                                )
                                .changed();
                            ui.horizontal_wrapped(|ui| {
                                ui.label(language.text("无操作后暂停", "Pause after inactivity"));
                                for seconds in [120, 300, 600] {
                                    if choice(ui, &format!("{} min", seconds / 60), idle == seconds)
                                        .clicked()
                                    {
                                        idle = seconds;
                                        changed = true;
                                    }
                                }
                            });
                            if changed {
                                service().enabled.store(enabled, Ordering::Relaxed);
                                service().idle.store(idle, Ordering::Relaxed);
                                let _ = service().sender.send(Write::Config(enabled, idle));
                            }
                            ui.label(
                                egui::RichText::new(language.text(
                                    "仅记录启用后的阅读时间；历史时长无法补算。",
                                    "Reading time before tracking was enabled is unavailable.",
                                ))
                                .small()
                                .color(palette().muted),
                            );
                        });
                        ui.add_space(16.0);
                    }
                    egui::ScrollArea::vertical()
                        .id_salt(("statistics-page", self.selected.clone()))
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_max_width(1120.0);
                            ui.spacing_mut().item_spacing.y = 12.0;
                            if let Some(id) = self.selected.clone() {
                                self.detail(ui, language, &id);
                            } else {
                                self.overview(ui, language);
                            }
                        });
                });
            });
    }

    fn overview(&mut self, ui: &mut egui::Ui, language: AppLanguage) {
        use crate::ui::palette;
        ui.horizontal_wrapped(|ui| {
            for (days, zh, en) in [
                (7, "最近7天", "Last 7 days"),
                (30, "最近30天", "Last 30 days"),
                (365, "今年", "This year"),
                (0, "全部", "All time"),
            ] {
                if choice(ui, language.text(zh, en), !self.custom && self.days == days).clicked() {
                    self.days = days;
                    self.custom = false;
                }
            }
            if choice(ui, language.text("自定义", "Custom"), self.custom).clicked() {
                self.custom = true;
                if self.custom_start.is_empty() {
                    self.custom_start =
                        (Local::now().date_naive() - chrono::Duration::days(29)).to_string();
                    self.custom_end = Local::now().date_naive().to_string();
                }
            }
        });
        if self.custom {
            ui.horizontal_wrapped(|ui| {
                ui.label(language.text("从", "From"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_start)
                        .desired_width(110.0)
                        .hint_text("YYYY-MM-DD"),
                );
                ui.label(language.text("至", "To"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_end)
                        .desired_width(110.0)
                        .hint_text("YYYY-MM-DD"),
                );
            });
        }
        let today = Local::now().date_naive();
        let start = if self.custom {
            NaiveDate::parse_from_str(&self.custom_start, "%Y-%m-%d").ok()
        } else if self.days == 365 {
            NaiveDate::from_ymd_opt(today.year(), 1, 1)
        } else if self.days > 0 {
            today.checked_sub_signed(chrono::Duration::days(self.days - 1))
        } else {
            None
        };
        let end = if self.custom {
            NaiveDate::parse_from_str(&self.custom_end, "%Y-%m-%d")
                .ok()
                .unwrap_or(today)
        } else {
            today
        };
        if self.custom
            && (start.is_none()
                || NaiveDate::parse_from_str(&self.custom_end, "%Y-%m-%d").is_err()
                || start.is_some_and(|s| s > end))
        {
            ui.colored_label(
                palette().error_text,
                language.text("请输入有效日期范围", "Enter a valid date range"),
            );
            return;
        }
        let in_range = |day: &str| {
            NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .is_ok_and(|d| start.is_none_or(|s| d >= s) && d <= end)
        };
        let intervals = self
            .books
            .values()
            .flat_map(|b| b.intervals.iter().copied())
            .collect::<Vec<_>>();
        let days = daily(&intervals);
        let total = days
            .iter()
            .filter(|(d, _)| in_range(d))
            .map(|(_, v)| *v)
            .sum();
        let valid = self
            .books
            .values()
            .flat_map(|b| b.valid_intervals.iter().copied())
            .collect::<Vec<_>>();
        let reading_days = daily(&valid).keys().filter(|d| in_range(d)).count();
        let finished = self
            .books
            .values()
            .filter(|b| b.status == Status::Finished && b.finished.as_deref().is_some_and(in_range))
            .count();
        let metrics = [
            (
                language.text("今日阅读", "Today"),
                duration(*days.get(&today.to_string()).unwrap_or(&0)),
            ),
            (
                language.text("期间阅读时长", "Reading time"),
                duration(total),
            ),
            (
                language.text("期间读完", "Books finished"),
                finished.to_string(),
            ),
            (
                language.text("期间阅读天数", "Reading days"),
                reading_days.to_string(),
            ),
        ];
        let columns = if ui.available_width() < 650.0 { 2 } else { 4 };
        for row in metrics.chunks(columns) {
            ui.columns(columns, |uis| {
                for (column, (label, value)) in uis.iter_mut().zip(row) {
                    card().show(column, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(egui::RichText::new(*label).color(palette().muted));
                        ui.label(egui::RichText::new(value).size(28.0).color(palette().text));
                    });
                }
            });
        }
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                egui::RichText::new(language.text("阅读趋势", "Reading trend"))
                    .size(17.0)
                    .strong(),
            );
            ui.label(
                egui::RichText::new(language.text(
                    "展示所选期间末尾30天，悬停查看每日时长",
                    "Last 30 days of the selected period. Hover for daily time.",
                ))
                .small()
                .color(palette().muted),
            );
            draw_trend(ui, &days, start, end);
        });
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                egui::RichText::new(language.text("最近读完", "Recently finished"))
                    .size(17.0)
                    .strong(),
            );
            let mut recent = self
                .books
                .iter()
                .filter(|(_, b)| {
                    b.status == Status::Finished && b.finished.as_deref().is_some_and(in_range)
                })
                .collect::<Vec<_>>();
            recent.sort_by_key(|(_, b)| std::cmp::Reverse(b.finished.clone()));
            if recent.is_empty() {
                empty_hint(
                    ui,
                    language.text(
                        "这个期间还没有读完记录",
                        "No books finished in this period.",
                    ),
                );
            }
            for (id, book) in recent.into_iter().take(5) {
                if book_row(
                    ui,
                    id,
                    book,
                    book.finished.as_deref().unwrap_or(""),
                    language,
                    self.covers.get(id).map(Vec::as_slice),
                    &mut self.textures,
                )
                .clicked()
                {
                    self.selected = Some(id.clone());
                    self.finish_date = book.finished.clone().unwrap_or_default();
                    self.clear_confirm = false;
                }
            }
        });
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(language.text("书籍阅读记录", "Books"))
                        .size(17.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{} {}",
                        self.books
                            .values()
                            .filter(|b| b.status == Status::Reading)
                            .count(),
                        language.text("本在读", "currently reading")
                    ))
                    .small()
                    .color(palette().muted),
                );
            });
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .desired_width(ui.available_width().min(320.0))
                    .hint_text(language.text("搜索书名或作者", "Search title or author")),
            );
            ui.horizontal_wrapped(|ui| {
                for status in [
                    None,
                    Some(Status::NotStarted),
                    Some(Status::Reading),
                    Some(Status::Finished),
                ] {
                    if choice(
                        ui,
                        status.map_or(language.text("全部", "All"), |s| s.label(language)),
                        self.status_filter == status,
                    )
                    .clicked()
                    {
                        self.status_filter = status;
                    }
                }
            });
            let query = self.query.trim().to_lowercase();
            let mut books = self
                .books
                .iter()
                .filter(|(_, b)| {
                    self.status_filter.is_none_or(|s| s == b.status)
                        && (b.title.to_lowercase().contains(&query)
                            || b.authors.to_lowercase().contains(&query))
                })
                .collect::<Vec<_>>();
            books.sort_by_key(|(_, b)| {
                std::cmp::Reverse(
                    b.days
                        .iter()
                        .filter(|(d, _)| in_range(d))
                        .map(|(_, v)| *v)
                        .sum::<u64>(),
                )
            });
            if books.is_empty() {
                empty_hint(ui, language.text("没有匹配的书籍", "No matching books."));
            }
            for (id, book) in books {
                let time = book
                    .days
                    .iter()
                    .filter(|(d, _)| in_range(d))
                    .map(|(_, v)| *v)
                    .sum();
                if book_row(
                    ui,
                    id,
                    book,
                    &duration(time),
                    language,
                    self.covers.get(id).map(Vec::as_slice),
                    &mut self.textures,
                )
                .clicked()
                {
                    self.selected = Some(id.clone());
                    self.finish_date = book.finished.clone().unwrap_or_else(|| today.to_string());
                    self.clear_confirm = false;
                }
            }
        });
    }
    fn detail(&mut self, ui: &mut egui::Ui, language: AppLanguage, id: &str) {
        if self.detail_key.as_deref() != Some(id) {
            self.detail_key = Some(id.into());
            self.status_draft = None;
            self.clear_confirm = false;
        }
        use crate::ui::{dialog_action_button, dialog_danger_button, palette};
        let Some(book) = self.books.get(id) else {
            empty_hint(
                ui,
                language.text("暂无书籍记录", "No book history available."),
            );
            return;
        };
        if !self.textures.contains_key(id) {
            if let Some(bytes) = self.covers.get(id) {
                if let Ok(image) = image::load_from_memory(bytes) {
                    let image = image.thumbnail(100, 150).to_rgba8();
                    self.textures.insert(
                        id.into(),
                        ui.ctx().load_texture(
                            format!("stats-{id}"),
                            egui::ColorImage::from_rgba_unmultiplied(
                                [image.width() as usize, image.height() as usize],
                                image.as_raw(),
                            ),
                            egui::TextureOptions::LINEAR,
                        ),
                    );
                }
            }
        }
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_top(|ui| {
                if let Some(texture) = self.textures.get(id) {
                    ui.image(texture);
                    ui.add_space(14.0);
                }
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&book.title).size(22.0).strong());
                    ui.label(egui::RichText::new(&book.authors).color(palette().muted));
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(book.status.label(language)).color(palette().accent),
                    );
                    ui.add(
                        egui::ProgressBar::new(book.progress as f32)
                            .desired_width(ui.available_width().min(340.0))
                            .text(format!("{:.1}%", book.progress * 100.0)),
                    );
                    ui.small(
                        egui::RichText::new(language.text(
                            "百分比为当前阅读位置",
                            "Percentage indicates current reading position",
                        ))
                        .color(palette().muted),
                    );
                });
            });
        });
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(duration(union_duration(&book.intervals))).size(26.0));
                ui.label(language.text("累计阅读", "total reading"));
                ui.add_space(20.0);
                ui.label(
                    egui::RichText::new(daily(&book.valid_intervals).len().to_string()).size(26.0),
                );
                ui.label(language.text("阅读天数", "reading days"));
                if let Some((highlights, notes)) = self.annotations.get(id) {
                    ui.label(format!(
                        " · {} {} · {} {}",
                        highlights,
                        language.text("高亮", "highlights"),
                        notes,
                        language.text("批注", "notes")
                    ));
                }
            });
            ui.add_space(12.0);
            egui::Grid::new(("stats-dates", id))
                .spacing(egui::vec2(28.0, 12.0))
                .show(ui, |ui| {
                    for (label, value) in [
                        (
                            language.text("加入书架", "Added"),
                            date((book.added > 0).then_some(book.added)),
                        ),
                        (language.text("开始阅读", "Started"), date(book.started)),
                        (language.text("最近阅读", "Last read"), date(book.last)),
                        (
                            language.text("读完日期", "Finished"),
                            book.finished.clone().unwrap_or_else(|| "—".into()),
                        ),
                    ] {
                        ui.label(egui::RichText::new(label).color(palette().muted));
                        ui.label(value);
                        ui.end_row();
                    }
                });
        });
        let current_status = book.status;
        let current_finished = book.finished.clone();
        let mut update = None;
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.collapsing(
                language.text("编辑阅读状态", "Edit reading status"),
                |ui| {
                    let draft = self.status_draft.get_or_insert(current_status);
                    ui.horizontal_wrapped(|ui| {
                        for status in [Status::NotStarted, Status::Reading, Status::Finished] {
                            if choice(ui, status.label(language), *draft == status).clicked() {
                                *draft = status;
                            }
                        }
                    });
                    if *draft == Status::Finished {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(language.text("读完日期", "Finished on"));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.finish_date)
                                    .desired_width(120.0)
                                    .hint_text("YYYY-MM-DD"),
                            );
                        });
                    }
                    let valid = *draft != Status::Finished
                        || NaiveDate::parse_from_str(&self.finish_date, "%Y-%m-%d")
                            .is_ok_and(|d| d <= Local::now().date_naive());
                    if !valid {
                        ui.colored_label(
                            palette().error_text,
                            language.text(
                                "请输入有效日期，不能晚于今天",
                                "Enter a valid date, no later than today.",
                            ),
                        );
                    }
                    let changed = *draft != current_status
                        || (*draft == Status::Finished
                            && current_finished.as_deref() != Some(&self.finish_date));
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled_ui(valid && changed, |ui| {
                                dialog_action_button(ui, language.text("保存", "Save"), true)
                            })
                            .inner
                            .clicked()
                        {
                            update = Some(*draft);
                        }
                        if dialog_action_button(
                            ui,
                            language.text("取消修改", "Discard changes"),
                            false,
                        )
                        .clicked()
                        {
                            *draft = current_status;
                            self.finish_date = current_finished
                                .clone()
                                .unwrap_or_else(|| Local::now().date_naive().to_string());
                        }
                    });
                },
            );
        });
        if let Some(status) = update {
            record(
                id,
                EventKind::Status {
                    status,
                    finished: (status == Status::Finished).then(|| self.finish_date.clone()),
                },
            );
            flush();
            self.reload();
            self.status_draft = None;
        }
        if let Some(book) = self.books.get(id) {
            card().show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    egui::RichText::new(language.text("阅读历史", "Reading history"))
                        .size(17.0)
                        .strong(),
                );
                if book.days.is_empty() {
                    empty_hint(
                        ui,
                        language.text(
                            "开始阅读后，每日时长会显示在这里",
                            "Your daily reading time will appear here.",
                        ),
                    );
                } else {
                    draw_trend(ui, &book.days, None, Local::now().date_naive());
                    ui.collapsing(language.text("查看每日明细", "Daily details"), |ui| {
                        egui::Grid::new(("stats-history", id))
                            .striped(true)
                            .spacing(egui::vec2(36.0, 10.0))
                            .show(ui, |ui| {
                                for (day, time) in book.days.iter().rev() {
                                    ui.label(day);
                                    ui.label(duration(*time));
                                    ui.end_row();
                                }
                            });
                    });
                }
            });
        }
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.collapsing(language.text("阅读会话", "Reading sessions"), |ui| {
                let mut sessions =
                    BTreeMap::<String, (u64, u64, Vec<(u64, u64, i32)>, f64, f64)>::new();
                for event in visible_history(&self.history)
                    .into_iter()
                    .filter(|e| e.book == id)
                {
                    if let EventKind::Reading {
                        session,
                        start,
                        end,
                        offset,
                        from,
                        to,
                    } = &event.kind
                    {
                        let row = sessions.entry(session.clone()).or_insert((
                            *start,
                            *end,
                            Vec::new(),
                            *from,
                            *to,
                        ));
                        row.1 = *end;
                        row.2.push((*start, *end, *offset));
                        row.4 = *to;
                    }
                }
                let mut rows = sessions.values().collect::<Vec<_>>();
                rows.sort_by_key(|r| std::cmp::Reverse(r.0));
                if rows.is_empty() {
                    empty_hint(
                        ui,
                        language.text("暂无阅读会话", "No reading sessions yet."),
                    );
                }
                for row in rows {
                    ui.label(format!(
                        "{} · {} · {:.1}% → {:.1}%",
                        date(Some(row.0)),
                        duration(union_duration(&row.2)),
                        row.3 * 100.0,
                        row.4 * 100.0
                    ));
                }
            });
        });
        ui.add_space(8.0);
        ui.collapsing(language.text("管理阅读记录","Manage reading history"),|ui| {
            ui.label(egui::RichText::new(language.text("清空统计会移除本书的阅读时间和完成记录，并同步至其他设备。书籍和批注会保留。","Clearing removes reading time and completion history across devices. The book and annotations remain.")).small().color(palette().muted));
            if !self.clear_confirm {
                if dialog_action_button(ui,language.text("清空本书统计…","Clear statistics…"),false).clicked() {self.clear_confirm=true;}
            } else {
                ui.horizontal(|ui| {
                    if dialog_danger_button(ui,language.text("确认清空","Confirm clear")).clicked() {record(id,EventKind::Clear);flush();self.reload();self.clear_confirm=false;self.status_draft=None;}
                    if dialog_action_button(ui,language.text("取消","Cancel"),false).clicked() {self.clear_confirm=false;}
                });
            }
        });
    }
}

pub(crate) async fn sync(
    webdav: &crate::sync::webdav::WebDavClient,
    device: &str,
) -> SyncResult<()> {
    flush();
    webdav.ensure_collection("statistics/").await?;
    for file in webdav.list_json_files("statistics/").await? {
        if file.contains('/') || file.contains('\\') {
            continue;
        }
        if let Some(remote) = webdav.get_optional(&format!("statistics/{file}")).await? {
            if remote.bytes.len() > 32 * 1024 * 1024 {
                return Err("Statistics shard too large".into());
            }
            let shard: Shard = serde_json::from_slice(&remote.bytes)?;
            if shard.version != 1 || shard.events.len() > 100_000 {
                return Err("Unsupported statistics shard".into());
            }
            insert(&mut database()?, &shard.events)?;
        }
    }
    let local = events(&database()?)?;
    let mut months = BTreeMap::<String, Vec<Event>>::new();
    for event in local.into_iter().filter(|e| e.device == device) {
        let month = Utc
            .timestamp_millis_opt(event.at as i64)
            .single()
            .ok_or("Invalid event timestamp")?
            .format("%Y-%m")
            .to_string();
        months.entry(month).or_default().push(event);
    }
    for (month, events) in months {
        webdav
            .put_mutable_json(
                &format!("statistics/{device}-{month}.json"),
                &Shard { version: 1, events },
            )
            .await?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Shard {
    version: u32,
    events: Vec<Event>,
}

fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(crate::ui::palette().surface)
        .stroke(egui::Stroke::new(1.0, crate::ui::palette().border))
        .corner_radius(10)
        .inner_margin(18)
}
fn choice(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let colors = crate::ui::palette();
    ui.add(
        egui::Button::new(egui::RichText::new(label).color(if selected {
            colors.accent
        } else {
            colors.text
        }))
        .fill(if selected {
            colors.accent_soft
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(egui::Stroke::NONE)
        .corner_radius(6)
        .min_size(egui::vec2(56.0, 32.0)),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}
fn empty_hint(ui: &mut egui::Ui, label: &str) {
    ui.add_space(12.0);
    ui.label(egui::RichText::new(label).color(crate::ui::palette().muted));
    ui.add_space(12.0);
}
fn book_row(
    ui: &mut egui::Ui,
    id: &str,
    book: &BookStats,
    value: &str,
    language: AppLanguage,
    cover: Option<&[u8]>,
    textures: &mut HashMap<String, egui::TextureHandle>,
) -> egui::Response {
    let colors = crate::ui::palette();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 68.0), egui::Sense::click());
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 6.0, colors.hovered_weak_fill);
    }
    let cover_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(10.0, 8.0), egui::vec2(36.0, 52.0));
    if ui.is_rect_visible(rect) {
        if !textures.contains_key(id)
            && let Some(bytes) = cover
            && let Ok(image) = image::load_from_memory(bytes)
        {
            let image = image.thumbnail(100, 150).to_rgba8();
            textures.insert(
                id.into(),
                ui.ctx().load_texture(
                    format!("stats-{id}"),
                    egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    ),
                    egui::TextureOptions::LINEAR,
                ),
            );
        }
        if let Some(texture) = textures.get(id) {
            let original = texture.size_vec2();
            let scale = (cover_rect.width() / original.x).min(cover_rect.height() / original.y);
            let image_rect = egui::Rect::from_center_size(cover_rect.center(), original * scale);
            egui::Image::new(texture)
                .corner_radius(3)
                .paint_at(ui, image_rect);
        } else {
            ui.painter()
                .rect_filled(cover_rect, 3.0, colors.surface_muted);
            crate::ui::paint_icon(
                ui,
                egui::Rect::from_center_size(cover_rect.center(), egui::vec2(18.0, 18.0)),
                crate::ui::Icon::BookOpen,
                colors.muted,
            );
        }
    }
    let narrow = rect.width() < 480.0;
    let reserve = if narrow { 100.0 } else { 240.0 };
    let title = if book.title.is_empty() {
        id
    } else {
        &book.title
    };
    let mut job = egui::text::LayoutJob::simple_singleline(
        title.into(),
        egui::FontId::proportional(14.0),
        colors.text,
    );
    job.wrap.max_width = (rect.width() - reserve - 70.0).max(40.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let title = ui.painter().layout_job(job);
    ui.painter()
        .galley(rect.min + egui::vec2(58.0, 10.0), title, colors.text);
    let subtitle = format!(
        "{} · {:.1}%",
        book.status.label(language),
        book.progress * 100.0
    );
    ui.painter().text(
        rect.min + egui::vec2(58.0, 42.0),
        egui::Align2::LEFT_CENTER,
        subtitle,
        egui::FontId::proportional(12.0),
        colors.muted,
    );
    ui.painter().text(
        egui::pos2(rect.right() - 24.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        value,
        egui::FontId::proportional(14.0),
        colors.text,
    );
    crate::ui::paint_icon(
        ui,
        egui::Rect::from_center_size(
            egui::pos2(rect.right() - 9.0, rect.center().y),
            egui::vec2(14.0, 14.0),
        ),
        crate::ui::Icon::ChevronRight,
        colors.muted,
    );
    response
        .on_hover_text(format!("{}\n{}", book.title, book.authors))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn draw_trend(
    ui: &mut egui::Ui,
    days: &BTreeMap<String, u64>,
    start: Option<NaiveDate>,
    end: NaiveDate,
) {
    let values = (0..30)
        .rev()
        .filter_map(|ago| {
            let day = end.checked_sub_signed(chrono::Duration::days(ago))?;
            start.is_none_or(|start| day >= start).then(|| {
                let text = day.to_string();
                let ms = *days.get(&text).unwrap_or(&0);
                (text, ms)
            })
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return;
    }
    let max = values.iter().map(|v| v.1).max().unwrap_or(1).max(1);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(850.0), 150.0),
        egui::Sense::hover(),
    );
    let width = rect.width() / values.len() as f32;
    for (index, (day, ms)) in values.iter().enumerate() {
        let x = rect.left() + index as f32 * width;
        let bar = egui::Rect::from_min_max(
            egui::pos2(
                x + 2.0,
                rect.bottom() - 130.0 * (*ms as f32 / max as f32).max(0.008),
            ),
            egui::pos2(x + width - 2.0, rect.bottom()),
        );
        ui.painter()
            .rect_filled(bar, 2.0, crate::ui::palette().accent);
        ui.interact(
            egui::Rect::from_min_max(
                egui::pos2(x, rect.top()),
                egui::pos2(x + width, rect.bottom()),
            ),
            ui.id().with(day),
            egui::Sense::hover(),
        )
        .on_hover_text(format!("{day}: {}", duration(*ms)));
    }
    ui.horizontal(|ui| {
        ui.small(&values[0].0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.small(&values[values.len() - 1].0);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overview_fits_narrow_and_wide_windows() {
        for width in [400.0, 1000.0] {
            let ctx = egui::Context::default();
            let mut page = Page::default();
            page.books.insert(
                "book".into(),
                BookStats {
                    title:
                        "A very long title about reading and typography repeated for narrow windows"
                            .into(),
                    ..Default::default()
                },
            );
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1800.0),
                    )),
                    ..Default::default()
                },
                |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        page.overview(ui, AppLanguage::default());
                        assert!(
                            ui.min_rect().right() <= width + 1.0,
                            "overview overflows at {width}: {:?}",
                            ui.min_rect()
                        );
                    });
                },
            );
            output.textures_delta.clear();
        }
    }
    #[test]
    fn overlap_and_midnight_are_counted_once() {
        assert_eq!(union_duration(&[(10, 30, 0), (20, 40, 0), (40, 50, 0)]), 40);
        let days = daily(&[(86_399_000, 86_401_000, 0)]);
        assert_eq!(days.values().copied().collect::<Vec<_>>(), vec![1000, 1000]);
    }
    #[test]
    fn duplicate_sync_is_idempotent() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE events(id TEXT PRIMARY KEY,device TEXT,at INTEGER,json TEXT)",
        )
        .unwrap();
        let event = Event {
            id: "a".into(),
            device: "d".into(),
            book: "b".into(),
            at: 1,
            kind: EventKind::Status {
                status: Status::Finished,
                finished: Some("2026-09-07".into()),
            },
        };
        insert(&mut db, &[event.clone()]).unwrap();
        insert(&mut db, &[event]).unwrap();
        assert_eq!(events(&db).unwrap().len(), 1);
    }

    #[test]
    fn completion_survives_rereading_and_short_sessions_do_not_start_a_book() {
        let reading = |id: &str, session: &str, start, end| Event {
            id: id.into(),
            device: "device".into(),
            book: "book".into(),
            at: end,
            kind: EventKind::Reading {
                session: session.into(),
                start,
                end,
                offset: 0,
                from: 0.8,
                to: 0.1,
            },
        };
        let short = reading("short", "short-session", 0, 20_000);
        assert_eq!(aggregate(&[short])["book"].status, Status::NotStarted);
        let entries = vec![
            reading("a", "one", 0, 15_000),
            reading("b", "one", 15_000, 30_000),
            Event {
                id: "c".into(),
                device: "device".into(),
                book: "book".into(),
                at: 31_000,
                kind: EventKind::Status {
                    status: Status::Finished,
                    finished: Some("2026-09-07".into()),
                },
            },
            reading("d", "two", 32_000, 62_000),
        ];
        let books = aggregate(&entries);
        assert_eq!(books["book"].status, Status::Finished);
        assert_eq!(books["book"].started, Some(0));
        assert_eq!(books["book"].finished.as_deref(), Some("2026-09-07"));
        assert_eq!(union_duration(&books["book"].intervals), 60_000);
    }

    #[test]
    fn time_zone_overlap_does_not_duplicate_personal_time() {
        let input = [(86_390_000, 86_410_000, 0), (86_390_000, 86_410_000, 28800)];
        assert_eq!(daily(&input).values().sum::<u64>(), 20_000);
    }

    #[test]
    fn invalid_sync_interval_rolls_back_entire_batch() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE events(id TEXT PRIMARY KEY,device TEXT,at INTEGER,json TEXT)",
        )
        .unwrap();
        let event = Event {
            id: "bad".into(),
            device: "device".into(),
            book: "book".into(),
            at: 10,
            kind: EventKind::Reading {
                session: "s".into(),
                start: 20,
                end: 10,
                offset: 0,
                from: 0.0,
                to: 0.0,
            },
        };
        assert!(insert(&mut db, &[event]).is_err());
        assert!(events(&db).unwrap().is_empty());
    }

    #[test]
    fn clear_marker_prevents_old_devices_from_restoring_history() {
        let meta = Event {
            id: "meta".into(),
            device: "a".into(),
            book: "book".into(),
            at: 1,
            kind: EventKind::Metadata {
                title: "Book".into(),
                authors: "Author".into(),
                added: 1,
            },
        };
        let finish = Event {
            id: "finish".into(),
            device: "b".into(),
            book: "book".into(),
            at: 2,
            kind: EventKind::Status {
                status: Status::Finished,
                finished: Some("2026-09-07".into()),
            },
        };
        let clear = Event {
            id: "clear".into(),
            device: "a".into(),
            book: "book".into(),
            at: 3,
            kind: EventKind::Clear,
        };
        let records = vec![meta, finish.clone(), clear, finish];
        let books = aggregate(&records);
        assert_eq!(books["book"].title, "Book");
        assert_eq!(books["book"].status, Status::NotStarted);
        assert!(books["book"].finished.is_none());
    }
}
