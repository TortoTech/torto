#[derive(Clone)]
pub(crate) struct PendingTask<T> {
    pub id: u64,
    pub payload: T,
}

#[derive(Debug)]
pub(crate) struct TaskResult<T> {
    pub id: u64,
    pub result: Result<T, String>,
}

pub(crate) struct TaskSlot<T> {
    pub pending: Option<PendingTask<T>>,
    in_flight: Option<PendingTask<T>>,
    next_id: u64,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl<T> Default for TaskSlot<T> {
    fn default() -> Self {
        Self {
            pending: None,
            in_flight: None,
            next_id: 1,
            worker: None,
        }
    }
}

impl<T> TaskSlot<T> {
    pub fn attach_worker(&mut self, worker: tokio::task::JoinHandle<()>) {
        self.worker = Some(worker);
    }
    pub fn active(&self) -> Option<&T> {
        self.in_flight
            .as_ref()
            .or(self.pending.as_ref())
            .map(|task| &task.payload)
    }
    pub fn is_pending(&self) -> bool {
        self.pending.is_some() || self.in_flight.is_some()
    }

    pub fn begin(&mut self, payload: T) -> u64 {
        if self.worker.is_some() {
            self.cancel();
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pending = Some(PendingTask { id, payload });
        id
    }

    pub fn take_pending(&mut self) -> Option<PendingTask<T>>
    where
        T: Clone,
    {
        let request = self.pending.take()?;
        self.in_flight = Some(request.clone());
        Some(request)
    }

    pub fn complete(&mut self, id: u64) -> Option<T> {
        if self.in_flight.as_ref().map(|request| request.id) != Some(id) {
            return None;
        }
        self.in_flight.take().map(|request| request.payload)
    }

    pub fn in_flight(&self, id: u64) -> Option<&T> {
        self.in_flight
            .as_ref()
            .filter(|request| request.id == id)
            .map(|request| &request.payload)
    }

    pub fn cancel(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        self.pending = None;
        self.in_flight = None;
    }
}

impl<T> Drop for TaskSlot<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_aborts_the_running_future_and_rejects_its_completion() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Dropped(Arc<AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let dropped = Arc::new(AtomicBool::new(false));
            let started = Arc::new(tokio::sync::Notify::new());
            let mut slot = TaskSlot::default();
            let id = slot.begin("old page");
            slot.take_pending();
            let flag = dropped.clone();
            let notify = started.clone();
            slot.attach_worker(tokio::spawn(async move {
                let _drop = Dropped(flag);
                notify.notify_one();
                std::future::pending::<()>().await;
            }));
            started.notified().await;
            slot.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while !dropped.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(dropped.load(Ordering::SeqCst));
            assert!(slot.complete(id).is_none());
            assert!(!slot.is_pending());
        });
    }

    #[test]
    fn stale_completion_cannot_clear_the_current_request() {
        let mut slot = TaskSlot::default();
        let first = slot.begin("first");
        let _ = slot.take_pending();
        slot.cancel();
        let second = slot.begin("second");
        let _ = slot.take_pending();

        assert_eq!(slot.complete(first), None);
        assert!(slot.is_pending());
        assert_eq!(slot.complete(second), Some("second"));
        assert!(!slot.is_pending());
    }
}
