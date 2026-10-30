use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::persistence::{self, BookRecord, write_json_atomic};

const GENERATED_METADATA_VERSION: u8 = 1;
const GENERATED_METADATA_DIRECTORY: &str = "generated-metadata";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeneratedPdfMetadata {
    pub(crate) title: String,
    pub(crate) authors: Vec<String>,
    pub(crate) provider_name: String,
    pub(crate) model: String,
}

#[derive(Serialize, Deserialize)]
struct StoredGeneratedMetadata {
    version: u8,
    book_id: String,
    metadata: GeneratedPdfMetadata,
}

impl BookRecord for StoredGeneratedMetadata {
    const VERSION: u8 = GENERATED_METADATA_VERSION;
    const KIND: &'static str = "generated metadata";
    fn identity(&self) -> (u8, &str) {
        (self.version, &self.book_id)
    }
}

pub(crate) fn save(book_id: &str, metadata: &GeneratedPdfMetadata) -> io::Result<()> {
    let path = generated_metadata_path(book_id)?;
    write_json_atomic(
        &path,
        &StoredGeneratedMetadata {
            version: GENERATED_METADATA_VERSION,
            book_id: book_id.to_owned(),
            metadata: normalize(metadata.clone()),
        },
    )?;
    crate::sync::mark_derived_dirty(book_id, crate::sync::DerivedDataKind::Metadata)
}

pub(crate) fn export_sync_bytes(book_id: &str) -> io::Result<Option<Vec<u8>>> {
    persistence::export_book_record::<StoredGeneratedMetadata>(
        &generated_metadata_path(book_id)?,
        book_id,
    )
}

pub(crate) fn import_sync_bytes(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    persistence::import_book_record::<StoredGeneratedMetadata>(
        &generated_metadata_path(book_id)?,
        book_id,
        bytes,
    )
}

pub(crate) fn validate_sync_bytes(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    persistence::validate_book_record::<StoredGeneratedMetadata>(book_id, bytes)
}

pub(crate) fn load(book_id: &str) -> io::Result<Option<GeneratedPdfMetadata>> {
    let Some(stored) = persistence::load_book_record::<StoredGeneratedMetadata>(
        &generated_metadata_path(book_id)?,
        book_id,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(normalize(stored.metadata)))
}

fn generated_metadata_path(book_id: &str) -> io::Result<PathBuf> {
    persistence::book_data_path(GENERATED_METADATA_DIRECTORY, book_id, "json")
}

fn normalize(mut metadata: GeneratedPdfMetadata) -> GeneratedPdfMetadata {
    metadata.title = metadata.title.trim().to_owned();
    metadata.authors = metadata
        .authors
        .into_iter()
        .map(|author| author.trim().to_owned())
        .filter(|author| !author.is_empty())
        .collect();
    metadata.authors.dedup();
    metadata.provider_name = metadata.provider_name.trim().to_owned();
    metadata.model = metadata.model.trim().to_owned();
    metadata
}

#[cfg(test)]
mod tests {
    use super::{GeneratedPdfMetadata, normalize};

    #[test]
    fn existing_wire_records_reuse_cache_and_invalid_imports_preserve_it() {
        use super::*;
        let path = std::env::temp_dir().join(format!("torto-record-{}.json", uuid::Uuid::new_v4()));
        let bytes = br#"{"version":1,"book_id":"book-1","metadata":{"title":" A Book ","authors":["Alice"],"provider_name":"provider","model":"model"}}"#;
        persistence::import_book_record::<StoredGeneratedMetadata>(&path, "book-1", bytes).unwrap();
        let record = persistence::load_book_record::<StoredGeneratedMetadata>(&path, "book-1")
            .unwrap()
            .unwrap();
        assert_eq!(normalize(record.metadata).title, "A Book");
        assert_eq!(
            persistence::export_book_record::<StoredGeneratedMetadata>(&path, "book-1")
                .unwrap()
                .unwrap(),
            bytes
        );
        assert!(
            persistence::load_book_record::<StoredGeneratedMetadata>(&path, "other")
                .unwrap()
                .is_none()
        );
        for invalid in [bytes.as_slice(), b"{\"version\":2}", b"not json"] {
            assert!(
                persistence::import_book_record::<StoredGeneratedMetadata>(&path, "other", invalid)
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn generated_metadata_is_normalized() {
        let metadata = normalize(GeneratedPdfMetadata {
            title: "  A Book  ".into(),
            authors: vec![" Alice ".into(), String::new(), "Alice".into()],
            provider_name: " Provider ".into(),
            model: " model ".into(),
        });
        assert_eq!(metadata.title, "A Book");
        assert_eq!(metadata.authors, ["Alice"]);
        assert_eq!(metadata.provider_name, "Provider");
        assert_eq!(metadata.model, "model");
    }
}
