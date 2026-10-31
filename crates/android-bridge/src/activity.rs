use crate::*;
use rebook_app_core::statistics as stats;
#[derive(Clone, uniffi::Record)]
pub struct DayReading {
    pub day: String,
    pub milliseconds: u64,
}
#[derive(Clone, uniffi::Record)]
pub struct BookReading {
    pub id: String,
    pub title: String,
    pub milliseconds: u64,
    pub progress: f64,
    pub finished: bool,
}
#[derive(Clone, uniffi::Record)]
pub struct ReadingStatistics {
    pub today: u64,
    pub week: u64,
    pub days: u32,
    pub total: u64,
    pub books: Vec<BookReading>,
    pub distribution: Vec<DayReading>,
}
#[uniffi::export]
impl MobileCore {
    pub async fn statistics(self: Arc<Self>) -> Result<ReadingStatistics> {
        work(move || {
            stats::flush();
            let books = stats::aggregate(
                &stats::events(&stats::database().map_err(error)?).map_err(error)?,
            );
            let intervals = books
                .values()
                .flat_map(|b| b.intervals.iter().copied())
                .collect::<Vec<_>>();
            let days = stats::daily(&intervals);
            let range = stats::period::Period::Week.range(chrono::Local::now().date_naive(), 0);
            let distribution = stats::period::Period::Week
                .distribution(range, &days)
                .into_iter()
                .map(|(day, milliseconds)| DayReading {
                    day: day.to_string(),
                    milliseconds,
                })
                .collect::<Vec<_>>();
            let store = services::store()?;
            let library = LocalLibrary::load_from(self.root.join("library")).map_err(error)?;
            let mut rows = Vec::new();
            for book in library.books() {
                let stat = books.get(&book.id);
                let progress = store
                    .load_progress(&book.id)
                    .map_err(error)?
                    .and_then(|p| p.locator.total_progression)
                    .unwrap_or(0.0);
                rows.push(BookReading {
                    id: book.id.clone(),
                    title: book.title.clone(),
                    milliseconds: stat.map_or(0, |s| stats::union_duration(&s.intervals)),
                    progress,
                    finished: stat.is_some_and(|s| s.status == stats::Status::Finished),
                });
            }
            rows.sort_by(|a, b| b.milliseconds.cmp(&a.milliseconds));
            Ok(ReadingStatistics {
                today: days
                    .get(&chrono::Local::now().format("%Y-%m-%d").to_string())
                    .copied()
                    .unwrap_or(0),
                week: distribution.iter().map(|d| d.milliseconds).sum(),
                days: distribution.iter().filter(|d| d.milliseconds > 0).count() as u32,
                total: stats::union_duration(&intervals),
                books: rows,
                distribution,
            })
        })
        .await
    }
}
#[uniffi::export]
impl MobileReader {
    /// The host ticks only while the reader is resumed and unobscured. No UI
    /// state is published, so reading time never causes a repaint loop.
    pub async fn reading_activity(self: Arc<Self>, eligible: bool, activity: bool) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.tracker
                    .tick(eligible, activity, s.session.snapshot().total_progression);
                Ok(())
            })
        })
        .await
    }
    pub async fn mark_finished(self: Arc<Self>) -> Result<()> {
        work(move || {
            self.with(|s| {
                s.tracker.mark_finished();
                Ok(())
            })
        })
        .await
    }
}
