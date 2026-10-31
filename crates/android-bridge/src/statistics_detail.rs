use crate::*;
use rebook_app_core::statistics as stats;
#[derive(Clone, uniffi::Record)]
pub struct StatisticsPeriod {
    pub period: String,
    pub offset: i32,
    pub start: String,
    pub end: String,
    pub total: u64,
    pub days: u32,
    pub finished: u32,
    pub distribution: Vec<activity::DayReading>,
    pub books: Vec<activity::BookReading>,
}
#[derive(Clone, uniffi::Record)]
pub struct ReadingEvent {
    pub at: u64,
    pub end: u64,
    pub duration: u64,
    pub progress: f64,
    pub device: String,
    pub kind: String,
}
#[derive(Clone, uniffi::Record)]
pub struct BookStatistics {
    pub id: String,
    pub title: String,
    pub authors: String,
    pub status: String,
    pub added: u64,
    pub started: Option<u64>,
    pub last: Option<u64>,
    pub finished: Option<String>,
    pub total: u64,
    pub days: u32,
    pub progress: f64,
    pub highlights: u32,
    pub notes: u32,
    pub distribution: Vec<activity::DayReading>,
    pub history: Vec<ReadingEvent>,
}
fn period(value: &str) -> Result<stats::period::Period> {
    use stats::period::Period as P;
    match value {
        "week" => Ok(P::Week),
        "month" => Ok(P::Month),
        "year" => Ok(P::Year),
        "all" => Ok(P::All),
        _ => Err(error("统计周期无效")),
    }
}
#[uniffi::export]
impl MobileCore {
    pub async fn statistics_period(
        self: Arc<Self>,
        kind: String,
        offset: i32,
    ) -> Result<StatisticsPeriod> {
        work(move || {
            if !(-200..=0).contains(&offset) {
                return Err(error("统计周期超出范围"));
            }
            stats::flush();
            let all = stats::aggregate(
                &stats::events(&stats::database().map_err(error)?).map_err(error)?,
            );
            let p = period(&kind)?;
            let range = p.range(chrono::Local::now().date_naive(), offset);
            let intervals = all
                .values()
                .flat_map(|b| b.intervals.iter().copied())
                .collect::<Vec<_>>();
            let days = stats::daily(&intervals);
            let valid = stats::daily(
                &all.values()
                    .flat_map(|b| b.valid_intervals.iter().copied())
                    .collect::<Vec<_>>(),
            );
            let store = services::store()?;
            let mut rows = vec![];
            let mut finished = 0;
            for (id, b) in &all {
                if b.finished.as_ref().is_some_and(|d| range.contains(d)) {
                    finished += 1;
                }
                let ms = range.total(&b.days);
                if ms == 0 && b.finished.as_ref().is_none_or(|d| !range.contains(d)) {
                    continue;
                }
                let progress = store
                    .load_progress(id)
                    .map_err(error)?
                    .and_then(|p| p.locator.total_progression)
                    .unwrap_or(b.display_progress());
                rows.push(activity::BookReading {
                    id: id.clone(),
                    title: b.title.clone(),
                    milliseconds: ms,
                    progress: if b.status == stats::Status::Finished {
                        1.0
                    } else {
                        progress
                    },
                    finished: b.status == stats::Status::Finished,
                });
            }
            rows.sort_by(|a, b| b.milliseconds.cmp(&a.milliseconds).then(a.id.cmp(&b.id)));
            Ok(StatisticsPeriod {
                period: kind,
                offset,
                start: range.start.map_or_else(String::new, |d| d.to_string()),
                end: range.end.to_string(),
                total: range.total(&days),
                days: valid
                    .iter()
                    .filter(|(d, ms)| **ms > 0 && range.contains(d))
                    .count() as u32,
                finished,
                distribution: p
                    .distribution(range, &days)
                    .into_iter()
                    .map(|(d, m)| activity::DayReading {
                        day: d.to_string(),
                        milliseconds: m,
                    })
                    .collect(),
                books: rows,
            })
        })
        .await
    }
    pub async fn book_statistics(self: Arc<Self>, id: String) -> Result<BookStatistics> {
        work(move || {
            stats::flush();
            let events = stats::events(&stats::database().map_err(error)?).map_err(error)?;
            let all = stats::aggregate(&events);
            let b = all.get(&id).ok_or_else(|| error("未找到阅读记录"))?;
            let store = services::store()?;
            let annotations = store.annotations_for_book(&id).map_err(error)?;
            let progress = store
                .load_progress(&id)
                .map_err(error)?
                .and_then(|p| p.locator.total_progression)
                .unwrap_or(b.display_progress());
            let mut history = stats::visible_history(&events)
                .into_iter()
                .filter(|e| e.book == id)
                .filter_map(|e| match &e.kind {
                    stats::EventKind::Reading { start, end, to, .. } => Some(ReadingEvent {
                        at: *start,
                        end: *end,
                        duration: end - start,
                        progress: *to,
                        device: e.device.clone(),
                        kind: "reading".into(),
                    }),
                    stats::EventKind::Status { status, .. } => Some(ReadingEvent {
                        at: e.at,
                        end: e.at,
                        duration: 0,
                        progress: b.display_progress(),
                        device: e.device.clone(),
                        kind: match status {
                            stats::Status::Finished => "finished",
                            stats::Status::Reading => "reading-status",
                            stats::Status::NotStarted => "not-started",
                        }
                        .into(),
                    }),
                    _ => None,
                })
                .collect::<Vec<_>>();
            history.sort_by(|a, b| b.at.cmp(&a.at));
            history.truncate(200);
            Ok(BookStatistics {
                id,
                title: b.title.clone(),
                authors: b.authors.clone(),
                status: match b.status {
                    stats::Status::Finished => "finished",
                    stats::Status::Reading => "reading",
                    stats::Status::NotStarted => "not-started",
                }
                .into(),
                added: b.added,
                started: b.started,
                last: b.last,
                finished: b.finished.clone(),
                total: stats::union_duration(&b.intervals),
                days: stats::daily(&b.valid_intervals)
                    .values()
                    .filter(|ms| **ms > 0)
                    .count() as u32,
                progress: if b.status == stats::Status::Finished {
                    1.0
                } else {
                    progress
                },
                highlights: annotations.len() as u32,
                notes: annotations
                    .iter()
                    .filter(|n| n.note.as_ref().is_some_and(|v| !v.trim().is_empty()))
                    .count() as u32,
                distribution: b
                    .days
                    .iter()
                    .map(|(d, m)| activity::DayReading {
                        day: d.clone(),
                        milliseconds: *m,
                    })
                    .collect(),
                history,
            })
        })
        .await
    }
}
