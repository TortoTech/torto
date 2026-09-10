use std::time::{Duration, Instant};

use crate::sync::SyncMode;

const FULL_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MAX_READING_DELAY: Duration = Duration::from_secs(15);
const RETRY_DELAY: Duration = Duration::from_secs(30);

pub(super) struct SyncSchedule {
    pub next_poll: Instant,
    pub derived_token: Option<String>,
    last_full: Instant,
    pending_since: Option<Instant>,
    retry_after: Instant,
    queued: Option<SyncMode>,
}

impl Default for SyncSchedule {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            next_poll: now,
            derived_token: None,
            last_full: now,
            pending_since: None,
            retry_after: now,
            queued: None,
        }
    }
}

impl SyncSchedule {
    pub fn started(&mut self, mode: SyncMode) {
        if matches!(mode, SyncMode::Full { .. }) || self.queued == Some(SyncMode::Reading) {
            self.queued = None;
        }
        self.pending_since = None;
    }

    pub fn request(&mut self, mode: SyncMode) {
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
        if let Some(mode) = self.queued.take() {
            return Some(mode);
        }
        if let Some(latest) = latest_change_ms {
            let since = *self.pending_since.get_or_insert(now);
            if now_ms.saturating_sub(latest) >= 2_000
                || now.duration_since(since) >= MAX_READING_DELAY
            {
                self.pending_since = None;
                return Some(SyncMode::Reading);
            }
        } else {
            self.pending_since = None;
        }
        (now.duration_since(self.last_full) >= FULL_CHECK_INTERVAL).then_some(SyncMode::Full {
            force_statistics: false,
        })
    }

    pub fn completed(&mut self, mode: SyncMode, success: bool, now: Instant) {
        if success {
            if matches!(mode, SyncMode::Full { .. }) {
                self.last_full = now;
            }
            self.retry_after = now;
        } else {
            self.request(mode);
            self.retry_after = now + RETRY_DELAY;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_sync_debounces_but_continuous_turns_cannot_starve_it() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.next_poll;
        assert_eq!(schedule.next(now, 1_000, Some(1_000)), None);
        assert_eq!(
            schedule.next(now + Duration::from_secs(1), 2_000, Some(1_800)),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(3), 4_000, Some(1_800)),
            Some(SyncMode::Reading)
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(4), 5_000, Some(5_000)),
            None
        );
        assert_eq!(
            schedule.next(now + Duration::from_secs(19), 20_000, Some(20_000)),
            Some(SyncMode::Reading)
        );
    }

    #[test]
    fn returning_to_library_flushes_immediately_and_failed_work_retries() {
        let mut schedule = SyncSchedule::default();
        let now = schedule.next_poll;
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
}
