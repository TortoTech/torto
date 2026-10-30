//! One lazily prepared formula + notes texture, shared by both preview paths.
use crate::plugins::semantic_layout::enhancements::SymbolNote;
use std::sync::mpsc;

#[derive(Clone, PartialEq, Eq)]
struct Key {
    latex: String,
    notes: Vec<rebook_publication::FormulaNote>,
}
struct Worker {
    requests: mpsc::SyncSender<(Key, egui::Context)>,
    results: mpsc::Receiver<(Key, Result<egui::ColorImage, String>)>,
}
impl Default for Worker {
    fn default() -> Self {
        let (requests, receiver) = mpsc::sync_channel::<(Key, egui::Context)>(1);
        let (sender, results) = mpsc::channel();
        std::thread::Builder::new()
            .name("formula-notes-image".into())
            .spawn(move || {
                while let Ok((key, ctx)) = receiver.recv() {
                    let result = rebook_layout::formula_notes::preview(&key.latex, &key.notes).map(
                        |image| {
                            egui::ColorImage::from_rgba_premultiplied(
                                [image.width as usize, image.height as usize],
                                &image.pixels,
                            )
                        },
                    );
                    if sender.send((key, result)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("formula notes worker should start");
        Self { requests, results }
    }
}
#[derive(Default)]
pub(crate) struct FormulaNotesView {
    worker: Option<Worker>,
    requested: Option<Key>,
    ready: Option<(Key, egui::TextureHandle)>,
}
impl FormulaNotesView {
    pub(crate) fn image(
        &mut self,
        ctx: &egui::Context,
        latex: &str,
        notes: &[SymbolNote],
    ) -> Option<(egui::TextureHandle, egui::Vec2)> {
        let key = Key {
            latex: latex.into(),
            notes: notes.iter().map(SymbolNote::render_note).collect(),
        };
        let worker = self.worker.get_or_insert_with(Worker::default);
        while let Ok((completed, result)) = worker.results.try_recv() {
            if self.requested.as_ref() == Some(&completed) {
                self.ready = result.ok().map(|image| {
                    (
                        completed,
                        ctx.load_texture("formula-with-notes", image, egui::TextureOptions::LINEAR),
                    )
                });
            }
        }
        if self.requested.as_ref() != Some(&key)
            && worker.requests.try_send((key.clone(), ctx.clone())).is_ok()
        {
            self.requested = Some(key.clone());
        }
        self.ready
            .as_ref()
            .filter(|(ready, _)| *ready == key)
            .map(|(_, texture)| (texture.clone(), texture.size_vec2()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rejects_obsolete_formula_pixels_and_reuses_completed_texture() {
        let (requests, receiver) = mpsc::sync_channel(1);
        let (sender, results) = mpsc::channel();
        let mut view = FormulaNotesView {
            worker: Some(Worker { requests, results }),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        assert!(view.image(&ctx, "x", &[]).is_none());
        let (old, _) = receiver.try_recv().unwrap();
        assert!(view.image(&ctx, "y", &[]).is_none());
        let (current, _) = receiver.try_recv().unwrap();
        sender
            .send((
                old,
                Ok(egui::ColorImage::filled([8, 16], egui::Color32::WHITE)),
            ))
            .unwrap();
        assert!(view.image(&ctx, "y", &[]).is_none());
        assert!(
            receiver.try_recv().is_err(),
            "pending frames must not duplicate work"
        );
        sender
            .send((
                current,
                Ok(egui::ColorImage::filled([12, 24], egui::Color32::WHITE)),
            ))
            .unwrap();
        let (texture, size) = view.image(&ctx, "y", &[]).unwrap();
        assert_eq!(size, egui::vec2(12.0, 24.0));
        assert_eq!(view.image(&ctx, "y", &[]).unwrap().0.id(), texture.id());
        assert!(
            receiver.try_recv().is_err(),
            "settled previews reuse their combined image"
        );
    }
}
