//! Coalesced process-local notifications. Durable database changes remain the source of truth.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::watch;

#[derive(Clone, Debug, Default)]
pub struct SyncSignal {
    pub revision: u64,
    pub network: u64,
    pub flush: u64,
    pub library: u64,
    pub derived: u64,
    pub annotations_at: u64,
    pub active: Option<String>,
    pub activation: u64,
}

#[derive(Debug)]
pub struct SyncWake(
    watch::Sender<SyncSignal>,
    Mutex<std::collections::VecDeque<(String, u64)>>,
);

#[derive(Clone, Copy)]
pub enum SyncChange {
    Reading,
    Annotations,
    Library,
    Derived,
    Network,
    Flush,
}

impl SyncWake {
    pub fn for_path(path: &Path) -> Arc<Self> {
        static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<SyncWake>>>> = OnceLock::new();
        let mut registry = REGISTRY
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        registry.retain(|_, value| value.strong_count() > 0);
        if let Some(wake) = registry.get(path).and_then(Weak::upgrade) {
            return wake;
        }
        let wake = Arc::new(Self(
            watch::channel(SyncSignal::default()).0,
            Mutex::default(),
        ));
        registry.insert(path.to_owned(), Arc::downgrade(&wake));
        wake
    }
    pub fn subscribe(&self) -> watch::Receiver<SyncSignal> {
        self.0.subscribe()
    }
    pub fn snapshot(&self) -> SyncSignal {
        self.0.borrow().clone()
    }
    pub fn changed(&self, kind: SyncChange) {
        self.0.send_modify(|signal| {
            signal.revision = signal.revision.wrapping_add(1);
            match kind {
                SyncChange::Reading => {}
                SyncChange::Flush => signal.flush = signal.flush.wrapping_add(1),
                SyncChange::Network => signal.network = signal.network.wrapping_add(1),
                SyncChange::Annotations => signal.annotations_at = super::rate_limit::now_ms(),
                SyncChange::Library => signal.library = signal.library.wrapping_add(1),
                SyncChange::Derived => signal.derived = signal.derived.wrapping_add(1),
            }
        });
    }
    pub fn protect_progress(&self, book: &str) {
        let mut protected = self.1.lock().unwrap_or_else(|e| e.into_inner());
        protected.retain(|(id, _)| id != book);
        protected.push_back((book.to_owned(), 0));
        while protected.len() > 16 {
            protected.pop_front();
        }
    }
    pub fn allow_progress(&self, book: &str) {
        let mut intent = self.1.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, allowed)) = intent.iter_mut().find(|(id, _)| id == book) {
            *allowed = allowed.wrapping_add(1).max(1);
        }
    }
    pub(super) fn progress_ticket(&self, book: &str) -> Option<u64> {
        let intent = self.1.lock().unwrap_or_else(|e| e.into_inner());
        match intent.iter().find(|(id, _)| id == book) {
            Some((_, 0)) => None,
            Some((_, ticket)) => Some(*ticket),
            None => Some(0),
        }
    }
    pub(super) fn finish_progress(&self, book: &str, ticket: u64) {
        let mut intent = self.1.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, value)) = intent.iter_mut().find(|(id, _)| id == book)
            && *value == ticket
        {
            *value = 0;
        }
    }
    pub fn active(&self, book: Option<String>) {
        self.0.send_modify(|signal| {
            signal.active = book;
            signal.activation = signal.activation.wrapping_add(1);
            signal.revision = signal.revision.wrapping_add(1);
        });
    }
}

pub(super) fn derived_changed() {
    if let Some(paths) = crate::environment::paths() {
        SyncWake::for_path(&paths.data.join("sync-v1.sqlite3")).changed(SyncChange::Derived);
    }
}
