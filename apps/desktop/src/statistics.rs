//! Local-first reading history. Immutable events are merged by ID, never by summing counters.
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
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

mod period;

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
    Flush(mpsc::Sender<()>),
}
struct Service {
    sender: mpsc::Sender<Write>,
    device: String,
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
    pub(crate) fn completion_summary(&mut self) -> SyncResult<(u64, Option<String>)> {
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

impl BookStats {
    fn display_progress(&self) -> f64 {
        if self.status == Status::Finished {
            1.0
        } else {
            self.progress.clamp(0.0, 1.0)
        }
    }
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
    period: period::Period,
    period_offset: i32,
    finish_date: String,
}
impl Page {
    pub(crate) fn shelf_snapshot(
        library: &[LibraryBook],
        store: Option<&crate::sync::SyncStore>,
    ) -> Result<Self, String> {
        let mut snapshot = Self::default();
        snapshot.open(library, store);
        if let Some(error) = snapshot.error.take() {
            return Err(error);
        }
        Ok(snapshot)
    }

    pub(crate) fn apply_shelf_snapshot(&mut self, snapshot: Self) {
        self.progress = snapshot.progress;
        self.annotations = snapshot.annotations;
        self.history = snapshot.history;
        self.books = snapshot.books;
        self.error = snapshot.error;
    }

    pub(crate) fn badge(&self, id: &str, language: AppLanguage) -> String {
        self.books.get(id).map_or_else(
            || Status::NotStarted.label(language).into(),
            |book| {
                if book.status == Status::Reading {
                    format!(
                        "{} · {:.1}%",
                        book.status.label(language),
                        book.display_progress() * 100.0
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
    pub(crate) fn ui(
        &mut self,
        root: &mut egui::Ui,
        language: AppLanguage,
        blocked: bool,
        return_to_shelf: egui::KeyboardShortcut,
    ) {
        use crate::ui::{Icon, dialog_action_button, icon_button, palette};
        if !blocked
            && !self.clear_confirm
            && root
                .ctx()
                .input_mut(|input| input.consume_shortcut(&return_to_shelf))
        {
            self.selected = None;
            self.open = false;
            root.ctx().request_repaint();
            return;
        }
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
                    .inner_margin(egui::Margin {
                        left: 24,
                        right: 24,
                        top: 28,
                        bottom: 28,
                    }),
            )
            .show(root, |ui| {
                let rect = ui.available_rect_before_wrap();
                let width = rect.width().min(800.0);
                let centered = egui::Rect::from_min_size(
                    egui::pos2(rect.center().x - width / 2.0, rect.top()),
                    egui::vec2(width, rect.height()),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(centered), |ui| {
                    ui.add_enabled_ui(!blocked, |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 44.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                if self.selected.is_some()
                                    && dialog_action_button(
                                        ui,
                                        language.text("返回概览", "Back to overview"),
                                        false,
                                    )
                                    .clicked()
                                {
                                    self.selected = None;
                                    self.clear_confirm = false;
                                }
                                ui.label(
                                    egui::RichText::new(
                                        language.text("阅读统计", "Reading statistics"),
                                    )
                                    .size(22.0)
                                    .strong(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if icon_button(ui, Icon::Library)
                                            .on_hover_text(
                                                language.text("返回书架", "Back to library"),
                                            )
                                            .clicked()
                                        {
                                            self.selected = None;
                                            self.clear_confirm = false;
                                            self.open = false;
                                        }
                                    },
                                );
                            },
                        );
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
                        egui::ScrollArea::vertical()
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .id_salt(("statistics-page", self.selected.clone()))
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.set_max_width(800.0);
                                ui.spacing_mut().item_spacing.y = 12.0;
                                if let Some(id) = self.selected.clone() {
                                    self.detail(ui, language, &id);
                                } else {
                                    self.overview(ui, language);
                                }
                            });
                    });
                });
            });
    }

    fn overview(&mut self, ui: &mut egui::Ui, language: AppLanguage) {
        use period::Period;

        let today = Local::now().date_naive();
        ui.horizontal_wrapped(|ui| {
            for (period, zh, en) in [
                (Period::Week, "周", "Week"),
                (Period::Month, "月", "Month"),
                (Period::Year, "年", "Year"),
                (Period::All, "总", "All"),
            ] {
                if choice(ui, language.text(zh, en), self.period == period).clicked() {
                    self.period = period;
                    self.period_offset = 0;
                }
            }
        });
        if self.period != Period::All {
            ui.horizontal_wrapped(|ui| {
                let (previous, next, current) = match self.period {
                    Period::Week => (
                        ("上一周", "Previous week"),
                        ("下一周", "Next week"),
                        ("本周", "This week"),
                    ),
                    Period::Month => (
                        ("上一月", "Previous month"),
                        ("下一月", "Next month"),
                        ("本月", "This month"),
                    ),
                    _ => (
                        ("上一年", "Previous year"),
                        ("下一年", "Next year"),
                        ("今年", "This year"),
                    ),
                };
                if crate::ui::icon_button(ui, crate::ui::Icon::ChevronLeft)
                    .on_hover_text(language.text(previous.0, previous.1))
                    .clicked()
                {
                    self.period_offset -= 1;
                }
                let range = self.period.range(today, self.period_offset);
                let start = range.start.unwrap();
                match self.period {
                    Period::Week => week_range_label(ui, start, range.end, today.year(), language),
                    Period::Month => {
                        period_range_label(ui, &[start.format("%Y/%m").to_string()]);
                    }
                    _ => {
                        period_range_label(ui, &[start.format("%Y").to_string()]);
                    }
                }
                if ui
                    .add_enabled_ui(self.period_offset < 0, |ui| {
                        crate::ui::icon_button(ui, crate::ui::Icon::ChevronRight)
                    })
                    .inner
                    .on_hover_text(language.text(next.0, next.1))
                    .clicked()
                {
                    self.period_offset += 1;
                }
                if self.period_offset < 0
                    && choice(ui, language.text(current.0, current.1), false).clicked()
                {
                    self.period_offset = 0;
                }
            });
        }
        let range = self.period.range(today, self.period_offset);
        let intervals = self
            .books
            .values()
            .flat_map(|b| b.intervals.iter().copied())
            .collect::<Vec<_>>();
        let days = daily(&intervals);
        let valid = self
            .books
            .values()
            .flat_map(|b| b.valid_intervals.iter().copied())
            .collect::<Vec<_>>();
        let reading_days = daily(&valid)
            .keys()
            .filter(|day| range.contains(day))
            .count();
        let finished = self
            .books
            .values()
            .filter(|b| {
                b.status == Status::Finished
                    && b.finished.as_deref().is_some_and(|day| range.contains(day))
            })
            .count();
        let books = longest_reading(&self.books, range);
        let metrics = [
            (
                language.text("阅读时长", "Reading time"),
                duration(range.total(&days)),
                "",
            ),
            (
                language.text("读完", "Finished"),
                finished.to_string(),
                language.text("本", "books"),
            ),
            (
                language.text("阅读", "Reading days"),
                reading_days.to_string(),
                language.text("天", "days"),
            ),
            (
                language.text("读过", "Books read"),
                books.len().to_string(),
                language.text("本", "books"),
            ),
        ];
        let columns = if ui.available_width() < 650.0 { 2 } else { 4 };
        for row in metrics.chunks(columns) {
            ui.columns(columns, |uis| {
                for (column, (label, value, unit)) in uis.iter_mut().zip(row) {
                    card().show(column, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(statistics_title(*label));
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 34.0),
                            egui::Layout::left_to_right(egui::Align::BOTTOM),
                            |ui| {
                                let job = if unit.is_empty() {
                                    reading_time_text(range.total(&days), language, 28.0)
                                } else {
                                    statistic_text(&[(value.clone(), *unit)], 28.0)
                                };
                                let galley = fitted_statistic_text(ui, job, ui.available_width());
                                ui.add(egui::Label::new(galley));
                            },
                        );
                    });
                }
            });
        }
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(statistics_title(
                language.text("阅读时长分布", "Reading time distribution"),
            ));
            draw_reading_distribution(
                ui,
                self.period,
                &self.period.distribution(range, &days),
                language,
            );
        });
        card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(statistics_title(
                language.text("阅读最久", "Most time spent reading"),
            ));
            if books.is_empty() {
                empty_hint(
                    ui,
                    language.text(
                        "这段时间还没有阅读记录",
                        "No reading recorded in this period.",
                    ),
                );
            }
            for (id, book, time) in books {
                if book_row(
                    ui,
                    id,
                    book,
                    time,
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
                        egui::ProgressBar::new(book.display_progress() as f32)
                            .desired_width(ui.available_width().min(340.0))
                            .text(format!("{:.1}%", book.display_progress() * 100.0)),
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

pub(crate) fn sync_due(
    webdav: &crate::sync::webdav::WebDavClient,
    force: bool,
) -> SyncResult<bool> {
    let last = webdav
        .cache_get("statistics:last-success")?
        .and_then(|value| String::from_utf8(value).ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    Ok(force || Utc::now().timestamp_millis().saturating_sub(last) >= 10 * 60 * 1000)
}

pub(crate) async fn sync(
    webdav: &crate::sync::webdav::WebDavClient,
    device: &str,
) -> SyncResult<()> {
    flush();
    sync_with_database(webdav, device, &mut database()?).await
}

async fn sync_with_database(
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
fn statistics_title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .size(crate::ui::scaled_font_size(14.0))
        .color(crate::ui::palette().muted)
}

fn week_range_label(
    ui: &mut egui::Ui,
    start: NaiveDate,
    end: NaiveDate,
    current_year: i32,
    language: AppLanguage,
) {
    let parts = [
        trend_date_label(start, current_year),
        language.text("至", "-").to_owned(),
        end.format(if start.year() == end.year() {
            "%m/%d"
        } else {
            "%Y/%m/%d"
        })
        .to_string(),
    ];
    period_range_label(ui, &parts);
}

fn period_range_text_y(
    center: f32,
    galley: &egui::Galley,
    digits: &egui::Galley,
    separator: bool,
) -> f32 {
    if separator {
        center - digits.size().y / 2.0 + digits.mesh_bounds.center().y
            - galley.mesh_bounds.center().y
    } else {
        center - galley.size().y / 2.0
    }
}

fn period_range_label(ui: &mut egui::Ui, parts: &[String]) -> egui::Response {
    let color = crate::ui::palette().muted;
    let galleys = parts
        .iter()
        .map(|part| {
            egui::WidgetText::from(statistics_title(part.clone())).into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::FontSelection::Default,
            )
        })
        .collect::<Vec<_>>();
    let width = galleys.iter().map(|galley| galley.size().x).sum::<f32>()
        + parts.len().saturating_sub(1) as f32 * 6.0;
    let height = galleys
        .iter()
        .map(|galley| galley.size().y)
        .fold(32.0_f32, f32::max);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let digits = egui::WidgetText::from(statistics_title("0123456789")).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::FontSelection::Default,
    );
    let mut x = rect.left();
    for (part, galley) in parts.iter().zip(galleys) {
        let y = period_range_text_y(
            rect.center().y,
            &galley,
            &digits,
            part == "-" || part == "至",
        );
        let advance = galley.size().x + 6.0;
        ui.painter().galley(egui::pos2(x, y), galley, color);
        x += advance;
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), parts.join(" "))
    });
    response
}

fn statistic_text(parts: &[(String, &str)], size: f32) -> egui::text::LayoutJob {
    let colors = crate::ui::palette();
    let mut job = egui::text::LayoutJob::default();
    for (index, (number, unit)) in parts.iter().enumerate() {
        job.append(
            number,
            if index == 0 { 0.0 } else { 3.0 },
            egui::TextFormat {
                font_id: egui::FontId::proportional(size),
                color: colors.text,
                valign: egui::Align::BOTTOM,
                ..Default::default()
            },
        );
        job.append(
            unit,
            3.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(size * 0.46),
                color: colors.text,
                valign: egui::Align::BOTTOM,
                ..Default::default()
            },
        );
    }
    job.wrap.max_rows = 1;
    job
}

fn reading_time_text(ms: u64, language: AppLanguage, size: f32) -> egui::text::LayoutJob {
    statistic_text(
        &[
            ((ms / 3_600_000).to_string(), language.text("小时", "hour")),
            (
                ((ms / 60_000) % 60).to_string(),
                language.text("分钟", "min"),
            ),
        ],
        size,
    )
}

fn fitted_statistic_text(
    ui: &egui::Ui,
    mut job: egui::text::LayoutJob,
    width: f32,
) -> Arc<egui::Galley> {
    let mut galley = ui.painter().layout_job(job.clone());
    if galley.size().x > width {
        let scale = width.max(1.0) / galley.size().x;
        for section in &mut job.sections {
            section.format.font_id.size *= scale;
            section.leading_space *= scale;
        }
        galley = ui.painter().layout_job(job.clone());
    }
    // Bottom alignment aligns line boxes, not font baselines. Give every
    // section the same descent so mixed font sizes share one baseline.
    let mut ascents = vec![0.0_f32; job.sections.len()];
    let mut descent = 0.0_f32;
    for (glyph, (byte_index, _)) in galley
        .rows
        .iter()
        .flat_map(|row| row.glyphs.iter())
        .zip(job.text.char_indices())
    {
        let ascent = glyph.font_face_ascent + 0.5 * (glyph.font_height - glyph.font_face_height);
        if let Some(index) = job.sections.iter().position(|section| {
            section.byte_range.start.0 <= byte_index && byte_index < section.byte_range.end.0
        }) {
            ascents[index] = ascent;
        }
        descent = descent.max(glyph.font_height - ascent);
    }
    for (section, ascent) in job.sections.iter_mut().zip(ascents) {
        section.format.line_height = Some(ascent + descent);
    }
    ui.painter().layout_job(job)
}

fn reading_row_text_positions(
    center_y: f32,
    time: &egui::Galley,
    title: &egui::Galley,
) -> (f32, f32) {
    let gap = 8.0;
    let top = center_y - (time.mesh_bounds.height() + gap + title.mesh_bounds.height()) / 2.0;
    (
        top - time.mesh_bounds.top(),
        top + time.mesh_bounds.height() + gap - title.mesh_bounds.top(),
    )
}

fn book_row(
    ui: &mut egui::Ui,
    id: &str,
    book: &BookStats,
    time: u64,
    language: AppLanguage,
    cover: Option<&[u8]>,
    textures: &mut HashMap<String, egui::TextureHandle>,
) -> egui::Response {
    let colors = crate::ui::palette();
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 88.0), egui::Sense::click());
    if response.hovered() || response.has_focus() {
        ui.painter()
            .rect_filled(rect, 6.0, colors.hovered_weak_fill);
    }
    let cover_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(10.0, 8.0), egui::vec2(48.0, 72.0));
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
    let content_left = rect.left() + 74.0;
    let content_width = (rect.right() - 28.0 - content_left).max(1.0);
    let time = reading_time_text(time, language, 20.0);
    let time = fitted_statistic_text(ui, time, content_width);
    let title = if book.title.is_empty() {
        id
    } else {
        &book.title
    };
    let mut job = egui::WidgetText::from(statistics_title(title))
        .into_layout_job(ui.style(), egui::FontSelection::Default, ui.text_valign())
        .as_ref()
        .clone();
    job.wrap.max_width = content_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let title = ui.painter().layout_job(job);
    let (top, title_top) = reading_row_text_positions(cover_rect.center().y, &time, &title);
    ui.painter()
        .galley(egui::pos2(content_left, top), time, colors.text);
    ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                egui::pos2(content_left, title_top),
                egui::vec2(content_width, title.size().y),
            ))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    )
    .add(
        egui::Label::new(statistics_title(if book.title.is_empty() {
            id
        } else {
            &book.title
        }))
        .truncate()
        .halign(egui::Align::Min),
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

fn longest_reading(
    books: &BTreeMap<String, BookStats>,
    range: period::DateRange,
) -> Vec<(&String, &BookStats, u64)> {
    let mut ranked = books
        .iter()
        .filter_map(|(id, book)| {
            let time = range.total(&book.days);
            (time > 0).then_some((id, book, time))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(_, _, time)| std::cmp::Reverse(*time));
    ranked
}

fn distribution_axis_label(
    period: period::Period,
    date: NaiveDate,
    language: AppLanguage,
) -> String {
    use period::Period;
    match period {
        Period::Week => {
            let labels = [
                ("一", "Mon"),
                ("二", "Tue"),
                ("三", "Wed"),
                ("四", "Thu"),
                ("五", "Fri"),
                ("六", "Sat"),
                ("日", "Sun"),
            ];
            let (zh, en) = labels[date.weekday().num_days_from_monday() as usize];
            language.text(zh, en).into()
        }
        Period::Month if date.day() == 1 || date.day().is_multiple_of(5) => date.day().to_string(),
        Period::Month => String::new(),
        Period::Year => date.month().to_string(),
        Period::All => date.year().to_string(),
    }
}

const DISTRIBUTION_INTERVALS: u32 = 3;

fn distribution_tick_step(max_ms: u64) -> f64 {
    let target_minutes = max_ms as f64 / 60_000.0 * 1.08 / f64::from(DISTRIBUTION_INTERVALS);
    for minutes in [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 180.0, 300.0, 600.0, 1200.0,
    ] {
        if minutes >= target_minutes {
            return minutes * 60_000.0;
        }
    }
    let hours = target_minutes / 60.0;
    let magnitude = 10.0_f64.powf(hours.log10().floor());
    let factor = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|factor| factor * magnitude >= hours)
        .unwrap();
    factor * magnitude * 3_600_000.0
}

fn distribution_tick_label(ms: f64, step_ms: f64) -> String {
    if ms == 0.0 {
        return "0".into();
    }
    let (value, unit) = if step_ms >= 3_600_000.0 {
        (ms / 3_600_000.0, "h")
    } else {
        (ms / 60_000.0, "min")
    };
    let value = format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned();
    format!("{value}{unit}")
}

fn draw_reading_distribution(
    ui: &mut egui::Ui,
    period: period::Period,
    values: &[(NaiveDate, u64)],
    language: AppLanguage,
) {
    let colors = crate::ui::palette();
    let max = values.iter().map(|(_, ms)| *ms).max().unwrap_or(0);
    let step = distribution_tick_step(max);
    let ceiling = step * f64::from(DISTRIBUTION_INTERVALS);
    let tick_labels = (0..=DISTRIBUTION_INTERVALS)
        .map(|tick| distribution_tick_label(step * f64::from(tick), step))
        .collect::<Vec<_>>();
    let font = egui::FontId::proportional(11.0);
    let axis_width = tick_labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap(label.clone(), font.clone(), colors.muted)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
        + 14.0;
    let (chart, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 240.0),
        egui::Sense::hover(),
    );
    let plot = egui::Rect::from_min_max(
        chart.min,
        egui::pos2(
            (chart.right() - axis_width).max(chart.left() + 1.0),
            chart.bottom(),
        ),
    );
    let bottom = chart.bottom() - 26.0;
    let plot_height = 190.0;
    for tick in 0..=DISTRIBUTION_INTERVALS {
        if max == 0 && tick > 0 {
            continue;
        }
        let y = bottom - plot_height * tick as f32 / DISTRIBUTION_INTERVALS as f32;
        ui.painter().extend(egui::Shape::dashed_line(
            &[egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            egui::Stroke::new(
                1.0,
                colors
                    .muted
                    .gamma_multiply(if tick == 0 { 0.25 } else { 0.12 }),
            ),
            4.0,
            4.0,
        ));
        ui.painter().text(
            egui::pos2(plot.right() + 10.0, y),
            egui::Align2::LEFT_CENTER,
            &tick_labels[tick as usize],
            font.clone(),
            colors.muted,
        );
    }
    if max == 0 {
        ui.painter().text(
            egui::pos2(plot.center().x, bottom - plot_height / 2.0),
            egui::Align2::CENTER_CENTER,
            language.text("暂无阅读记录", "No reading records."),
            egui::FontId::proportional(14.0),
            colors.muted,
        );
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(plot), |ui| {
        egui::ScrollArea::horizontal()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .id_salt(("reading-time-distribution", period as u8))
            .show(ui, |ui| {
                let minimum_slot = match period {
                    period::Period::Month => 8.0,
                    period::Period::All => 66.0,
                    _ => 28.0,
                };
                let width = plot.width().max(values.len() as f32 * minimum_slot);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(width, 240.0), egui::Sense::hover());
                let slot = width / values.len().max(1) as f32;
                for (index, (month, ms)) in values.iter().enumerate() {
                    let x = rect.left() + (index as f32 + 0.5) * slot;
                    let height = plot_height * (*ms as f64 / ceiling) as f32;
                    let bar = egui::Rect::from_min_max(
                        egui::pos2(x - (slot * 0.3).min(24.0), bottom - height.max(1.0)),
                        egui::pos2(x + (slot * 0.3).min(24.0), bottom),
                    );
                    if *ms > 0 {
                        ui.painter().rect_filled(bar, 3.0, colors.accent);
                    }
                    ui.painter().text(
                        egui::pos2(x, bottom + 16.0),
                        egui::Align2::CENTER_CENTER,
                        distribution_axis_label(period, *month, language),
                        egui::FontId::proportional(11.0),
                        colors.muted,
                    );
                    let response = ui.interact(
                        egui::Rect::from_min_max(
                            egui::pos2(x - slot / 2.0, rect.top()),
                            egui::pos2(x + slot / 2.0, rect.bottom()),
                        ),
                        ui.id().with(month),
                        egui::Sense::hover(),
                    );
                    let mut tooltip = egui::Tooltip::for_enabled(&response);
                    tooltip.popup = tooltip
                        .popup
                        .anchor(bar)
                        .align(egui::emath::RectAlign::TOP)
                        .align_alternatives(&[egui::emath::RectAlign::TOP])
                        .gap(6.0);
                    tooltip.show(|ui| {
                        ui.label(
                            month
                                .format(match period {
                                    period::Period::Week | period::Period::Month => "%Y/%m/%d",
                                    period::Period::All => "%Y",
                                    _ => "%Y/%m",
                                })
                                .to_string(),
                        );
                        let job = reading_time_text(*ms, language, 22.0);
                        let galley = fitted_statistic_text(ui, job, 260.0);
                        ui.add(egui::Label::new(galley));
                    });
                }
            });
    });
}

fn trend_date_label(day: NaiveDate, current_year: i32) -> String {
    day.format(if day.year() == current_year {
        "%m/%d"
    } else {
        "%Y/%m/%d"
    })
    .to_string()
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
                (day, ms)
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
    let current_year = Local::now().year();
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
        .on_hover_text(format!(
            "{}: {}",
            trend_date_label(*day, current_year),
            duration(*ms)
        ));
    }
    ui.allocate_ui_with_layout(
        egui::vec2(rect.width(), ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.small(trend_date_label(values[0].0, current_year));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.small(trend_date_label(values[values.len() - 1].0, current_year));
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_digits_keep_the_same_baseline_with_or_without_month() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let layout = |text: &str| {
                    egui::WidgetText::from(statistics_title(text)).into_galley(
                        ui,
                        Some(egui::TextWrapMode::Extend),
                        f32::INFINITY,
                        egui::FontSelection::Default,
                    )
                };
                let digits = layout("0123456789");
                let year = layout("2026");
                let expected =
                    period_range_text_y(16.0, &year, &digits, false) + year.rows[0].glyphs[0].pos.y;
                for text in ["2026/09", "09/07", "2025/12/29"] {
                    let date = layout(text);
                    let baseline = period_range_text_y(16.0, &date, &digits, false)
                        + date.rows[0].glyphs[0].pos.y;
                    assert!((baseline - expected).abs() < 0.01);
                }
                let separator = layout("-");
                let center = period_range_text_y(16.0, &separator, &digits, true)
                    + separator.mesh_bounds.center().y;
                assert!(
                    (center - (16.0 - digits.size().y / 2.0 + digits.mesh_bounds.center().y)).abs()
                        < 0.01
                );
            });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn period_range_rows_keep_the_same_height() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let mut heights = Vec::new();
                for parts in [
                    vec!["09/07", "-", "09/13"],
                    vec!["2026/09"],
                    vec!["2026"],
                    vec!["All time"],
                ] {
                    let parts = parts.into_iter().map(str::to_owned).collect::<Vec<_>>();
                    ui.horizontal(|ui| {
                        ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
                        heights.push(period_range_label(ui, &parts).rect.height());
                    });
                }
                assert!(
                    heights
                        .iter()
                        .all(|height| (*height - heights[0]).abs() < 0.01)
                );
            });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn reading_row_centers_visible_text_and_keeps_an_eight_point_gap() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                for width in [120.0, 300.0] {
                    let time = fitted_statistic_text(
                        ui,
                        reading_time_text(123_456_789, AppLanguage::English, 20.0),
                        width,
                    );
                    let title = ui.painter().layout_no_wrap(
                        "Typography and reading".into(),
                        egui::TextStyle::Body.resolve(ui.style()),
                        crate::ui::palette().muted,
                    );
                    let (time_y, title_y) = reading_row_text_positions(44.0, &time, &title);
                    let top = time_y + time.mesh_bounds.top();
                    let bottom = title_y + title.mesh_bounds.bottom();
                    assert!(((top + bottom) / 2.0 - 44.0).abs() < 0.01);
                    assert!(
                        (title_y + title.mesh_bounds.top()
                            - time_y
                            - time.mesh_bounds.bottom()
                            - 8.0)
                            .abs()
                            < 0.01
                    );
                }
            });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn distribution_scale_leaves_headroom_from_short_sessions_to_years() {
        for max in [
            0,
            1_000,
            60_000,
            900_000,
            3_600_000,
            90_000_000,
            90_000_000_000,
        ] {
            let step = distribution_tick_step(max);
            assert!(step > 0.0);
            assert!(step * f64::from(DISTRIBUTION_INTERVALS) >= max as f64 * 1.08);
            let labels = (0..=DISTRIBUTION_INTERVALS)
                .map(|tick| distribution_tick_label(step * f64::from(tick), step))
                .collect::<Vec<_>>();
            assert_eq!(labels.len(), 4);
            assert!(labels.iter().all(|label| !label.contains('.')));
            assert!(labels.windows(2).all(|pair| pair[0] != pair[1]));
        }
        assert_eq!(distribution_tick_label(0.0, 60_000.0), "0");
        assert_eq!(distribution_tick_label(900_000.0, 300_000.0), "15min");
        assert_eq!(distribution_tick_label(5_400_000.0, 1_800_000.0), "90min");
    }

    #[test]
    fn statistic_numbers_and_units_share_a_baseline_after_fitting() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                for width in [120.0, 300.0] {
                    let mut job = reading_time_text(123_456_789, AppLanguage::English, 28.0);
                    let unit_format = job.sections.last().unwrap().format.clone();
                    job.append("37.2%", 18.0, unit_format);
                    let galley = fitted_statistic_text(ui, job, width);
                    assert!(galley.size().x <= width + 1.0);
                    let glyphs = &galley.rows[0].glyphs;
                    let baseline = glyphs[0].pos.y;
                    assert!(
                        glyphs
                            .iter()
                            .all(|glyph| (glyph.pos.y - baseline).abs() <= 1.0)
                    );
                }
            });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn distribution_labels_follow_selected_granularity() {
        use period::Period;
        let language = AppLanguage::SimplifiedChinese;
        let today = NaiveDate::from_ymd_opt(2026, 2, 11).unwrap();
        let labels = |period: Period| {
            period
                .distribution(period.range(today, 0), &BTreeMap::new())
                .iter()
                .map(|(date, _)| distribution_axis_label(period, *date, language))
                .filter(|label| !label.is_empty())
                .collect::<Vec<_>>()
        };
        assert_eq!(Page::default().period, Period::Week);
        assert_eq!(
            labels(Period::Week),
            ["一", "二", "三", "四", "五", "六", "日"]
        );
        assert_eq!(labels(Period::Month), ["1", "5", "10", "15", "20", "25"]);
        assert_eq!(
            labels(Period::Year),
            (1..=12).map(|month| month.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(
            distribution_axis_label(
                Period::Month,
                NaiveDate::from_ymd_opt(2026, 1, 30).unwrap(),
                language
            ),
            "30"
        );
        assert_eq!(
            distribution_axis_label(
                Period::Month,
                NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
                language
            ),
            ""
        );
    }

    #[test]
    fn longest_reading_uses_selected_period_and_excludes_books_without_time() {
        let mut books = BTreeMap::new();
        for (id, days) in [
            ("old", vec![("2025-12-31", 900_000)]),
            (
                "first",
                vec![("2026-09-01", 120_000), ("2026-09-30", 180_000)],
            ),
            (
                "second",
                vec![("2026-09-11", 240_000), ("2026-10-01", 900_000)],
            ),
            ("empty", vec![]),
            ("zero", vec![("2026-09-11", 0)]),
        ] {
            books.insert(
                id.into(),
                BookStats {
                    days: days.into_iter().map(|(d, ms)| (d.into(), ms)).collect(),
                    ..Default::default()
                },
            );
        }
        let today = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
        let ranked = longest_reading(&books, period::Period::Month.range(today, 0));
        assert_eq!(
            ranked
                .iter()
                .map(|(id, _, ms)| (id.as_str(), *ms))
                .collect::<Vec<_>>(),
            [("first", 300_000), ("second", 240_000)]
        );
        let ranked = longest_reading(&books, period::Period::All.range(today, 0));
        assert_eq!(
            ranked
                .iter()
                .map(|(id, _, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["old", "second", "first"]
        );
    }

    #[test]
    fn finished_books_show_full_progress_without_overwriting_resume_position() {
        let mut book = BookStats {
            status: Status::Finished,
            progress: 0.37,
            ..Default::default()
        };
        assert_eq!(book.display_progress(), 1.0);
        assert_eq!(book.progress, 0.37);
        book.status = Status::Reading;
        assert_eq!(book.display_progress(), 0.37);
        book.status = Status::Finished;
        book.progress = 0.0;
        assert_eq!(book.display_progress(), 1.0);
    }

    #[test]
    fn background_snapshot_preserves_open_statistics_controls() {
        let mut page = Page {
            open: true,
            selected: Some("book".into()),
            period: period::Period::Month,
            period_offset: -2,
            clear_confirm: true,
            ..Default::default()
        };
        page.covers.insert("book".into(), vec![1, 2, 3]);
        let mut snapshot = Page::default();
        snapshot.books.insert(
            "book".into(),
            BookStats {
                title: "Updated title".into(),
                ..Default::default()
            },
        );
        page.apply_shelf_snapshot(snapshot);
        assert!(page.open);
        assert!(page.clear_confirm);
        assert_eq!(page.selected.as_deref(), Some("book"));
        assert_eq!(page.period, period::Period::Month);
        assert_eq!(page.period_offset, -2);
        assert_eq!(page.covers["book"], [1, 2, 3]);
        assert_eq!(page.books["book"].title, "Updated title");
    }

    #[test]
    fn statistics_sync_uploads_only_changed_months_and_repairs_deleted_shards() {
        let server = crate::sync::FakeWebDav::start();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut settings = crate::sync::SyncSettings::new_device();
        settings.provider = crate::sync::CloudProviderKind::Custom;
        settings.base_url = server.base_url();
        settings.username = "reader".into();
        let path =
            std::env::temp_dir().join(format!("stats-sync-cache-{}.sqlite", uuid::Uuid::new_v4()));
        let store =
            crate::sync::SyncStore::open_at(path.clone(), settings.device_id.clone()).unwrap();
        let client = crate::sync::webdav::WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store, crate::sync::account_key(&settings));
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE events(id TEXT PRIMARY KEY,device TEXT,at INTEGER,json TEXT)",
        )
        .unwrap();
        let event = |id: &str, month| Event {
            id: id.into(),
            device: settings.device_id.clone(),
            book: "book".into(),
            at: Utc
                .with_ymd_and_hms(2026, month, 5, 0, 0, 0)
                .unwrap()
                .timestamp_millis() as u64,
            kind: EventKind::Clear,
        };
        insert(&mut db, &[event("one", 1), event("two", 2)]).unwrap();
        assert!(sync_due(&client, false).unwrap());
        runtime
            .block_on(sync_with_database(&client, &settings.device_id, &mut db))
            .unwrap();
        assert_eq!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, _, _)| method == "PUT")
                .count(),
            2
        );
        assert!(!sync_due(&client, false).unwrap());
        assert!(sync_due(&client, true).unwrap());
        server.requests.lock().unwrap().clear();
        runtime
            .block_on(sync_with_database(&client, &settings.device_id, &mut db))
            .unwrap();
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(method, _, _)| method != "PUT")
        );
        insert(&mut db, &[event("three", 2)]).unwrap();
        server.requests.lock().unwrap().clear();
        runtime
            .block_on(sync_with_database(&client, &settings.device_id, &mut db))
            .unwrap();
        let puts = server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _, _)| method == "PUT")
            .map(|(_, path, _)| path.clone())
            .collect::<Vec<_>>();
        assert_eq!(puts.len(), 1);
        assert!(puts[0].ends_with("-2026-02.json"));
        server
            .objects
            .lock()
            .unwrap()
            .retain(|path, _| !path.ends_with("-2026-01.json"));
        server.requests.lock().unwrap().clear();
        runtime
            .block_on(sync_with_database(&client, &settings.device_id, &mut db))
            .unwrap();
        let puts = server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _, _)| method == "PUT")
            .map(|(_, path, _)| path.clone())
            .collect::<Vec<_>>();
        assert_eq!(puts.len(), 1);
        assert!(puts[0].ends_with("-2026-01.json"));
        drop(client);
        let _ = std::fs::remove_file(path);
        server.stop();
    }
    #[test]
    fn trend_dates_omit_only_the_current_year() {
        assert_eq!(
            trend_date_label(NaiveDate::from_ymd_opt(2026, 9, 7).unwrap(), 2026),
            "09/07"
        );
        assert_eq!(
            trend_date_label(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(), 2026),
            "2025/12/31"
        );
        assert_eq!(
            trend_date_label(NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(), 2026),
            "2027/01/01"
        );
    }
    #[test]
    fn overview_fits_narrow_and_wide_windows() {
        for (width, period) in [400.0, 700.0, 1000.0].into_iter().flat_map(|width| {
            [
                period::Period::Week,
                period::Period::Month,
                period::Period::Year,
                period::Period::All,
            ]
            .map(move |period| (width, period))
        }) {
            let ctx = egui::Context::default();
            let mut page = Page {
                period,
                ..Default::default()
            };
            let end = now_ms();
            let intervals = vec![(end - 3_600_000 * 12_345, end, 0)];
            page.books.insert(
                "book".into(),
                BookStats {
                    title:
                        "A very long title about reading and typography repeated for narrow windows"
                            .into(),
                    days: daily(&intervals),
                    valid_intervals: intervals.clone(),
                    intervals,
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
