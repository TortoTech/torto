use std::sync::mpsc::{self, Receiver, TryRecvError};

pub(super) struct BackgroundJob<T> {
    receiver: Option<Receiver<T>>,
}

impl<T> Default for BackgroundJob<T> {
    fn default() -> Self {
        Self { receiver: None }
    }
}

impl<T: Send + 'static> BackgroundJob<T> {
    pub fn is_running(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn start(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        work: impl FnOnce() -> T + Send + 'static,
        wake: impl FnOnce() + Send + 'static,
    ) {
        assert!(!self.is_running());
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        runtime.spawn_blocking(move || {
            let result = work();
            let _ = sender.send(result);
            wake();
        });
    }

    pub fn poll(&mut self) -> Option<T> {
        match self.receiver.as_ref()?.try_recv() {
            Ok(result) => {
                self.receiver = None;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.receiver = None;
                tracing::warn!("shelf background job ended without a result");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_database_work_does_not_block_ui_polling() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut job = BackgroundJob::default();
        let (release, wait) = mpsc::channel();
        let (woke, wake) = mpsc::channel();
        job.start(
            &runtime,
            move || {
                wait.recv().unwrap();
                42
            },
            move || {
                woke.send(()).unwrap();
            },
        );
        for _ in 0..100 {
            assert_eq!(job.poll(), None);
        }
        assert!(job.is_running());
        release.send(()).unwrap();
        wake.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(job.poll(), Some(42));
        assert!(!job.is_running());
    }
}
