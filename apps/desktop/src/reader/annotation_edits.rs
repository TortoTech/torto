//! Source parsing and annotation persistence never run on the UI thread.
use super::*;
use rebook_app_core::highlights::selection::SelectionAction;
use std::sync::mpsc;

#[derive(Default)]
pub(super) struct AnnotationEdits {
    context: Option<egui::Context>,
    pub(super) pending: Option<mpsc::Receiver<Result<Vec<StoredHighlight>, String>>>,
    close_selection: bool,
}
impl DesktopReader {
    pub(super) fn poll_annotation_edits(&mut self, ctx: &egui::Context) {
        self.annotation_edits.context = Some(ctx.clone());
        let result = self
            .annotation_edits
            .pending
            .as_ref()
            .and_then(|r| match r.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Annotation worker stopped".into()))
                }
            });
        if let Some(result) = result {
            self.annotation_edits.pending = None;
            match result {
                Ok(rows) => {
                    self.highlights = rows;
                    if self.annotation_edits.close_selection {
                        self.cancel_text_selection();
                    }
                    self.annotation_note_draft = None;
                    self.selected_highlight_id = None;
                    self.bump_scene_revision();
                    self.error = None;
                }
                Err(error) => self.error = Some(format!("保存划线失败：{error}")),
            }
        }
    }
    pub(super) fn save_selection_annotation(
        &mut self,
        ranges: Vec<SourceRange>,
        text: String,
        note: Option<String>,
        action: SelectionAction,
        close_selection: bool,
    ) {
        if self.annotation_edits.pending.is_some() {
            return;
        }
        let mut requested = StoredHighlight::with_note(self.book_id.clone(), ranges, text, note);
        if !close_selection
            && requested.note.is_some()
            && let Some(existing) = self.highlights.iter().find(|r| {
                r.note.is_some()
                    && rebook_app_core::highlights::selection::same_ranges(
                        &r.ranges,
                        &requested.ranges,
                    )
            })
        {
            requested.id.clone_from(&existing.id);
            requested.created_at = existing.created_at;
        }
        let store = self.highlight_store.clone();
        let source = self.rewrite_source.clone();
        let context = self.annotation_edits.context.clone();
        let (send, receive) = mpsc::channel();
        self.annotation_edits.pending = Some(receive);
        self.annotation_edits.close_selection = close_selection;
        std::thread::spawn(move || {
            let result = store
                .save_selection(source.as_ref(), requested, action)
                .map_err(|e| e.to_string());
            if send.send(result).is_ok()
                && let Some(ctx) = context
            {
                ctx.request_repaint();
            }
        });
    }
    pub(super) fn toggle_selected_highlight(&mut self) {
        if let Some(selection) = &self.selection {
            let action = if self.selected_highlight_exists() {
                SelectionAction::Remove
            } else {
                SelectionAction::Add
            };
            self.save_selection_annotation(
                selection.ranges.clone(),
                selection.text.clone(),
                None,
                action,
                true,
            );
        }
    }
    pub(super) fn selected_highlight_exists(&self) -> bool {
        self.selection.as_ref().is_some_and(|selection| {
            rebook_app_core::highlights::selection::contains_highlight(
                &self.highlights,
                &selection.ranges,
            )
        })
    }
}
