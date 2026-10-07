use std::collections::{HashMap, VecDeque};

use rebook_publication::LocatorV1;

use super::background::BackgroundJob;
use crate::sync::SyncStore;

struct ActivityRequest {
    store: SyncStore,
    book_id: String,
    initial_locator: LocatorV1,
}

type ActivityResult = (u64, Vec<(String, Result<(), String>)>);

/// UI-owned reading order. Disk updates run independently of the reader's
/// lifetime, including when it is closed before the worker has started.
#[derive(Default)]
pub(super) struct ReadingActivity {
    times: HashMap<String, u64>,
    optimistic: HashMap<String, u64>,
    ready: bool,
    generation: u64,
    pending: VecDeque<ActivityRequest>,
    job: BackgroundJob<ActivityResult>,
}

impl ReadingActivity {
    pub fn times(&self) -> &HashMap<String, u64> {
        &self.times
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty() || self.job.is_running()
    }

    pub fn record(
        &mut self,
        store: SyncStore,
        book_id: String,
        initial_locator: LocatorV1,
        opened_ms: u64,
    ) {
        self.times.insert(book_id.clone(), opened_ms);
        self.optimistic.insert(book_id.clone(), opened_ms);
        // At most one queued request per book; keep the latest opening order.
        self.pending.retain(|request| request.book_id != book_id);
        self.pending.push_back(ActivityRequest {
            store,
            book_id,
            initial_locator,
        });
    }

    pub fn apply_snapshot(&mut self, mut times: HashMap<String, u64>) {
        // A refresh may have read the database before a queued write finished.
        // Do not let it undo the immediate order shown after opening a book.
        self.optimistic.retain(|book_id, opened_ms| {
            if times.get(book_id).is_some_and(|saved| saved >= opened_ms) {
                false
            } else {
                times.insert(book_id.clone(), *opened_ms);
                true
            }
        });
        self.times = times;
        self.ready = true;
    }

    pub fn load_failed(&mut self) {
        // Allow normal import-order selection when the local store is unreadable.
        self.ready = true;
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.times.clear();
        self.optimistic.clear();
        self.pending.clear();
        self.ready = false;
    }

    pub fn remove(&mut self, book_id: &str) {
        self.times.remove(book_id);
        self.optimistic.remove(book_id);
        self.pending.retain(|request| request.book_id != book_id);
    }

    pub fn spawn(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        wake: impl FnOnce() + Send + 'static,
    ) {
        if self.pending.is_empty() || self.job.is_running() {
            return;
        }
        let generation = self.generation;
        let requests = self.pending.drain(..).collect::<Vec<_>>();
        self.job.start(
            runtime,
            move || {
                let results = requests
                    .into_iter()
                    .map(|request| {
                        let result = request
                            .store
                            .record_reading_activity(&request.book_id, &request.initial_locator)
                            .map_err(|error| error.to_string());
                        (request.book_id, result)
                    })
                    .collect();
                (generation, results)
            },
            wake,
        );
    }

    /// A completed write requests a fresh snapshot; it never installs old data.
    pub fn poll(&mut self) -> bool {
        let Some((generation, results)) = self.job.poll() else {
            return false;
        };
        if generation != self.generation {
            return false;
        }
        for (book_id, result) in results {
            if let Err(error) = result {
                tracing::warn!(%error, %book_id, "failed to record reading activity");
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "reading_activity/tests.rs"]
mod tests;
