use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::plugins::{PdfOcrSyncData, export_pdf_ocr_sync_data, import_pdf_ocr_sync_data};

use super::SyncResult;
use super::webdav::WebDavClient;

const DERIVED_SYNC_VERSION: u8 = 1;
const DERIVED_SYNC_DIRECTORY: &str = "derived-sync-v1";
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 768 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DerivedDataKind {
    Ocr,
    Metadata,
}

impl DerivedDataKind {
    const fn marker_name(self) -> &'static str {
        match self {
            Self::Ocr => "ocr",
            Self::Metadata => "metadata",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OcrManifest {
    version: u8,
    book_id: String,
    content_sha256: String,
    content_length: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct BookDerivedMetadata {
    version: u8,
    book_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    toc: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

enum DerivedOperation {
    UploadMetadata {
        book_id: String,
        bytes: Vec<u8>,
    },
    DownloadMetadata {
        book_id: String,
        bytes: Vec<u8>,
    },
    UploadOcr {
        book_id: String,
        bytes: Vec<u8>,
        manifest: OcrManifest,
    },
    DownloadOcr {
        book_id: String,
        manifest: OcrManifest,
    },
}

impl DerivedOperation {
    fn length(&self) -> u64 {
        match self {
            Self::UploadMetadata { bytes, .. }
            | Self::DownloadMetadata { bytes, .. }
            | Self::UploadOcr { bytes, .. } => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            Self::DownloadOcr { manifest, .. } => manifest.content_length,
        }
    }
}

pub(crate) fn mark_derived_dirty(book_id: &str, kind: DerivedDataKind) -> io::Result<()> {
    let path = dirty_marker_path(book_id, kind)?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "derived sync marker has no parent directory",
        )
    })?;
    fs::create_dir_all(parent)?;
    fs::write(path, [])
}

pub(crate) async fn sync_derived_data<F>(
    webdav: &WebDavClient,
    book_ids: impl IntoIterator<Item = String>,
    cache_dir: &Path,
    mut progress: F,
) -> SyncResult<()>
where
    F: FnMut(u64, u64),
{
    let mut operations = Vec::new();
    for book_id in book_ids {
        let remote_files = webdav
            .list_json_files(&format!("derived/{book_id}/"))
            .await?
            .into_iter()
            .collect::<BTreeSet<_>>();
        collect_metadata_operation(
            webdav,
            &book_id,
            remote_files.contains("metadata.json"),
            &mut operations,
        )
        .await?;
        collect_ocr_operation(
            webdav,
            &book_id,
            remote_files.contains("ocr.json"),
            &mut operations,
        )
        .await?;
    }

    let total = operations.iter().map(DerivedOperation::length).sum();
    let mut completed = 0_u64;
    progress(completed, total);
    fs::create_dir_all(cache_dir)?;
    for operation in operations {
        completed = execute_operation(
            webdav,
            operation,
            cache_dir,
            completed,
            total,
            &mut progress,
        )
        .await?;
        progress(completed, total);
    }
    Ok(())
}

async fn execute_operation<F>(
    webdav: &WebDavClient,
    operation: DerivedOperation,
    cache_dir: &Path,
    completed: u64,
    total: u64,
    progress: &mut F,
) -> SyncResult<u64>
where
    F: FnMut(u64, u64),
{
    match operation {
        DerivedOperation::UploadMetadata { book_id, bytes } => {
            let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            webdav
                .ensure_collection(&format!("derived/{book_id}/"))
                .await?;
            webdav
                .put_mutable_bytes(
                    &format!("derived/{book_id}/metadata.json"),
                    bytes,
                    "application/json",
                )
                .await?;
            clear_dirty_marker(&book_id, DerivedDataKind::Metadata)?;
            Ok(completed.saturating_add(length))
        }
        DerivedOperation::DownloadMetadata { book_id, bytes } => {
            let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            apply_metadata_document(&book_id, &bytes)?;
            clear_dirty_marker(&book_id, DerivedDataKind::Metadata)?;
            Ok(completed.saturating_add(length))
        }
        DerivedOperation::UploadOcr {
            book_id,
            bytes,
            manifest,
        } => {
            let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            webdav
                .ensure_collection(&format!("derived/{book_id}/"))
                .await?;
            webdav
                .put_mutable_bytes(
                    &format!("derived/{book_id}/ocr.zip"),
                    bytes,
                    "application/zip",
                )
                .await?;
            webdav
                .put_mutable_json(&format!("derived/{book_id}/ocr.json"), &manifest)
                .await?;
            clear_dirty_marker(&book_id, DerivedDataKind::Ocr)?;
            Ok(completed.saturating_add(length))
        }
        DerivedOperation::DownloadOcr { book_id, manifest } => {
            download_ocr(
                webdav, &book_id, &manifest, cache_dir, completed, total, progress,
            )
            .await
        }
    }
}

async fn download_ocr<F>(
    webdav: &WebDavClient,
    book_id: &str,
    manifest: &OcrManifest,
    cache_dir: &Path,
    completed: u64,
    total: u64,
    progress: &mut F,
) -> SyncResult<u64>
where
    F: FnMut(u64, u64),
{
    let cache_path = cache_dir.join(format!("{book_id}-{}.part", manifest.content_sha256));
    let found = webdav
        .download_to_file(
            &format!("derived/{book_id}/ocr.zip"),
            &cache_path,
            manifest.content_length,
            |downloaded| progress(completed.saturating_add(downloaded), total),
        )
        .await?;
    if !found {
        return Err(
            io::Error::new(io::ErrorKind::NotFound, "synced PDF OCR archive is missing").into(),
        );
    }
    let bytes = fs::read(&cache_path)?;
    if sha256(&bytes) != manifest.content_sha256 {
        fs::remove_file(&cache_path).ok();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "synced PDF OCR archive failed its checksum",
        )
        .into());
    }
    import_pdf_ocr_sync_data(book_id, unpack_ocr_archive(bytes)?)?;
    fs::remove_file(cache_path).ok();
    clear_dirty_marker(book_id, DerivedDataKind::Ocr)?;
    Ok(completed.saturating_add(manifest.content_length))
}

async fn collect_metadata_operation(
    webdav: &WebDavClient,
    book_id: &str,
    remote_present: bool,
    operations: &mut Vec<DerivedOperation>,
) -> SyncResult<()> {
    let mut local = local_metadata_document(book_id)?;
    let path = format!("derived/{book_id}/metadata.json");
    let remote = if remote_present {
        webdav.get_optional(&path).await?.map(|object| object.bytes)
    } else {
        None
    };
    let remote_document = remote
        .as_deref()
        .map(|bytes| parse_metadata_document(book_id, bytes))
        .transpose()?;
    match (local.as_mut(), remote_document.as_ref()) {
        (Some(local), Some(remote)) if local == remote => {
            clear_dirty_marker(book_id, DerivedDataKind::Metadata)?;
        }
        (Some(local), None) => {
            operations.push(DerivedOperation::UploadMetadata {
                book_id: book_id.to_owned(),
                bytes: serde_json::to_vec_pretty(local)?,
            });
        }
        (Some(local), Some(remote)) if is_dirty(book_id, DerivedDataKind::Metadata)? => {
            local.toc = local.toc.take().or_else(|| remote.toc.clone());
            local.metadata = local.metadata.take().or_else(|| remote.metadata.clone());
            operations.push(DerivedOperation::UploadMetadata {
                book_id: book_id.to_owned(),
                bytes: serde_json::to_vec_pretty(local)?,
            });
        }
        (_, Some(_)) => operations.push(DerivedOperation::DownloadMetadata {
            book_id: book_id.to_owned(),
            bytes: remote.expect("remote metadata bytes exist with a parsed document"),
        }),
        (None, None) => clear_dirty_marker(book_id, DerivedDataKind::Metadata)?,
    }
    Ok(())
}

fn local_metadata_document(book_id: &str) -> io::Result<Option<BookDerivedMetadata>> {
    let toc = crate::generated_toc::export_sync_bytes(book_id)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other))
        .transpose()?;
    let metadata = crate::generated_metadata::export_sync_bytes(book_id)?
        .map(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other))
        .transpose()?;
    if toc.is_none() && metadata.is_none() {
        return Ok(None);
    }
    Ok(Some(BookDerivedMetadata {
        version: DERIVED_SYNC_VERSION,
        book_id: book_id.to_owned(),
        toc,
        metadata,
    }))
}

fn parse_metadata_document(book_id: &str, bytes: &[u8]) -> io::Result<BookDerivedMetadata> {
    let document: BookDerivedMetadata = serde_json::from_slice(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if document.version != DERIVED_SYNC_VERSION || document.book_id != book_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "synced generated metadata does not match the book",
        ));
    }
    if let Some(toc) = &document.toc {
        crate::generated_toc::validate_sync_bytes(
            book_id,
            &serde_json::to_vec(toc).map_err(io::Error::other)?,
        )?;
    }
    if let Some(metadata) = &document.metadata {
        crate::generated_metadata::validate_sync_bytes(
            book_id,
            &serde_json::to_vec(metadata).map_err(io::Error::other)?,
        )?;
    }
    Ok(document)
}

fn apply_metadata_document(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    let document = parse_metadata_document(book_id, bytes)?;
    if let Some(toc) = document.toc {
        crate::generated_toc::import_sync_bytes(
            book_id,
            &serde_json::to_vec_pretty(&toc).map_err(io::Error::other)?,
        )?;
    }
    if let Some(metadata) = document.metadata {
        crate::generated_metadata::import_sync_bytes(
            book_id,
            &serde_json::to_vec_pretty(&metadata).map_err(io::Error::other)?,
        )?;
    }
    Ok(())
}

async fn collect_ocr_operation(
    webdav: &WebDavClient,
    book_id: &str,
    remote_present: bool,
    operations: &mut Vec<DerivedOperation>,
) -> SyncResult<()> {
    let local = export_pdf_ocr_sync_data(book_id)?
        .map(pack_ocr_archive)
        .transpose()?;
    let manifest_path = format!("derived/{book_id}/ocr.json");
    let remote = if remote_present {
        webdav
            .get_optional(&manifest_path)
            .await?
            .map(|object| serde_json::from_slice::<OcrManifest>(&object.bytes))
            .transpose()?
    } else {
        None
    };
    if let Some(manifest) = &remote {
        validate_ocr_manifest(book_id, manifest)?;
    }
    match (local, remote) {
        (Some(bytes), Some(manifest)) if sha256(&bytes) == manifest.content_sha256 => {
            clear_dirty_marker(book_id, DerivedDataKind::Ocr)?;
        }
        (Some(bytes), None) => {
            let manifest = ocr_manifest(book_id, &bytes);
            operations.push(DerivedOperation::UploadOcr {
                book_id: book_id.to_owned(),
                bytes,
                manifest,
            });
        }
        (Some(bytes), Some(_)) if is_dirty(book_id, DerivedDataKind::Ocr)? => {
            let manifest = ocr_manifest(book_id, &bytes);
            operations.push(DerivedOperation::UploadOcr {
                book_id: book_id.to_owned(),
                bytes,
                manifest,
            });
        }
        (_, Some(manifest)) => operations.push(DerivedOperation::DownloadOcr {
            book_id: book_id.to_owned(),
            manifest,
        }),
        (None, None) => clear_dirty_marker(book_id, DerivedDataKind::Ocr)?,
    }
    Ok(())
}

fn pack_ocr_archive(data: PdfOcrSyncData) -> io::Result<Vec<u8>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file("document.json", options)?;
    writer.write_all(&data.document)?;
    for (file_name, bytes) in data.resources {
        writer.start_file(format!("resources/{file_name}"), options)?;
        writer.write_all(&bytes)?;
    }
    Ok(writer.finish()?.into_inner())
}

fn unpack_ocr_archive(bytes: Vec<u8>) -> io::Result<PdfOcrSyncData> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut document = None;
    let mut resources = BTreeMap::new();
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        total_size = total_size.saturating_add(entry.size());
        if entry.size() > MAX_ARCHIVE_ENTRY_BYTES || total_size > MAX_ARCHIVE_TOTAL_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "synced PDF OCR archive entry is too large",
            ));
        }
        let name = entry.name().replace('\\', "/");
        let mut contents = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or_default());
        entry.read_to_end(&mut contents)?;
        if name == "document.json" {
            if document.replace(contents).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "synced PDF OCR archive contains duplicate documents",
                ));
            }
        } else if let Some(file_name) = name.strip_prefix("resources/") {
            if file_name.is_empty()
                || file_name.contains('/')
                || resources.insert(file_name.to_owned(), contents).is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "synced PDF OCR archive contains an unsafe resource",
                ));
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "synced PDF OCR archive contains an unknown entry",
            ));
        }
    }
    Ok(PdfOcrSyncData {
        document: document.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "synced PDF OCR archive has no document",
            )
        })?,
        resources: resources.into_iter().collect(),
    })
}

fn ocr_manifest(book_id: &str, bytes: &[u8]) -> OcrManifest {
    OcrManifest {
        version: DERIVED_SYNC_VERSION,
        book_id: book_id.to_owned(),
        content_sha256: sha256(bytes),
        content_length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

fn validate_ocr_manifest(book_id: &str, manifest: &OcrManifest) -> SyncResult<()> {
    if manifest.version != DERIVED_SYNC_VERSION
        || manifest.book_id != book_id
        || manifest.content_sha256.len() != 64
        || manifest
            .content_sha256
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "synced PDF OCR manifest is invalid",
        )
        .into());
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn dirty_marker_path(book_id: &str, kind: DerivedDataKind) -> io::Result<PathBuf> {
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
        sha256(book_id.as_bytes())
    };
    Ok(project
        .data_local_dir()
        .join(DERIVED_SYNC_DIRECTORY)
        .join("dirty")
        .join(format!("{safe_id}.{}", kind.marker_name())))
}

fn is_dirty(book_id: &str, kind: DerivedDataKind) -> io::Result<bool> {
    Ok(dirty_marker_path(book_id, kind)?.exists())
}

fn clear_dirty_marker(book_id: &str, kind: DerivedDataKind) -> io::Result<()> {
    match fs::remove_file(dirty_marker_path(book_id, kind)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PdfOcrSyncData {
        PdfOcrSyncData {
            document: br#"{"version":1,"book_id":"book"}"#.to_vec(),
            resources: vec![
                ("figure-1.png".into(), vec![1, 2, 3]),
                ("figure-2.jpg".into(), vec![4, 5]),
            ],
        }
    }

    #[test]
    fn ocr_archive_is_deterministic_and_round_trips_resources() {
        let first = pack_ocr_archive(fixture()).unwrap();
        let second = pack_ocr_archive(fixture()).unwrap();
        assert_eq!(first, second);

        let unpacked = unpack_ocr_archive(first).unwrap();
        assert_eq!(unpacked.document, fixture().document);
        assert_eq!(unpacked.resources, fixture().resources);
    }

    #[test]
    fn ocr_manifest_identity_does_not_include_provider_or_model() {
        let manifest = ocr_manifest("book", b"current result");
        let json = serde_json::to_value(manifest).unwrap();
        assert_eq!(json["book_id"], "book");
        assert!(json.get("provider").is_none());
        assert!(json.get("model").is_none());
    }

    #[test]
    fn title_authors_and_toc_share_one_metadata_document() {
        let document = BookDerivedMetadata {
            version: DERIVED_SYNC_VERSION,
            book_id: "book".into(),
            toc: Some(serde_json::json!({
                "version": 1,
                "book_id": "book",
                "provider_name": "provider",
                "model": "model",
                "source_pages": [],
                "entries": []
            })),
            metadata: Some(serde_json::json!({
                "version": 1,
                "book_id": "book",
                "metadata": {
                    "title": "Title",
                    "authors": ["Author"],
                    "provider_name": "provider",
                    "model": "model"
                }
            })),
        };
        let bytes = serde_json::to_vec_pretty(&document).unwrap();
        let parsed = parse_metadata_document("book", &bytes).unwrap();
        assert!(parsed.toc.is_some());
        assert_eq!(parsed.metadata.unwrap()["metadata"]["title"], "Title");
    }
}
