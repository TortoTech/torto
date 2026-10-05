use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::sync_schedule::SyncSchedule;
use crate::sync::{SyncMode, SyncStore};

const CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// UI configuration at dispatch time. Results are valid only for this snapshot.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct MonitorKey {
    pub enabled: bool,
    pub account: String,
    pub device_id: String,
    pub store_path: Option<PathBuf>,
    pub running: bool,
    pub schedule: SyncSchedule,
}

#[derive(Default)]
pub(super) struct SyncMonitor {
    generation: u64,
    key: Option<MonitorKey>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

pub(crate) struct SyncCheckMessage {
    generation: u64,
    decision: SyncDecision,
}

pub(super) struct SyncDecision {
    pub schedule: SyncSchedule,
    pub mode: Option<SyncMode>,
    pub refresh: bool,
}

struct Probe {
    reading: Result<Option<u64>, String>,
    derived: Result<Option<String>, String>,
}

struct MonitorState {
    schedule: SyncSchedule,
    running: bool,
}

impl MonitorState {
    fn observe(&mut self, probe: Probe, now: Instant, now_ms: u64) -> Option<SyncDecision> {
        let mut refresh = false;
        if let Ok(token) = probe.derived {
            refresh = token.is_some() && token != self.schedule.derived_token;
            self.schedule.derived_token = token;
            if refresh {
                self.schedule.request(SyncMode::Full {
                    force_statistics: false,
                });
            }
        }
        let mode = if self.running {
            None
        } else {
            match probe.reading {
                Ok(latest) => self.schedule.next(now, now_ms, latest),
                Err(error) => {
                    tracing::warn!(%error, "failed to inspect pending sync changes");
                    None
                }
            }
        };
        (refresh || mode.is_some()).then(|| SyncDecision {
            schedule: self.schedule.clone(),
            mode,
            refresh,
        })
    }
}

impl SyncMonitor {
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.key = None;
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }

    pub fn update(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        store: Option<SyncStore>,
        key: MonitorKey,
        notify: impl Fn(SyncCheckMessage) + Send + 'static,
    ) {
        if self.key.as_ref() == Some(&key) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        self.key = Some(key.clone());
        let Some(store) = store.filter(|_| key.enabled) else {
            return;
        };
        let generation = self.generation;
        self.worker = Some(runtime.spawn(async move {
            let mut state = MonitorState {
                schedule: key.schedule,
                running: key.running,
            };
            let mut timer = tokio::time::interval(CHECK_INTERVAL);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let store = store.clone();
                let account = key.account.clone();
                let running = key.running;
                // Database and filesystem work stays off both the UI thread
                // and Tokio's async worker threads.
                let result = tokio::task::spawn_blocking(move || Probe {
                    reading: if running {
                        Ok(None)
                    } else {
                        store
                            .pending_reading(&account)
                            .map(|pending| pending.into_iter().map(|(_, _, changed)| changed).max())
                            .map_err(|error| error.to_string())
                    },
                    derived: crate::sync::derived_change_token().map_err(|error| error.to_string()),
                })
                .await;
                let probe = match result {
                    Ok(probe) => probe,
                    Err(error) => {
                        tracing::warn!(%error, "background sync check failed");
                        continue;
                    }
                };
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if let Some(decision) = state.observe(probe, Instant::now(), now_ms) {
                    notify(SyncCheckMessage {
                        generation,
                        decision,
                    });
                    // The UI publishes a new snapshot after consuming the
                    // decision. Do not enqueue duplicate sync starts meanwhile.
                    return;
                }
            }
        }));
    }

    pub fn complete(&self, message: SyncCheckMessage, key: &MonitorKey) -> Option<SyncDecision> {
        (message.generation == self.generation && self.key.as_ref() == Some(key))
            .then_some(message.decision)
    }
}

impl Drop for SyncMonitor {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(reading: Option<u64>, derived: Option<&str>) -> Probe {
        Probe {
            reading: Ok(reading),
            derived: Ok(derived.map(str::to_owned)),
        }
    }

    #[test]
    fn idle_checks_are_silent_and_pending_reading_is_debounced() {
        let now = Instant::now();
        let mut state = MonitorState {
            schedule: SyncSchedule::default(),
            running: false,
        };
        for tick in 0..60 {
            assert!(
                state
                    .observe(
                        probe(None, None),
                        now + Duration::from_secs(tick),
                        tick * 1000
                    )
                    .is_none()
            );
        }
        assert!(
            state
                .observe(
                    probe(Some(60_000), None),
                    now + Duration::from_secs(60),
                    60_000
                )
                .is_none()
        );
        assert!(
            state
                .observe(
                    probe(Some(60_000), None),
                    now + Duration::from_secs(61),
                    61_000
                )
                .is_none()
        );
        assert_eq!(
            state
                .observe(
                    probe(Some(60_000), None),
                    now + Duration::from_secs(62),
                    62_000
                )
                .unwrap()
                .mode,
            Some(SyncMode::Reading)
        );
    }

    #[test]
    fn derived_changes_queue_full_sync_during_running_work() {
        let mut state = MonitorState {
            schedule: SyncSchedule::default(),
            running: true,
        };
        let now = Instant::now();
        let decision = state.observe(probe(None, Some("changed")), now, 0).unwrap();
        assert!(decision.refresh);
        assert_eq!(decision.mode, None);
        assert!(
            state
                .observe(probe(None, Some("changed")), now, 0)
                .is_none()
        );
        state.running = false;
        let decision = state.observe(probe(None, Some("changed")), now, 0).unwrap();
        assert_eq!(
            decision.mode,
            Some(SyncMode::Full {
                force_statistics: false
            })
        );
    }

    #[test]
    fn results_are_rejected_after_configuration_changes_or_restart() {
        let key = MonitorKey {
            enabled: true,
            account: "account".into(),
            device_id: "device".into(),
            store_path: None,
            running: false,
            schedule: SyncSchedule::default(),
        };
        let mut monitor = SyncMonitor {
            generation: 7,
            key: Some(key.clone()),
            worker: None,
        };
        let message = |generation| SyncCheckMessage {
            generation,
            decision: SyncDecision {
                schedule: key.schedule.clone(),
                mode: Some(SyncMode::Reading),
                refresh: false,
            },
        };
        assert!(monitor.complete(message(6), &key).is_none());
        let mut disabled = key.clone();
        disabled.enabled = false;
        assert!(monitor.complete(message(7), &disabled).is_none());
        let mut changed = key.clone();
        changed.account = "other".into();
        assert!(monitor.complete(message(7), &changed).is_none());
        changed = key.clone();
        changed.schedule.request(SyncMode::Full {
            force_statistics: false,
        });
        assert!(monitor.complete(message(7), &changed).is_none());
        assert!(monitor.complete(message(7), &key).is_some());
        monitor.invalidate();
        assert!(monitor.complete(message(7), &key).is_none());
    }

    #[test]
    fn continuous_changes_do_not_starve_reading_sync() {
        let mut state = MonitorState {
            schedule: SyncSchedule::default(),
            running: false,
        };
        let now = Instant::now();
        for tick in 0..15 {
            assert!(
                state
                    .observe(
                        probe(Some(tick * 1000), None),
                        now + Duration::from_secs(tick),
                        tick * 1000
                    )
                    .is_none()
            );
        }
        assert_eq!(
            state
                .observe(
                    probe(Some(15_000), None),
                    now + Duration::from_secs(15),
                    15_000
                )
                .unwrap()
                .mode,
            Some(SyncMode::Reading)
        );
    }

    #[test]
    fn full_sync_timer_and_retry_work_without_ui_frames() {
        let mut state = MonitorState {
            schedule: SyncSchedule::default(),
            running: false,
        };
        let now = Instant::now();
        let later = now + Duration::from_secs(15 * 60);
        let mode = state
            .observe(probe(None, None), later, 900_000)
            .unwrap()
            .mode
            .unwrap();
        assert_eq!(
            mode,
            SyncMode::Full {
                force_statistics: false
            }
        );
        state.schedule.completed(mode, false, later);
        assert!(
            state
                .observe(probe(None, None), later + Duration::from_secs(29), 929_000)
                .is_none()
        );
        assert_eq!(
            state
                .observe(probe(None, None), later + Duration::from_secs(30), 930_000)
                .unwrap()
                .mode,
            Some(mode)
        );
    }
}
