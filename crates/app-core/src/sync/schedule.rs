use std::time::{Duration, Instant};

use super::{SyncMode, SyncReport};
use serde::{Deserialize, Serialize};

const FULL_CHECK_INTERVAL: Duration = Duration::from_secs(20 * 60);
const READING_QUIET_MS: u64 = 15_000;
const MAX_READING_DELAY: Duration = Duration::from_secs(60);
const RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30 * 60);

/// Portable deadlines for hosts whose process can be reclaimed between wakes.
/// No credentials or book contents are stored here.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScheduleSnapshot {
    saved_at_ms: u64,
    full_age_ms: u64,
    pending_age_ms: Option<u64>,
    retry_in_ms: u64,
    queued: Option<SyncMode>,
    failures: u32,
    rate_limit_in_ms: Option<u64>,
    statistics_retry_in_ms: Option<u64>,
    statistics_failures: u32,
    derived_token: Option<String>,
    #[serde(default)]
    current_age_ms: u64,
    #[serde(default)]
    current_requested: bool,
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSchedule {
    pub derived_token: Option<String>,
    last_full: Instant,
    pending_since: Option<Instant>,
    retry_after: Instant,
    queued: Option<SyncMode>,
    failures: u32,
    rate_limited_until: Option<Instant>,
    statistics_retry: Option<Instant>,
    statistics_failures: u32,
    reading_deadline: Option<Instant>,
    annotations_deadline: Option<Instant>,
    last_current: Instant,
    current_requested: bool,
    current_active: bool,
    current_book: Option<String>,
    seen_network: u64,
    seen_flush: u64,
    seen_activation: u64,
    seen_library: u64,
}

impl Default for SyncSchedule {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl SyncSchedule {
    pub fn new(now: Instant) -> Self {
        Self {
            derived_token: None,
            last_full: now,
            pending_since: None,
            retry_after: now,
            queued: None,
            failures: 0,
            rate_limited_until: None,
            statistics_retry: None,
            statistics_failures: 0,
            reading_deadline: None,
            annotations_deadline: None,
            last_current: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            current_requested: false,
            current_active: false,
            current_book: None,
            seen_network: 0,
            seen_flush: 0,
            seen_activation: 0,
            seen_library: 0,
        }
    }

    pub fn start_requested(&mut self, mode: SyncMode, now: Instant) -> bool {
        if self.rate_limit_deadline(now).is_some() {
            self.request(mode);
            return false;
        }
        self.started(mode);
        true
    }

    pub fn observe_derived(&mut self, token: Option<String>) -> bool {
        let changed = token.is_some() && token != self.derived_token;
        self.derived_token = token;
        if changed {
            self.request(SyncMode::Derived);
        }
        changed
    }

    /// One completion policy for success, ordinary errors, quotas and a
    /// statistics-only failure after an otherwise successful library sync.
    pub fn finished(
        &mut self,
        mode: SyncMode,
        result: Result<&SyncReport, Option<Duration>>,
        now: Instant,
    ) {
        self.completed_with_limit(
            mode,
            result.is_ok(),
            now,
            result.as_ref().err().copied().flatten(),
        );
        if let Ok(report) = result {
            if report.statistics_error.is_some() {
                self.completed_with_limit(
                    SyncMode::Statistics,
                    false,
                    now,
                    report.statistics_rate_limit,
                );
            } else if matches!(mode, SyncMode::Full { .. }) {
                self.completed(SyncMode::Statistics, true, now);
            }
        }
    }

    /// Bind process-local counters after restoring durable deadlines. These
    /// counters restart with the process and are not durable change versions.
    pub fn attach_signal(&mut self, signal: &super::SyncSignal) {
        self.current_active = signal.active.is_some();
        self.current_book = signal.active.clone();
        self.seen_activation = signal.activation;
        self.seen_library = signal.library;
        self.seen_network = signal.network;
        self.seen_flush = signal.flush;
    }
    pub fn observe_signal(&mut self, signal: &super::SyncSignal, now: Instant) {
        let changed_book = self.current_book != signal.active;
        self.current_book = signal.active.clone();
        self.current_active = signal.active.is_some();
        if signal.flush != self.seen_flush {
            self.seen_flush = signal.flush;
            self.request(SyncMode::Reading);
        }
        if signal.network != self.seen_network {
            self.seen_network = signal.network;
            self.request(if self.current_active {
                SyncMode::Current
            } else {
                SyncMode::Full {
                    force_statistics: false,
                }
            });
        }
        if signal.activation != self.seen_activation {
            self.seen_activation = signal.activation;
            if self.current_active
                && (changed_book
                    || now.saturating_duration_since(self.last_current) >= Duration::from_secs(10))
            {
                self.current_requested = true;
            }
        }
        if signal.library != self.seen_library {
            self.seen_library = signal.library;
            self.request(SyncMode::Full {
                force_statistics: false,
            });
        }
    }
    pub fn observe_annotations(&mut self, latest: Option<u64>, now: Instant, now_ms: u64) {
        self.annotations_deadline = latest.map(|at| {
            now + Duration::from_millis(3000u64.saturating_sub(now_ms.saturating_sub(at)))
        });
    }
    /// Sleep until the next real deadline. Local commits wake the host earlier.
    pub fn poll_delay(&self, now: Instant) -> Duration {
        if now < self.retry_after {
            return self.retry_after - now;
        }
        if self.queued.is_some() || self.current_requested {
            return Duration::from_millis(1);
        }
        let mut deadline = self.last_full + FULL_CHECK_INTERVAL;
        for at in [
            self.reading_deadline,
            self.annotations_deadline,
            self.statistics_retry,
            self.current_active
                .then_some(self.last_current + Duration::from_secs(60)),
        ]
        .into_iter()
        .flatten()
        {
            deadline = deadline.min(at);
        }
        deadline
            .saturating_duration_since(now)
            .max(Duration::from_millis(1))
    }

    /// The platform may schedule a background wake at or after this deadline.
    pub fn retry_delay(&self, now: Instant) -> Option<Duration> {
        let ordinary = (self.queued.is_some() || self.current_requested)
            .then(|| self.retry_after.saturating_duration_since(now));
        let statistics = self
            .statistics_retry
            .map(|at| at.max(self.retry_after).saturating_duration_since(now));
        ordinary.into_iter().chain(statistics).min()
    }

    pub fn snapshot(&self, now: Instant, now_ms: u64) -> ScheduleSnapshot {
        ScheduleSnapshot {
            saved_at_ms: now_ms,
            full_age_ms: millis(now.saturating_duration_since(self.last_full)),
            pending_age_ms: self
                .pending_since
                .map(|at| millis(now.saturating_duration_since(at))),
            retry_in_ms: millis(self.retry_after.saturating_duration_since(now)),
            queued: self.queued,
            failures: self.failures,
            rate_limit_in_ms: self
                .rate_limited_until
                .map(|at| millis(at.saturating_duration_since(now))),
            statistics_retry_in_ms: self
                .statistics_retry
                .map(|at| millis(at.saturating_duration_since(now))),
            statistics_failures: self.statistics_failures,
            derived_token: self.derived_token.clone(),
            current_age_ms: millis(now.saturating_duration_since(self.last_current)),
            current_requested: self.current_requested,
        }
    }

    pub fn restore(snapshot: ScheduleSnapshot, now: Instant, now_ms: u64) -> Self {
        let elapsed = now_ms.saturating_sub(snapshot.saved_at_ms);
        // Bound persisted values before doing arithmetic on monotonic time.
        let maximum = Duration::from_secs(30 * 24 * 60 * 60);
        let deadline = |remaining: u64| {
            now + Duration::from_millis(remaining.saturating_sub(elapsed)).min(maximum)
        };
        let age = Duration::from_millis(snapshot.full_age_ms.saturating_add(elapsed))
            .min(FULL_CHECK_INTERVAL);
        let mut schedule = Self {
            derived_token: snapshot.derived_token,
            last_full: now.checked_sub(age).unwrap_or(now),
            pending_since: snapshot.pending_age_ms.map(|age| {
                now.checked_sub(
                    Duration::from_millis(age.saturating_add(elapsed)).min(MAX_READING_DELAY),
                )
                .unwrap_or(now)
            }),
            retry_after: deadline(snapshot.retry_in_ms),
            queued: snapshot.queued,
            failures: snapshot.failures,
            rate_limited_until: snapshot
                .rate_limit_in_ms
                .filter(|remaining| *remaining > elapsed)
                .map(deadline),
            statistics_retry: snapshot.statistics_retry_in_ms.map(deadline),
            statistics_failures: snapshot.statistics_failures,
            reading_deadline: None,
            annotations_deadline: None,
            last_current: now
                .checked_sub(
                    Duration::from_millis(snapshot.current_age_ms.saturating_add(elapsed))
                        .min(Duration::from_secs(60)),
                )
                .unwrap_or(now),
            current_requested: snapshot.current_requested,
            current_active: false,
            current_book: None,
            seen_network: 0,
            seen_flush: 0,
            seen_activation: 0,
            seen_library: 0,
        };
        if age >= FULL_CHECK_INTERVAL {
            schedule.request(SyncMode::Full {
                force_statistics: false,
            });
        }
        schedule
    }

    pub fn rate_limit_deadline(&self, now: Instant) -> Option<Instant> {
        self.rate_limited_until.filter(|deadline| *deadline > now)
    }

    pub fn started(&mut self, mode: SyncMode) {
        if mode == SyncMode::Current {
            self.current_requested = false;
        }
        if matches!(mode, SyncMode::Full { .. })
            || mode != SyncMode::Statistics && self.queued == Some(SyncMode::Reading)
            || mode == SyncMode::Derived && self.queued == Some(SyncMode::Derived)
        {
            self.queued = None;
        }
        if !matches!(mode, SyncMode::Statistics | SyncMode::Current) {
            self.pending_since = None;
            self.reading_deadline = None;
            self.annotations_deadline = None;
        }
    }

    pub fn request(&mut self, mode: SyncMode) {
        if mode == SyncMode::Current {
            self.current_requested = true;
            return;
        }
        if mode == SyncMode::Statistics {
            self.statistics_retry.get_or_insert_with(Instant::now);
            return;
        }
        self.queued = Some(match (self.queued, mode) {
            (
                Some(SyncMode::Full {
                    force_statistics: previous,
                }),
                SyncMode::Full {
                    force_statistics: next,
                },
            ) => SyncMode::Full {
                force_statistics: previous || next,
            },
            (Some(full @ SyncMode::Full { .. }), _) => full,
            (_, full @ SyncMode::Full { .. }) => full,
            (Some(SyncMode::Derived), _) | (_, SyncMode::Derived) => SyncMode::Derived,
            (_, mode) => mode,
        });
    }

    pub fn next(
        &mut self,
        now: Instant,
        now_ms: u64,
        latest_change_ms: Option<u64>,
    ) -> Option<SyncMode> {
        if now < self.retry_after {
            return None;
        }
        self.reading_deadline = latest_change_ms.map(|latest| {
            let since = *self.pending_since.get_or_insert(now);
            (now + Duration::from_millis(
                READING_QUIET_MS.saturating_sub(now_ms.saturating_sub(latest)),
            ))
            .min(since + MAX_READING_DELAY)
        });
        // Full sync already starts with the current-book exchange. Do not
        // admit a second job ahead of the initial/overdue library check.
        if matches!(self.queued, Some(SyncMode::Full { .. }))
            || (now.duration_since(self.last_full) >= FULL_CHECK_INTERVAL && self.queued!=Some(SyncMode::Reading))
        {
            self.current_requested = false;
            return Some(match self.queued.take() {
                Some(full @ SyncMode::Full { .. }) => full,
                _ => SyncMode::Full {
                    force_statistics: false,
                },
            });
        }
        if self.current_requested
            || self.current_active
                && now.duration_since(self.last_current) >= Duration::from_secs(60)
        {
            self.current_requested = false;
            self.last_current = now;
            return Some(SyncMode::Current);
        }
        if latest_change_ms.is_some() && self.annotations_deadline.is_some_and(|at| now >= at) {
            self.annotations_deadline = None;
            return Some(SyncMode::Reading);
        }
        if let Some(mode) = self.queued.take() {
            return Some(mode);
        }
        if let Some(latest) = latest_change_ms {
            let since = *self.pending_since.get_or_insert(now);
            if now_ms.saturating_sub(latest) >= READING_QUIET_MS
                || now.duration_since(since) >= MAX_READING_DELAY
            {
                self.pending_since = None;
                return Some(SyncMode::Reading);
            }
        } else {
            self.pending_since = None;
        }
        if self
            .statistics_retry
            .is_some_and(|deadline| now >= deadline)
            && now.duration_since(self.last_full) < FULL_CHECK_INTERVAL
        {
            return Some(SyncMode::Statistics);
        }
        (now.duration_since(self.last_full) >= FULL_CHECK_INTERVAL).then_some(SyncMode::Full {
            force_statistics: false,
        })
    }

    pub fn completed(&mut self, mode: SyncMode, success: bool, now: Instant) {
        self.completed_with_limit(mode, success, now, None);
    }

    pub fn completed_with_limit(
        &mut self,
        mode: SyncMode,
        success: bool,
        now: Instant,
        rate_limit: Option<Duration>,
    ) {
        if mode == SyncMode::Statistics {
            if success {
                self.statistics_retry = None;
                self.statistics_failures = 0;
            } else {
                self.statistics_failures = self.statistics_failures.saturating_add(1);
                let delay = Duration::from_secs(5 * 60)
                    .saturating_mul(1_u32 << (self.statistics_failures - 1).min(6))
                    .min(MAX_RETRY_DELAY)
                    .max(rate_limit.unwrap_or_default());
                self.statistics_retry = Some(now + delay);
                if rate_limit.is_some() {
                    self.retry_after = now + delay;
                    self.rate_limited_until = Some(self.retry_after);
                }
            }
            return;
        }
        if success {
            if matches!(mode, SyncMode::Full { .. }) {
                self.last_full = now;
            }
            if matches!(mode, SyncMode::Full { .. } | SyncMode::Current) {
                self.last_current = now;
            }
            self.retry_after = now;
            self.failures = 0;
            self.rate_limited_until = None;
        } else {
            self.request(mode);
            self.failures = self.failures.saturating_add(1);
            let base = if rate_limit.is_some() {
                Duration::from_secs(5 * 60)
            } else {
                RETRY_DELAY
            };
            let delay = base
                .saturating_mul(1_u32 << (self.failures - 1).min(6))
                .min(MAX_RETRY_DELAY)
                .max(rate_limit.unwrap_or_default());
            self.retry_after = now + delay;
            if rate_limit.is_some() {
                self.rate_limited_until = Some(self.retry_after);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_deadlines_are_fifteen_seconds_twenty_minutes_and_three_seconds() {
        let now = Instant::now();
        let mut schedule = SyncSchedule::new(now);
        assert_eq!(schedule.poll_delay(now), Duration::from_secs(1200));
        assert_eq!(schedule.next(now, 10_000, Some(10_000)), None);
        assert_eq!(schedule.poll_delay(now), Duration::from_secs(15));
        assert_eq!(
            schedule.next(now + Duration::from_millis(14999), 24999, Some(10_000)),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(15), 25000, Some(10_000)),
            Some(SyncMode::Reading)
        );
        let mut schedule = SyncSchedule::new(now);
        schedule.observe_annotations(Some(10_000), now, 10_000);
        assert_eq!(schedule.poll_delay(now), Duration::from_secs(3));
        assert_eq!(
            schedule.next(now + Duration::from_millis(2999), 12999, Some(10000)),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(3), 13000, Some(10000)),
            Some(SyncMode::Reading)
        );
        let mut schedule = SyncSchedule::new(now);
        assert_eq!(
            schedule.next(now + Duration::from_secs(1199), 0, None),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(1200), 0, None),
            Some(SyncMode::Full {
                force_statistics: false
            })
        );
    }

    #[test]
    fn opening_switching_and_current_retry_do_not_rescan_the_library() {
        let now = Instant::now();
        let mut schedule = SyncSchedule::new(now);
        let mut signal = super::super::SyncSignal::default();
        signal.active = Some("a".into());
        signal.activation = 1;
        schedule.observe_signal(&signal, now);
        assert_eq!(schedule.next(now, 0, None), Some(SyncMode::Current));
        schedule.completed(SyncMode::Current, true, now);
        signal.activation += 1;
        schedule.observe_signal(&signal, now);
        assert_eq!(schedule.next(now, 0, None), None);
        signal.active = Some("b".into());
        signal.activation += 1;
        schedule.observe_signal(&signal, now);
        assert_eq!(schedule.next(now, 0, None), Some(SyncMode::Current));
        schedule.completed(SyncMode::Current, false, now);
        assert_eq!(schedule.retry_delay(now), Some(Duration::from_secs(30)));
        let restored_at = now + Duration::from_secs(5);
        let mut restored = SyncSchedule::restore(schedule.snapshot(now, 10000), restored_at, 15000);
        assert_eq!(restored.next(restored_at, 15000, None), None);
        assert_eq!(
            restored.next(now + Duration::from_secs(30), 40000, None),
            Some(SyncMode::Current)
        );
    }

    #[test]
    fn restoring_process_counters_does_not_duplicate_a_full_sync_and_exit_flush_keeps_it_queued() {
        let now = Instant::now();
        let mut schedule = SyncSchedule::new(now);
        let signal = super::super::SyncSignal {
            library: 8,
            activation: 9,
            flush: 3,
            network: 2,
            active: Some("a".into()),
            ..Default::default()
        };
        schedule.completed(SyncMode::Full {force_statistics:false},true,now);
        schedule.attach_signal(&signal);
        schedule.observe_signal(&signal, now);
        assert_eq!(schedule.next(now, 0, None), None);
        schedule.request(SyncMode::Full {
            force_statistics: false,
        });
        assert!(schedule.start_requested(SyncMode::Reading, now));
        schedule.completed(SyncMode::Reading, true, now);
        assert_eq!(
            schedule.next(now, 0, None),
            Some(SyncMode::Full {
                force_statistics: false
            })
        );
    }

    #[test]
    fn snapshot_preserves_long_quota_and_queued_work_across_process_wakes() {
        let mut schedule = SyncSchedule::default();
        let now = Instant::now();
        schedule.completed_with_limit(
            SyncMode::Reading,
            false,
            now,
            Some(Duration::from_secs(7200)),
        );
        schedule.request(SyncMode::Full {
            force_statistics: true,
        });
        let snapshot = schedule.snapshot(now + Duration::from_secs(60), 60_000);
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let snapshot = serde_json::from_slice(&bytes).unwrap();
        let restored_at = Instant::now();
        let mut restored = SyncSchedule::restore(snapshot, restored_at, 660_000);
        assert_eq!(restored.poll_delay(restored_at), Duration::from_secs(6540));
        assert!(!restored.start_requested(
            SyncMode::Full {
                force_statistics: true
            },
            restored_at
        ));
        assert_eq!(
            restored.next(restored_at + Duration::from_secs(6539), 0, None),
            None
        );
        assert_eq!(
            restored.next(restored_at + Duration::from_secs(6540), 0, None),
            Some(SyncMode::Full {
                force_statistics: true
            })
        );
    }

    #[test]
    fn statistics_warning_retries_alone_after_a_process_restart() {
        let mut schedule = SyncSchedule::default();
        let now = Instant::now();
        schedule.finished(
            SyncMode::Full {
                force_statistics: false,
            },
            Ok(&SyncReport {
                statistics_error: Some("statistics unavailable".into()),
                ..Default::default()
            }),
            now,
        );
        assert_eq!(schedule.retry_delay(now), Some(Duration::from_secs(300)));
        let snapshot = schedule.snapshot(now, 10_000);
        let restored_at = Instant::now();
        let mut restored = SyncSchedule::restore(snapshot, restored_at, 130_000);
        assert_eq!(restored.next(restored_at, 0, None), None);
        assert_eq!(
            restored.next(restored_at + Duration::from_secs(180), 0, None),
            Some(SyncMode::Statistics)
        );
        restored.finished(
            SyncMode::Statistics,
            Ok(&SyncReport::default()),
            restored_at + Duration::from_secs(180),
        );
        assert_eq!(
            restored.retry_delay(restored_at + Duration::from_secs(180)),
            None
        );
    }

    #[test]
    fn restoring_a_snapshot_keeps_failure_escalation_and_clock_rollback_safe() {
        let mut schedule = SyncSchedule::default();
        let now = Instant::now();
        schedule.completed(SyncMode::Reading, false, now);
        let snapshot = schedule.snapshot(now, 10_000);
        let restored_at = Instant::now();
        let mut restored = SyncSchedule::restore(snapshot, restored_at, 5_000);
        assert_eq!(restored.poll_delay(restored_at), Duration::from_secs(30));
        let later = restored_at + Duration::from_secs(30);
        assert_eq!(restored.next(later, 0, None), Some(SyncMode::Reading));
        restored.finished(SyncMode::Reading, Err(None), later);
        assert_eq!(restored.poll_delay(later), Duration::from_secs(60));
    }

    #[test]
    fn dirty_derived_data_is_coalesced_and_full_discovery_survives_a_restart() {
        let mut schedule = SyncSchedule::default();
        let now = Instant::now();
        assert!(schedule.observe_derived(Some("revision-1".into())));
        assert!(!schedule.observe_derived(Some("revision-1".into())));
        let snapshot = schedule.snapshot(now, 10_000);
        let mut restored = SyncSchedule::restore(snapshot, now, 1_210_000);
        assert_eq!(
            restored.next(now, 0, None),
            Some(SyncMode::Full {
                force_statistics: false
            })
        );
    }

    #[test]
    fn scoped_work_coalesces_but_cannot_starve_full_discovery() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.last_full;
        schedule.request(SyncMode::Derived);
        schedule.request(SyncMode::Reading);
        assert_eq!(schedule.next(now, 0, None), Some(SyncMode::Derived));
        schedule.completed(SyncMode::Derived, true, now);
        schedule.request(SyncMode::Derived);
        assert_eq!(
            schedule.next(now + FULL_CHECK_INTERVAL, 0, None),
            Some(SyncMode::Full {
                force_statistics: false
            })
        );
        schedule.request(SyncMode::Full {
            force_statistics: true,
        });
        schedule.request(SyncMode::Derived);
        assert_eq!(
            schedule.next(now + FULL_CHECK_INTERVAL, 0, None),
            Some(SyncMode::Full {
                force_statistics: true
            })
        );
    }

    #[test]
    fn reading_sync_debounces_but_continuous_turns_cannot_starve_it() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.last_full;
        assert_eq!(schedule.next(now, 1_000, Some(1_000)), None);
        assert_eq!(
            schedule.next(now + Duration::from_secs(1), 2_000, Some(1_800)),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(16), 17_000, Some(1_800)),
            Some(SyncMode::Reading)
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(17), 18_000, Some(18_000)),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(77), 78_000, Some(78_000)),
            Some(SyncMode::Reading)
        );
    }

    #[test]
    fn statistics_failure_retries_alone_and_does_not_block_reading_or_rescan_library() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.last_full;
        schedule.completed(
            SyncMode::Full {
                force_statistics: false,
            },
            true,
            now,
        );
        schedule.completed(SyncMode::Statistics, false, now);
        assert_eq!(schedule.next(now + Duration::from_secs(30), 0, None), None);
        schedule.request(SyncMode::Reading);
        assert_eq!(
            schedule.next(now + Duration::from_secs(31), 0, None),
            Some(SyncMode::Reading)
        );
        schedule.completed(SyncMode::Reading, true, now + Duration::from_secs(31));
        assert_eq!(
            schedule.next(now + Duration::from_secs(300), 0, None),
            Some(SyncMode::Statistics)
        );
        schedule.completed(SyncMode::Statistics, true, now + Duration::from_secs(300));
        assert_eq!(schedule.next(now + Duration::from_secs(301), 0, None), None);
    }

    #[test]
    fn statistics_rate_limit_is_global_but_queues_no_library_retry() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.last_full;
        schedule.completed_with_limit(
            SyncMode::Statistics,
            false,
            now,
            Some(Duration::from_secs(600)),
        );
        assert_eq!(schedule.next(now + Duration::from_secs(599), 0, None), None);
        assert_eq!(
            schedule.next(now + Duration::from_secs(600), 0, None),
            Some(SyncMode::Statistics)
        );
        assert!(schedule.queued.is_none());
    }

    #[test]
    fn returning_to_library_flushes_immediately_and_failed_work_retries() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.last_full;
        schedule.request(SyncMode::Reading);
        assert_eq!(
            schedule.next(now, 1_000, Some(1_000)),
            Some(SyncMode::Reading)
        );
        schedule.completed(SyncMode::Reading, false, now);
        assert_eq!(
            schedule.next(now + Duration::from_secs(29), 30_000, Some(1_000)),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(30), 31_000, Some(1_000)),
            Some(SyncMode::Reading)
        );
    }

    #[test]
    fn consecutive_failures_back_off_and_success_resets_the_delay() {
        let mut schedule = SyncSchedule::default();
        let mut now = schedule.last_full;
        for delay in [30, 60, 120, 240, 480, 960, 1800, 1800] {
            schedule.completed(SyncMode::Reading, false, now);
            assert_eq!(
                schedule.next(now + Duration::from_secs(delay - 1), 0, None),
                None
            );
            now += Duration::from_secs(delay);
            assert_eq!(schedule.next(now, 0, None), Some(SyncMode::Reading));
        }
        schedule.completed(SyncMode::Reading, true, now);
        schedule.completed(SyncMode::Reading, false, now);
        assert_eq!(
            schedule.next(now + Duration::from_secs(30), 0, None),
            Some(SyncMode::Reading)
        );
    }

    #[test]
    fn rate_limit_keeps_full_work_queued_and_respects_long_server_deadlines() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.last_full;
        schedule.completed_with_limit(
            SyncMode::Reading,
            false,
            now,
            Some(Duration::from_secs(7200)),
        );
        let full = SyncMode::Full {
            force_statistics: false,
        };
        schedule.request(full);
        assert!(schedule.rate_limit_deadline(now).is_some());
        assert_eq!(
            schedule.next(now + Duration::from_secs(7199), 0, None),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(7200), 0, None),
            Some(full)
        );
        schedule.completed(full, true, now + Duration::from_secs(7200));
        assert_eq!(
            schedule.rate_limit_deadline(now + Duration::from_secs(7200)),
            None
        );
    }
}
