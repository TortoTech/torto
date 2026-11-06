//! Reader model-request scheduling, shared by every UI host. Hosts supply source
//! snapshots and adopt results; no UI/session lock is held by this dispatcher.
use futures_util::future::{Either, select};
use rebook_publication::SourceRange;
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

pub mod batching;
pub const SETTLE: Duration = Duration::from_millis(200);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
pub const TRANSLATION_BATCH_CHARS: usize = 2_000;
const IDLE_GRACE: Duration = Duration::from_millis(250);
pub const MAX_QUEUED: usize = 32;
const MAX_ACTIVE: usize = 3;
pub type Demand = Vec<(usize, Vec<SourceRange>)>;
pub fn translation_key(
    book: &str,
    index: usize,
    settings: &crate::plugins::PluginSettings,
    blocks: &[crate::plugins::TranslationBlockInput],
) -> String {
    let inputs = blocks
        .iter()
        .map(|b| (b.block_index, b.segment_index, &b.text))
        .collect::<Vec<_>>();
    blake3::hash(
        &serde_json::to_vec(&(
            book,
            index,
            crate::translation_cache::configuration_key(settings).ok(),
            inputs,
        ))
        .unwrap(),
    )
    .to_hex()
    .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Structure,
    Translation,
    Enhancement,
    Contents,
}
#[derive(Clone, Copy)]
pub struct Flow {
    pub structure: bool,
    pub translation: bool,
}
impl Flow {
    /// Background batches warm structural recognition. Body translation remains
    /// scoped to the requested viewport (including its host-provided lookahead).
    pub fn scope(structure: bool, translation: bool, visible: bool, prefetch: bool) -> Self {
        Self {
            structure: structure && (visible || prefetch),
            translation: translation && visible,
        }
    }
    pub fn stages(self) -> impl Iterator<Item = Kind> {
        [
            self.structure.then_some(Kind::Structure),
            self.translation.then_some(Kind::Translation),
        ]
        .into_iter()
        .flatten()
    }
    pub fn translation_ready(self, structure_done: bool) -> bool {
        !self.structure || structure_done
    }
    pub fn presentation_ready(self, translation_done: bool) -> bool {
        !self.translation || translation_done
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Interrupted,
    KeepOriginal,
    StopBatch,
}
pub fn failure(kind: Kind, error: &str) -> Failure {
    if interrupted(error) {
        Failure::Interrupted
    } else if matches!(kind, Kind::Structure | Kind::Enhancement) {
        Failure::KeepOriginal
    } else {
        Failure::StopBatch
    }
}
impl Kind {
    fn lane(self) -> u8 {
        match self {
            Self::Structure => 0,
            Self::Translation | Self::Contents => 1,
            Self::Enhancement => 2,
        }
    }
}
#[derive(Clone)]
pub struct Task {
    pub key: String,
    pub kind: Kind,
    pub section: usize,
    pub sources: Vec<SourceRange>,
    pub prefetch: bool,
}
impl Task {
    pub fn rank(&self, demand: &Demand) -> u8 {
        if self.kind == Kind::Contents {
            return 4;
        }
        let visible = relevant(self.section, &self.sources, demand);
        match (self.kind, visible) {
            (Kind::Translation, true) => 0,
            (Kind::Structure, true) => 1,
            (Kind::Enhancement, true) => 3,
            (Kind::Translation | Kind::Structure, _) => 5,
            _ => 6,
        }
    }
}
pub fn overlap(a: &SourceRange, b: &SourceRange) -> bool {
    a.start.spine == b.start.spine
        && (a.start.node == b.start.node
            || a.start.node == b.end.node
            || a.end.node == b.start.node)
}
pub fn relevant(index: usize, sources: &[SourceRange], demand: &Demand) -> bool {
    demand
        .iter()
        .any(|(i, r)| *i == index && sources.iter().any(|s| r.iter().any(|r| overlap(s, r))))
}
#[derive(Clone, Default)]
pub struct Cancellation(Arc<CancelState>);
#[derive(Default)]
struct CancelState {
    stopped: AtomicBool,
    notify: Notify,
}
impl Cancellation {
    pub fn cancel(&self) {
        self.0.stopped.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
    }
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}
pub fn interrupted(error: &str) -> bool {
    error.starts_with("已停止处理") || error.starts_with("AI request superseded")
}
struct Entry {
    task: Task,
    cancel: Cancellation,
    active: bool,
    epoch: u64,
}
struct State {
    entries: HashMap<u64, Entry>,
    next: u64,
    epoch: u64,
    demand: Demand,
    paused: bool,
    settled: Instant,
    last_primary: Instant,
}
impl Default for State {
    fn default() -> Self {
        let now = Instant::now() - SETTLE;
        Self {
            entries: HashMap::new(),
            next: 0,
            epoch: 0,
            demand: vec![],
            paused: false,
            settled: now,
            last_primary: now,
        }
    }
}
struct Inner {
    state: Mutex<State>,
    changed: Notify,
    timeout: Duration,
}
impl Default for Inner {
    fn default() -> Self {
        Self {
            state: Default::default(),
            changed: Notify::new(),
            timeout: REQUEST_TIMEOUT,
        }
    }
}
#[derive(Clone, Default)]
pub struct Scheduler(Arc<Inner>);
struct Registration {
    scheduler: Scheduler,
    id: u64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut state) = self.scheduler.0.state.lock() {
            if let Some(entry) = state.entries.remove(&self.id) {
                if entry.active && entry.task.rank(&state.demand) <= 1 {
                    state.last_primary = Instant::now();
                }
            }
        }
        self.scheduler.0.changed.notify_waiters();
    }
}
impl Scheduler {
    pub fn update(&self, mut demand: Demand, paused: bool) {
        for (_, ranges) in &mut demand {
            for range in ranges.iter_mut() {
                range.start.text_offset = 0;
                range.end.text_offset = 0;
            }
            ranges.dedup();
        }
        let mut state = self.0.state.lock().unwrap();
        if state.demand != demand {
            state.settled = Instant::now() + SETTLE;
            state.demand = demand;
        }
        state.paused = paused;
        drop(state);
        self.0.changed.notify_waiters();
    }
    pub fn pause(&self) {
        self.0.state.lock().unwrap().paused = true;
        self.0.changed.notify_waiters();
    }
    pub fn invalidate(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.epoch = state.epoch.wrapping_add(1);
        for entry in state.entries.values() {
            entry.cancel.cancel();
        }
        drop(state);
        self.0.changed.notify_waiters();
    }
    pub async fn run<T>(
        &self,
        task: Task,
        cancel: &Cancellation,
        future: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let (registration, own) = {
            let mut state = self.0.state.lock().unwrap();
            if cancel.is_cancelled() {
                return Err("已停止处理".into());
            }
            if state
                .entries
                .values()
                .any(|e| e.epoch == state.epoch && e.task.key == task.key)
            {
                return Err("AI request superseded: duplicate".into());
            }
            if state.entries.values().filter(|e| !e.active).count() >= MAX_QUEUED {
                let worst = state
                    .entries
                    .iter()
                    .filter(|(_, e)| !e.active)
                    .max_by_key(|(id, e)| (e.task.rank(&state.demand), **id))
                    .map(|(id, e)| (*id, e.task.rank(&state.demand)));
                if let Some((id, rank)) = worst.filter(|(_, rank)| *rank > task.rank(&state.demand))
                {
                    let _ = rank;
                    state.entries.get(&id).unwrap().cancel.cancel();
                    state.entries.remove(&id);
                } else {
                    return Err("AI request superseded: queue full".into());
                }
            }
            state.next = state.next.wrapping_add(1);
            let id = state.next;
            let epoch = state.epoch;
            let own = Cancellation::default();
            state.entries.insert(
                id,
                Entry {
                    task,
                    cancel: own.clone(),
                    active: false,
                    epoch,
                },
            );
            (
                Registration {
                    scheduler: self.clone(),
                    id,
                },
                own,
            )
        };
        self.0.changed.notify_waiters();
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (admitted, deadline) = {
                let mut state = self.0.state.lock().unwrap();
                let Some(entry) = state.entries.get(&registration.id) else {
                    return Err("AI request superseded".into());
                };
                if entry.epoch != state.epoch || own.is_cancelled() {
                    return Err("AI request superseded".into());
                }
                let rank = entry.task.rank(&state.demand);
                let lane = entry.task.kind.lane();
                let deadline = if rank >= 3 {
                    state.settled.max(state.last_primary + IDLE_GRACE)
                } else {
                    state.settled
                };
                let best = state
                    .entries
                    .iter()
                    .filter(|(_, e)| {
                        !e.active && !e.cancel.is_cancelled() && e.task.kind.lane() == lane
                    })
                    .min_by_key(|(id, e)| (e.task.rank(&state.demand), **id))
                    .map(|(id, _)| *id);
                let active = state
                    .entries
                    .values()
                    .filter(|e| e.active && !e.cancel.is_cancelled())
                    .count();
                let blockers = state
                    .entries
                    .values()
                    .filter(|e| e.active && e.task.kind.lane() == lane && !e.cancel.is_cancelled())
                    .collect::<Vec<_>>();
                let primary_waiting = state.entries.values().any(|e| {
                    !e.active && !e.cancel.is_cancelled() && e.task.rank(&state.demand) <= 1
                });
                let body_active = state.entries.values().any(|e| {
                    e.active && !e.cancel.is_cancelled() && e.task.rank(&state.demand) <= 1
                });
                if !state.paused
                    && best == Some(registration.id)
                    && rank <= 1
                    && Instant::now() >= deadline
                {
                    for old in &blockers {
                        if old.task.rank(&state.demand) > rank {
                            old.cancel.cancel();
                        }
                    }
                }
                let free = blockers.is_empty();
                let ready = !state.paused
                    && best == Some(registration.id)
                    && free
                    && active < MAX_ACTIVE
                    && Instant::now() >= deadline
                    && !(rank >= 3 && primary_waiting)
                    && !(rank == 4 && body_active);
                if ready {
                    state.entries.get_mut(&registration.id).unwrap().active = true;
                }
                (ready, deadline)
            };
            if admitted {
                break;
            }
            let wait = async {
                if deadline > Instant::now() {
                    let _ = select(
                        Box::pin(notified),
                        Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(
                            deadline,
                        ))),
                    )
                    .await;
                } else {
                    notified.await;
                }
            };
            if let Either::Left((reason, _)) =
                select(Box::pin(cancelled(cancel, &own)), Box::pin(wait)).await
            {
                return Err(reason);
            }
        }
        match select(
            Box::pin(cancelled(cancel, &own)),
            Box::pin(tokio::time::timeout(self.0.timeout, future)),
        )
        .await
        {
            Either::Left((reason, _)) => Err(reason),
            Either::Right((result, _)) => {
                result.unwrap_or_else(|_| Err("AI request timed out".into()))
            }
        }
    }
}
async fn cancelled(external: &Cancellation, internal: &Cancellation) -> String {
    match select(
        Box::pin(external.cancelled()),
        Box::pin(internal.cancelled()),
    )
    .await
    {
        Either::Left(_) => "已停止处理".into(),
        Either::Right(_) => "AI request superseded".into(),
    }
}

#[cfg(test)]
mod tests;
