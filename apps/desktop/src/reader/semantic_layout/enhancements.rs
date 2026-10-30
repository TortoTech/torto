use super::*;
use std::collections::VecDeque;

type Completion = (Job, Result<Option<Recognition>, String>);

#[derive(Default)]
pub(super) struct State {
    pending: VecDeque<(Job, Recognition)>,
    worker: Option<tokio::task::JoinHandle<()>>,
    receiver: Option<mpsc::Receiver<Completion>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_formula_metadata_updates_an_already_open_preview() {
        let (mut reader, original, range) = super::super::tests::fixture();
        reader.plugin_settings.semantic_layout.enabled = true;
        reader
            .semantic_source
            .configure("late-preview-notes", &reader.plugin_settings);
        let ctx = egui::Context::default();
        let mut preview = crate::ui::image_preview::ImagePreview::new(
            &ctx,
            egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
            "late-notes-test",
        );
        preview.copy_text = Some("x".into());
        preview.formula_source = Some(range.clone());
        preview.notes_open = true;
        reader.image_preview = Some(preview);
        reader.refresh_semantic_layout();
        assert!(
            reader
                .image_preview
                .as_ref()
                .unwrap()
                .formula_notes
                .is_empty()
        );
        let result = serde_json::from_value(serde_json::json!({
            "fingerprint":fingerprint(&original), "formulas_checked":true,
            "annotations":[{"FormulaNotes":{
                "source":range, "latex":"x", "language":"English",
                "notes":[{"symbol":"x","meaning":"Position","unit":"m","known":true,"evidence":"Position"}]
            }}]
        })).unwrap();
        assert!(reader.semantic_source.install(0, result));
        reader.refresh_semantic_layout();
        let preview = reader.image_preview.as_ref().unwrap();
        assert_eq!(preview.formula_notes.len(), 1);
        assert_eq!(preview.formula_notes[0].symbol, "x");
        assert_eq!(preview.formula_notes[0].meaning, "Position");
        assert_eq!(preview.formula_notes[0].unit, "m");
        assert!(preview.notes_open);
    }

    #[test]
    fn translations_commit_while_enhancements_are_pending_and_stale_results_are_rejected() {
        let (mut reader, original, range) = super::super::tests::fixture();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        reader.translation.enabled = true;
        reader.translation_source.set_enabled(true).unwrap();
        reader.plugin_settings.providers[0].api_key = "fixture".into();
        reader.plugin_settings.providers[0].base_url = "http://127.0.0.1:9".into();
        let hash = fingerprint(&original);
        reader
            .semantic_layout
            .originals
            .insert(0, std::sync::Arc::new(original.clone()));
        reader.semantic_layout.hashes.insert(0, hash.clone());
        reader.semantic_layout.inputs.insert(
            0,
            crate::plugins::prepare_translation_inputs(&original, false),
        );
        let job = Job {
            id: "split-enhancements".into(),
            started: Instant::now(),
            provider: reader.plugin_settings.providers[0].clone(),
            model: "fixture".into(),
            index: 0,
            offset: 0,
            target: 1..2,
            section: original.clone(),
            hash,
            scope_sources: vec![range.clone()],
        };
        assert!(!reader.translation_semantics_ready(0, 1));
        reader.stage_semantic_result(&job, empty_recognition(&original), true);
        assert!(reader.translation_semantics_ready(0, 1));
        reader.semantic_layout.enhancements.worker = Some(runtime.spawn(std::future::pending()));
        reader.stage_translation_batch(
            0,
            vec![BlockTranslation {
                block_index: 1,
                segment_index: None,
                text: "Translation before chart metadata".into(),
            }],
        );
        assert!(reader.commit_ready_content(&vec![(0, vec![range])], true));
        assert!(
            !reader
                .semantic_layout
                .enhancements
                .worker
                .as_ref()
                .unwrap()
                .is_finished()
        );
        let displayed = reader.semantic_source.parse_section(0).unwrap();
        assert!(
            crate::reader::block_focus_text(&displayed.blocks[1])
                .contains("Translation before chart metadata")
        );
        reader
            .semantic_layout
            .hashes
            .insert(0, "changed-source".into());
        reader.stage_semantic_result(&job, empty_recognition(&original), false);
        assert!(reader.semantic_layout.groups.is_empty());
        reader.invalidate_semantic_plan("test_configuration_changed");
        assert!(reader.semantic_layout.enhancements.worker.is_none());
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

impl DesktopReader {
    pub(super) fn stage_semantic_result(&mut self, job: &Job, result: Recognition, base: bool) {
        if self.semantic_layout.hashes.get(&job.index) != Some(&job.hash) {
            return;
        }
        for (range, result) in recognition_groups(&job.section, job.target.clone(), result) {
            let sources = job.section.blocks[range.clone()]
                .iter()
                .flat_map(block_ranges)
                .collect();
            let range = range.start + job.offset..range.end + job.offset;
            if base {
                for block in range.clone() {
                    self.semantic_layout
                        .done
                        .insert((job.index, block), job.hash.clone());
                }
            }
            self.stage_semantic_group(Group {
                index: job.index,
                range,
                sources,
                hash: job.hash.clone(),
                result: Some(result),
            });
        }
    }

    pub(super) fn queue_semantic_enhancements(&mut self, job: Job, base: Recognition) {
        let pending = &mut self.semantic_layout.enhancements.pending;
        // Retain a bounded backlog while the reader advances through prefetched scopes.
        if pending.len() >= 32 {
            pending.pop_front();
        }
        pending.push_back((job, base));
    }

    pub(super) fn tick_semantic_enhancements(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<UserEvent>,
        demand: &Demand,
    ) {
        let completion = self
            .semantic_layout
            .enhancements
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        if let Some((job, result)) = completion {
            self.semantic_layout.enhancements.worker = None;
            self.semantic_layout.enhancements.receiver = None;
            job.log(
                "enhancements.received",
                serde_json::json!({"success":result.is_ok()}),
            );
            if let Ok(Some(result)) = result {
                self.stage_semantic_result(&job, result, false);
            }
        }
        if self.semantic_layout.enhancements.worker.is_some() {
            return;
        }
        let pending = &mut self.semantic_layout.enhancements.pending;
        pending.retain(|(job, _)| self.semantic_layout.hashes.get(&job.index) == Some(&job.hash));
        let next = pending
            .iter()
            .position(|(job, _)| relevant(job.index, &job.scope_sources, demand));
        let Some((job, base)) = next
            .and_then(|index| pending.remove(index))
            .or_else(|| pending.pop_front())
        else {
            return;
        };
        let settings = self.plugin_settings.clone();
        let book =
            crate::plugins::pdf_native::cache_identity(&self.book_id, self.rewrite_source.as_ref());
        let proxy = proxy.clone();
        let (tx, rx) = mpsc::channel();
        self.semantic_layout.enhancements.receiver = Some(rx);
        job.log("enhancements.scheduled", serde_json::json!({}));
        let id = job.id.clone();
        self.semantic_layout.enhancements.worker = Some(runtime.spawn(
            crate::plugins::semantic_layout::with_job(id, async move {
                let result = tokio::time::timeout(
                    Duration::from_secs(180),
                    crate::plugins::semantic_layout::enhance_visible(
                        &job.section,
                        job.target.clone(),
                        &book,
                        &settings,
                        base,
                    ),
                )
                .await
                .unwrap_or_else(|_| Err("AI enhancements timed out".into()));
                let _ = tx.send((job, result));
                let _ = proxy.send_event(UserEvent::RepaintAfter(Duration::ZERO));
            }),
        ));
    }
}
