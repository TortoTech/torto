use super::*;
use rebook_app_core::plugins::semantic_layout::{PassageChart, configuration_key};

pub(super) struct PendingPassageChart {
    source: SourceRange,
    index: usize,
    revision: u64,
    configuration: Option<String>,
    translated: bool,
    worker: Option<tokio::task::JoinHandle<()>>,
    receiver: Option<mpsc::Receiver<Result<Option<PassageChart>, String>>>,
}

impl Drop for PendingPassageChart {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

impl DesktopReader {
    pub(in crate::reader) fn queue_passage_chart(&mut self, index: usize, source: SourceRange) {
        if self.semantic_layout.passage_chart.is_some()
            || self.plugin_settings.semantic_layout_endpoint().is_err()
        {
            return;
        }
        self.semantic_layout.passage_chart = Some(PendingPassageChart {
            source,
            index,
            revision: self.rewrite_source.revision(),
            configuration: passage_key(&self.book_id, &self.plugin_settings),
            translated: self.translation.enabled,
            worker: None,
            receiver: None,
        });
    }

    pub(in crate::reader) fn passage_chart_pending(&self, source: &SourceRange) -> bool {
        self.semantic_layout
            .passage_chart
            .as_ref()
            .is_some_and(|job| job.source.start == source.start)
    }

    pub(super) fn tick_passage_chart(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
    ) {
        if self.semantic_layout.passage_chart_translated != self.translation.enabled {
            self.semantic_layout.passage_chart_translated = self.translation.enabled;
            self.semantic_source.clear_passage_charts();
        }
        let Some(job) = self.semantic_layout.passage_chart.as_mut() else {
            return;
        };
        if job.revision != self.rewrite_source.revision()
            || job.configuration != passage_key(&self.book_id, &self.plugin_settings)
            || job.translated != self.translation.enabled
        {
            self.semantic_layout.passage_chart = None;
            let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
            return;
        }
        if let Some(receiver) = &job.receiver {
            let outcome = match receiver.try_recv() {
                Ok(outcome) => outcome,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => Ok(None),
            };
            let source = job.source.clone();
            self.semantic_layout.passage_chart = None;
            if let Ok(Some(chart)) = outcome
                && self.semantic_source.install_passage_chart(chart)
            {
                if self
                    .focus_units
                    .get(self.focus_unit_index)
                    .is_some_and(|unit| unit.range.start == source.start)
                {
                    self.semantic_layout.presentation_anchor = Some(source);
                }
                self.refresh_semantic_layout();
            }
            let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
            return;
        }
        let index = job.index;
        let target = job.source.clone();
        let task = rebook_app_core::reading_ai::Task {
            key: serde_json::json!([
                "passage-chart",
                job.configuration,
                job.source,
                job.translated
            ])
            .to_string(),
            kind: rebook_app_core::reading_ai::Kind::Enhancement,
            section: index,
            sources: vec![target.clone()],
            prefetch: false,
        };
        let engine = self.semantic_source.clone();
        let settings = self.plugin_settings.clone();
        let scheduler = self.semantic_layout.scheduler.clone();
        let proxy = proxy.clone();
        let (tx, rx) = mpsc::channel();
        job.receiver = Some(rx);
        job.worker = Some(runtime.spawn(async move {
            let result = scheduler
                .run(
                    task,
                    &Default::default(),
                    engine.visualize_passage(index, target, &settings),
                )
                .await;
            let _ = tx.send(result);
            let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
        }));
    }
}

fn passage_key(book: &str, settings: &crate::plugins::PluginSettings) -> Option<String> {
    let mut effective = settings.clone();
    effective.semantic_layout.enabled = true;
    configuration_key(book, &effective)
}
