use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::persistence::write_json_atomic;

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

pub(crate) fn save(book_id: &str, metadata: &GeneratedPdfMetadata) -> io::Result<()> {
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
            metadata: normalize(metadata.clone()),
        },
    )
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
    Ok(Some(normalize(stored.metadata)))
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
