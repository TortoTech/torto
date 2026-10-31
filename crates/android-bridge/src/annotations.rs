use crate::*;
use rebook_app_core::highlights::{HighlightRepository, StoredHighlight};

#[derive(Clone, uniffi::Record)]
pub struct Annotation {
    pub id: String,
    pub quote: String,
    pub note: Option<String>,
    pub source: String,
}
fn list(s: &ReaderState) -> Result<Vec<Annotation>> {
    s.store
        .annotations_for_book(&s.book_id)
        .map_err(error)?
        .into_iter()
        .map(|a| {
            Ok(Annotation {
                id: a.id,
                quote: a.quote,
                note: a.note,
                source: serde_json::to_string(
                    a.ranges
                        .first()
                        .ok_or_else(|| error("Annotation has no source"))?,
                )
                .map_err(error)?,
            })
        })
        .collect()
}
#[uniffi::export]
impl MobileReader {
    pub async fn annotations(self: Arc<Self>) -> Result<Vec<Annotation>> {
        work(move || self.with(|s| list(s))).await
    }
    pub async fn save_annotation(self: Arc<Self>, note: Option<String>) -> Result<Vec<Annotation>> {
        work(move || {
            self.with(|s| {
                let selected = s.selected.as_ref().ok_or_else(|| error("请先选择正文"))?;
                let annotation = StoredHighlight::with_note(
                    s.book_id.clone(),
                    selected.ranges.clone(),
                    selected.text.clone(),
                    note,
                );
                s.store.insert_highlight(&annotation).map_err(error)?;
                s.selected = None;
                list(s)
            })
        })
        .await
    }
    pub async fn edit_annotation(
        self: Arc<Self>,
        id: String,
        note: Option<String>,
    ) -> Result<Vec<Annotation>> {
        work(move || {
            self.with(|s| {
                if !s
                    .store
                    .annotations_for_book(&s.book_id)
                    .map_err(error)?
                    .iter()
                    .any(|a| a.id == id)
                {
                    return Err(error("Annotation not found"));
                }
                s.store.update_annotation_note(&id, note).map_err(error)?;
                list(s)
            })
        })
        .await
    }
    pub async fn delete_annotation(self: Arc<Self>, id: String) -> Result<Vec<Annotation>> {
        work(move || {
            self.with(|s| {
                if !s
                    .store
                    .annotations_for_book(&s.book_id)
                    .map_err(error)?
                    .iter()
                    .any(|a| a.id == id)
                {
                    return Err(error("Annotation not found"));
                }
                s.store.delete_annotation(&id).map_err(error)?;
                list(s)
            })
        })
        .await
    }
}
