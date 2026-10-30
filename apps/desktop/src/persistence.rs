use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

/// Preserve the filenames already used by generated data and sync markers.
pub(crate) fn book_data_path(
    directory: impl AsRef<Path>,
    book_id: &str,
    suffix: &str,
) -> io::Result<PathBuf> {
    let project = crate::smoke::project_dirs().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "application data directory is unavailable",
        )
    })?;
    Ok(project
        .data_local_dir()
        .join(directory)
        .join(format!("{}.{suffix}", safe_book_id(book_id))))
}

fn safe_book_id(book_id: &str) -> String {
    if book_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        book_id.to_owned()
    } else {
        format!("{:x}", Sha256::digest(book_id.as_bytes()))
    }
}

/// Records retain their existing wire shape and domain-specific validation.
pub(crate) trait BookRecord: DeserializeOwned {
    const VERSION: u8;
    const KIND: &'static str;
    fn identity(&self) -> (u8, &str);
}

fn decode_record<T: BookRecord>(bytes: &[u8]) -> io::Result<T> {
    serde_json::from_slice(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn load_book_record<T: BookRecord>(path: &Path, book_id: &str) -> io::Result<Option<T>> {
    read_optional(path)?
        .map(|bytes| {
            let record: T = decode_record(&bytes)?;
            Ok((record.identity() == (T::VERSION, book_id)).then_some(record))
        })
        .transpose()
        .map(Option::flatten)
}

pub(crate) fn validate_book_record<T: BookRecord>(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    let record: T = decode_record(bytes)?;
    if record.identity() != (T::VERSION, book_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not match the synced book", T::KIND),
        ));
    }
    Ok(())
}

pub(crate) fn export_book_record<T: BookRecord>(
    path: &Path,
    book_id: &str,
) -> io::Result<Option<Vec<u8>>> {
    let bytes = read_optional(path)?;
    if let Some(bytes) = &bytes {
        validate_book_record::<T>(book_id, bytes)?;
    }
    Ok(bytes)
}

pub(crate) fn import_book_record<T: BookRecord>(
    path: &Path,
    book_id: &str,
    bytes: &[u8],
) -> io::Result<()> {
    validate_book_record::<T>(book_id, bytes)?;
    write_bytes_atomic(path, bytes)
}

pub(crate) fn write_json_atomic<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_bytes_atomic(path, &bytes)
}

pub(crate) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "持久化路径没有父目录"))?;
    fs::create_dir_all(parent)?;
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_paths_preserve_legacy_names_and_hash_unsafe_identities() {
        for book_id in ["book-1_2", "../book", "中文书籍", ""] {
            let safe = safe_book_id(book_id);
            let path = book_data_path("generated-toc", book_id, "json").unwrap();
            assert_eq!(
                path.file_name().unwrap().to_str().unwrap(),
                format!("{safe}.json")
            );
            assert_eq!(path.parent().unwrap().file_name().unwrap(), "generated-toc");
            if book_id == "../book" || book_id == "中文书籍" {
                assert_eq!(safe, format!("{:x}", Sha256::digest(book_id.as_bytes())));
            } else {
                assert_eq!(safe, book_id);
            }
        }
    }

    #[test]
    fn atomic_write_replaces_existing_contents() {
        let path = std::env::temp_dir().join(format!(
            "rebook-atomic-write-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        write_bytes_atomic(&path, b"old").unwrap();
        write_bytes_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_file(path).unwrap();
    }
}
