use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rebook_publication::SourceRange;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type HighlightResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredHighlight {
    pub id: String,
    pub book_id: String,
    pub ranges: Vec<SourceRange>,
    pub quote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: u64,
}

impl StoredHighlight {
    pub fn with_note(
        book_id: String,
        ranges: Vec<SourceRange>,
        quote: String,
        note: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            book_id,
            ranges,
            quote,
            note: note.and_then(|note| {
                let note = note.trim().to_owned();
                (!note.is_empty()).then_some(note)
            }),
            created_at: unix_timestamp_millis(),
        }
    }
}

pub trait HighlightRepository: Send + Sync {
    fn highlights_for_book(&self, book_id: &str) -> HighlightResult<Vec<StoredHighlight>>;
    fn insert_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<()>;
    fn update_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<bool>;
    fn remove_highlight(&self, id: &str) -> HighlightResult<bool>;
}

pub struct HighlightStore {
    repository: Arc<dyn HighlightRepository>,
}

impl HighlightStore {
    pub fn from_repository(repository: impl HighlightRepository + 'static) -> Self {
        Self {
            repository: Arc::new(repository),
        }
    }

    pub fn for_book(&self, book_id: &str) -> Vec<StoredHighlight> {
        self.repository
            .highlights_for_book(book_id)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "failed to load highlights from sync store");
                Vec::new()
            })
    }

    pub fn insert(&mut self, highlight: &StoredHighlight) -> HighlightResult<()> {
        self.repository.insert_highlight(highlight)
    }

    pub fn update(&mut self, highlight: &StoredHighlight) -> HighlightResult<bool> {
        self.repository.update_highlight(highlight)
    }

    pub fn remove(&mut self, id: &str) -> HighlightResult<bool> {
        self.repository.remove_highlight(id)
    }
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rebook_publication::{SourceAnchor, SourceRange, SpineItemId};

    use super::{HighlightRepository, HighlightResult, HighlightStore, StoredHighlight};

    #[derive(Clone, Default)]
    struct MemoryRepository {
        highlights: Arc<Mutex<Vec<StoredHighlight>>>,
    }

    impl HighlightRepository for MemoryRepository {
        fn highlights_for_book(&self, book_id: &str) -> HighlightResult<Vec<StoredHighlight>> {
            Ok(self
                .highlights
                .lock()
                .unwrap()
                .iter()
                .filter(|highlight| highlight.book_id == book_id)
                .cloned()
                .collect())
        }

        fn insert_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<()> {
            self.highlights.lock().unwrap().push(highlight.clone());
            Ok(())
        }

        fn update_highlight(&self, highlight: &StoredHighlight) -> HighlightResult<bool> {
            let mut highlights = self.highlights.lock().unwrap();
            let Some(existing) = highlights.iter_mut().find(|item| item.id == highlight.id) else {
                return Ok(false);
            };
            *existing = highlight.clone();
            Ok(true)
        }

        fn remove_highlight(&self, id: &str) -> HighlightResult<bool> {
            let mut highlights = self.highlights.lock().unwrap();
            let previous_len = highlights.len();
            highlights.retain(|highlight| highlight.id != id);
            Ok(highlights.len() != previous_len)
        }
    }

    fn new_store(repository: &MemoryRepository) -> HighlightStore {
        HighlightStore {
            repository: Arc::new(repository.clone()),
        }
    }

    #[test]
    fn highlights_round_trip_and_are_scoped_by_book() {
        let repository = MemoryRepository::default();
        let mut store = new_store(&repository);
        let range = SourceRange {
            start: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: "p1".into(),
                text_offset: 2,
            },
            end: SourceAnchor {
                spine: SpineItemId::new("chapter").unwrap(),
                node: "p1".into(),
                text_offset: 6,
            },
        };
        let highlight =
            StoredHighlight::with_note("book-a".into(), vec![range], "text".into(), None);
        let id = highlight.id.clone();
        store.insert(&highlight).unwrap();

        let mut loaded = new_store(&repository);
        assert_eq!(loaded.for_book("book-a").len(), 1);
        assert!(loaded.for_book("book-b").is_empty());
        assert!(loaded.remove(&id).unwrap());
        assert!(new_store(&repository).for_book("book-a").is_empty());
    }

    #[test]
    fn annotation_note_is_normalized_and_persisted() {
        let repository = MemoryRepository::default();
        let mut store = new_store(&repository);
        let highlight = StoredHighlight::with_note(
            "book-a".into(),
            Vec::new(),
            "quote".into(),
            Some("  my note  ".into()),
        );
        store.insert(&highlight).unwrap();

        assert_eq!(
            new_store(&repository).for_book("book-a")[0].note.as_deref(),
            Some("my note")
        );
    }

    #[test]
    fn annotation_note_can_be_updated() {
        let repository = MemoryRepository::default();
        let mut store = new_store(&repository);
        let mut highlight = StoredHighlight::with_note(
            "book-a".into(),
            Vec::new(),
            "quote".into(),
            Some("old".into()),
        );
        store.insert(&highlight).unwrap();
        highlight.note = Some("new".into());

        assert!(store.update(&highlight).unwrap());
        assert_eq!(store.for_book("book-a")[0].note.as_deref(), Some("new"));
    }
}
