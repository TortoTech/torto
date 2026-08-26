use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
pub(crate) use rebook_session::GeneratedPdfMetadata;
use rebook_session::normalize_generated_pdf_metadata;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::persistence::{write_bytes_atomic, write_json_atomic};

const GENERATED_METADATA_VERSION: u8 = 1;
const GENERATED_METADATA_DIRECTORY: &str = "generated-metadata";

#[derive(Serialize, Deserialize)]
struct StoredGeneratedMetadata {
    version: u8,
    book_id: String,
    metadata: GeneratedPdfMetadata,
}

pub(crate) fn persist_normalized(book_id: &str, metadata: &GeneratedPdfMetadata) -> io::Result<()> {
    let path = generated_metadata_path(book_id)?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "generated metadata path does not have a parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    write_json_atomic(
        &path,
        &StoredGeneratedMetadata {
            version: GENERATED_METADATA_VERSION,
            book_id: book_id.to_owned(),
            metadata: metadata.clone(),
        },
    )?;
    crate::sync::mark_derived_dirty(book_id, crate::sync::DerivedDataKind::Metadata)
}

pub(crate) fn export_sync_bytes(book_id: &str) -> io::Result<Option<Vec<u8>>> {
    match fs::read(generated_metadata_path(book_id)?) {
        Ok(bytes) => {
            validate_sync_bytes(book_id, &bytes)?;
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn import_sync_bytes(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    validate_sync_bytes(book_id, bytes)?;
    write_bytes_atomic(&generated_metadata_path(book_id)?, bytes)
}

pub(crate) fn validate_sync_bytes(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    let stored: StoredGeneratedMetadata = serde_json::from_slice(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored.version != GENERATED_METADATA_VERSION || stored.book_id != book_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "generated metadata does not match the synced book",
        ));
    }
    Ok(())
}

pub(crate) fn load(book_id: &str) -> io::Result<Option<GeneratedPdfMetadata>> {
    let path = generated_metadata_path(book_id)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let stored: StoredGeneratedMetadata = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if stored.version != GENERATED_METADATA_VERSION || stored.book_id != book_id {
        return Ok(None);
    }
    Ok(Some(normalize_generated_pdf_metadata(stored.metadata)))
}

fn generated_metadata_path(book_id: &str) -> io::Result<PathBuf> {
    let project = ProjectDirs::from("com", "Rebook", "Rebook").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "application data directory is unavailable",
        )
    })?;
    let safe_id = if book_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        book_id.to_owned()
    } else {
        format!("{:x}", Sha256::digest(book_id.as_bytes()))
    };
    Ok(project
        .data_local_dir()
        .join(GENERATED_METADATA_DIRECTORY)
        .join(format!("{safe_id}.json")))
}

#[cfg(test)]
mod tests {
    use super::GeneratedPdfMetadata;
    use rebook_session::normalize_generated_pdf_metadata;

    #[test]
    fn generated_metadata_is_normalized() {
        let metadata = normalize_generated_pdf_metadata(GeneratedPdfMetadata {
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
