use rebook_layout::LayoutViewport;

use super::*;

#[derive(Clone)]
struct OpenRequest {
    book: LibraryBook,
    store: SyncStore,
    viewport: LayoutViewport,
    settings: Option<AppliedSettings>,
    started: std::time::Instant,
}

#[derive(Default)]
pub(super) struct Opening {
    task: TaskSlot<OpenRequest>,
    viewport: Option<LayoutViewport>,
    settings: Option<AppliedSettings>,
    // Blocking preparation cannot be aborted once running. Keep one worker
    // alive and replace only the queued request while geometry/settings change.
    worker: Option<(u64, tokio::task::JoinHandle<()>)>,
    first_presentation: Option<(String, std::time::Instant)>,
}

pub(crate) type OpenTaskMessage = TaskResult<DesktopReader>;

impl Opening {
    #[cfg(test)]
    pub(super) fn begin(&mut self, book: LibraryBook, store: SyncStore) {
        self.begin_at(book, store, std::time::Instant::now());
    }

    pub(super) fn begin_at(
        &mut self,
        book: LibraryBook,
        store: SyncStore,
        started: std::time::Instant,
    ) {
        // The shelf remains interactive during preparation. A second click on
        // the same book must not discard that work and queue a full cold open.
        // Geometry/account/settings changes supersede it through their own paths.
        if self.task.active().is_some_and(|request| {
            request.book.id == book.id && request.store.path() == store.path()
        }) {
            return;
        }
        self.task.cancel();
        self.first_presentation = None;
        crate::diagnostics::log(
            "reader.open_requested",
            &[
                crate::diagnostics::Field::Detail("book_id", &book.id),
                crate::diagnostics::Field::F32(
                    "prepare_request_ms",
                    started.elapsed().as_secs_f32() * 1000.0,
                ),
            ],
        );
        self.task.begin(OpenRequest {
            book,
            store,
            viewport: self
                .viewport
                .unwrap_or_else(crate::reader::default_open_viewport),
            settings: self.settings.clone(),
            started,
        });
    }

    pub(super) fn cancel(&mut self) {
        self.task.cancel();
        self.first_presentation = None;
    }

    pub(super) fn update_viewport(&mut self, ui: &egui::Ui) {
        let viewport = crate::reader::open_viewport(ui);
        self.viewport = Some(viewport);
        if let Some(mut request) = self.task.active().cloned()
            && request.viewport != viewport
        {
            request.viewport = viewport;
            self.task.cancel();
            self.task.begin(request);
        }
    }

    pub(super) fn restart(&mut self, settings: &AppliedSettings, store: Option<SyncStore>) {
        self.settings = Some(settings.clone());
        if let Some(request) = self.task.active().cloned() {
            self.cancel();
            if let Some(store) = store {
                self.begin_at(request.book, store, request.started);
            }
        }
    }

    fn take_pending(&mut self) -> Option<crate::async_task::PendingTask<OpenRequest>> {
        if self.worker.is_some() {
            return None;
        }
        self.task.take_pending()
    }

    fn complete(&mut self, id: u64) -> Option<OpenRequest> {
        if self
            .worker
            .as_ref()
            .is_some_and(|(worker_id, _)| *worker_id == id)
        {
            self.worker = None;
        }
        self.task.complete(id)
    }

    pub(super) fn spawn(
        &mut self,
        fonts: Arc<[Blob<u8>]>,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<crate::platform::UserEvent>,
    ) {
        let Some(request) = self.take_pending() else {
            return;
        };
        let id = request.id;
        let proxy = proxy.clone();
        let worker = runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let request = request.payload;
                let worker_started = std::time::Instant::now();
                crate::diagnostics::log(
                    "reader.open_worker",
                    &[
                        crate::diagnostics::Field::Detail("book_id", &request.book.id),
                        crate::diagnostics::Field::F32(
                            "queue_ms",
                            request.started.elapsed().as_secs_f32() * 1000.0,
                        ),
                    ],
                );
                let mut reader = crate::reader::open_reader_in_viewport(
                    &request.book.path,
                    fonts,
                    Some(BookDisplayMetadata::from(&request.book)),
                    request.book.cover_bytes,
                    request.store,
                    request.viewport,
                )
                .map_err(|error| error.to_string())?;
                if let Some(settings) = request.settings {
                    reader.apply_global_settings(&settings);
                }
                crate::diagnostics::log(
                    "reader.open_prepared",
                    &[
                        crate::diagnostics::Field::Detail("book_id", &request.book.id),
                        crate::diagnostics::Field::F32(
                            "worker_ms",
                            worker_started.elapsed().as_secs_f32() * 1000.0,
                        ),
                        crate::diagnostics::Field::F32(
                            "elapsed_ms",
                            request.started.elapsed().as_secs_f32() * 1000.0,
                        ),
                    ],
                );
                Ok(reader)
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            let _ = proxy.send_event(crate::platform::UserEvent::ShelfOpen(OpenTaskMessage {
                id: request.id,
                result,
            }));
        });
        self.worker = Some((id, worker));
    }
}

impl Drop for Opening {
    fn drop(&mut self) {
        if let Some((_, worker)) = self.worker.take() {
            worker.abort();
        }
    }
}

impl ShelfFeature {
    pub(crate) fn record_open_presentation(&mut self, ready: bool) {
        if ready && let Some((book_id, started)) = self.opening.first_presentation.take() {
            crate::diagnostics::log(
                "reader.open_presented",
                &[
                    crate::diagnostics::Field::Detail("book_id", &book_id),
                    crate::diagnostics::Field::F32(
                        "elapsed_ms",
                        started.elapsed().as_secs_f32() * 1000.0,
                    ),
                ],
            );
        }
    }

    pub(crate) fn update_open_viewport(&mut self, ui: &egui::Ui) {
        self.opening.update_viewport(ui);
    }

    pub(crate) fn complete_open(
        &mut self,
        message: OpenTaskMessage,
        runtime: &tokio::runtime::Runtime,
    ) -> bool {
        let Some(request) = self.opening.complete(message.id) else {
            runtime.spawn_blocking(move || drop(message));
            return false;
        };
        if !self
            .shelf
            .library
            .books()
            .iter()
            .any(|book| book.id == request.book.id)
        {
            runtime.spawn_blocking(move || drop(message));
            return false;
        }
        match message.result {
            Ok(reader) => {
                crate::diagnostics::log(
                    "reader.open_accepted",
                    &[
                        crate::diagnostics::Field::Detail("book_id", &request.book.id),
                        crate::diagnostics::Field::F32(
                            "elapsed_ms",
                            request.started.elapsed().as_secs_f32() * 1000.0,
                        ),
                    ],
                );
                self.opening.first_presentation = Some((request.book.id.clone(), request.started));
                let opened_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX);
                self.read_activity.record(
                    request.store,
                    request.book.id.clone(),
                    reader.progress_locator(),
                    opened_ms,
                );
                self.refresh_read_activity();
                self.cover_textures.suspend();
                self.pending_reader = Some(reader);
                self.last_opened_book_id = Some(request.book.id);
                self.shelf.error = None;
                self.shelf.error_dismiss_at = None;
                true
            }
            Err(error) => {
                self.show_error(format!(
                    "{}: {error}",
                    self.language.text("无法打开书籍", "Unable to open book")
                ));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(opening: &mut Opening, id: &str, store: &SyncStore) {
        opening.begin(
            LibraryBook {
                id: id.into(),
                title: id.into(),
                authors: vec![],
                file_name: format!("{id}.epub"),
                path: PathBuf::from(format!("{id}.epub")),
                cover_bytes: None,
                added_at: 0,
            },
            store.clone(),
        );
    }

    fn store() -> SyncStore {
        SyncStore::open_at(
            std::env::temp_dir().join(format!("torto-open-{}.sqlite3", uuid::Uuid::new_v4())),
            "open-test",
        )
        .unwrap()
    }

    #[test]
    fn repeated_click_keeps_the_running_open_and_its_completion() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "same", &store);
        let first = opening.take_pending().unwrap();
        let started = first.payload.started;
        opening.worker = Some((first.id, runtime.spawn(std::future::pending())));
        request(&mut opening, "same", &store);
        request(&mut opening, "same", &store);
        assert_eq!(opening.task.active().unwrap().started, started);
        assert!(opening.take_pending().is_none());
        opening.worker.as_ref().unwrap().1.abort();
        assert_eq!(opening.complete(first.id).unwrap().book.id, "same");
        assert!(opening.take_pending().is_none());
    }

    #[test]
    fn replacement_waits_for_blocking_preparation_and_rejects_its_result() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "first", &store);
        let first = opening.take_pending().unwrap();
        opening.worker = Some((first.id, runtime.spawn(std::future::pending())));
        request(&mut opening, "second", &store);
        request(&mut opening, "latest", &store);
        assert!(opening.take_pending().is_none());
        assert_eq!(opening.task.active().unwrap().book.id, "latest");
        let (_, worker) = opening.worker.as_ref().unwrap();
        worker.abort(); // End this stand-in for the completed blocking worker.
        assert!(opening.complete(first.id).is_none());
        let latest = opening.take_pending().unwrap();
        assert_eq!(latest.payload.book.id, "latest");
        assert_eq!(opening.complete(latest.id).unwrap().book.id, "latest");
    }

    #[test]
    fn cancel_rejects_completion_and_geometry_updates_keep_only_latest_request() {
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "first", &store);
        let first = opening.take_pending().unwrap();
        opening.cancel();
        assert!(opening.complete(first.id).is_none());
        assert!(opening.task.active().is_none());
        request(&mut opening, "second", &store);
        let ctx = egui::Context::default();
        for width in [1200.0, 1000.0, 800.0] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 700.0),
                    )),
                    ..Default::default()
                },
                |ui| opening.update_viewport(ui),
            );
            output.textures_delta.clear();
        }
        let latest = opening.take_pending().unwrap();
        assert_eq!(latest.payload.viewport.width, 800);
        assert_eq!(Some(latest.payload.viewport), opening.viewport);
        assert_eq!(latest.payload.book.id, "second");
    }
}
