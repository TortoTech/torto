use crate::*;
use rebook_app_core::highlights::StoredHighlight;

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
        work(move || self.with(|s| save_selection(s,note,rebook_app_core::highlights::selection::SelectionAction::Add))).await
    }
    pub async fn toggle_selection_highlight(self: Arc<Self>, remove: bool) -> Result<Vec<Annotation>> {
        work(move || self.with(|s| save_selection(s,None,if remove {rebook_app_core::highlights::selection::SelectionAction::Remove}else{rebook_app_core::highlights::selection::SelectionAction::Add}))).await
    }
    pub async fn edit_annotation(
        self: Arc<Self>,
        id: String,
        note: Option<String>,
    ) -> Result<Vec<Annotation>> {
        work(move || {
            self.with(|s| {
                let rows=s.store.annotations_for_book(&s.book_id).map_err(error)?;
                let old=rows.iter().find(|a|a.id==id).ok_or_else(||error("Annotation not found"))?;
                if old.note.is_none() && note.as_ref().is_some_and(|n|!n.trim().is_empty()) {
                    // A thought attached to a highlight is independently editable;
                    // removing that highlight must never erase the user's note.
                    let requested=StoredHighlight::with_note(s.book_id.clone(),old.ranges.clone(),old.quote.clone(),note);
                    rebook_app_core::highlights::HighlightStore::from_repository(s.store.clone())
                        .save_selection(s.pipeline.rewrite.as_ref(),requested,rebook_app_core::highlights::selection::SelectionAction::Add).map_err(error)?;
                } else {s.store.update_annotation_note(&id, note).map_err(error)?;}
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

fn save_selection(s:&mut ReaderState,note:Option<String>,action:rebook_app_core::highlights::selection::SelectionAction)->Result<Vec<Annotation>> {
    let selected=s.selected.as_ref().ok_or_else(||error("请先选择正文"))?;
    let requested=StoredHighlight::with_note(s.book_id.clone(),selected.ranges.clone(),selected.text.clone(),note);
    let store=rebook_app_core::highlights::HighlightStore::from_repository(s.store.clone());
    store.save_selection(s.pipeline.rewrite.as_ref(),requested,action).map_err(error)?;
    clear_selection_state(s);
    list(s)
}
