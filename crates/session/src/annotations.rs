use std::collections::HashMap;

use rebook_assistant::{AssistantAnnotationAction, AssistantAnnotationTarget};
use rebook_sync::{HighlightStore, StoredHighlight};

enum StoredHighlightUndoKind {
    RemoveCreated(String),
    RestoreUpdated(StoredHighlight),
    ReinsertDeleted(StoredHighlight),
}

/// Opaque rollback value produced by [`StoredHighlightMutationTarget`].
pub struct StoredHighlightUndo(StoredHighlightUndoKind);

/// Transaction adapter joining shared assistant actions to durable highlights.
///
/// The working map mirrors the caller's current document annotations while the
/// store remains the authority. The shared confirmation coordinator can apply a
/// mixed batch and roll back every earlier write if a later action fails.
pub struct StoredHighlightMutationTarget<'a> {
    store: &'a mut HighlightStore,
    working: HashMap<String, StoredHighlight>,
}

impl<'a> StoredHighlightMutationTarget<'a> {
    #[must_use]
    pub fn new(
        store: &'a mut HighlightStore,
        current: impl IntoIterator<Item = StoredHighlight>,
    ) -> Self {
        Self {
            store,
            working: current
                .into_iter()
                .map(|annotation| (annotation.id.clone(), annotation))
                .collect(),
        }
    }
}

impl AssistantAnnotationTarget<StoredHighlight> for StoredHighlightMutationTarget<'_> {
    type Undo = StoredHighlightUndo;

    fn apply_annotation(
        &mut self,
        action: &AssistantAnnotationAction<StoredHighlight>,
    ) -> Result<Self::Undo, String> {
        match action {
            AssistantAnnotationAction::Create(annotation) => {
                if self.working.contains_key(&annotation.id) {
                    return Err(format!("批注已存在：{}", annotation.id));
                }
                self.store
                    .insert(annotation)
                    .map_err(|error| error.to_string())?;
                self.working
                    .insert(annotation.id.clone(), annotation.clone());
                Ok(StoredHighlightUndo(StoredHighlightUndoKind::RemoveCreated(
                    annotation.id.clone(),
                )))
            }
            AssistantAnnotationAction::Update(annotation) => {
                let previous = self
                    .working
                    .get(&annotation.id)
                    .cloned()
                    .ok_or_else(|| format!("批注不存在：{}", annotation.id))?;
                if !self
                    .store
                    .update(annotation)
                    .map_err(|error| error.to_string())?
                {
                    return Err(format!("批注不存在：{}", annotation.id));
                }
                self.working
                    .insert(annotation.id.clone(), annotation.clone());
                Ok(StoredHighlightUndo(
                    StoredHighlightUndoKind::RestoreUpdated(previous),
                ))
            }
            AssistantAnnotationAction::Delete { annotation_id } => {
                let previous = self
                    .working
                    .get(annotation_id)
                    .cloned()
                    .ok_or_else(|| format!("批注不存在：{annotation_id}"))?;
                if !self
                    .store
                    .remove(annotation_id)
                    .map_err(|error| error.to_string())?
                {
                    return Err(format!("批注不存在：{annotation_id}"));
                }
                self.working.remove(annotation_id);
                Ok(StoredHighlightUndo(
                    StoredHighlightUndoKind::ReinsertDeleted(previous),
                ))
            }
        }
    }

    fn rollback_annotation(&mut self, undo: Self::Undo) -> Result<(), String> {
        match undo.0 {
            StoredHighlightUndoKind::RemoveCreated(annotation_id) => {
                if !self
                    .store
                    .remove(&annotation_id)
                    .map_err(|error| error.to_string())?
                {
                    return Err(format!("无法回滚新建批注：{annotation_id}"));
                }
                self.working.remove(&annotation_id);
            }
            StoredHighlightUndoKind::RestoreUpdated(annotation) => {
                if !self
                    .store
                    .update(&annotation)
                    .map_err(|error| error.to_string())?
                {
                    return Err(format!("无法回滚批注更新：{}", annotation.id));
                }
                self.working.insert(annotation.id.clone(), annotation);
            }
            StoredHighlightUndoKind::ReinsertDeleted(annotation) => {
                self.store
                    .insert(&annotation)
                    .map_err(|error| error.to_string())?;
                self.working.insert(annotation.id.clone(), annotation);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rebook_assistant::{AssistantMutationResolution, confirm_annotation_actions};
    use rebook_publication::{SourceAnchor, SourceRange, SpineItemId};
    use rebook_sync::{HighlightRepository, HighlightResult};

    use super::*;

    #[derive(Clone, Default)]
    struct MemoryRepository {
        annotations: Arc<Mutex<Vec<StoredHighlight>>>,
    }

    impl HighlightRepository for MemoryRepository {
        fn highlights_for_book(&self, book_id: &str) -> HighlightResult<Vec<StoredHighlight>> {
            Ok(self
                .annotations
                .lock()
                .unwrap()
                .iter()
                .filter(|annotation| annotation.book_id == book_id)
                .cloned()
                .collect())
        }

        fn insert_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<()> {
            self.annotations.lock().unwrap().push(highlight.clone());
            Ok(())
        }

        fn update_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<bool> {
            let mut annotations = self.annotations.lock().unwrap();
            let Some(existing) = annotations
                .iter_mut()
                .find(|annotation| annotation.id == highlight.id)
            else {
                return Ok(false);
            };
            *existing = highlight.clone();
            Ok(true)
        }

        fn remove_highlight(&self, id: &str) -> HighlightResult<bool> {
            let mut annotations = self.annotations.lock().unwrap();
            let previous_len = annotations.len();
            annotations.retain(|annotation| annotation.id != id);
            Ok(annotations.len() != previous_len)
        }
    }

    fn range() -> SourceRange {
        let spine = SpineItemId::new("chapter-1").unwrap();
        SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 3,
            },
            end: SourceAnchor {
                spine,
                node: "paragraph-1".into(),
                text_offset: 11,
            },
        }
    }

    #[test]
    fn later_failure_rolls_back_an_earlier_durable_create() {
        let repository = MemoryRepository::default();
        let mut store = HighlightStore::from_repository(repository.clone());
        let created =
            StoredHighlight::with_note("book".into(), vec![range()], "created".into(), None);
        let missing = StoredHighlight::with_note(
            "book".into(),
            vec![range()],
            "missing".into(),
            Some("updated".into()),
        );
        let actions = vec![
            AssistantAnnotationAction::Create(created),
            AssistantAnnotationAction::Update(missing),
        ];
        let mut target = StoredHighlightMutationTarget::new(&mut store, Vec::new());

        assert!(confirm_annotation_actions(&mut target, &actions).is_err());
        assert!(repository.annotations.lock().unwrap().is_empty());
    }

    #[test]
    fn confirmed_create_preserves_the_exact_source_range() {
        let repository = MemoryRepository::default();
        let mut store = HighlightStore::from_repository(repository.clone());
        let range = range();
        let annotation = StoredHighlight::with_note(
            "book".into(),
            vec![range.clone()],
            "source text".into(),
            None,
        );
        let actions = vec![AssistantAnnotationAction::Create(annotation)];
        let mut target = StoredHighlightMutationTarget::new(&mut store, Vec::new());

        assert_eq!(
            confirm_annotation_actions(&mut target, &actions).unwrap(),
            AssistantMutationResolution::Confirmed { applied: 1 }
        );
        assert_eq!(
            repository.annotations.lock().unwrap()[0].ranges,
            vec![range]
        );
    }
}
