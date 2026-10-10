use rebook_layout::LayoutViewport;

use super::*;

#[derive(Clone)]
struct OpenRequest {
    book: LibraryBook,
    store: SyncStore,
    viewport: LayoutViewport,
    settings: Option<AppliedSettings>,
    started: std::time::Instant,
    initial_href: Option<rebook_publication::PublicationUrl>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
pub(super) struct Opening {
    pub(super) classic_sidebar: crate::reader::chrome::SidebarState,
    task: TaskSlot<OpenRequest>,
    shell: Option<super::opening_view::OpeningShell>,
    failed_request: Option<OpenRequest>,
    viewport: Option<LayoutViewport>,
    settings: Option<AppliedSettings>,
    // Blocking preparation cannot be aborted once running. Keep one worker
    // alive and replace only the queued request while geometry/settings change.
    worker: Option<(u64, tokio::task::JoinHandle<()>)>,
    first_presentation: Option<(String, std::time::Instant)>,
}

pub(crate) type OpenTaskMessage = TaskResult<DesktopReader>;
pub(crate) struct OpenHeaderMessage {
    pub id: u64,
    pub mode: crate::preferences::ReadingMode,
    pub toc: Arc<[rebook_publication::TocEntry]>,
    pub title: Option<String>,
    pub pdf_toolbar: crate::reader::chrome::PdfToolbarState,
}

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
        // A duplicate external open during preparation must retain the worker. A second click on
        // the same book must not discard that work and queue a full cold open.
        // Geometry/account/settings changes supersede it through their own paths.
        if self.task.active().is_some_and(|request| {
            request.book.id == book.id && request.store.path() == store.path()
        }) {
            return;
        }
        self.cancel_task();
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
        self.shell = Some(super::opening_view::OpeningShell::new(
            book.clone(),
            started,
            self.settings
                .as_ref()
                .map_or(crate::preferences::ReadingMode::Focus, |settings| {
                    settings.reading_mode
                }),
            self.classic_sidebar,
        ));
        self.failed_request = None;
        self.task.begin(OpenRequest {
            initial_href: None,
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            book,
            store,
            viewport: self
                .viewport
                .unwrap_or_else(crate::reader::default_open_viewport),
            settings: self.settings.clone(),
            started,
        });
    }

    fn cancel_task(&mut self) {
        if let Some(request) = self.task.active() {
            request
                .cancelled
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.task.cancel();
    }

    fn queue_replacement(&mut self, mut request: OpenRequest) {
        self.cancel_task();
        request.cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.task.begin(request);
    }

    fn navigate(&mut self, href: rebook_publication::PublicationUrl) {
        if let Some(mut request) = self
            .task
            .active()
            .cloned()
            .or_else(|| self.failed_request.take())
        {
            if request.initial_href.as_ref() == Some(&href) && self.task.is_pending() {
                return;
            }
            request.initial_href = Some(href.clone());
            self.queue_replacement(request);
            if let Some(shell) = &mut self.shell {
                shell.header_title = shell
                    .toc
                    .as_deref()
                    .and_then(|toc| crate::reader::chrome::toc_label_for_href(toc, &href))
                    .map(str::to_owned);
                shell.target = Some(href);
                shell.error = None;
            }
        }
    }

    pub(super) fn cancel(&mut self) {
        self.cancel_task();
        self.shell = None;
        self.failed_request = None;
        self.first_presentation = None;
    }

    fn retry(&mut self) {
        if let Some(mut request) = self.failed_request.take() {
            request.started = Instant::now();
            request.settings = self.settings.clone();
            request.viewport = self.viewport.unwrap_or(request.viewport);
            if let Some(shell) = &mut self.shell {
                shell.error = None;
                shell.started = request.started;
                shell.shell_presented = false;
                shell.toc_presented = false;
            }
            self.queue_replacement(request);
        }
    }

    pub(super) fn update_viewport(&mut self, ui: &egui::Ui) {
        let mut viewport = crate::reader::open_viewport(ui);
        let reserved = if let Some(shell) = &mut self.shell {
            shell.sidebar = shell.sidebar.constrained(ui.max_rect().width());
            shell.sidebar.reserved_width(shell.mode)
        } else {
            let mode = self
                .settings
                .as_ref()
                .map_or(crate::preferences::ReadingMode::Focus, |settings| {
                    settings.reading_mode
                });
            self.classic_sidebar
                .constrained(ui.max_rect().width())
                .reserved_width(mode)
        };
        viewport.width = viewport
            .width
            .saturating_sub(reserved.round() as u32)
            .max(1);
        self.viewport = Some(viewport);
        if let Some(mut request) = self.task.active().cloned()
            && request.viewport != viewport
        {
            request.viewport = viewport;
            self.queue_replacement(request);
        }
    }

    pub(super) fn restart(&mut self, settings: &AppliedSettings, store: Option<SyncStore>) {
        self.settings = Some(settings.clone());
        if let Some(shell) = &mut self.shell {
            shell.set_mode(settings.reading_mode, self.classic_sidebar);
        }
        if let Some(request) = self.failed_request.as_mut() {
            if let Some(store) = store.as_ref() {
                request.store = store.clone();
                request.settings = Some(settings.clone());
            } else {
                self.cancel();
            }
        }
        if let Some(mut request) = self.task.active().cloned() {
            self.cancel_task();
            if let Some(store) = store {
                request.store = store;
                request.settings = Some(settings.clone());
                request.cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.task.begin(request);
            } else {
                self.cancel();
            }
        }
    }

    fn accept_header(
        &mut self,
        message: OpenHeaderMessage,
    ) -> Option<(String, Arc<[rebook_publication::TocEntry]>)> {
        if self.task.active_id() != Some(message.id) {
            return None;
        }
        let shell = self.shell.as_mut()?;
        shell.set_effective_mode(message.mode, self.classic_sidebar);
        shell.toc = Some(message.toc.clone());
        shell.header_title = message.title;
        shell.pdf_toolbar = message.pdf_toolbar;
        Some((shell.book.id.clone(), message.toc))
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
        let header_proxy = proxy.clone();
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
                if request.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    return Err("superseded book open".to_owned());
                }
                let cache_book = request.book.clone();
                let cancelled = request.cancelled.clone();
                let mut reader = crate::reader::open_reader_staged(
                    &request.book.path,
                    fonts,
                    Some(BookDisplayMetadata::from(&request.book)),
                    request.book.cover_bytes,
                    request.store,
                    request.viewport,
                    request.initial_href.as_ref(),
                    move |toc, mode, title, pdf_toolbar| {
                        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                            return false;
                        }
                        let _ = header_proxy.send_event(
                            crate::platform::UserEvent::ShelfOpenHeader(OpenHeaderMessage {
                                id,
                                mode,
                                toc: toc.clone(),
                                title,
                                pdf_toolbar,
                            }),
                        );
                        if cache_book.cached_toc.as_deref() != Some(toc.as_ref())
                            && let Err(error) = crate::library::cache_book_toc(&cache_book, &toc)
                        {
                            tracing::warn!(%error, "failed to cache library TOC");
                        }
                        !cancelled.load(std::sync::atomic::Ordering::Acquire)
                    },
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
    pub(crate) fn remember_reader_sidebar(&mut self, sidebar: crate::reader::chrome::SidebarState) {
        self.sidebar_preferences.remember(sidebar);
        self.opening.classic_sidebar = self.sidebar_preferences.classic;
    }
    pub(crate) fn opening_error(&self) -> Option<&str> {
        self.opening
            .shell
            .as_ref()
            .and_then(|shell| shell.error.as_deref())
    }

    pub(crate) fn opening_visible(&self) -> bool {
        self.opening.shell.is_some()
    }
    pub(crate) fn opening_drawn(&self) -> bool {
        self.opening.shell.as_ref().is_some_and(|shell| shell.drawn)
    }

    pub(crate) fn complete_open_header(&mut self, message: OpenHeaderMessage) {
        if let Some((book_id, toc)) = self.opening.accept_header(message) {
            self.shelf.library.adopt_cached_toc(&book_id, toc);
        }
    }

    pub(crate) fn opening_ui(&mut self, ui: &mut egui::Ui, blocked: bool) {
        self.cover_textures.begin_frame(ui.ctx());
        let action = self.opening.shell.as_mut().and_then(|shell| {
            shell.ui(
                ui,
                &mut self.cover_textures,
                self.language,
                &self
                    .opening
                    .settings
                    .as_ref()
                    .map(|settings| settings.shortcuts.clone())
                    .unwrap_or_default(),
                blocked,
            )
        });
        if let Some(shell) = &self.opening.shell
            && shell.mode == crate::preferences::ReadingMode::Classic
        {
            self.sidebar_preferences.remember(shell.sidebar);
            self.opening.classic_sidebar = self.sidebar_preferences.classic;
        }
        // A pinned sidebar toggle changes the worker's initial content width.
        // Replace preparation before accepting any result for the old geometry.
        self.opening.update_viewport(ui);
        match action {
            Some(super::opening_view::Action::Back) => {
                self.opening.cancel();
                self.resume();
                ui.ctx().request_repaint();
            }
            Some(super::opening_view::Action::Retry) => {
                self.opening.retry();
                ui.ctx().request_repaint();
            }
            Some(super::opening_view::Action::Navigate(href)) => self.opening.navigate(href),
            None => {}
        }
    }

    pub(crate) fn record_open_presentation(&mut self, ready: bool) {
        if let Some(shell) = &mut self.opening.shell
            && shell.chrome_drawn()
        {
            for (event, recorded, available) in [
                (
                    "reader.open_shell_presented",
                    &mut shell.shell_presented,
                    true,
                ),
                (
                    "reader.open_toc_presented",
                    &mut shell.toc_presented,
                    shell.toc.is_some() && shell.sidebar.open,
                ),
            ] {
                if available && !*recorded {
                    *recorded = true;
                    crate::diagnostics::log(
                        event,
                        &[
                            crate::diagnostics::Field::Detail("book_id", &shell.book.id),
                            crate::diagnostics::Field::F32(
                                "elapsed_ms",
                                shell.started.elapsed().as_secs_f32() * 1000.0,
                            ),
                        ],
                    );
                }
            }
        }
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
        if let Some(shell) = &mut self.opening.shell {
            shell.drawn = false;
        }
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
            self.opening.cancel();
            runtime.spawn_blocking(move || drop(message));
            return false;
        }
        match message.result {
            Ok(mut reader) => {
                if let Some(shell) = self.opening.shell.take() {
                    reader.adopt_opening_chrome(
                        shell.mode,
                        shell.sidebar,
                        self.opening.classic_sidebar,
                    );
                }
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
                self.opening.failed_request = Some(request);
                if let Some(shell) = &mut self.opening.shell {
                    shell.error = Some(error);
                }

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
                cached_toc: None,
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
    fn opening_layout_uses_visible_pinned_sidebar_width_and_rejects_old_geometry() {
        use crate::preferences::ReadingMode;
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "book", &store);
        let first = opening.take_pending().unwrap();
        let ctx = egui::Context::default();
        for (mode, open, pinned, width) in [
            (ReadingMode::Classic, false, true, 1200),
            (ReadingMode::Classic, true, true, 890),
            (ReadingMode::Classic, true, false, 1200),
            (ReadingMode::Focus, true, false, 1200),
        ] {
            let shell = opening.shell.as_mut().unwrap();
            shell.mode = mode;
            shell.sidebar = crate::reader::chrome::SidebarState {
                open,
                pinned,
                width: 310.0,
            };
            shell.drawn = true;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| opening.update_viewport(ui),
            );
            output.textures_delta.clear();
            assert_eq!(opening.task.active().unwrap().viewport.width, width);
            assert!(opening.shell.as_ref().unwrap().drawn);
        }
        assert!(opening.complete(first.id).is_none());
        let active = opening.task.active_id().unwrap();
        let sidebar = opening.shell.as_ref().unwrap().sidebar;
        opening.accept_header(OpenHeaderMessage {
            pdf_toolbar: Default::default(),
            id: active,
            title: None,
            mode: ReadingMode::Focus,
            toc: Arc::from([]),
        });
        assert_eq!(opening.shell.as_ref().unwrap().sidebar, sidebar);
    }

    #[test]
    fn retry_retains_temporary_sidebar_and_selected_chapter() {
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "book", &store);
        let href = rebook_publication::PublicationUrl::parse("chapter.xhtml#target").unwrap();
        opening.navigate(href.clone());
        let failed = opening.take_pending().unwrap();
        opening.failed_request = opening.complete(failed.id);
        opening.shell.as_mut().unwrap().sidebar.open = true;
        opening.shell.as_mut().unwrap().error = Some("failed".into());
        opening.retry();
        assert!(opening.shell.as_ref().unwrap().sidebar.open);
        assert!(opening.shell.as_ref().unwrap().error.is_none());
        assert_eq!(
            opening.take_pending().unwrap().payload.initial_href,
            Some(href)
        );
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
    #[test]
    fn shell_is_immediate_and_cancelled_workers_cannot_restore_it() {
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "first", &store);
        assert_eq!(opening.shell.as_ref().unwrap().book.id, "first");
        assert!(opening.worker.is_none());
        let first = opening.take_pending().unwrap();
        opening.cancel();
        assert!(
            first
                .payload
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert!(opening.shell.is_none());
        assert!(opening.complete(first.id).is_none());
    }

    #[test]
    fn toc_navigation_coalesces_to_latest_target_and_keeps_the_shell() {
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "book", &store);
        let first = opening.take_pending().unwrap();
        let first_href = rebook_publication::PublicationUrl::parse("chapter.xhtml#first").unwrap();
        let latest_href =
            rebook_publication::PublicationUrl::parse("chapter.xhtml#latest").unwrap();
        opening.navigate(first_href);
        let middle_cancelled = opening.task.active().unwrap().cancelled.clone();
        opening.navigate(latest_href.clone());
        assert!(middle_cancelled.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            first
                .payload
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert!(opening.complete(first.id).is_none());
        let latest = opening.take_pending().unwrap();
        assert_eq!(latest.payload.initial_href, Some(latest_href.clone()));
        assert_eq!(opening.shell.as_ref().unwrap().target, Some(latest_href));
        assert_eq!(latest.payload.started, first.payload.started);
        assert!(
            !latest
                .payload
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
        );
    }
    #[test]
    fn choosing_a_chapter_after_failure_retries_inside_the_reader_shell() {
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "book", &store);
        let failed = opening.take_pending().unwrap();
        opening.failed_request = opening.complete(failed.id);
        opening.shell.as_mut().unwrap().error = Some("chapter failed".into());
        let target = rebook_publication::PublicationUrl::parse("another.xhtml#start").unwrap();
        opening.failed_request.as_mut().unwrap().initial_href = Some(target.clone());
        opening.navigate(target.clone());
        assert!(opening.failed_request.is_none());
        assert!(opening.shell.as_ref().unwrap().error.is_none());
        assert_eq!(
            opening.take_pending().unwrap().payload.initial_href,
            Some(target)
        );
    }

    #[test]
    fn headers_are_rejected_after_book_target_or_cancel_changes() {
        let store = store();
        let mut opening = Opening::default();
        request(&mut opening, "old", &store);
        let first = opening.take_pending().unwrap();
        request(&mut opening, "new", &store);
        let toc: Arc<[rebook_publication::TocEntry]> = vec![rebook_publication::TocEntry {
            label: "New chapter".into(),
            href: None,
            children: vec![],
        }]
        .into();
        assert!(
            opening
                .accept_header(OpenHeaderMessage {
                    pdf_toolbar: Default::default(),
                    id: first.id,
                    title: Some("Stale chapter".into()),
                    mode: crate::preferences::ReadingMode::Focus,
                    toc: toc.clone()
                })
                .is_none()
        );
        assert!(opening.shell.as_ref().unwrap().toc.is_none());
        assert!(opening.shell.as_ref().unwrap().header_title.is_none());
        let current = opening.task.active_id().unwrap();
        assert!(
            opening
                .accept_header(OpenHeaderMessage {
                    pdf_toolbar: Default::default(),
                    id: current,
                    title: Some("Current chapter".into()),
                    mode: crate::preferences::ReadingMode::Focus,
                    toc: toc.clone()
                })
                .is_some()
        );
        assert_eq!(
            opening.shell.as_ref().unwrap().toc.as_deref(),
            Some(toc.as_ref())
        );
        assert_eq!(
            opening.shell.as_ref().unwrap().header_title.as_deref(),
            Some("Current chapter")
        );
        opening.cancel();
        assert!(
            opening
                .accept_header(OpenHeaderMessage {
                    pdf_toolbar: Default::default(),
                    id: current,
                    title: None,
                    toc,
                    mode: crate::preferences::ReadingMode::Focus
                })
                .is_none()
        );
    }
}
