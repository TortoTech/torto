use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static DERIVED_REVISION: AtomicU64 = AtomicU64::new(0);

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::plugins::{
    PdfOcrSyncData, export_pdf_ocr_sync_data, import_pdf_ocr_sync_data, pdf_ocr_sync_fingerprint,
};

use super::SyncResult;
use super::engine::{SyncProgress, SyncStage};
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
        dirty_revision: Option<Vec<u8>>,
        bytes: Vec<u8>,
    },
    DownloadMetadata {
        book_id: String,
        dirty_revision: Option<Vec<u8>>,
        bytes: Vec<u8>,
    },
    UploadOcr {
        book_id: String,
        dirty_revision: Option<Vec<u8>>,
        bytes: Vec<u8>,
        manifest: OcrManifest,
    },
    DownloadOcr {
        book_id: String,
        dirty_revision: Option<Vec<u8>>,
        manifest: OcrManifest,
    },
}

impl DerivedOperation {
    fn length(&self) -> u64 {
        match self {
            Self::UploadMetadata { bytes, .. } | Self::UploadOcr { bytes, .. } => {
                u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            }
            Self::DownloadOcr { manifest, .. } => manifest.content_length,
            // Metadata is fetched while planning; applying it is not another download.
            Self::DownloadMetadata { .. } => 0,
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
    crate::persistence::write_bytes_atomic(&path, uuid::Uuid::new_v4().to_string().as_bytes())?;
    DERIVED_REVISION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub(crate) fn derived_change_token() -> io::Result<Option<String>> {
    let revision = DERIVED_REVISION.load(Ordering::Relaxed);
    Ok((revision > 0).then(|| revision.to_string()))
}

pub(crate) async fn prepare_derived_data<F>(
    webdav: &WebDavClient,
    book_ids: impl IntoIterator<Item = String>,
    archive_dir: &Path,
    mut progress: F,
) -> SyncResult<PreparedDerivedData>
where
    F: FnMut(SyncStage, u64, u64),
{
    let mut operations = Vec::new();
    let book_ids = book_ids.into_iter().collect::<Vec<_>>();
    let book_total = book_ids.len() as u64;
    progress(SyncStage::CheckingDerivedData, 0, book_total);
    for (index, book_id) in book_ids.into_iter().enumerate() {
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
            archive_dir,
            remote_files.contains("ocr.json"),
            &mut operations,
        )
        .await?;
        progress(
            SyncStage::CheckingDerivedData,
            (index + 1) as u64,
            book_total,
        );
    }

    let (uploads, mut downloads): (Vec<_>, Vec<_>) =
        operations.into_iter().partition(|operation| {
            matches!(
                operation,
                DerivedOperation::UploadMetadata { .. } | DerivedOperation::UploadOcr { .. }
            )
        });
    downloads
        .sort_by_key(|operation| matches!(operation, DerivedOperation::DownloadMetadata { .. }));
    Ok(PreparedDerivedData { uploads, downloads })
}

pub(crate) struct PreparedDerivedData {
    uploads: Vec<DerivedOperation>,
    downloads: Vec<DerivedOperation>,
}

impl PreparedDerivedData {
    pub(crate) fn upload_bytes(&self) -> u64 {
        self.uploads.iter().map(DerivedOperation::length).sum()
    }

    pub(crate) fn download_bytes(&self) -> u64 {
        self.downloads.iter().map(DerivedOperation::length).sum()
    }

    pub(crate) fn operation_count(&self) -> usize {
        self.uploads.len() + self.downloads.len()
    }

    pub(crate) async fn execute(
        &mut self,
        webdav: &WebDavClient,
        cache_dir: &Path,
        stage: SyncStage,
        mut completed: u64,
        total: u64,
        progress: &mut impl FnMut(SyncProgress),
    ) -> SyncResult<()> {
        let operations = match stage {
            SyncStage::Uploading => std::mem::take(&mut self.uploads),
            SyncStage::Downloading => std::mem::take(&mut self.downloads),
            _ => unreachable!("derived transfers require an upload or download direction"),
        };
        fs::create_dir_all(cache_dir)?;
        for operation in operations {
            let operation_stage = if matches!(operation, DerivedOperation::DownloadMetadata { .. })
            {
                SyncStage::CheckingDerivedData
            } else {
                stage
            };
            progress(SyncProgress::Stage {
                stage: operation_stage,
                completed,
                total,
            });
            completed = execute_operation(
                webdav,
                operation,
                cache_dir,
                completed,
                total,
                &mut |completed, total| {
                    progress(SyncProgress::Stage {
                        stage: operation_stage,
                        completed,
                        total,
                    })
                },
            )
            .await?;
            progress(SyncProgress::Stage {
                stage: operation_stage,
                completed,
                total,
            });
        }
        Ok(())
    }
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
        DerivedOperation::UploadMetadata {
            book_id,
            bytes,
            dirty_revision,
        } => {
            let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            webdav
                .ensure_collection(&format!("derived/{book_id}/"))
                .await?;
            webdav
                .put_mutable_bytes_with_progress(
                    &format!("derived/{book_id}/metadata.json"),
                    bytes,
                    "application/json",
                    |sent| {
                        progress(
                            completed.saturating_add(sent.min(length.saturating_sub(1))),
                            total,
                        )
                    },
                )
                .await?;
            acknowledge_derived(webdav, &book_id, DerivedDataKind::Metadata, &dirty_revision)?;
            Ok(completed.saturating_add(length))
        }
        DerivedOperation::DownloadMetadata {
            book_id,
            bytes,
            dirty_revision,
        } => {
            require_unchanged(&book_id, DerivedDataKind::Metadata, &dirty_revision)?;
            apply_metadata_document(&book_id, &bytes)?;
            acknowledge_derived(webdav, &book_id, DerivedDataKind::Metadata, &dirty_revision)?;
            Ok(completed)
        }
        DerivedOperation::UploadOcr {
            book_id,
            dirty_revision,
            bytes,
            manifest,
        } => {
            let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            webdav
                .ensure_collection(&format!("derived/{book_id}/"))
                .await?;
            webdav
                .put_mutable_bytes_with_progress(
                    &format!("derived/{book_id}/ocr.zip"),
                    bytes,
                    "application/zip",
                    |sent| {
                        progress(
                            completed.saturating_add(sent.min(length.saturating_sub(1))),
                            total,
                        )
                    },
                )
                .await?;
            webdav
                .put_mutable_json(&format!("derived/{book_id}/ocr.json"), &manifest)
                .await?;
            acknowledge_derived(webdav, &book_id, DerivedDataKind::Ocr, &dirty_revision)?;
            Ok(completed.saturating_add(length))
        }
        DerivedOperation::DownloadOcr {
            book_id,
            manifest,
            dirty_revision,
        } => {
            download_ocr(
                webdav,
                &book_id,
                &manifest,
                cache_dir,
                completed,
                total,
                progress,
                &dirty_revision,
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
    dirty_revision: &Option<Vec<u8>>,
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
            |downloaded| {
                progress(
                    completed
                        .saturating_add(downloaded.min(manifest.content_length.saturating_sub(1))),
                    total,
                )
            },
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
    require_unchanged(book_id, DerivedDataKind::Ocr, dirty_revision)?;
    import_pdf_ocr_sync_data(book_id, unpack_ocr_archive(bytes)?)?;
    fs::remove_file(cache_path).ok();
    acknowledge_derived(webdav, book_id, DerivedDataKind::Ocr, dirty_revision)?;
    Ok(completed.saturating_add(manifest.content_length))
}

async fn collect_metadata_operation(
    webdav: &WebDavClient,
    book_id: &str,
    remote_present: bool,
    operations: &mut Vec<DerivedOperation>,
) -> SyncResult<()> {
    let dirty_revision = read_dirty_revision(book_id, DerivedDataKind::Metadata)?;
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
            acknowledge_derived(webdav, book_id, DerivedDataKind::Metadata, &dirty_revision)?;
        }
        (Some(local), None) => {
            operations.push(DerivedOperation::UploadMetadata {
                book_id: book_id.to_owned(),
                dirty_revision,
                bytes: serde_json::to_vec_pretty(local)?,
            });
        }
        (Some(local), Some(remote)) if is_dirty(webdav, book_id, DerivedDataKind::Metadata)? => {
            local.toc = local.toc.take().or_else(|| remote.toc.clone());
            local.metadata = local.metadata.take().or_else(|| remote.metadata.clone());
            operations.push(DerivedOperation::UploadMetadata {
                book_id: book_id.to_owned(),
                dirty_revision,
                bytes: serde_json::to_vec_pretty(local)?,
            });
        }
        (_, Some(_)) => operations.push(DerivedOperation::DownloadMetadata {
            book_id: book_id.to_owned(),
            dirty_revision,
            bytes: remote.expect("remote metadata bytes exist with a parsed document"),
        }),
        (None, None) => {
            acknowledge_derived(webdav, book_id, DerivedDataKind::Metadata, &dirty_revision)?
        }
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
    archive_dir: &Path,
    remote_present: bool,
    operations: &mut Vec<DerivedOperation>,
) -> SyncResult<()> {
    let dirty_revision = read_dirty_revision(book_id, DerivedDataKind::Ocr)?;
    let local = cached_ocr_archive(webdav, book_id, archive_dir)?;
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
        (Some(local), Some(manifest))
            if local.manifest.content_sha256 == manifest.content_sha256 =>
        {
            acknowledge_derived(webdav, book_id, DerivedDataKind::Ocr, &dirty_revision)?;
        }
        (Some(local), None) => {
            let bytes = fs::read(&local.path)?;
            let manifest = local.manifest;
            operations.push(DerivedOperation::UploadOcr {
                book_id: book_id.to_owned(),
                dirty_revision,
                bytes,
                manifest,
            });
        }
        (Some(local), Some(_)) if is_dirty(webdav, book_id, DerivedDataKind::Ocr)? => {
            let bytes = fs::read(&local.path)?;
            let manifest = local.manifest;
            operations.push(DerivedOperation::UploadOcr {
                book_id: book_id.to_owned(),
                dirty_revision,
                bytes,
                manifest,
            });
        }
        (_, Some(manifest)) => operations.push(DerivedOperation::DownloadOcr {
            book_id: book_id.to_owned(),
            dirty_revision,
            manifest,
        }),
        (None, None) => {
            acknowledge_derived(webdav, book_id, DerivedDataKind::Ocr, &dirty_revision)?
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct CachedOcrArchive {
    fingerprint: String,
    manifest: OcrManifest,
    path: PathBuf,
}

fn cached_ocr_archive(
    webdav: &WebDavClient,
    book_id: &str,
    directory: &Path,
) -> SyncResult<Option<CachedOcrArchive>> {
    let Some(fingerprint) = pdf_ocr_sync_fingerprint(book_id)? else {
        return Ok(None);
    };
    let key = format!("ocr-archive:{book_id}");
    let previous = webdav
        .cache_get(&key)?
        .and_then(|bytes| serde_json::from_slice::<CachedOcrArchive>(&bytes).ok());
    if let Some(cached) = previous.as_ref()
        && cached.fingerprint == fingerprint
        && fs::metadata(&cached.path)
            .is_ok_and(|metadata| metadata.len() == cached.manifest.content_length)
    {
        return Ok(previous);
    }
    let Some(data) = export_pdf_ocr_sync_data(book_id)? else {
        return Ok(None);
    };
    let bytes = pack_ocr_archive(data)?;
    if pdf_ocr_sync_fingerprint(book_id)?.as_deref() != Some(fingerprint.as_str()) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "OCR changed while preparing sync; retry required",
        )
        .into());
    }
    fs::create_dir_all(directory)?;
    let path = directory.join(format!("{book_id}-{fingerprint}-upload.zip"));
    crate::persistence::write_bytes_atomic(&path, &bytes)?;
    let cached = CachedOcrArchive {
        fingerprint,
        manifest: ocr_manifest(book_id, &bytes),
        path,
    };
    webdav.cache_set(&key, &serde_json::to_vec(&cached)?)?;
    if let Some(previous) = previous
        && previous.path != cached.path
        && previous.path.parent() == Some(directory)
    {
        fs::remove_file(previous.path).ok();
    }
    Ok(Some(cached))
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

fn read_dirty_revision(book_id: &str, kind: DerivedDataKind) -> io::Result<Option<Vec<u8>>> {
    match fs::read(dirty_marker_path(book_id, kind)?) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_dirty(webdav: &WebDavClient, book_id: &str, kind: DerivedDataKind) -> SyncResult<bool> {
    let Some(revision) = read_dirty_revision(book_id, kind)? else {
        return Ok(false);
    };
    Ok(webdav
        .cache_get(&format!("derived-ack:{book_id}:{}", kind.marker_name()))?
        .as_ref()
        != Some(&revision))
}

fn acknowledge_derived(
    webdav: &WebDavClient,
    book_id: &str,
    kind: DerivedDataKind,
    revision: &Option<Vec<u8>>,
) -> SyncResult<()> {
    if let Some(revision) = revision {
        webdav.cache_set(
            &format!("derived-ack:{book_id}:{}", kind.marker_name()),
            revision,
        )?;
    }
    Ok(())
}

fn require_unchanged(
    book_id: &str,
    kind: DerivedDataKind,
    expected: &Option<Vec<u8>>,
) -> SyncResult<()> {
    if &read_dirty_revision(book_id, kind)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Local derived data changed during download; retry required",
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ocr_sync_cache_reuses_archive_until_local_files_change() {
        let id = sha256(uuid::Uuid::new_v4().as_bytes());
        let root = std::env::temp_dir().join(format!("ocr-cache-test-{id}"));
        let store = crate::sync::SyncStore::open_at(root.join("cache.sqlite"), "device").unwrap();
        let mut settings = crate::sync::SyncSettings::new_device();
        settings.provider = crate::sync::CloudProviderKind::Custom;
        settings.base_url = "http://127.0.0.1:1".into();
        settings.username = "reader".into();
        let client = WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store, "account".into());
        let document = |markdown: &str| PdfOcrSyncData {
            document: serde_json::to_vec(&serde_json::json!({
                "version": 1, "book_id": id, "provider": "paddle-ocr", "model": "fixture",
                "view_mode": "original", "pages": [{ "markdown": markdown }], "resources": []
            }))
            .unwrap(),
            resources: Vec::new(),
        };
        import_pdf_ocr_sync_data(&id, document("first page")).unwrap();
        let first = cached_ocr_archive(&client, &id, &root).unwrap().unwrap();
        let modified = fs::metadata(&first.path).unwrap().modified().unwrap();
        let second = cached_ocr_archive(&client, &id, &root).unwrap().unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(
            fs::metadata(&second.path).unwrap().modified().unwrap(),
            modified
        );
        import_pdf_ocr_sync_data(&id, document("a changed and longer page")).unwrap();
        let changed = cached_ocr_archive(&client, &id, &root).unwrap().unwrap();
        assert_ne!(
            first.manifest.content_sha256,
            changed.manifest.content_sha256
        );
        assert_ne!(first.path, changed.path);
        fs::remove_file(&changed.path).unwrap();
        let repaired = cached_ocr_archive(&client, &id, &root).unwrap().unwrap();
        assert_eq!(
            repaired.manifest.content_sha256,
            changed.manifest.content_sha256
        );
        drop(client);
        let directory = ProjectDirs::from("com", "Rebook", "Rebook")
            .unwrap()
            .data_local_dir()
            .join("pdf-ocr")
            .join(&id);
        fs::remove_file(directory.join("document.json")).unwrap();
        fs::remove_dir(directory.join("resources")).unwrap();
        fs::remove_dir(directory).unwrap();
        for entry in fs::read_dir(&root).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn derived_sync_ack_keeps_newer_edits_and_other_accounts_pending() {
        let id = uuid::Uuid::new_v4().to_string();
        let database = std::env::temp_dir().join(format!("derived-revision-{id}.sqlite"));
        let store = crate::sync::SyncStore::open_at(database.clone(), "device").unwrap();
        let mut settings = crate::sync::SyncSettings::new_device();
        settings.provider = crate::sync::CloudProviderKind::Custom;
        settings.base_url = "http://127.0.0.1:1".into();
        settings.username = "reader".into();
        let first = WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store.clone(), "first".into());
        let second = WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store, "second".into());
        mark_derived_dirty(&id, DerivedDataKind::Ocr).unwrap();
        let old = read_dirty_revision(&id, DerivedDataKind::Ocr).unwrap();
        mark_derived_dirty(&id, DerivedDataKind::Ocr).unwrap();
        acknowledge_derived(&first, &id, DerivedDataKind::Ocr, &old).unwrap();
        assert!(is_dirty(&first, &id, DerivedDataKind::Ocr).unwrap());
        assert!(require_unchanged(&id, DerivedDataKind::Ocr, &old).is_err());
        let current = read_dirty_revision(&id, DerivedDataKind::Ocr).unwrap();
        acknowledge_derived(&first, &id, DerivedDataKind::Ocr, &current).unwrap();
        assert!(!is_dirty(&first, &id, DerivedDataKind::Ocr).unwrap());
        assert!(is_dirty(&second, &id, DerivedDataKind::Ocr).unwrap());
        fs::remove_file(dirty_marker_path(&id, DerivedDataKind::Ocr).unwrap()).unwrap();
        drop(first);
        drop(second);
        let _ = fs::remove_file(database);
    }

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
