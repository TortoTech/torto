use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::platform::UserEvent;
use crate::plugins::AiProvider;
use crate::plugins::semantic_layout::{
    Recognition, SemanticLayoutSettings, fingerprint, recognize_with_source as recognize,
};
use rebook_publication::RenditionLayout;

use super::{DesktopReader, SnapshotEffects};

type ResultMessage = (usize, String, Result<Recognition, String>);

#[derive(Default)]
pub(super) struct SemanticLayoutState {
    config: Option<(SemanticLayoutSettings, Option<AiProvider>)>,
    worker: Option<tokio::task::JoinHandle<()>>,
    active_section: Option<usize>,
    receiver: Option<mpsc::Receiver<ResultMessage>>,
    ready: Option<ResultMessage>,
    // Includes negative results and failures: do not repeatedly bill on redraw.
    scanned: HashMap<usize, String>,
    next_check: Option<Instant>,
}

impl Drop for SemanticLayoutState {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

impl DesktopReader {
    pub(super) fn tick_semantic_layout(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        self.sync_semantic_layout_settings(proxy);
        let config = &self.plugin_settings.semantic_layout;
        if !config.enabled || self.source.book().metadata.layout == RenditionLayout::PrePaginated {
            return;
        }
        let current = self.snapshot.location.section_index;
        if self
            .semantic_layout
            .active_section
            .is_some_and(|index| index != current && index != current + 1)
        {
            if let Some(worker) = self.semantic_layout.worker.take() {
                worker.abort();
            }
            self.semantic_layout.receiver = None;
            self.semantic_layout.ready = None;
            self.semantic_layout.active_section = None;
            self.semantic_layout.next_check = None;
        }
        if self.semantic_layout.ready.is_none() {
            self.semantic_layout.ready = self
                .semantic_layout
                .receiver
                .as_ref()
                .and_then(|rx| rx.try_recv().ok());
        }
        let interacting = self.selection.is_some()
            || self.selection_anchor.is_some()
            || self.focus_selection_anchor.is_some()
            || self.ui.focus_scroll_motion.is_some()
            || self.annotation_note_draft.is_some();
        if !interacting && let Some((index, hash, result)) = self.semantic_layout.ready.take() {
            self.semantic_layout.receiver = None;
            self.semantic_layout.worker = None;
            self.semantic_layout.active_section = None;
            self.semantic_layout.scanned.insert(index, hash);
            match result {
                Ok(result) => {
                    if self.semantic_source.install(index, result) {
                        self.refresh_semantic_layout();
                        let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, index, "AI semantic layout failed; retaining original layout");
                    self.notice_timer.show(
                        &mut self.notice,
                        self.language
                            .text(
                                "AI排版失败，已保留原排版",
                                "AI layout failed; original layout retained",
                            )
                            .into(),
                        Instant::now(),
                    );
                }
            }
        }
        if self.semantic_layout.worker.is_some() {
            return;
        }
        if let Some(next) = self.semantic_layout.next_check
            && Instant::now() < next
        {
            let _ = proxy.send_event(UserEvent::RepaintAfter(
                next.saturating_duration_since(Instant::now()),
            ));
            return;
        }
        self.semantic_layout.next_check = Some(Instant::now() + Duration::from_secs(1));
        if self.plugin_settings.semantic_layout_endpoint().is_err() {
            return;
        }
        for index in
            current..=(current + 1).min(self.source.book().sections.len().saturating_sub(1))
        {
            let original = self.semantic_source.original();
            let Ok(section) = original.parse_section(index) else {
                continue;
            };
            let hash = fingerprint(&section);
            if self.semantic_source.has_recognition(index, &hash) {
                self.semantic_layout.scanned.insert(index, hash);
                continue;
            }
            if self.semantic_layout.scanned.get(&index) == Some(&hash) {
                continue;
            }
            let settings = self.plugin_settings.clone();
            let book_id = self.book_id.clone();
            let proxy = proxy.clone();
            let (tx, rx) = mpsc::channel();
            self.semantic_layout.receiver = Some(rx);
            self.semantic_layout.active_section = Some(index);
            self.semantic_layout.worker = Some(runtime.spawn(async move {
                let result = recognize(&section, &book_id, &settings, original.as_ref()).await;
                let _ = tx.send((index, hash, result));
                let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
            }));
            break;
        }
    }

    fn sync_semantic_layout_settings(
        &mut self,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        let config = self.plugin_settings.semantic_layout.clone();
        let provider = self
            .plugin_settings
            .providers
            .iter()
            .find(|p| p.id == config.provider)
            .cloned();
        let identity = (config.clone(), provider);
        if self.semantic_layout.config.as_ref() != Some(&identity) {
            if let Some(worker) = self.semantic_layout.worker.take() {
                worker.abort();
            }
            self.semantic_layout.receiver = None;
            self.semantic_layout.active_section = None;
            self.semantic_layout.ready = None;
            self.semantic_layout.scanned.clear();
            self.semantic_layout.next_check = None;
            let had_config = self.semantic_layout.config.is_some();
            self.semantic_layout.config = Some(identity);
            if had_config {
                self.semantic_source
                    .configure(&self.book_id, &self.plugin_settings);
                self.refresh_semantic_layout();
                let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
            }
        }
    }

    fn refresh_semantic_layout(&mut self) {
        let scroll_source = self.progress_source_range();
        let focus_reflow_anchor =
            self.capture_focus_reflow_anchor(super::FocusReflowKind::DocumentLayout);
        match self.reader.refresh_source() {
            Ok(snapshot) => {
                self.apply_snapshot(snapshot, SnapshotEffects::static_content_change());
                self.focus_reflow_anchor = focus_reflow_anchor;
                if self.is_scroll_mode() && !self.is_focus_mode() {
                    self.scroll_target_source = scroll_source;
                }
            }
            Err(error) => tracing::warn!(%error, "failed to refresh semantic layout"),
        }
    }
}

#[cfg(test)]
mod tests;
