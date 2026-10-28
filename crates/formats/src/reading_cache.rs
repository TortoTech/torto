//! Disposable source IR, before reader settings, semantic overlays and translation.
//! The binary envelope contains bounded, compressed named-field JSON: the reading
//! model uses optional/skipped fields, so positional bincode would lose fidelity.
//! One bounded writer handles all books. No whole-book parsing or raster caching.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use rebook_publication::{
    Block, Book, BookSource, Inline, PublicationError, PublicationUrl, RenditionLayout, Resource,
    Section, TableOfContentsOrigin, TextBlock,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{BookFormat, FormatError, OpenedPublication, epub::EpubPublication};

#[path = "reading_cache_image_dimensions.rs"]
mod image_dimensions;
use image_dimensions::ImageDimensions;

// Bump when source parsing, structural heuristics, or the stored model changes.
// Release versions additionally isolate caches from older executables.
const REVISION: u32 = 12;
const MAGIC: &[u8; 8] = b"TRDIR\x02\0\0";
const HEADER_BYTES: usize = 84;
const MAX_ENTRY_BYTES: usize = 32 * 1024 * 1024;
const MAX_QUEUED_SECTION_WEIGHT: usize = 8 * 1024 * 1024;
const DISK_BUDGET: u64 = 256 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Index {
    book: Book,
    origin: TableOfContentsOrigin,
    revision: Option<u64>,
    epub: Option<crate::epub::ReadingIndex>,
}

pub(super) fn open(
    path: &Path,
    known_id: Option<&str>,
    directory: &Path,
) -> Result<OpenedPublication, FormatError> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let Some(format) = BookFormat::from_file_name(name) else {
        return crate::open_file_for_reading(path, known_id);
    };
    if matches!(format, BookFormat::Pdf | BookFormat::Cbz | BookFormat::Chm) {
        return crate::open_file_for_reading(path, known_id);
    }
    // Validate against the actual bytes, not just size/mtime or the shelf ID.
    // The same allocation is shared with the original lazy resource source.
    let bytes: Arc<[u8]> = fs::read(path)?.into();
    let digest = *blake3::hash(&bytes).as_bytes();
    let root = directory.join(version_directory());
    // Keep formats and names distinct: some parsers use the file name as a
    // fallback title, even when the source bytes and publication ID are equal.
    let entry = root.join(format!(
        "{}-{}",
        format.storage_extension(),
        entry_key(&digest, name)
    ));
    let index_path = entry.join("index.bin");
    let cached = read::<Index>(&index_path, &digest).ok().filter(|index| {
        !index.book.sections.is_empty()
            && index.book.sections.len() <= 10_000
            && index.book.metadata.layout == RenditionLayout::Reflowable
            && known_id.is_none_or(|id| index.book.id.as_str() == id)
            && match (&index.epub, format) {
                (Some(epub), BookFormat::Epub) => epub.book == index.book,
                (None, BookFormat::Epub) | (Some(_), _) => false,
                (None, _) => true,
            }
    });
    let original = OnceLock::new();
    let index = if let Some(index) = cached {
        index
    } else {
        let (source, epub) = if format == BookFormat::Epub {
            let source = EpubPublication::open_bytes_with_id(
                Arc::clone(&bytes),
                // Publication IDs remain SHA-256 for existing data and sync.
                // Only a cache miss needs to derive this persistent identity.
                rebook_publication::PublicationId::new(hex_digest(&Sha256::digest(&bytes).into()))?,
            )
            .map_err(|error| FormatError::Epub(error.to_string()))?;
            let index = source.reading_index();
            (Arc::new(source) as Arc<dyn BookSource>, Some(index))
        } else {
            (
                crate::open_shared_source(Arc::clone(&bytes), name, format)?,
                None,
            )
        };
        if source.book().metadata.layout != RenditionLayout::Reflowable {
            return Ok(crate::finish_open(format, source, false));
        }
        let index = Index {
            book: source.book().clone(),
            origin: source.table_of_contents_origin(),
            revision: source.content_revision(),
            epub,
        };
        enqueue(index_path, directory.to_owned(), digest, || {
            Payload::Index(Box::new(index.clone()))
        });
        let _ = original.set(Ok(source));
        index
    };
    let lazy_bytes = original.get().is_none().then_some(bytes);
    let image_dimensions = ImageDimensions::open(&entry, directory, digest);
    let source = Arc::new(CachedSource {
        index,
        original,
        bytes: Mutex::new(lazy_bytes),
        name: name.to_owned(),
        format,
        entry,
        root: directory.to_owned(),
        digest,
        image_dimensions,
        section_fingerprints: Mutex::new(HashMap::new()),
    });
    Ok(crate::finish_open(format, source, false))
}

fn hex_digest(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut value = String::with_capacity(64);
    for byte in digest {
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn version_directory() -> String {
    format!(
        "v{REVISION}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        env!("TORTO_SOURCE_IR_BUILD")
    )
}

fn entry_key(digest: &[u8; 32], name: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(digest);
    hash.update(name.as_bytes());
    hex_digest(&hash.finalize().into())
}

struct CachedSource {
    index: Index,
    original: OnceLock<Result<Arc<dyn BookSource>, String>>,
    bytes: Mutex<Option<Arc<[u8]>>>,
    name: String,
    format: BookFormat,
    entry: PathBuf,
    root: PathBuf,
    digest: [u8; 32],
    image_dimensions: Arc<ImageDimensions>,
    section_fingerprints: Mutex<HashMap<usize, [u8; 32]>>,
}

impl CachedSource {
    fn original(&self) -> Result<&Arc<dyn BookSource>, PublicationError> {
        self.original
            .get_or_init(|| {
                let bytes = self
                    .bytes
                    .lock()
                    .map_err(|_| "unavailable reading source")?
                    .take()
                    .ok_or("unavailable reading source")?;
                if let Some(index) = &self.index.epub {
                    EpubPublication::from_reading_index(bytes, index.clone())
                        .map(|source| Arc::new(source) as Arc<dyn BookSource>)
                        .map_err(|e| e.to_string())
                } else {
                    crate::open_shared_source(bytes, &self.name, self.format)
                        .map_err(|e| e.to_string())
                }
            })
            .as_ref()
            .map_err(|e| PublicationError::InvalidPublication(e.clone()))
    }
}

impl BookSource for CachedSource {
    fn book(&self) -> &Book {
        &self.index.book
    }
    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.index.origin
    }
    fn content_revision(&self) -> Option<u64> {
        self.index.revision
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let descriptor = self.index.book.sections.get(index).ok_or_else(|| {
            PublicationError::InvalidPublication("section index out of bounds".into())
        })?;
        let path = self.entry.join(format!("section-{index}.bin"));
        if let Ok((section, fingerprint)) = read_entry::<Section>(&path, &self.digest)
            && section.id == descriptor.id
            && section.href == descriptor.href
        {
            let mut fingerprints = self.section_fingerprints.lock().unwrap_or_else(|e| e.into_inner());
            if fingerprints.len() >= 16 && !fingerprints.contains_key(&index)
                && let Some(oldest) = fingerprints.keys().next().copied() {
                fingerprints.remove(&oldest);
            }
            fingerprints.insert(index, fingerprint);
            return Ok(section);
        }
        self.section_fingerprints.lock().unwrap_or_else(|e| e.into_inner()).remove(&index);
        let section = self.original()?.parse_section(index)?;
        if section_weight(&section) <= MAX_QUEUED_SECTION_WEIGHT {
            enqueue(path, self.root.clone(), self.digest, || {
                Payload::Section(Box::new(section.clone()))
            });
        }
        Ok(section)
    }

    fn cached_section_fingerprint(&self, index: usize) -> Option<[u8; 32]> {
        self.section_fingerprints.lock().ok()?.get(&index).copied()
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        let resource = self.original()?.resource(href)?;
        if resource.media_type.starts_with("image/") {
            self.image_dimensions.record(href, &resource.bytes);
        }
        Ok(resource)
    }
    fn cover_section(&self) -> Result<Option<Section>, PublicationError> {
        self.original()?.cover_section()
    }

    fn image_resource_dimensions(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<[u32; 2]>, PublicationError> {
        if let Some(dimensions) = self.image_dimensions.get(href) {
            return Ok(dimensions);
        }
        let resource = self.original()?.resource(href)?;
        Ok(self.image_dimensions.record(href, &resource.bytes))
    }
}

// Conservative queue weight: count strings and reserve ample structural overhead.
// ReaderSession continues to own the existing bounded in-memory section cache.
fn section_weight(section: &Section) -> usize {
    fn text(text: &TextBlock) -> usize {
        text.content.iter().fold(512usize, |size, inline| {
            size.saturating_add(
                256 + match inline {
                    Inline::Text(run) => {
                        run.text.len()
                            + run.link.as_ref().map_or(0, |url| {
                                url.path().len() + url.fragment().map_or(0, str::len)
                            })
                    }
                    Inline::Image(image) => image.image.alt.len(),
                    Inline::Math(formula) => formula.latex.len(),
                    Inline::Ruby(ruby) => ruby
                        .base
                        .iter()
                        .chain(ruby.annotation.iter())
                        .map(|run| run.text.len() + 256)
                        .sum(),
                    Inline::Break => 0,
                },
            )
        })
    }
    fn block(value: &Block) -> usize {
        let content = match value {
            Block::Text(t) => text(t),
            Block::Quote(q) => q.body.iter().chain(q.attribution.iter()).map(text).sum(),
            Block::Table(t) => t.text_blocks().map(text).sum(),
            Block::Figure(f) => f.captions.iter().map(text).sum::<usize>() + f.body.iter().map(block).sum::<usize>() + f.images.len() * 512,
            Block::Note(n) => n.blocks.iter().map(block).sum(),
            Block::Separator(s) => s.text.as_ref().map_or(0, text),
            _ => 0,
        };
        512usize.saturating_add(content)
    }
    section
        .blocks
        .iter()
        .map(block)
        .sum::<usize>()
        .saturating_add(section.anchors.len().saturating_mul(256))
}

enum Payload {
    Index(Box<Index>),
    Section(Box<Section>),
    ImageDimensions(Arc<ImageDimensions>),
}
struct Job {
    path: PathBuf,
    root: PathBuf,
    digest: [u8; 32],
    payload: Payload,
}
struct Writer {
    sender: mpsc::SyncSender<Job>,
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}
static WRITER: OnceLock<Option<Writer>> = OnceLock::new();

fn enqueue(path: PathBuf, root: PathBuf, digest: [u8; 32], payload: impl FnOnce() -> Payload) {
    let Some(writer) = WRITER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Job>(2);
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let active = Arc::clone(&pending);
        std::thread::Builder::new()
            .name("reading-ir-cache".into())
            .spawn(move || {
                for job in receiver {
                    let result = match &job.payload {
                        Payload::Index(index) => write(&job.path, &job.digest, index),
                        Payload::Section(section) => write(&job.path, &job.digest, section),
                        Payload::ImageDimensions(dimensions) => dimensions.persist(),
                    };
                    if result.is_ok() {
                        prune(&job.root);
                    }
                    if let Ok(mut pending) = active.lock() {
                        pending.remove(&job.path);
                    }
                    if result.is_ok()
                        && let Payload::ImageDimensions(dimensions) = &job.payload
                    {
                        dimensions.schedule_if_dirty();
                    }
                }
            })
            .ok()?;
        Some(Writer { sender, pending })
    }) else {
        return;
    };
    let Ok(mut pending) = writer.pending.lock() else {
        return;
    };
    if pending.len() >= 3 || !pending.insert(path.clone()) {
        return;
    }
    drop(pending);
    if writer
        .sender
        .try_send(Job {
            path: path.clone(),
            root,
            digest,
            payload: payload(),
        })
        .is_err()
        && let Ok(mut pending) = writer.pending.lock()
    {
        pending.remove(&path);
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid reading IR cache")
}

fn read<T: DeserializeOwned>(path: &Path, digest: &[u8; 32]) -> io::Result<T> {
    read_entry(path, digest).map(|(value, _)| value)
}

fn read_entry<T: DeserializeOwned>(path: &Path, digest: &[u8; 32]) -> io::Result<(T, [u8; 32])> {
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > (MAX_ENTRY_BYTES + HEADER_BYTES) as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take((MAX_ENTRY_BYTES + HEADER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() < HEADER_BYTES
        || bytes.len() > MAX_ENTRY_BYTES + HEADER_BYTES
        || &bytes[..8] != MAGIC
        || &bytes[8..40] != digest
    {
        return Err(invalid());
    }
    let checksum = u32::from_le_bytes(bytes[40..44].try_into().unwrap());
    let length = u64::from_le_bytes(bytes[44..52].try_into().unwrap());
    if length > MAX_ENTRY_BYTES as u64 || crc32fast::hash(&bytes[44..]) != checksum {
        return Err(invalid());
    }
    let mut json = Vec::new();
    let mut decoder = ZlibDecoder::new(&bytes[HEADER_BYTES..]);
    (&mut decoder).take(length + 1).read_to_end(&mut json)?;
    if json.len() as u64 != length || decoder.total_in() != (bytes.len() - HEADER_BYTES) as u64 {
        return Err(invalid());
    }
    serde_json::from_slice(&json).map(|value| (value, bytes[52..84].try_into().unwrap())).map_err(|_| invalid())
}

struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_ENTRY_BYTES {
            return Err(invalid());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write(path: &Path, digest: &[u8; 32], value: &impl Serialize) -> io::Result<()> {
    let mut json = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut json, value).map_err(io::Error::other)?;
    let mut encoder = ZlibEncoder::new(BoundedBytes(Vec::new()), Compression::fast());
    encoder.write_all(&json.0)?;
    let compressed = encoder.finish()?.0;
    let length = (json.0.len() as u64).to_le_bytes();
    // Preserve existing AI recognition keys. Computing this on the writer
    // avoids serializing and hashing large source sections again on opening.
    let fingerprint: [u8; 32] = Sha256::digest(&json.0).into();
    let mut checksum = crc32fast::Hasher::new();
    checksum.update(&length);
    checksum.update(&fingerprint);
    checksum.update(&compressed);
    let checksum = checksum.finalize();
    let parent = path.parent().ok_or_else(invalid)?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(MAGIC)?;
        file.write_all(digest)?;
        file.write_all(&checksum.to_le_bytes())?;
        file.write_all(&length)?;
        file.write_all(&fingerprint)?;
        file.write_all(&compressed)?;
        file.flush()?;
        drop(file);
        // Same-directory replacement publishes a complete entry atomically.
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

// Runs only on the writer. All cache versions share a finite disk budget;
// eviction never affects source books and a removed chapter is rebuilt lazily.
fn prune(root: &Path) {
    prune_with_budget(root, DISK_BUDGET);
}

fn prune_with_budget(root: &Path, budget: u64) {
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut directories = vec![(root.to_owned(), 0)];
    while let Some((directory, depth)) = directories.pop() {
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < 2 {
                directories.push((entry.path(), depth + 1));
                continue;
            }
            if !kind.is_file() || entry.path().extension().is_none_or(|s| s != "bin") {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            total = total.saturating_add(metadata.len());
            files.push((metadata.modified().ok(), entry.path(), metadata.len()));
        }
    }
    if total <= budget {
        return;
    }
    files.sort_unstable_by_key(|file| file.0);
    for (_, path, size) in files {
        if total <= budget {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
#[path = "reading_cache_tests.rs"]
mod tests;
