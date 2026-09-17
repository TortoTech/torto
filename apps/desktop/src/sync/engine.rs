use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::library::RemoteLibraryBook;

use super::SyncResult;
use super::derived::prepare_derived_data;
use super::protocol::{
    BookManifest, DeviceBookState, DeviceLibrary, PROTOCOL_VERSION, ProtocolDocument,
};
use super::settings::SyncSettings;
use super::store::SyncStore;
use super::webdav::WebDavClient;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncMode {
    Reading,
    Full { force_statistics: bool },
}

#[derive(Clone, Debug)]
pub(crate) struct LocalSyncBook {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub file_name: String,
    pub path: PathBuf,
    pub cover_bytes: Option<Vec<u8>>,
    pub added_at: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SyncReport {
    pub uploaded_books: usize,
    pub downloaded_books: usize,
    pub merged_annotations: usize,
    pub updated_progress: usize,
    pub derived_operations: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncStage {
    Preparing,
    Checking,
    Uploading,
    Discovering,
    PreparingDownload,
    Downloading,
    ReadingData,
    CheckingDerivedData,
    Statistics,
}

#[derive(Clone, Debug)]
pub(crate) enum SyncProgress {
    Stage {
        stage: SyncStage,
        completed: u64,
        total: u64,
    },
    Downloaded {
        book: Box<RemoteLibraryBook>,
        cache_path: PathBuf,
        completed: u64,
        total: u64,
    },
}

pub(crate) async fn run_sync<F>(
    settings: SyncSettings,
    password: String,
    local_books: Vec<LocalSyncBook>,
    mode: SyncMode,
    mut progress: F,
) -> SyncResult<SyncReport>
where
    F: FnMut(SyncProgress),
{
    let store = SyncStore::open_default(settings.device_id.clone())?;
    let cache_dir = download_cache_dir()?;
    let webdav = WebDavClient::new(&settings, password.clone())?
        .with_store(store.clone(), super::account_key(&settings));
    let device = settings.device_id.clone();
    if mode == SyncMode::Reading {
        return sync_changed_reading(&webdav, &store, &settings, &mut progress).await;
    }
    let report = run_sync_with_store(
        settings,
        password,
        local_books,
        store,
        cache_dir,
        &mut progress,
    )
    .await?;
    if crate::statistics::sync_due(
        &webdav,
        matches!(
            mode,
            SyncMode::Full {
                force_statistics: true
            }
        ),
    )? {
        progress(SyncProgress::Stage {
            stage: SyncStage::Statistics,
            completed: 0,
            total: 0,
        });
        crate::statistics::sync(&webdav, &device).await?;
    }
    Ok(report)
}

async fn run_sync_with_store<F>(
    settings: SyncSettings,
    password: String,
    local_books: Vec<LocalSyncBook>,
    store: SyncStore,
    download_cache_dir: PathBuf,
    mut progress: F,
) -> SyncResult<SyncReport>
where
    F: FnMut(SyncProgress),
{
    settings.validate()?;
    progress(SyncProgress::Stage {
        stage: SyncStage::Preparing,
        completed: 0,
        total: 0,
    });
    let webdav = WebDavClient::new(&settings, password)?
        .with_store(store.clone(), super::account_key(&settings));
    // Reading changes are small and must not wait behind an OCR archive or library scan.
    let mut report = sync_changed_reading(&webdav, &store, &settings, &mut progress).await?;
    webdav.ensure_base_layout().await?;
    if webdav.get_optional("protocol.json").await?.is_none() {
        webdav
            .put_immutable(
                "protocol.json",
                serde_json::to_vec_pretty(&ProtocolDocument {
                    version: PROTOCOL_VERSION,
                    protocol: "rebook-webdav".into(),
                })?,
                "application/json",
            )
            .await?;
    }

    let (local_by_id, pending_uploads) =
        prepare_local_books(&webdav, &store, local_books, &mut progress).await?;
    progress(SyncProgress::Stage {
        stage: SyncStage::Discovering,
        completed: 0,
        total: 0,
    });
    let remote_book_ids = discover_remote_books(&webdav).await?;
    let mut all_book_ids = remote_book_ids.clone();
    all_book_ids.extend(local_by_id.keys().cloned());
    let manifests = prepare_downloads(
        &webdav,
        &store,
        &local_by_id,
        &remote_book_ids,
        &download_cache_dir,
        &mut progress,
    )
    .await?;
    let mut derived = prepare_derived_data(
        &webdav,
        all_book_ids.iter().cloned(),
        &download_cache_dir.join("derived"),
        |stage, completed, total| {
            progress(SyncProgress::Stage {
                stage,
                completed,
                total,
            });
        },
    )
    .await?;
    report.derived_operations = derived.operation_count();
    let book_upload_bytes = pending_uploads
        .iter()
        .map(|(length, _)| *length)
        .sum::<u64>();
    let book_download_bytes = manifests
        .iter()
        .map(|(_, manifest)| manifest.content_length)
        .sum::<u64>();
    let upload_total = book_upload_bytes.saturating_add(derived.upload_bytes());
    let download_total = book_download_bytes.saturating_add(derived.download_bytes());
    let derived_cache = download_cache_dir.join("derived");
    upload_books(
        &webdav,
        pending_uploads,
        upload_total,
        &mut report,
        &mut progress,
    )
    .await?;
    derived
        .execute(
            &webdav,
            &derived_cache,
            SyncStage::Uploading,
            book_upload_bytes,
            upload_total,
            &mut progress,
        )
        .await?;
    // Publish only after book content and manifests exist on the server.
    progress(SyncProgress::Stage {
        stage: SyncStage::Discovering,
        completed: 0,
        total: 0,
    });
    publish_device_library(&webdav, &store, &settings, &local_by_id).await?;
    download_books(
        &webdav,
        manifests,
        &download_cache_dir,
        download_total,
        &mut report,
        &mut progress,
    )
    .await?;
    derived
        .execute(
            &webdav,
            &derived_cache,
            SyncStage::Downloading,
            book_download_bytes,
            download_total,
            &mut progress,
        )
        .await?;
    sync_reading_data(
        &webdav,
        &store,
        &settings,
        all_book_ids,
        &mut report,
        &mut progress,
    )
    .await?;

    Ok(report)
}

async fn prepare_local_books<F>(
    webdav: &WebDavClient,
    store: &SyncStore,
    local_books: Vec<LocalSyncBook>,
    progress: &mut F,
) -> SyncResult<(BTreeMap<String, LocalSyncBook>, Vec<(u64, LocalSyncBook)>)>
where
    F: FnMut(SyncProgress),
{
    let mut local_by_id = BTreeMap::new();
    let books_with_lengths = local_books
        .into_iter()
        .map(|book| Ok((fs::metadata(&book.path)?.len(), book)))
        .collect::<io::Result<Vec<_>>>()?;
    let total = books_with_lengths.len() as u64;
    progress(SyncProgress::Stage {
        stage: SyncStage::Checking,
        completed: 0,
        total,
    });
    let mut completed = 0_u64;
    let mut pending_uploads = Vec::new();
    for (content_length, book) in books_with_lengths {
        validate_book_id(&book.id)?;
        if store.is_locally_removed(&book.id)? {
            completed += 1;
            progress(SyncProgress::Stage {
                stage: SyncStage::Checking,
                completed,
                total,
            });
            continue;
        }
        store.register_book_if_missing(&book.id)?;
        if !remote_book_exists(webdav, &book, content_length).await? {
            pending_uploads.push((content_length, book.clone()));
        }
        completed += 1;
        local_by_id.insert(book.id.clone(), book);
        progress(SyncProgress::Stage {
            stage: SyncStage::Checking,
            completed,
            total,
        });
    }
    Ok((local_by_id, pending_uploads))
}

async fn upload_books(
    webdav: &WebDavClient,
    pending_uploads: Vec<(u64, LocalSyncBook)>,
    upload_total: u64,
    report: &mut SyncReport,
    progress: &mut impl FnMut(SyncProgress),
) -> SyncResult<()> {
    if upload_total > 0 {
        progress(SyncProgress::Stage {
            stage: SyncStage::Uploading,
            completed: 0,
            total: upload_total,
        });
        let mut uploaded_bytes = 0_u64;
        for (content_length, book) in pending_uploads {
            if upload_new_book(webdav, &book, content_length, |sent| {
                progress(SyncProgress::Stage {
                    stage: SyncStage::Uploading,
                    completed: uploaded_bytes
                        .saturating_add(sent.min(content_length.saturating_sub(1))),
                    total: upload_total,
                });
            })
            .await?
            {
                report.uploaded_books += 1;
            }
            uploaded_bytes = uploaded_bytes.saturating_add(content_length);
            progress(SyncProgress::Stage {
                stage: SyncStage::Uploading,
                completed: uploaded_bytes,
                total: upload_total,
            });
        }
    }
    Ok(())
}

async fn publish_device_library(
    webdav: &WebDavClient,
    store: &SyncStore,
    settings: &SyncSettings,
    local_by_id: &BTreeMap<String, LocalSyncBook>,
) -> SyncResult<()> {
    let local_ids = local_by_id.keys().cloned().collect::<Vec<_>>();
    let path = format!("library/devices/{}.json", settings.device_id);
    let books = store.membership_entries(&local_ids)?;
    if let Some(remote) = webdav.get_optional(&path).await? {
        let previous: DeviceLibrary = serde_json::from_slice(&remote.bytes)?;
        if previous.version == PROTOCOL_VERSION
            && previous.device_id == settings.device_id
            && previous.device_name == settings.device_name
            && previous.books == books
        {
            return Ok(());
        }
    }
    let library = DeviceLibrary {
        version: PROTOCOL_VERSION,
        device_id: settings.device_id.clone(),
        device_name: settings.device_name.clone(),
        updated_at: store.tick()?,
        books,
    };
    webdav.put_mutable_json(&path, &library).await
}

async fn prepare_downloads<F>(
    webdav: &WebDavClient,
    store: &SyncStore,
    local_by_id: &BTreeMap<String, LocalSyncBook>,
    remote_book_ids: &BTreeSet<String>,
    cache_dir: &Path,
    progress: &mut F,
) -> SyncResult<Vec<(String, BookManifest)>>
where
    F: FnMut(SyncProgress),
{
    fs::create_dir_all(cache_dir)?;
    for book_id in local_by_id.keys() {
        fs::remove_file(partial_download_path(cache_dir, book_id)).ok();
    }
    let mut missing = Vec::new();
    for book_id in remote_book_ids {
        if !local_by_id.contains_key(book_id) && !store.is_locally_removed(book_id)? {
            missing.push(book_id.clone());
        }
    }
    let mut manifests = Vec::with_capacity(missing.len());
    let manifest_total = missing.len() as u64;
    progress(SyncProgress::Stage {
        stage: SyncStage::PreparingDownload,
        completed: 0,
        total: manifest_total,
    });
    for book_id in missing {
        manifests.push((book_id.clone(), download_manifest(webdav, &book_id).await?));
        progress(SyncProgress::Stage {
            stage: SyncStage::PreparingDownload,
            completed: manifests.len() as u64,
            total: manifest_total,
        });
    }
    manifests.sort_by_key(|(_, manifest)| manifest.content_length);
    Ok(manifests)
}

async fn download_books(
    webdav: &WebDavClient,
    manifests: Vec<(String, BookManifest)>,
    cache_dir: &Path,
    total: u64,
    report: &mut SyncReport,
    progress: &mut impl FnMut(SyncProgress),
) -> SyncResult<()> {
    let mut partial_lengths = BTreeMap::new();
    let mut completed = 0_u64;
    for (book_id, manifest) in &manifests {
        let path = partial_download_path(cache_dir, book_id);
        let length = valid_partial_length(&path, manifest.content_length)?
            .min(manifest.content_length.saturating_sub(1));
        completed = completed.saturating_add(length);
        partial_lengths.insert(book_id.clone(), length);
    }
    progress(SyncProgress::Stage {
        stage: SyncStage::Downloading,
        completed,
        total,
    });
    for (book_id, manifest) in manifests {
        let content_length = manifest.content_length;
        let cache_path = partial_download_path(cache_dir, &book_id);
        let mut current_length = partial_lengths.get(&book_id).copied().unwrap_or_default();
        let book = download_book(webdav, &book_id, manifest, &cache_path, |downloaded| {
            let downloaded = downloaded.min(content_length.saturating_sub(1));
            completed = completed
                .saturating_sub(current_length)
                .saturating_add(downloaded);
            current_length = downloaded;
            progress(SyncProgress::Stage {
                stage: SyncStage::Downloading,
                completed,
                total,
            });
        })
        .await?;
        completed = completed
            .saturating_sub(current_length)
            .saturating_add(content_length);
        report.downloaded_books += 1;
        progress(SyncProgress::Downloaded {
            book: Box::new(book),
            cache_path,
            completed,
            total,
        });
    }
    Ok(())
}

async fn push_reading_state(
    webdav: &WebDavClient,
    store: &SyncStore,
    settings: &SyncSettings,
    book_id: &str,
) -> SyncResult<u64> {
    let (revision, state) = store.reading_snapshot(book_id)?;
    webdav
        .put_mutable_json(
            &format!("state/{book_id}/devices/{}.json", settings.device_id),
            &state,
        )
        .await?;
    Ok(revision)
}

async fn merge_remote_reading(
    webdav: &WebDavClient,
    store: &SyncStore,
    settings: &SyncSettings,
    book_id: &str,
    files: Vec<String>,
    report: &mut SyncReport,
) -> SyncResult<()> {
    for file in files {
        if file == format!("{}.json", settings.device_id) {
            continue;
        }
        let path = format!("state/{book_id}/devices/{file}");
        let Some(object) = webdav.get_optional(&path).await? else {
            continue;
        };
        let digest = format!("{:x}", Sha256::digest(&object.bytes));
        let key = format!("applied:{path}");
        if webdav.cache_get(&key)?.as_deref() == Some(digest.as_bytes()) {
            continue;
        }
        let remote: DeviceBookState = serde_json::from_slice(&object.bytes)?;
        validate_state(&remote, book_id)?;
        if let Some(progress) = &remote.progress
            && store.merge_progress(progress)?
        {
            report.updated_progress += 1;
        }
        report.merged_annotations += store.merge_annotations(&remote.annotations)?;
        webdav.cache_set(&key, digest.as_bytes())?;
    }
    Ok(())
}

async fn sync_changed_reading(
    webdav: &WebDavClient,
    store: &SyncStore,
    settings: &SyncSettings,
    progress: &mut impl FnMut(SyncProgress),
) -> SyncResult<SyncReport> {
    let pending = store.pending_reading(&super::account_key(settings))?;
    let mut report = SyncReport::default();
    let total = pending.len() as u64;
    if total > 0 {
        progress(SyncProgress::Stage {
            stage: SyncStage::ReadingData,
            completed: 0,
            total,
        });
    }
    for (index, (book_id, _, _)) in pending.into_iter().enumerate() {
        validate_book_id(&book_id)?;
        let revision = push_reading_state(webdav, store, settings, &book_id).await?;
        let files = webdav
            .list_json_files(&format!("state/{book_id}/devices/"))
            .await?;
        merge_remote_reading(webdav, store, settings, &book_id, files, &mut report).await?;
        store.acknowledge_reading(&super::account_key(settings), &book_id, revision)?;
        progress(SyncProgress::Stage {
            stage: SyncStage::ReadingData,
            completed: (index + 1) as u64,
            total,
        });
    }
    Ok(report)
}

async fn sync_reading_data<F>(
    webdav: &WebDavClient,
    store: &SyncStore,
    settings: &SyncSettings,
    all_book_ids: BTreeSet<String>,
    report: &mut SyncReport,
    progress: &mut F,
) -> SyncResult<()>
where
    F: FnMut(SyncProgress),
{
    let pending = store
        .pending_reading(&super::account_key(settings))?
        .into_iter()
        .map(|(id, _, _)| id)
        .collect::<BTreeSet<_>>();
    let total = all_book_ids.len() as u64;
    progress(SyncProgress::Stage {
        stage: SyncStage::ReadingData,
        completed: 0,
        total,
    });
    for (index, book_id) in all_book_ids.into_iter().enumerate() {
        validate_book_id(&book_id)?;
        let files = webdav
            .list_json_files(&format!("state/{book_id}/devices/"))
            .await?;
        let own_file = format!("{}.json", settings.device_id);
        let missing = !files.contains(&own_file);
        if missing {
            webdav.invalidate_object(&format!("state/{book_id}/devices/{own_file}"))?;
        }
        let uploaded = if missing || pending.contains(&book_id) {
            Some(push_reading_state(webdav, store, settings, &book_id).await?)
        } else {
            None
        };
        merge_remote_reading(webdav, store, settings, &book_id, files, report).await?;
        if let Some(revision) = uploaded {
            store.acknowledge_reading(&super::account_key(settings), &book_id, revision)?;
        }
        progress(SyncProgress::Stage {
            stage: SyncStage::ReadingData,
            completed: (index + 1) as u64,
            total,
        });
    }
    Ok(())
}

async fn remote_book_exists(
    webdav: &WebDavClient,
    book: &LocalSyncBook,
    content_length: u64,
) -> SyncResult<bool> {
    let directory = format!("books/{}/", book.id);
    webdav.ensure_collection(&directory).await?;
    let manifest_path = format!("{directory}manifest.json");
    if let Some(object) = webdav.get_optional(&manifest_path).await? {
        let manifest: BookManifest = serde_json::from_slice(&object.bytes)?;
        validate_manifest(&manifest, &book.id)?;
        if manifest.content_length != content_length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("远端书籍清单与本地文件大小不一致：{}", book.file_name),
            )
            .into());
        }
        return Ok(true);
    }
    Ok(false)
}

async fn upload_new_book(
    webdav: &WebDavClient,
    book: &LocalSyncBook,
    content_length: u64,
    progress: impl FnMut(u64),
) -> SyncResult<bool> {
    let extension = storage_extension(&book.path, &book.file_name)?;
    let directory = format!("books/{}/", book.id);
    let content_path = format!("{directory}content.{extension}");
    let manifest_path = format!("{directory}manifest.json");
    let content = fs::read(&book.path)?;
    let digest = format!("{:x}", Sha256::digest(&content));
    if digest != book.id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("本地书籍内容校验失败：{}", book.file_name),
        )
        .into());
    }
    let uploaded = webdav
        .put_immutable_with_progress(&content_path, content, "application/octet-stream", progress)
        .await?;
    let cover_path = if let Some(cover) = &book.cover_bytes {
        let path = format!("{directory}cover.bin");
        webdav
            .put_immutable(&path, cover.clone(), "application/octet-stream")
            .await?;
        Some(path)
    } else {
        None
    };
    let manifest = BookManifest {
        version: PROTOCOL_VERSION,
        book_id: book.id.clone(),
        title: book.title.clone(),
        authors: book.authors.clone(),
        file_name: book.file_name.clone(),
        content_path,
        content_sha256: book.id.clone(),
        content_length,
        cover_path,
        added_at: book.added_at,
    };
    webdav
        .put_immutable(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest)?,
            "application/json",
        )
        .await?;
    Ok(uploaded)
}

async fn discover_remote_books(webdav: &WebDavClient) -> SyncResult<BTreeSet<String>> {
    let mut books = BTreeSet::new();
    for file_name in webdav.list_json_files("library/devices/").await? {
        let Some(object) = webdav
            .get_optional(&format!("library/devices/{file_name}"))
            .await?
        else {
            continue;
        };
        let library: DeviceLibrary = serde_json::from_slice(&object.bytes)?;
        if library.version != PROTOCOL_VERSION || library.device_id.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "远端书架清单版本或设备标识无效",
            )
            .into());
        }
        for entry in library.books {
            validate_book_id(&entry.book_id)?;
            if entry.present {
                books.insert(entry.book_id);
            }
        }
    }
    Ok(books)
}

async fn download_manifest(webdav: &WebDavClient, book_id: &str) -> SyncResult<BookManifest> {
    validate_book_id(book_id)?;
    let manifest_path = format!("books/{book_id}/manifest.json");
    let manifest_object = webdav
        .get_optional(&manifest_path)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, manifest_path.clone()))?;
    let manifest: BookManifest = serde_json::from_slice(&manifest_object.bytes)?;
    validate_manifest(&manifest, book_id)?;
    Ok(manifest)
}

async fn download_book(
    webdav: &WebDavClient,
    book_id: &str,
    manifest: BookManifest,
    cache_path: &Path,
    progress: impl FnMut(u64),
) -> SyncResult<RemoteLibraryBook> {
    if !webdav
        .download_to_file(
            &manifest.content_path,
            cache_path,
            manifest.content_length,
            progress,
        )
        .await?
    {
        return Err(io::Error::new(io::ErrorKind::NotFound, manifest.content_path.clone()).into());
    }
    let content = fs::read(cache_path)?;
    if u64::try_from(content.len()).unwrap_or(u64::MAX) != manifest.content_length
        || format!("{:x}", Sha256::digest(&content)) != manifest.content_sha256
    {
        fs::remove_file(cache_path).ok();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("远端书籍内容校验失败：{book_id}"),
        )
        .into());
    }
    let cover = if let Some(path) = &manifest.cover_path {
        Some(
            webdav
                .get_optional(path)
                .await?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.clone()))?
                .bytes,
        )
    } else {
        None
    };
    Ok(RemoteLibraryBook {
        id: manifest.book_id,
        title: manifest.title,
        authors: manifest.authors,
        file_name: manifest.file_name,
        content_sha256: manifest.content_sha256,
        added_at: manifest.added_at,
        content,
        cover,
    })
}

fn download_cache_dir() -> SyncResult<PathBuf> {
    let project = crate::smoke::project_dirs().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Unable to determine the WebDAV download cache directory",
        )
    })?;
    Ok(project.cache_dir().join("sync-downloads-v1"))
}

fn partial_download_path(cache_dir: &Path, book_id: &str) -> PathBuf {
    cache_dir.join(format!("{book_id}.part"))
}

fn valid_partial_length(path: &Path, expected_length: u64) -> io::Result<u64> {
    let length = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or_default();
    if length <= expected_length {
        return Ok(length);
    }
    fs::remove_file(path)?;
    Ok(0)
}

fn validate_manifest(manifest: &BookManifest, expected_book_id: &str) -> SyncResult<()> {
    validate_book_id(&manifest.book_id)?;
    if manifest.version != PROTOCOL_VERSION
        || manifest.book_id != expected_book_id
        || manifest.content_sha256 != expected_book_id
    {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "远端书籍清单与内容标识不一致").into(),
        );
    }
    let content_prefix = format!("books/{expected_book_id}/content.");
    if !manifest.content_path.starts_with(&content_prefix)
        || manifest.content_path[content_prefix.len()..]
            .chars()
            .any(|character| !character.is_ascii_alphanumeric())
        || manifest
            .cover_path
            .as_ref()
            .is_some_and(|path| path != &format!("books/{expected_book_id}/cover.bin"))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "远端书籍清单包含非法路径").into());
    }
    Ok(())
}

fn validate_state(state: &DeviceBookState, expected_book_id: &str) -> SyncResult<()> {
    if state.version != PROTOCOL_VERSION
        || state.book_id != expected_book_id
        || state.device_id.trim().is_empty()
        || state
            .progress
            .as_ref()
            .is_some_and(|progress| progress.locator.publication_id.as_str() != expected_book_id)
        || state
            .annotations
            .iter()
            .any(|annotation| annotation.book_id != expected_book_id)
    {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "远端阅读状态与书籍标识不一致").into(),
        );
    }
    Ok(())
}

fn validate_book_id(book_id: &str) -> SyncResult<()> {
    if book_id.len() != 64
        || book_id
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "书籍内容哈希无效").into());
    }
    Ok(())
}

fn storage_extension(path: &std::path::Path, file_name: &str) -> SyncResult<String> {
    let extension = path
        .extension()
        .or_else(|| std::path::Path::new(file_name).extension())
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|value| {
            !value.is_empty()
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "书籍扩展名无效"))?;
    Ok(extension)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use uuid::Uuid;

    type FakeObjects = Arc<Mutex<HashMap<String, (Vec<u8>, String)>>>;

    #[test]
    fn manifest_paths_are_content_addressed_and_cannot_escape_root() {
        let id = "a".repeat(64);
        let mut manifest = BookManifest {
            version: PROTOCOL_VERSION,
            book_id: id.clone(),
            title: "Book".into(),
            authors: Vec::new(),
            file_name: "book.epub".into(),
            content_path: format!("books/{id}/content.epub"),
            content_sha256: id.clone(),
            content_length: 10,
            cover_path: Some(format!("books/{id}/cover.bin")),
            added_at: 0,
        };
        validate_manifest(&manifest, &id).unwrap();

        manifest.content_path = "../secrets.txt".into();
        assert!(validate_manifest(&manifest, &id).is_err());
    }

    #[test]
    fn rejects_non_hash_book_identity() {
        assert!(validate_book_id("book-title").is_err());
        assert!(validate_book_id(&"A".repeat(64)).is_err());
        assert!(validate_book_id(&"f".repeat(64)).is_ok());
    }

    fn reading_locator(id: &str, value: f64) -> rebook_publication::LocatorV1 {
        let mut locator = rebook_publication::LocatorV1::at_start(
            rebook_publication::PublicationId::new(id).unwrap(),
            rebook_publication::PublicationUrl::parse("chapter.xhtml").unwrap(),
        );
        locator.total_progression = Some(value);
        locator
    }

    #[test]
    fn progress_only_sync_uses_two_requests_and_failed_uploads_remain_pending() {
        let server = FakeWebDav::start();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let settings = test_settings(server.base_url(), "Fast sync");
        let account = super::super::account_key(&settings);
        let database = test_database("fast-sync");
        let store = SyncStore::open_at(database.clone(), settings.device_id.clone()).unwrap();
        let client = WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store.clone(), account.clone());
        let id = "a".repeat(64);
        // Other library entries must not add requests to the reading-only path.
        for index in 0..100 {
            store
                .set_book_present(&format!("{index:064x}"), true)
                .unwrap();
        }
        store
            .save_progress(&id, &reading_locator(&id, 0.2))
            .unwrap();
        runtime
            .block_on(sync_changed_reading(
                &client,
                &store,
                &settings,
                &mut |_| {},
            ))
            .unwrap();
        server.requests.lock().unwrap().clear();
        store
            .save_progress(&id, &reading_locator(&id, 0.21))
            .unwrap();
        runtime
            .block_on(sync_changed_reading(
                &client,
                &store,
                &settings,
                &mut |_| {},
            ))
            .unwrap();
        let requests = std::mem::take(&mut *server.requests.lock().unwrap());
        assert_eq!(
            requests.len(),
            2,
            "warm progress sync should only PUT its state and list peer states: {requests:?}"
        );
        assert!(
            requests
                .iter()
                .all(|(_, path, _)| path.starts_with(&format!("/dav/Rebook/v1/state/{id}/")))
        );
        assert_eq!(requests[0].0, "PUT");
        assert_eq!(requests[1].0, "PROPFIND");
        runtime
            .block_on(sync_changed_reading(
                &client,
                &store,
                &settings,
                &mut |_| {},
            ))
            .unwrap();
        assert!(server.requests.lock().unwrap().is_empty());
        store
            .save_progress(&id, &reading_locator(&id, 0.3))
            .unwrap();
        server.fail_next_put.store(true, Ordering::Relaxed);
        assert!(
            runtime
                .block_on(sync_changed_reading(
                    &client,
                    &store,
                    &settings,
                    &mut |_| {}
                ))
                .is_err()
        );
        assert_eq!(store.pending_reading(&account).unwrap().len(), 1);
        runtime
            .block_on(sync_changed_reading(
                &client,
                &store,
                &settings,
                &mut |_| {},
            ))
            .unwrap();
        assert!(store.pending_reading(&account).unwrap().is_empty());
        drop(client);
        drop(store);
        let _ = fs::remove_file(database);
        server.stop();
    }

    #[test]
    fn remote_etag_cache_survives_client_recreation_and_detects_changes() {
        let server = FakeWebDav::start();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let settings = test_settings(server.base_url(), "ETag cache");
        let database = test_database("etag-cache");
        let store = SyncStore::open_at(database.clone(), settings.device_id.clone()).unwrap();
        let account = super::super::account_key(&settings);
        let client = WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store.clone(), account.clone());
        server.objects.lock().unwrap().insert(
            "/dav/Rebook/v1/probe.json".into(),
            (b"first".to_vec(), "\"1\"".into()),
        );
        assert_eq!(
            runtime
                .block_on(client.get_optional("probe.json"))
                .unwrap()
                .unwrap()
                .bytes,
            b"first"
        );
        let client = WebDavClient::new(&settings, "secret".into())
            .unwrap()
            .with_store(store.clone(), account);
        assert_eq!(
            runtime
                .block_on(client.get_optional("probe.json"))
                .unwrap()
                .unwrap()
                .bytes,
            b"first"
        );
        assert_eq!(
            server.requests.lock().unwrap().last().unwrap().2.as_deref(),
            Some("\"1\"")
        );
        server.objects.lock().unwrap().insert(
            "/dav/Rebook/v1/probe.json".into(),
            (b"second".to_vec(), "\"2\"".into()),
        );
        assert_eq!(
            runtime
                .block_on(client.get_optional("probe.json"))
                .unwrap()
                .unwrap()
                .bytes,
            b"second"
        );
        server
            .objects
            .lock()
            .unwrap()
            .remove("/dav/Rebook/v1/probe.json");
        assert!(
            runtime
                .block_on(client.get_optional("probe.json"))
                .unwrap()
                .is_none()
        );
        assert!(client.cache_get("object:probe.json").unwrap().is_none());
        drop(client);
        drop(store);
        let _ = fs::remove_file(database);
        server.stop();
    }

    #[test]
    fn books_and_ocr_share_one_progress_total_in_each_direction() {
        let server = FakeWebDav::start();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let content = Uuid::new_v4().to_string().into_bytes();
        let id = format!("{:x}", Sha256::digest(&content));
        let source = std::env::temp_dir().join(format!("sync-grouped-{id}.pdf"));
        fs::write(&source, &content).unwrap();
        let ocr_document = serde_json::json!({
            "version": 1, "book_id": id, "provider": "paddle-ocr", "model": "fixture",
            "view_mode": "original", "pages": [{"markdown": "A synced OCR page"}], "resources": []
        });
        crate::plugins::import_pdf_ocr_sync_data(
            &id,
            crate::plugins::PdfOcrSyncData {
                document: serde_json::to_vec(&ocr_document).unwrap(),
                resources: Vec::new(),
            },
        )
        .unwrap();
        let ocr_directory = crate::smoke::project_dirs()
            .unwrap()
            .data_local_dir()
            .join("pdf-ocr")
            .join(&id);
        let settings = test_settings(server.base_url(), "Grouped upload");
        let first_database = test_database("grouped-upload");
        let second_database = test_database("grouped-download");
        let cache = test_download_cache("grouped");
        let book = LocalSyncBook {
            id: id.clone(),
            title: "Grouped fixture".into(),
            authors: Vec::new(),
            file_name: "fixture.pdf".into(),
            path: source.clone(),
            cover_bytes: None,
            added_at: 0,
        };
        let store = SyncStore::open_at(first_database.clone(), settings.device_id.clone()).unwrap();
        let mut uploads = Vec::new();
        let report = runtime
            .block_on(run_sync_with_store(
                settings,
                "secret".into(),
                vec![book],
                store,
                cache.clone(),
                |event| {
                    if let SyncProgress::Stage {
                        stage: SyncStage::Uploading,
                        completed,
                        total,
                    } = event
                    {
                        uploads.push((completed, total));
                    }
                },
            ))
            .unwrap();
        assert_eq!(report.uploaded_books, 1);
        assert_eq!(report.derived_operations, 1);
        let ocr_length = server
            .objects
            .lock()
            .unwrap()
            .iter()
            .find(|(path, _)| path.ends_with("/ocr.zip"))
            .unwrap()
            .1
            .0
            .len() as u64;
        let total = content.len() as u64 + ocr_length;
        assert_grouped_progress(&uploads, content.len() as u64, total);

        // Remove only this test's uniquely identified OCR document to emulate another device.
        fs::remove_file(ocr_directory.join("document.json")).unwrap();
        let settings = test_settings(server.base_url(), "Grouped download");
        let store =
            SyncStore::open_at(second_database.clone(), settings.device_id.clone()).unwrap();
        let mut downloads = Vec::new();
        let report = runtime
            .block_on(run_sync_with_store(
                settings,
                "secret".into(),
                Vec::new(),
                store,
                cache.clone(),
                |event| match event {
                    SyncProgress::Stage {
                        stage: SyncStage::Downloading,
                        completed,
                        total,
                    }
                    | SyncProgress::Downloaded {
                        completed, total, ..
                    } => downloads.push((completed, total)),
                    _ => {}
                },
            ))
            .unwrap();
        assert_eq!(report.downloaded_books, 1);
        assert_eq!(report.derived_operations, 1);
        assert_grouped_progress(&downloads, content.len() as u64, total);
        assert!(
            crate::plugins::export_pdf_ocr_sync_data(&id)
                .unwrap()
                .is_some()
        );
        fs::remove_file(ocr_directory.join("document.json")).unwrap();
        fs::remove_dir(ocr_directory.join("resources")).unwrap();
        fs::remove_dir(ocr_directory).unwrap();
        fs::remove_file(source).unwrap();
        fs::remove_file(partial_download_path(&cache, &id)).unwrap();
        for entry in fs::read_dir(cache.join("derived")).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        fs::remove_dir(cache.join("derived")).unwrap();
        fs::remove_dir(cache).unwrap();
        let _ = fs::remove_file(first_database);
        let _ = fs::remove_file(second_database);
        server.stop();
    }

    fn assert_grouped_progress(updates: &[(u64, u64)], book_bytes: u64, total: u64) {
        assert!(total > book_bytes);
        assert_eq!(updates.first(), Some(&(0, total)));
        assert_eq!(updates.last(), Some(&(total, total)));
        assert!(
            updates
                .iter()
                .all(|&(completed, denominator)| denominator == total && completed <= total)
        );
        assert!(updates.contains(&(book_bytes, total)));
        assert!(updates.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    }

    #[test]
    fn cached_download_does_not_report_completion_before_checksum_validation() {
        let server = FakeWebDav::start();
        let settings = test_settings(server.base_url(), "Checksum test");
        let client = WebDavClient::new(&settings, "secret".into()).unwrap();
        let database = test_database("checksum");
        let store = SyncStore::open_at(database.clone(), settings.device_id).unwrap();
        let cache = test_download_cache("checksum");
        fs::create_dir_all(&cache).unwrap();
        let id = format!("{:x}", Sha256::digest(b"valid"));
        let manifest = BookManifest {
            version: PROTOCOL_VERSION,
            book_id: id.clone(),
            title: "Checksum fixture".into(),
            authors: Vec::new(),
            file_name: "fixture.epub".into(),
            content_path: format!("books/{id}/content.epub"),
            content_sha256: id.clone(),
            content_length: 5,
            cover_path: None,
            added_at: 0,
        };
        server.objects.lock().unwrap().insert(
            format!("/dav/Rebook/v1/books/{id}/manifest.json"),
            (
                serde_json::to_vec(&manifest).unwrap(),
                "\"manifest\"".into(),
            ),
        );
        let cached = partial_download_path(&cache, &id);
        fs::write(&cached, b"wrong").unwrap();
        let mut updates = Vec::new();
        let result = tokio::runtime::Runtime::new().unwrap().block_on(async {
            let manifests = prepare_downloads(
                &client,
                &store,
                &BTreeMap::new(),
                &BTreeSet::from([id]),
                &cache,
                &mut |event| updates.push(event),
            )
            .await?;
            download_books(
                &client,
                manifests,
                &cache,
                5,
                &mut SyncReport::default(),
                &mut |event| updates.push(event),
            )
            .await
        });
        assert!(result.unwrap_err().to_string().contains("内容校验失败"));
        assert!(!cached.exists());
        assert!(updates.iter().any(|event| matches!(
            event,
            SyncProgress::Stage {
                stage: SyncStage::Downloading,
                completed: 4,
                total: 5
            }
        )));
        for event in updates {
            match event {
                SyncProgress::Stage {
                    stage: SyncStage::Downloading,
                    completed,
                    total,
                } => assert!(completed < total),
                SyncProgress::Downloaded { .. } => panic!("corrupt download must not be imported"),
                _ => {}
            }
        }
        drop(store);
        let _ = fs::remove_file(database);
        fs::remove_dir(cache).unwrap();
        server.stop();
    }

    #[test]
    fn two_desktop_devices_exchange_a_content_addressed_book_directly() {
        let server = FakeWebDav::start();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let content = b"direct desktop webdav fixture".to_vec();
        let book_id = format!("{:x}", Sha256::digest(&content));
        let source_path = std::env::temp_dir().join(format!("{book_id}.epub"));
        fs::write(&source_path, &content).unwrap();

        let first_settings = test_settings(server.base_url(), "First desktop");
        let first_database = test_database("first");
        let download_cache = test_download_cache("exchange");
        let local_book = LocalSyncBook {
            id: book_id.clone(),
            title: "Fixture".into(),
            authors: vec!["Rebook".into()],
            file_name: "fixture.epub".into(),
            path: source_path.clone(),
            cover_bytes: None,
            added_at: 42,
        };
        let first_store =
            SyncStore::open_at(first_database.clone(), first_settings.device_id.clone()).unwrap();
        let mut checks = Vec::new();
        let first_report = runtime
            .block_on(run_sync_with_store(
                first_settings.clone(),
                "app-password".into(),
                vec![local_book.clone()],
                first_store,
                download_cache.clone(),
                |event| {
                    if let SyncProgress::Stage {
                        stage,
                        completed,
                        total,
                    } = event
                    {
                        if stage == SyncStage::Checking {
                            checks.push((completed, total));
                        }
                        if stage == SyncStage::Uploading && completed == total {
                            assert!(
                                server
                                    .paths()
                                    .iter()
                                    .any(|path| path.ends_with("/manifest.json"))
                            );
                        }
                    }
                },
            ))
            .unwrap();
        assert_eq!(first_report.uploaded_books, 1);
        assert_eq!(checks, [(0, 1), (1, 1)]);
        let content_etag = server.etag_for_suffix("/content.epub").unwrap();

        let (repeat_report, repeat_stages) = repeat_sync(
            &runtime,
            first_settings,
            &first_database,
            local_book,
            &download_cache,
        );
        assert_eq!(repeat_report.uploaded_books, 0);
        assert!(repeat_stages.contains(&SyncStage::Checking));
        assert!(!repeat_stages.contains(&SyncStage::Uploading));
        assert_eq!(
            server.etag_for_suffix("/content.epub").as_deref(),
            Some(content_etag.as_str())
        );

        let second_settings = test_settings(server.base_url(), "Second desktop");
        let second_database = test_database("second");
        let second_store =
            SyncStore::open_at(second_database.clone(), second_settings.device_id.clone()).unwrap();
        fs::create_dir_all(&download_cache).unwrap();
        fs::write(
            partial_download_path(&download_cache, &book_id),
            &content[..8],
        )
        .unwrap();
        let mut downloaded = Vec::new();
        let mut initial_download_progress = None;
        let second_report = runtime
            .block_on(run_sync_with_store(
                second_settings,
                "app-password".into(),
                Vec::new(),
                second_store,
                download_cache.clone(),
                |progress| {
                    if let SyncProgress::Stage {
                        stage: SyncStage::Downloading,
                        completed,
                        total,
                    } = &progress
                    {
                        assert!(completed < total, "download must still await validation");
                        if initial_download_progress.is_none() {
                            initial_download_progress = Some((*completed, *total));
                        }
                    }
                    if let SyncProgress::Downloaded {
                        book,
                        completed,
                        total,
                        ..
                    } = progress
                    {
                        assert_eq!(completed, total);
                        downloaded.push(*book);
                    }
                },
            ))
            .unwrap();

        assert_eq!(second_report.downloaded_books, 1);
        assert_eq!(downloaded[0].id, book_id);
        assert_eq!(downloaded[0].content, content);
        assert_eq!(initial_download_progress, Some((8, content.len() as u64)));
        assert!(server.range_request_count() > 0);
        assert!(
            server
                .paths()
                .iter()
                .any(|path| path.ends_with("/manifest.json"))
        );

        fs::remove_file(source_path).unwrap();
        let _ = fs::remove_file(first_database);
        let _ = fs::remove_file(second_database);
        let _ = fs::remove_dir_all(download_cache);
        server.stop();
    }

    fn repeat_sync(
        runtime: &tokio::runtime::Runtime,
        settings: SyncSettings,
        database: &Path,
        local_book: LocalSyncBook,
        download_cache: &Path,
    ) -> (SyncReport, Vec<SyncStage>) {
        let store = SyncStore::open_at(database.to_owned(), settings.device_id.clone()).unwrap();
        let mut stages = Vec::new();
        let report = runtime
            .block_on(run_sync_with_store(
                settings,
                "app-password".into(),
                vec![local_book],
                store,
                download_cache.to_owned(),
                |progress| {
                    if let SyncProgress::Stage { stage, .. } = progress {
                        stages.push(stage);
                    }
                },
            ))
            .unwrap();
        (report, stages)
    }

    fn test_settings(base_url: String, device_name: &str) -> SyncSettings {
        SyncSettings {
            enabled: true,
            provider: crate::sync::CloudProviderKind::Custom,
            base_url,
            username: "reader".into(),
            device_id: Uuid::new_v4().to_string(),
            device_name: device_name.into(),
        }
    }

    fn test_database(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rebook-webdav-integration-{name}-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn test_download_cache(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rebook-webdav-downloads-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    pub(crate) struct FakeWebDav {
        address: std::net::SocketAddr,
        running: Arc<AtomicBool>,
        pub(crate) objects: FakeObjects,
        range_requests: Arc<AtomicU64>,
        pub(crate) requests: Arc<Mutex<Vec<(String, String, Option<String>)>>>,
        fail_next_put: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl FakeWebDav {
        pub(crate) fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let running = Arc::new(AtomicBool::new(true));
            let objects = Arc::new(Mutex::new(HashMap::new()));
            let thread_running = Arc::clone(&running);
            let thread_objects = Arc::clone(&objects);
            let range_requests = Arc::new(AtomicU64::new(0));
            let thread_range_requests = Arc::clone(&range_requests);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let thread_requests = Arc::clone(&requests);
            let fail_next_put = Arc::new(AtomicBool::new(false));
            let thread_fail = Arc::clone(&fail_next_put);
            let handle = thread::spawn(move || {
                while thread_running.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            handle_webdav_request(
                                stream,
                                &thread_objects,
                                &thread_range_requests,
                                &thread_requests,
                                &thread_fail,
                            );
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fake WebDAV accept failed: {error}"),
                    }
                }
            });
            Self {
                address,
                running,
                objects,
                range_requests,
                requests,
                fail_next_put,
                handle: Some(handle),
            }
        }

        pub(crate) fn base_url(&self) -> String {
            format!("http://{}/dav", self.address)
        }

        fn paths(&self) -> Vec<String> {
            self.objects.lock().unwrap().keys().cloned().collect()
        }

        fn etag_for_suffix(&self, suffix: &str) -> Option<String> {
            self.objects
                .lock()
                .unwrap()
                .iter()
                .find(|(path, _)| path.ends_with(suffix))
                .map(|(_, (_, etag))| etag.clone())
        }

        fn range_request_count(&self) -> u64 {
            self.range_requests.load(Ordering::Relaxed)
        }

        pub(crate) fn stop(mut self) {
            self.running.store(false, Ordering::Release);
            self.handle.take().unwrap().join().unwrap();
        }
    }

    fn handle_webdav_request(
        mut stream: TcpStream,
        objects: &FakeObjects,
        range_requests: &AtomicU64,
        requests: &Mutex<Vec<(String, String, Option<String>)>>,
        fail_next_put: &AtomicBool,
    ) {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 8_192];
        let header_end = loop {
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&chunk[..count]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8(request[..header_end].to_vec()).unwrap();
        let mut lines = headers.lines();
        let request_line = lines.next().unwrap();
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap();
        let path = request_parts.next().unwrap().to_owned();
        let header_map = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect::<HashMap<_, _>>();
        let content_length = header_map
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_default();
        while request.len() < header_end + content_length {
            let count = stream.read(&mut chunk).unwrap();
            request.extend_from_slice(&chunk[..count]);
        }
        let body = request[header_end..header_end + content_length].to_vec();
        requests.lock().unwrap().push((
            method.to_owned(),
            path.clone(),
            header_map.get("if-none-match").cloned(),
        ));
        if method == "PUT" && fail_next_put.swap(false, Ordering::Relaxed) {
            write_response(&mut stream, 500, &[], None);
            return;
        }

        match method {
            "MKCOL" => write_response(&mut stream, 201, &[], None),
            "GET" => {
                let object = objects.lock().unwrap().get(&path).cloned();
                if let Some((bytes, etag)) = object {
                    if header_map.get("if-none-match") == Some(&etag) {
                        write_response(&mut stream, 304, &[], Some(&etag));
                        return;
                    }
                    if let Some(start) = header_map
                        .get("range")
                        .and_then(|value| value.strip_prefix("bytes="))
                        .and_then(|value| value.strip_suffix('-'))
                        .and_then(|value| value.parse::<usize>().ok())
                        .filter(|start| *start < bytes.len())
                    {
                        range_requests.fetch_add(1, Ordering::Relaxed);
                        write_partial_response(&mut stream, &bytes, start, &etag);
                    } else {
                        write_response(&mut stream, 200, &bytes, Some(&etag));
                    }
                } else {
                    write_response(&mut stream, 404, &[], None);
                }
            }
            "PUT" => {
                static NEXT_ETAG: AtomicU64 = AtomicU64::new(1);
                let mut objects = objects.lock().unwrap();
                let existing = objects.get(&path).map(|(_, etag)| etag.clone());
                let precondition_failed = header_map
                    .get("if-none-match")
                    .is_some_and(|value| value == "*" && existing.is_some())
                    || header_map
                        .get("if-match")
                        .is_some_and(|value| existing.as_ref() != Some(value));
                if precondition_failed {
                    write_response(&mut stream, 412, &[], existing.as_deref());
                } else {
                    let etag = format!("\"{}\"", NEXT_ETAG.fetch_add(1, Ordering::Relaxed));
                    objects.insert(path, (body, etag.clone()));
                    write_response(&mut stream, 201, &[], Some(&etag));
                }
            }
            "PROPFIND" => {
                let objects = objects.lock().unwrap();
                let mut hrefs = objects
                    .keys()
                    .filter(|candidate| {
                        candidate.starts_with(&path)
                            && candidate[path.len()..].trim_matches('/').split('/').count() == 1
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                hrefs.sort();
                let mut responses = String::new();
                for href in hrefs {
                    responses.push_str("<d:response><d:href>");
                    responses.push_str(&href);
                    responses.push_str("</d:href></d:response>");
                }
                let xml = format!(
                    "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">{responses}</d:multistatus>"
                );
                write_response(&mut stream, 207, xml.as_bytes(), None);
            }
            _ => write_response(&mut stream, 405, &[], None),
        }
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &[u8], etag: Option<&str>) {
        let reason = match status {
            200 => "OK",
            206 => "Partial Content",
            201 => "Created",
            207 => "Multi-Status",
            404 => "Not Found",
            405 => "Method Not Allowed",
            412 => "Precondition Failed",
            _ => "Error",
        };
        let etag = etag.map_or_else(String::new, |value| format!("ETag: {value}\r\n"));
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n{etag}Connection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }

    fn write_partial_response(stream: &mut TcpStream, body: &[u8], start: usize, etag: &str) {
        let partial = &body[start..];
        let end = body.len() - 1;
        let header = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
            partial.len(),
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(partial).unwrap();
    }
}
