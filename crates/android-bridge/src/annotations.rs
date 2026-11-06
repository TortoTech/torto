use crate::*;
use rebook_app_core::highlights::{HighlightRepository, StoredHighlight};

#[derive(Clone, uniffi::Record)]
pub struct Annotation {
    pub id: String,
    pub quote: String,
    pub note: Option<String>,
    pub source: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub section_index: u32,
}
fn list(s: &ReaderState) -> Result<Vec<Annotation>> {
    let stored = s.store
        .annotations_for_book(&s.book_id)
        .map_err(error)?;
    let anchors = stored.iter().map(|a| a.ranges.first().map(|r| r.start.clone())
        .ok_or_else(|| error("Annotation has no source"))).collect::<Result<Vec<_>>>()?;
    let contexts = s.session.source_contexts(&anchors);
    let mut rows = stored
        .into_iter()
        .zip(contexts)
        .map(|(a, context)| {
            let anchor = &a.ranges[0].start;
            let section = context.as_ref().map_or(usize::MAX, |c| c.section_index);
            let order = context.as_ref().and_then(|c| c.node_order).unwrap_or(usize::MAX);
            let toc = context.as_ref().and_then(|c| c.toc.as_ref());
            let chapter_id = toc.map_or_else(|| format!("spine:{}", anchor.spine.as_str()), |t| t.id.clone());
            let chapter_title = toc.map_or_else(String::new, |t| t.label.clone());
            let offset = anchor.text_offset;
            Ok(((section, order, offset, a.id.clone()), Annotation {
                id: a.id,
                quote: a.quote,
                note: a.note,
                source: serde_json::to_string(
                    a.ranges
                        .first()
                        .ok_or_else(|| error("Annotation has no source"))?,
                )
                .map_err(error)?,
                chapter_id,
                chapter_title,
                section_index: section.try_into().unwrap_or(u32::MAX),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows.into_iter().map(|(_, note)| note).collect())
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
