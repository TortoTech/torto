use std::fs;
use std::io::{self, Write};
use std::path::Path;

use atomicwrites::{AllowOverwrite, AtomicFile};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

pub fn safe_book_id(book_id: &str) -> String {
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
pub trait BookRecord: DeserializeOwned {
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

pub fn load_book_record<T: BookRecord>(path: &Path, book_id: &str) -> io::Result<Option<T>> {
    read_optional(path)?
        .map(|bytes| {
            let record: T = decode_record(&bytes)?;
            Ok((record.identity() == (T::VERSION, book_id)).then_some(record))
        })
        .transpose()
        .map(Option::flatten)
}

pub fn validate_book_record<T: BookRecord>(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    let record: T = decode_record(bytes)?;
    if record.identity() != (T::VERSION, book_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not match the synced book", T::KIND),
        ));
    }
    Ok(())
}

pub fn export_book_record<T: BookRecord>(
    path: &Path,
    book_id: &str,
) -> io::Result<Option<Vec<u8>>> {
    let bytes = read_optional(path)?;
    if let Some(bytes) = &bytes {
        validate_book_record::<T>(book_id, bytes)?;
    }
    Ok(bytes)
}

pub fn import_book_record<T: BookRecord>(
    path: &Path,
    book_id: &str,
    bytes: &[u8],
) -> io::Result<()> {
    validate_book_record::<T>(book_id, bytes)?;
    write_bytes_atomic(path, bytes)
}

pub fn write_json_atomic<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_bytes_atomic(path, &bytes)
}

pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
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

pub fn book_data_path(
    directory: impl AsRef<Path>,
    book_id: &str,
    suffix: &str,
) -> io::Result<std::path::PathBuf> {
    let paths = crate::environment::paths()
        .ok_or_else(|| io::Error::other("Core environment unavailable"))?;
    Ok(paths
        .data
        .join(directory)
        .join(format!("{}.{suffix}", safe_book_id(book_id))))
}
