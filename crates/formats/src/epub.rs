//! Safe, pull-based EPUB publication parser.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};

use crc32fast::hash as crc32;
use flate2::read::DeflateDecoder;
use image::ImageReader;
use quick_xml::Reader;
use quick_xml::events::Event;
use rebook_html::{SectionParseHints, parse_section_with_hints_and_image_classifier};
use rebook_publication::{
    Book, BookSource, Metadata, PublicationError, PublicationId, PublicationUrl, RenditionLayout,
    Resource, Section, SpineItem, SpineItemId, TocEntry, promote_single_toc_root,
};
use roxmltree::{Document, Node};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zip::{CompressionMethod, ZipArchive};

use crate::source::{TocHeadingHint, collect_toc_heading_hints, promote_toc_headings};

/// Resource budgets applied before and during decompression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EpubLimits {
    /// Maximum compressed EPUB file size.
    archive_bytes: u64,
    /// Maximum number of non-directory archive entries.
    entries: usize,
    /// Maximum uncompressed size of one entry.
    entry_bytes: u64,
    /// Maximum declared uncompressed size across all entries.
    total_uncompressed_bytes: u64,
    /// Maximum uncompressed/compressed ratio for a non-empty entry.
    compression_ratio: u64,
    /// Maximum bytes accepted for one XML document.
    xml_bytes: u64,
    /// Maximum XML element nesting depth.
    xml_depth: usize,
}

impl Default for EpubLimits {
    fn default() -> Self {
        Self {
            archive_bytes: 512 * 1024 * 1024,
            entries: 10_000,
            entry_bytes: 64 * 1024 * 1024,
            total_uncompressed_bytes: 1024 * 1024 * 1024,
            compression_ratio: 200,
            xml_bytes: 8 * 1024 * 1024,
            xml_depth: 128,
        }
    }
}

/// Parser behavior for spec violations commonly found in real publications.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EpubOpenOptions {
    /// Resource budgets.
    limits: EpubLimits,
    /// When true, missing or misplaced `mimetype` is an error instead of a warning.
    strict_container: bool,
}

/// Parsed EPUB backed by an immutable in-memory archive and lazy resource reads.
#[derive(Debug)]
pub(super) struct EpubPublication {
    book: Book,
    media_types: HashMap<String, String>,
    archive: EpubArchive,
    decorative_separator_images: Mutex<HashMap<String, bool>>,
    note_section_paths: HashSet<String>,
    toc_heading_hints: HashMap<String, Vec<TocHeadingHint>>,
}

impl EpubPublication {
    /// Opens an EPUB from immutable bytes using default limits.
    pub(super) fn open_bytes(bytes: impl Into<Arc<[u8]>>) -> Result<Self, EpubError> {
        Self::open_bytes_with_options(bytes, EpubOpenOptions::default())
    }

    /// Opens an EPUB from immutable bytes with explicit limits and compatibility behavior.
    fn open_bytes_with_options(
        bytes: impl Into<Arc<[u8]>>,
        options: EpubOpenOptions,
    ) -> Result<Self, EpubError> {
        let bytes = bytes.into();
        ensure_limit(
            u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= options.limits.archive_bytes,
            "archive exceeds configured byte limit",
        )?;
        let archive = EpubArchive::new(bytes.clone(), options.limits)?;
        validate_mimetype(&archive, options.strict_container)?;

        let container_url = PublicationUrl::parse("META-INF/container.xml")?;
        let container = archive.read_xml(&container_url)?;
        let rootfile_path = parse_container(&container)?;
        let package_url = PublicationUrl::parse(&rootfile_path)?.resource_url();
        let package = archive.read_xml(&package_url)?;
        let package_model = parse_package(&package, &package_url)?;

        let mut media_types = HashMap::new();
        for item in package_model
            .manifest_order
            .iter()
            .filter_map(|id| package_model.manifest.get(id))
        {
            media_types.insert(item.href.path().to_owned(), item.media_type.clone());
        }

        let mut reading_order = build_reading_order(&package_model)?;
        let table_of_contents =
            promote_single_toc_root(parse_navigation(&archive, &package_model)?);
        let note_section_paths = collect_note_section_paths(&table_of_contents);
        for section in &mut reading_order {
            if note_section_paths.contains(section.href.path()) && !section.is_note_section() {
                section
                    .properties
                    .push(rebook_publication::NOTE_SECTION_PROPERTY.to_owned());
            }
        }
        let toc_heading_hints = collect_toc_heading_hints(&table_of_contents);
        let digest = Sha256::digest(bytes.as_ref());
        let id = PublicationId::new(format!("{digest:x}"))?;

        Ok(Self {
            book: Book {
                id,
                metadata: package_model.metadata,
                cover: package_model.cover,
                sections: reading_order,
                table_of_contents,
            },
            media_types,
            archive,
            decorative_separator_images: Mutex::new(HashMap::new()),
            note_section_paths,
            toc_heading_hints,
        })
    }
}

fn collect_note_section_paths(entries: &[TocEntry]) -> HashSet<String> {
    fn visit(entries: &[TocEntry], classifications: &mut HashMap<String, (bool, bool)>) {
        for entry in entries {
            if let Some(href) = &entry.href {
                let classification = classifications.entry(href.path().to_owned()).or_default();
                if is_note_navigation_label(&entry.label) {
                    classification.0 = true;
                } else {
                    classification.1 = true;
                }
            }
            visit(&entry.children, classifications);
        }
    }

    let mut classifications = HashMap::new();
    visit(entries, &mut classifications);
    classifications
        .into_iter()
        .filter_map(|(path, (is_notes, has_non_note_entry))| {
            (is_notes && !has_non_note_entry).then_some(path)
        })
        .collect()
}

fn is_note_navigation_label(label: &str) -> bool {
    let label = label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches([':', '：'])
        .to_owned();
    matches!(
        label.to_ascii_lowercase().as_str(),
        "note"
            | "notes"
            | "endnote"
            | "endnotes"
            | "footnote"
            | "footnotes"
            | "注释"
            | "注解"
            | "尾注"
            | "本章注"
            | "章节注释"
            | "作者附注"
    )
}

impl BookSource for EpubPublication {
    fn book(&self) -> &Book {
        &self.book
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        self.parse_section_ir(index)
            .map_err(EpubError::into_publication_error)
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        let href = href.resource_url();
        let bytes = self
            .archive
            .read(&href)
            .map_err(EpubError::into_publication_error)?;
        let media_type = self
            .media_types
            .get(href.path())
            .cloned()
            .unwrap_or_else(|| guess_media_type(href.path()).to_owned());
        Ok(Resource {
            href,
            media_type,
            bytes: bytes.into(),
        })
    }
}

impl EpubPublication {
    fn parse_section_ir(&self, index: usize) -> Result<Section, EpubError> {
        let descriptor = self.book.sections.get(index).ok_or_else(|| {
            EpubError::InvalidArchive(format!("section index out of bounds: {index}"))
        })?;
        if descriptor.media_type != "application/xhtml+xml" && descriptor.media_type != "text/html"
        {
            return Err(EpubError::Unsupported(format!(
                "reflowable section media type: {}",
                descriptor.media_type
            )));
        }

        let xml = self.archive.read_xml(&descriptor.href)?;
        let mut section = parse_section_with_hints_and_image_classifier(
            &xml,
            descriptor,
            |href| self.archive.read_stylesheet(href).ok(),
            |href| self.is_decorative_separator_image(href),
            SectionParseHints {
                note_section: self.note_section_paths.contains(descriptor.href.path()),
            },
        )?;
        if let Some(hints) = self.toc_heading_hints.get(descriptor.href.path()) {
            promote_toc_headings(&mut section, hints);
        }
        Ok(section)
    }

    fn is_decorative_separator_image(&self, href: &PublicationUrl) -> bool {
        if let Some(classified) = self
            .decorative_separator_images
            .lock()
            .ok()
            .and_then(|cache| cache.get(href.path()).copied())
        {
            return classified;
        }

        let classified = self
            .archive
            .read(href)
            .ok()
            .and_then(|bytes| {
                ImageReader::new(Cursor::new(bytes))
                    .with_guessed_format()
                    .ok()?
                    .into_dimensions()
                    .ok()
            })
            .is_some_and(|(width, height)| {
                (1..=8).contains(&height)
                    && (32..=512).contains(&width)
                    && width >= height.saturating_mul(8)
            });
        if let Ok(mut cache) = self.decorative_separator_images.lock() {
            cache.insert(href.path().to_owned(), classified);
        }
        classified
    }
}

#[derive(Debug)]
struct EpubArchive {
    bytes: Arc<[u8]>,
    entries: HashMap<String, ArchiveEntry>,
    limits: EpubLimits,
}

#[derive(Debug, Clone)]
struct ArchiveEntry {
    location: ArchiveLocation,
    size: u64,
    compressed_size: u64,
    stored: bool,
    crc32: u32,
    ordinal: usize,
}

#[derive(Debug, Clone, Copy)]
enum ArchiveLocation {
    ZipIndex(usize),
    LocalHeader { data_start: usize, method: u16 },
}

#[derive(Debug, Clone)]
struct CentralDirectoryEntry {
    compressed_size: u64,
    size: u64,
    crc32: u32,
    method: u16,
    encrypted: bool,
    symbolic_link: bool,
}

const ZIP_SIGNATURE_SIZE: usize = 4;
const ZIP_LOCAL_HEADER_SIZE: usize = 30;
const ZIP_CENTRAL_HEADER_SIZE: usize = 46;
const ZIP_EOCD_SIZE: usize = 22;
const ZIP_MAX_COMMENT_SIZE: usize = u16::MAX as usize;
const ZIP_LOCAL_HEADER_SIGNATURE: [u8; ZIP_SIGNATURE_SIZE] = [0x50, 0x4b, 0x03, 0x04];
const ZIP_CENTRAL_HEADER_SIGNATURE: [u8; ZIP_SIGNATURE_SIZE] = [0x50, 0x4b, 0x01, 0x02];
const ZIP_EOCD_SIGNATURE: [u8; ZIP_SIGNATURE_SIZE] = [0x50, 0x4b, 0x05, 0x06];
const ZIP_METHOD_STORED: u16 = 0;
const ZIP_METHOD_DEFLATED: u16 = 8;

struct LocalHeader<'a> {
    name_bytes: &'a [u8],
    data_start: usize,
    compressed_size: u64,
    size: u64,
    crc32: u32,
    method: u16,
    encrypted: bool,
}

impl EpubArchive {
    fn new(bytes: Arc<[u8]>, limits: EpubLimits) -> Result<Self, EpubError> {
        match Self::from_zip_archive(Arc::clone(&bytes), limits) {
            Ok(archive) => Ok(archive),
            Err(EpubError::Zip(_)) => Self::from_local_headers(bytes, limits),
            Err(error) => Err(error),
        }
    }

    fn from_zip_archive(bytes: Arc<[u8]>, limits: EpubLimits) -> Result<Self, EpubError> {
        let mut archive = ZipArchive::new(Cursor::new(bytes.clone()))?;
        ensure_limit(
            archive.len() <= limits.entries,
            format!(
                "archive contains {} entries; limit is {}",
                archive.len(),
                limits.entries
            ),
        )?;

        let mut entries = HashMap::with_capacity(archive.len());
        let mut total_uncompressed = 0_u64;
        for index in 0..archive.len() {
            let file = archive.by_index(index)?;
            if file.is_dir() {
                continue;
            }
            if file.encrypted() {
                return Err(EpubError::Unsupported(
                    "encrypted ZIP entries are not supported".into(),
                ));
            }
            if file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
            {
                return Err(EpubError::InvalidArchive(format!(
                    "symbolic-link ZIP entry is not allowed: {}",
                    file.name()
                )));
            }

            let href = PublicationUrl::parse(file.name())?.resource_url();
            ensure_limit(
                file.size() <= limits.entry_bytes,
                format!(
                    "entry {} declares {} uncompressed bytes; per-entry limit is {}",
                    href,
                    file.size(),
                    limits.entry_bytes
                ),
            )?;
            ensure_compression_ratio(file.size(), file.compressed_size(), limits, &href)?;
            total_uncompressed = total_uncompressed
                .checked_add(file.size())
                .ok_or_else(|| EpubError::ResourceLimit("uncompressed size overflow".into()))?;
            ensure_limit(
                total_uncompressed <= limits.total_uncompressed_bytes,
                format!(
                    "archive declares {total_uncompressed} uncompressed bytes; total limit is {}",
                    limits.total_uncompressed_bytes
                ),
            )?;

            let entry = ArchiveEntry {
                location: ArchiveLocation::ZipIndex(index),
                size: file.size(),
                compressed_size: file.compressed_size(),
                stored: file.compression() == CompressionMethod::Stored,
                crc32: file.crc32(),
                ordinal: index,
            };
            if entries.insert(href.path().to_owned(), entry).is_some() {
                return Err(EpubError::InvalidArchive(format!(
                    "duplicate canonical ZIP entry: {href}"
                )));
            }
        }
        Ok(Self {
            bytes,
            entries,
            limits,
        })
    }

    fn from_local_headers(bytes: Arc<[u8]>, limits: EpubLimits) -> Result<Self, EpubError> {
        let central_entries = parse_central_directory(&bytes);
        let mut entries = HashMap::new();
        let mut total_uncompressed = 0_u64;
        let mut physical_entry_count = 0_usize;
        let mut scan_start = 0_usize;

        while scan_start + ZIP_LOCAL_HEADER_SIZE <= bytes.len() {
            let Some(relative_offset) = bytes[scan_start..]
                .windows(ZIP_SIGNATURE_SIZE)
                .position(|window| window == ZIP_LOCAL_HEADER_SIGNATURE)
            else {
                break;
            };
            let header_offset = scan_start + relative_offset;
            scan_start = header_offset.saturating_add(ZIP_SIGNATURE_SIZE);
            let Some((path, recovered)) = recover_local_entry(
                &bytes,
                header_offset,
                &central_entries,
                limits,
                physical_entry_count,
            )?
            else {
                continue;
            };
            total_uncompressed = total_uncompressed
                .checked_add(recovered.size)
                .ok_or_else(|| EpubError::ResourceLimit("uncompressed size overflow".into()))?;
            ensure_limit(
                total_uncompressed <= limits.total_uncompressed_bytes,
                format!(
                    "archive declares {total_uncompressed} uncompressed bytes; total limit is {}",
                    limits.total_uncompressed_bytes
                ),
            )?;
            physical_entry_count = physical_entry_count.saturating_add(1);
            ensure_limit(
                physical_entry_count <= limits.entries,
                format!(
                    "archive contains more than {} recoverable entries",
                    limits.entries
                ),
            )?;
            entries.insert(path, recovered);
        }

        if entries.is_empty() {
            return Err(EpubError::InvalidArchive(
                "archive has no recoverable local file headers".into(),
            ));
        }
        Ok(Self {
            bytes,
            entries,
            limits,
        })
    }

    fn read(&self, href: &PublicationUrl) -> Result<Vec<u8>, EpubError> {
        let entry = self
            .entries
            .get(href.path())
            .ok_or_else(|| EpubError::ResourceNotFound(href.to_string()))?;
        ensure_limit(
            entry.size <= self.limits.entry_bytes,
            format!("resource exceeded size budget: {href}"),
        )?;
        ensure_compression_ratio(entry.size, entry.compressed_size, self.limits, href)?;

        let bytes = match entry.location {
            ArchiveLocation::ZipIndex(index) => {
                let mut archive = ZipArchive::new(Cursor::new(self.bytes.clone()))?;
                let mut file = archive.by_index(index)?;
                read_bounded(&mut file, entry.size, self.limits.entry_bytes, href)?
            }
            ArchiveLocation::LocalHeader { data_start, method } => {
                let compressed_size = usize::try_from(entry.compressed_size).map_err(|_| {
                    EpubError::ResourceLimit(format!(
                        "compressed resource size cannot be represented: {href}"
                    ))
                })?;
                let data_end = data_start.checked_add(compressed_size).ok_or_else(|| {
                    EpubError::InvalidArchive(format!(
                        "compressed resource range overflowed: {href}"
                    ))
                })?;
                let compressed = self.bytes.get(data_start..data_end).ok_or_else(|| {
                    EpubError::InvalidArchive(format!(
                        "compressed resource range is outside archive: {href}"
                    ))
                })?;
                if method == ZIP_METHOD_STORED {
                    read_bounded(
                        &mut Cursor::new(compressed),
                        entry.size,
                        self.limits.entry_bytes,
                        href,
                    )?
                } else {
                    read_bounded(
                        &mut DeflateDecoder::new(compressed),
                        entry.size,
                        self.limits.entry_bytes,
                        href,
                    )?
                }
            }
        };
        ensure_limit(
            u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= self.limits.entry_bytes,
            format!("resource expanded beyond size budget: {href}"),
        )?;
        ensure_limit(
            u64::try_from(bytes.len()).unwrap_or(u64::MAX) == entry.size,
            format!(
                "resource size disagrees with ZIP metadata for {href}: expected {}, decoded {}",
                entry.size,
                bytes.len()
            ),
        )?;
        if crc32(&bytes) != entry.crc32 {
            return Err(EpubError::InvalidArchive(format!(
                "resource CRC32 check failed: {href}"
            )));
        }
        Ok(bytes)
    }

    fn read_xml(&self, href: &PublicationUrl) -> Result<String, EpubError> {
        let entry = self
            .entries
            .get(href.path())
            .ok_or_else(|| EpubError::ResourceNotFound(href.to_string()))?;
        ensure_limit(
            entry.size <= self.limits.xml_bytes,
            format!(
                "XML resource {} declares {} bytes; XML limit is {}",
                href, entry.size, self.limits.xml_bytes
            ),
        )?;
        let bytes = self.read(href)?;
        let text = decode_xml(&bytes, href)?;
        sanitize_and_validate_xml(&text, href, self.limits.xml_depth)
    }

    fn read_stylesheet(&self, href: &PublicationUrl) -> Result<String, EpubError> {
        let entry = self
            .entries
            .get(href.path())
            .ok_or_else(|| EpubError::ResourceNotFound(href.to_string()))?;
        ensure_limit(
            entry.size <= self.limits.xml_bytes,
            format!(
                "stylesheet {} declares {} bytes; text limit is {}",
                href, entry.size, self.limits.xml_bytes
            ),
        )?;
        decode_xml(&self.read(href)?, href)
    }
}

fn recover_local_entry(
    bytes: &[u8],
    header_offset: usize,
    central_entries: &HashMap<String, CentralDirectoryEntry>,
    limits: EpubLimits,
    ordinal: usize,
) -> Result<Option<(String, ArchiveEntry)>, EpubError> {
    let Some(header) = parse_local_header(bytes, header_offset) else {
        return Ok(None);
    };
    let Some(file_name) = decode_zip_name(header.name_bytes) else {
        return Ok(None);
    };
    let central = central_entries.get(&file_name);
    if (!central_entries.is_empty() && central.is_none()) || file_name.ends_with('/') {
        return Ok(None);
    }
    if header.encrypted || central.is_some_and(|entry| entry.encrypted) {
        return Err(EpubError::Unsupported(
            "encrypted ZIP entries are not supported".into(),
        ));
    }
    if central.is_some_and(|entry| entry.symbolic_link) {
        return Err(EpubError::InvalidArchive(format!(
            "symbolic-link ZIP entry is not allowed: {file_name}"
        )));
    }

    let method = central.map_or(header.method, |entry| entry.method);
    if method != header.method {
        return Err(EpubError::InvalidArchive(format!(
            "ZIP compression method disagrees between headers: {file_name}"
        )));
    }
    if method != ZIP_METHOD_STORED && method != ZIP_METHOD_DEFLATED {
        return Err(EpubError::Unsupported(format!(
            "ZIP compression method {method} is not supported"
        )));
    }
    let compressed_size = central
        .map_or(header.compressed_size, |entry| entry.compressed_size)
        .max(header.compressed_size);
    let size = central
        .map_or(header.size, |entry| entry.size)
        .max(header.size);
    let data_end = header
        .data_start
        .checked_add(usize::try_from(compressed_size).unwrap_or(usize::MAX));
    if data_end.is_none_or(|end| end > bytes.len()) {
        return Ok(None);
    }

    let href = PublicationUrl::parse(&file_name)?.resource_url();
    ensure_limit(
        size <= limits.entry_bytes,
        format!(
            "entry {href} declares {size} uncompressed bytes; per-entry limit is {}",
            limits.entry_bytes
        ),
    )?;
    ensure_compression_ratio(size, compressed_size, limits, &href)?;
    Ok(Some((
        href.path().to_owned(),
        ArchiveEntry {
            location: ArchiveLocation::LocalHeader {
                data_start: header.data_start,
                method,
            },
            size,
            compressed_size,
            stored: method == ZIP_METHOD_STORED,
            crc32: central.map_or(header.crc32, |entry| entry.crc32),
            ordinal,
        },
    )))
}

fn parse_local_header(bytes: &[u8], offset: usize) -> Option<LocalHeader<'_>> {
    let header = bytes.get(offset..offset.checked_add(ZIP_LOCAL_HEADER_SIZE)?)?;
    if header.get(..ZIP_SIGNATURE_SIZE)? != ZIP_LOCAL_HEADER_SIGNATURE {
        return None;
    }
    let flags = read_u16(header, 6)?;
    let method = read_u16(header, 8)?;
    let crc32 = read_u32(header, 14)?;
    let compressed_size = u64::from(read_u32(header, 18)?);
    let size = u64::from(read_u32(header, 22)?);
    let name_length = usize::from(read_u16(header, 26)?);
    let extra_length = usize::from(read_u16(header, 28)?);
    if name_length == 0 || name_length > 1024 {
        return None;
    }
    let name_start = offset.checked_add(ZIP_LOCAL_HEADER_SIZE)?;
    let name_end = name_start.checked_add(name_length)?;
    let data_start = name_end.checked_add(extra_length)?;
    Some(LocalHeader {
        name_bytes: bytes.get(name_start..name_end)?,
        data_start,
        compressed_size,
        size,
        crc32,
        method,
        encrypted: flags & 1 != 0,
    })
}

fn parse_central_directory(bytes: &[u8]) -> HashMap<String, CentralDirectoryEntry> {
    if bytes.len() < ZIP_EOCD_SIZE {
        return HashMap::new();
    }
    let search_start = bytes
        .len()
        .saturating_sub(ZIP_EOCD_SIZE + ZIP_MAX_COMMENT_SIZE);
    let Some(relative_eocd) = bytes[search_start..]
        .windows(ZIP_SIGNATURE_SIZE)
        .rposition(|window| window == ZIP_EOCD_SIGNATURE)
    else {
        return HashMap::new();
    };
    let eocd_offset = search_start + relative_eocd;
    let Some(eocd) = bytes.get(eocd_offset..eocd_offset.saturating_add(ZIP_EOCD_SIZE)) else {
        return HashMap::new();
    };
    if read_u16(eocd, 4) != Some(0) || read_u16(eocd, 6) != Some(0) {
        return HashMap::new();
    }
    let Some(entry_count) = read_u16(eocd, 10).map(usize::from) else {
        return HashMap::new();
    };
    let Some(directory_size) = read_u32(eocd, 12).and_then(|value| usize::try_from(value).ok())
    else {
        return HashMap::new();
    };
    let Some(mut position) = read_u32(eocd, 16).and_then(|value| usize::try_from(value).ok())
    else {
        return HashMap::new();
    };
    if position
        .checked_add(directory_size)
        .is_none_or(|end| end > bytes.len())
    {
        return HashMap::new();
    }

    let mut entries = HashMap::with_capacity(entry_count);
    for _ in 0..entry_count {
        let Some(header) = bytes.get(position..position.saturating_add(ZIP_CENTRAL_HEADER_SIZE))
        else {
            return HashMap::new();
        };
        if header.get(..ZIP_SIGNATURE_SIZE) != Some(ZIP_CENTRAL_HEADER_SIGNATURE.as_slice()) {
            return HashMap::new();
        }
        let Some(name_length) = read_u16(header, 28).map(usize::from) else {
            return HashMap::new();
        };
        let Some(extra_length) = read_u16(header, 30).map(usize::from) else {
            return HashMap::new();
        };
        let Some(comment_length) = read_u16(header, 32).map(usize::from) else {
            return HashMap::new();
        };
        let Some(name_start) = position.checked_add(ZIP_CENTRAL_HEADER_SIZE) else {
            return HashMap::new();
        };
        let Some(name_end) = name_start.checked_add(name_length) else {
            return HashMap::new();
        };
        let Some(entry_end) = name_end
            .checked_add(extra_length)
            .and_then(|value| value.checked_add(comment_length))
        else {
            return HashMap::new();
        };
        let Some(name) = bytes.get(name_start..name_end).and_then(decode_zip_name) else {
            return HashMap::new();
        };
        let flags = read_u16(header, 8).unwrap_or_default();
        let external_attributes = read_u32(header, 38).unwrap_or_default();
        let host_system = header.get(5).copied().unwrap_or_default();
        let unix_mode = external_attributes >> 16;
        entries.insert(
            name,
            CentralDirectoryEntry {
                compressed_size: u64::from(read_u32(header, 20).unwrap_or_default()),
                size: u64::from(read_u32(header, 24).unwrap_or_default()),
                crc32: read_u32(header, 16).unwrap_or_default(),
                method: read_u16(header, 10).unwrap_or_default(),
                encrypted: flags & 1 != 0,
                symbolic_link: host_system == 3 && unix_mode & 0o170_000 == 0o120_000,
            },
        );
        position = entry_end;
    }
    entries
}

fn decode_zip_name(bytes: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(bytes).ok()?;
    (!name.contains('\0')).then(|| name.to_owned())
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let value: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(value))
}

fn read_bounded(
    reader: &mut impl Read,
    expected_size: u64,
    limit: u64,
    href: &PublicationUrl,
) -> Result<Vec<u8>, EpubError> {
    let capacity = usize::try_from(expected_size).unwrap_or(0);
    let mut bytes = Vec::with_capacity(capacity);
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    ensure_limit(
        u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= limit,
        format!("resource expanded beyond size budget: {href}"),
    )?;
    Ok(bytes)
}

#[derive(Debug)]
struct PackageModel {
    metadata: Metadata,
    cover: Option<PublicationUrl>,
    manifest: BTreeMap<String, ManifestItem>,
    manifest_order: Vec<String>,
    spine: Vec<SpineReference>,
    ncx_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ManifestItem {
    id: String,
    href: PublicationUrl,
    media_type: String,
    properties: Vec<String>,
}

#[derive(Debug)]
struct SpineReference {
    idref: String,
    linear: bool,
    properties: Vec<String>,
}

fn validate_mimetype(archive: &EpubArchive, strict: bool) -> Result<(), EpubError> {
    let href = PublicationUrl::parse("mimetype")?;
    let Some(entry) = archive.entries.get(href.path()) else {
        return container_issue(strict, "EPUB archive is missing its root mimetype entry");
    };
    let bytes = archive.read(&href)?;
    let valid_value = bytes == b"application/epub+zip";
    if !valid_value || !entry.stored || entry.ordinal != 0 {
        return container_issue(
            strict,
            "mimetype must be the first, stored entry and contain application/epub+zip",
        );
    }
    Ok(())
}

fn container_issue(strict: bool, message: &str) -> Result<(), EpubError> {
    if strict {
        Err(EpubError::InvalidArchive(message.into()))
    } else {
        Ok(())
    }
}

fn parse_container(xml: &str) -> Result<String, EpubError> {
    let document = Document::parse(xml)?;
    document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "rootfile")
        .and_then(|node| attribute_local(node, "full-path"))
        .map(str::to_owned)
        .ok_or_else(|| EpubError::InvalidXml {
            resource: "META-INF/container.xml".into(),
            message: "container has no rootfile full-path".into(),
        })
}

fn parse_package_metadata(package: Node<'_, '_>) -> (Metadata, Option<String>) {
    let metadata_node = package
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "metadata");
    let title = metadata_node
        .and_then(|metadata| first_descendant_text(metadata, "title"))
        .unwrap_or_else(|| "Untitled publication".into());
    let authors =
        metadata_node.map_or_else(Vec::new, |metadata| descendant_texts(metadata, "creator"));
    let languages =
        metadata_node.map_or_else(Vec::new, |metadata| descendant_texts(metadata, "language"));
    let layout = metadata_node
        .and_then(|metadata| {
            metadata.descendants().find(|node| {
                node.is_element()
                    && node.tag_name().name() == "meta"
                    && attribute_local(*node, "property") == Some("rendition:layout")
            })
        })
        .and_then(normalized_node_text)
        .filter(|value| value == "pre-paginated")
        .map_or(RenditionLayout::Reflowable, |_| {
            RenditionLayout::PrePaginated
        });
    let epub2_cover_id = metadata_node.and_then(|metadata| {
        metadata
            .descendants()
            .find(|node| {
                node.is_element()
                    && node.tag_name().name() == "meta"
                    && attribute_local(*node, "name") == Some("cover")
            })
            .and_then(|node| attribute_local(node, "content"))
            .map(str::to_owned)
    });
    (
        Metadata {
            title,
            authors,
            languages,
            layout,
        },
        epub2_cover_id,
    )
}

fn parse_package(xml: &str, package_url: &PublicationUrl) -> Result<PackageModel, EpubError> {
    let document = Document::parse(xml)?;
    let package = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "package")
        .ok_or_else(|| EpubError::InvalidXml {
            resource: package_url.to_string(),
            message: "package document has no package element".into(),
        })?;

    let (metadata, epub2_cover_id) = parse_package_metadata(package);

    let manifest_node = package
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "manifest")
        .ok_or_else(|| EpubError::InvalidXml {
            resource: package_url.to_string(),
            message: "package document has no manifest".into(),
        })?;
    let mut manifest = BTreeMap::new();
    let mut manifest_order = Vec::new();
    for item in manifest_node
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
    {
        let id = required_attribute(item, "id", package_url)?;
        let raw_href = required_attribute(item, "href", package_url)?;
        let media_type = required_attribute(item, "media-type", package_url)?;
        let href = package_url.resolve(&raw_href)?.resource_url();
        let properties = token_attribute(item, "properties");
        let model = ManifestItem {
            id: id.clone(),
            href,
            media_type,
            properties,
        };
        if manifest.insert(id.clone(), model).is_some() {
            return Err(EpubError::InvalidXml {
                resource: package_url.to_string(),
                message: format!("duplicate manifest ID: {id}"),
            });
        }
        manifest_order.push(id);
    }
    let cover = manifest_order
        .iter()
        .filter_map(|id| manifest.get(id))
        .find(|item| {
            item.properties
                .iter()
                .any(|property| property == "cover-image")
        })
        .or_else(|| {
            epub2_cover_id
                .as_ref()
                .and_then(|cover_id| manifest.get(cover_id))
        })
        .map(|item| item.href.clone());

    let spine_node = package
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "spine")
        .ok_or_else(|| EpubError::InvalidXml {
            resource: package_url.to_string(),
            message: "package document has no spine".into(),
        })?;
    let ncx_id = attribute_local(spine_node, "toc").map(str::to_owned);
    let mut spine = Vec::new();
    for itemref in spine_node
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "itemref")
    {
        spine.push(SpineReference {
            idref: required_attribute(itemref, "idref", package_url)?,
            linear: attribute_local(itemref, "linear") != Some("no"),
            properties: token_attribute(itemref, "properties"),
        });
    }
    if spine.is_empty() {
        return Err(EpubError::InvalidXml {
            resource: package_url.to_string(),
            message: "package spine is empty".into(),
        });
    }

    Ok(PackageModel {
        metadata,
        cover,
        manifest,
        manifest_order,
        spine,
        ncx_id,
    })
}

fn build_reading_order(package: &PackageModel) -> Result<Vec<SpineItem>, EpubError> {
    package
        .spine
        .iter()
        .map(|reference| {
            let item = package.manifest.get(&reference.idref).ok_or_else(|| {
                EpubError::InvalidArchive(format!(
                    "spine references unknown manifest ID: {}",
                    reference.idref
                ))
            })?;
            let mut properties = item.properties.clone();
            for property in &reference.properties {
                if !properties.contains(property) {
                    properties.push(property.clone());
                }
            }
            Ok(SpineItem {
                id: SpineItemId::new(item.id.clone())?,
                href: item.href.clone(),
                media_type: item.media_type.clone(),
                linear: reference.linear,
                properties,
            })
        })
        .collect()
}

fn parse_navigation(
    archive: &EpubArchive,
    package: &PackageModel,
) -> Result<Vec<TocEntry>, EpubError> {
    let nav_item = package
        .manifest_order
        .iter()
        .filter_map(|id| package.manifest.get(id))
        .find(|item| item.properties.iter().any(|property| property == "nav"));
    if let Some(nav_item) = nav_item {
        let xml = archive.read_xml(&nav_item.href)?;
        let toc = parse_epub_navigation(&xml, &nav_item.href)?;
        if !toc.is_empty() {
            return Ok(toc);
        }
    }

    let ncx_item = package
        .ncx_id
        .as_ref()
        .and_then(|id| package.manifest.get(id))
        .or_else(|| {
            package
                .manifest_order
                .iter()
                .filter_map(|id| package.manifest.get(id))
                .find(|item| item.media_type == "application/x-dtbncx+xml")
        });
    ncx_item.map_or_else(
        || Ok(Vec::new()),
        |item| {
            let xml = archive.read_xml(&item.href)?;
            parse_ncx(&xml, &item.href)
        },
    )
}

fn parse_epub_navigation(xml: &str, nav_url: &PublicationUrl) -> Result<Vec<TocEntry>, EpubError> {
    let document = Document::parse(xml)?;
    let nav = document.descendants().find(|node| {
        node.is_element()
            && node.tag_name().name() == "nav"
            && attribute_local(*node, "type")
                .is_some_and(|value| value.split_ascii_whitespace().any(|token| token == "toc"))
    });
    let Some(ordered_list) = nav.and_then(|node| direct_child(node, "ol")) else {
        return Ok(Vec::new());
    };
    parse_nav_list(ordered_list, nav_url)
}

fn parse_nav_list(
    list: Node<'_, '_>,
    nav_url: &PublicationUrl,
) -> Result<Vec<TocEntry>, EpubError> {
    list.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "li")
        .map(|item| {
            let label_node = item
                .children()
                .find(|node| node.is_element() && matches!(node.tag_name().name(), "a" | "span"));
            let label = label_node
                .and_then(normalized_node_text)
                .unwrap_or_else(|| "Untitled section".into());
            let href = label_node
                .and_then(|node| attribute_local(node, "href"))
                .map(|value| nav_url.resolve(value))
                .transpose()
                .or_else(|error| match error {
                    PublicationError::ExternalUrl(_) => Ok(None),
                    other => Err(other),
                })?;
            let children = direct_child(item, "ol")
                .map(|nested| parse_nav_list(nested, nav_url))
                .transpose()?
                .unwrap_or_default();
            Ok(TocEntry {
                label,
                href,
                children,
            })
        })
        .collect()
}

fn parse_ncx(xml: &str, ncx_url: &PublicationUrl) -> Result<Vec<TocEntry>, EpubError> {
    let document = Document::parse(xml)?;
    let Some(nav_map) = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "navMap")
    else {
        return Ok(Vec::new());
    };
    parse_nav_points(nav_map, ncx_url)
}

fn parse_nav_points(
    parent: Node<'_, '_>,
    ncx_url: &PublicationUrl,
) -> Result<Vec<TocEntry>, EpubError> {
    parent
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
        .map(|point| {
            let label = point
                .descendants()
                .find(|node| node.is_element() && node.tag_name().name() == "navLabel")
                .and_then(|node| first_descendant_text(node, "text"))
                .unwrap_or_else(|| "Untitled section".into());
            let href = point
                .children()
                .find(|node| node.is_element() && node.tag_name().name() == "content")
                .and_then(|node| attribute_local(node, "src"))
                .map(|value| ncx_url.resolve(value))
                .transpose()
                .or_else(|error| match error {
                    PublicationError::ExternalUrl(_) => Ok(None),
                    other => Err(other),
                })?;
            Ok(TocEntry {
                label,
                href,
                children: parse_nav_points(point, ncx_url)?,
            })
        })
        .collect()
}

fn sanitize_and_validate_xml(
    xml: &str,
    href: &PublicationUrl,
    max_depth: usize,
) -> Result<String, EpubError> {
    let xml = escape_invalid_xml_ampersands(xml);
    let xml = xml.as_ref();
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut depth = 0_usize;
    let mut doctype_range = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(_)) => {
                depth = depth.saturating_add(1);
                if depth > max_depth {
                    return Err(EpubError::ResourceLimit(format!(
                        "XML depth exceeds {max_depth}: {href}"
                    )));
                }
            }
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::DocType(doctype)) => {
                // EPUB 2 content commonly declares the public XHTML or NCX DTD.
                // quick-xml does not load that external resource, and we remove the
                // declaration before handing the document to roxmltree. Internal
                // subsets remain forbidden because they can define custom entities.
                let doctype_bytes: &[u8] = doctype.as_ref();
                if doctype_bytes.contains(&b'[') {
                    return Err(EpubError::InvalidXml {
                        resource: href.to_string(),
                        message: "DOCTYPE internal subsets and entity declarations are disabled for untrusted EPUB XML"
                            .into(),
                    });
                }
                if doctype_range.is_some() {
                    return Err(EpubError::InvalidXml {
                        resource: href.to_string(),
                        message: "multiple DOCTYPE declarations are not allowed".into(),
                    });
                }
                let end = usize::try_from(reader.buffer_position()).unwrap_or(xml.len());
                let Some(start) = xml[..end].to_ascii_lowercase().rfind("<!doctype") else {
                    return Err(EpubError::InvalidXml {
                        resource: href.to_string(),
                        message: "failed to locate validated DOCTYPE".into(),
                    });
                };
                doctype_range = Some(start..end);
            }
            Ok(Event::Eof) => {
                let Some(range) = doctype_range else {
                    return Ok(xml.to_owned());
                };
                let mut sanitized = String::with_capacity(xml.len() - range.len());
                sanitized.push_str(&xml[..range.start]);
                sanitized.push_str(&xml[range.end..]);
                return Ok(sanitized);
            }
            Ok(_) => {}
            Err(error) => {
                return Err(EpubError::InvalidXml {
                    resource: href.to_string(),
                    message: error.to_string(),
                });
            }
        }
    }
}

fn escape_invalid_xml_ampersands(xml: &str) -> Cow<'_, str> {
    let mut sanitized = None::<String>;
    let mut copied_until = 0;

    for (index, _) in xml.match_indices('&') {
        if is_well_formed_xml_reference(&xml.as_bytes()[index + 1..]) {
            continue;
        }

        let output = sanitized.get_or_insert_with(|| String::with_capacity(xml.len() + 4));
        output.push_str(&xml[copied_until..index]);
        output.push_str("&amp;");
        copied_until = index + 1;
    }

    match sanitized {
        Some(mut sanitized) => {
            sanitized.push_str(&xml[copied_until..]);
            Cow::Owned(sanitized)
        }
        None => Cow::Borrowed(xml),
    }
}

fn is_well_formed_xml_reference(tail: &[u8]) -> bool {
    let Some(&first) = tail.first() else {
        return false;
    };

    if first == b'#' {
        let (digits, valid_digit): (&[u8], fn(&u8) -> bool) = match tail.get(1) {
            Some(b'x') => (&tail[2..], u8::is_ascii_hexdigit),
            _ => (&tail[1..], u8::is_ascii_digit),
        };
        let digit_count = digits.iter().take_while(|byte| valid_digit(byte)).count();
        return digit_count > 0 && digits.get(digit_count) == Some(&b';');
    }

    if !is_xml_name_start(first) {
        return false;
    }
    let name_length = tail
        .iter()
        .take_while(|&&byte| is_xml_name_character(byte))
        .count();
    tail.get(name_length) == Some(&b';')
}

fn is_xml_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':')
}

fn is_xml_name_character(byte: u8) -> bool {
    is_xml_name_start(byte) || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
}

fn decode_xml(bytes: &[u8], href: &PublicationUrl) -> Result<String, EpubError> {
    if let Some(body) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(body, true, href);
    }
    if let Some(body) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(body, false, href);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec()).map_err(|error| EpubError::InvalidXml {
        resource: href.to_string(),
        message: format!("XML must use UTF-8 or UTF-16: {error}"),
    })
}

fn decode_utf16(
    bytes: &[u8],
    little_endian: bool,
    href: &PublicationUrl,
) -> Result<String, EpubError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(EpubError::InvalidXml {
            resource: href.to_string(),
            message: "UTF-16 XML has an odd byte length".into(),
        });
    }
    let words = bytes.chunks_exact(2).map(|chunk| {
        let pair = [chunk[0], chunk[1]];
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    char::decode_utf16(words)
        .collect::<Result<String, _>>()
        .map_err(|error| EpubError::InvalidXml {
            resource: href.to_string(),
            message: format!("invalid UTF-16 XML: {error}"),
        })
}

fn ensure_compression_ratio(
    size: u64,
    compressed_size: u64,
    limits: EpubLimits,
    href: &PublicationUrl,
) -> Result<(), EpubError> {
    if size == 0 {
        return Ok(());
    }
    ensure_limit(
        compressed_size > 0 && size / compressed_size.max(1) <= limits.compression_ratio,
        format!(
            "entry compression ratio exceeds {}: {href}",
            limits.compression_ratio
        ),
    )
}

fn ensure_limit(condition: bool, message: impl Into<String>) -> Result<(), EpubError> {
    if condition {
        Ok(())
    } else {
        Err(EpubError::ResourceLimit(message.into()))
    }
}

fn required_attribute(
    node: Node<'_, '_>,
    name: &str,
    resource: &PublicationUrl,
) -> Result<String, EpubError> {
    attribute_local(node, name)
        .map(str::to_owned)
        .ok_or_else(|| EpubError::InvalidXml {
            resource: resource.to_string(),
            message: format!("{} element is missing {name}", node.tag_name().name()),
        })
}

fn attribute_local<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value())
}

fn token_attribute(node: Node<'_, '_>, name: &str) -> Vec<String> {
    attribute_local(node, name)
        .map(|value| value.split_ascii_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn direct_child<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

fn first_descendant_text(node: Node<'_, '_>, name: &str) -> Option<String> {
    node.descendants()
        .find(|child| child.is_element() && child.tag_name().name() == name)
        .and_then(normalized_node_text)
}

fn descendant_texts(node: Node<'_, '_>, name: &str) -> Vec<String> {
    node.descendants()
        .filter(|child| child.is_element() && child.tag_name().name() == name)
        .filter_map(normalized_node_text)
        .collect()
}

fn normalized_node_text(node: Node<'_, '_>) -> Option<String> {
    let text = node
        .descendants()
        .filter(roxmltree::Node::is_text)
        .filter_map(|descendant| descendant.text())
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

fn guess_media_type(path: &str) -> &'static str {
    let extension = path.rsplit_once('.').map(|(_, extension)| extension);
    match extension.map(str::to_ascii_lowercase).as_deref() {
        Some("xhtml" | "html" | "htm") => "application/xhtml+xml",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("otf") => "font/otf",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

/// EPUB open and resource errors.
#[derive(Debug, Error)]
pub(super) enum EpubError {
    /// File-system access failed.
    #[error("EPUB I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// ZIP central directory or entry data was invalid.
    #[error("invalid EPUB ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
    /// XML tree parsing failed.
    #[error("invalid EPUB XML: {0}")]
    XmlTree(#[from] roxmltree::Error),
    /// A specific XML resource was invalid.
    #[error("invalid XML in {resource}: {message}")]
    InvalidXml {
        /// Resource being parsed.
        resource: String,
        /// Parser or validation detail.
        message: String,
    },
    /// ZIP structure or EPUB relationships were invalid.
    #[error("invalid EPUB archive: {0}")]
    InvalidArchive(String),
    /// A resource was not present in the archive.
    #[error("EPUB resource not found: {0}")]
    ResourceNotFound(String),
    /// A safety budget was exceeded.
    #[error("EPUB resource limit exceeded: {0}")]
    ResourceLimit(String),
    /// An intentionally unsupported container feature was encountered.
    #[error("unsupported EPUB feature: {0}")]
    Unsupported(String),
    /// Shared HTML reading-IR parsing failed.
    #[error(transparent)]
    Html(#[from] rebook_html::HtmlError),
    /// Format-neutral publication validation failed.
    #[error(transparent)]
    Publication(#[from] PublicationError),
}

impl EpubError {
    fn into_publication_error(self) -> PublicationError {
        match self {
            Self::ResourceNotFound(resource) => PublicationError::ResourceNotFound(resource),
            Self::ResourceLimit(message) => PublicationError::ResourceLimit(message),
            Self::Publication(error) => error,
            Self::Io(error) => PublicationError::Io(error.to_string()),
            other => PublicationError::InvalidPublication(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use image::{DynamicImage, ImageFormat};
    use rebook_publication::{
        Block, BookSource, Inline, PublicationUrl, RenditionLayout, TextBlockKind, TocEntry,
        promote_single_toc_root,
    };
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::{
        EpubError, EpubLimits, EpubOpenOptions, EpubPublication, ZIP_CENTRAL_HEADER_SIGNATURE,
        ZIP_CENTRAL_HEADER_SIZE, ZIP_SIGNATURE_SIZE, collect_note_section_paths, read_u16,
    };

    #[test]
    fn promotes_a_single_root_without_flattening_deeper_sections() {
        let entries = vec![TocEntry {
            label: "  STRUCTURED   Writing ".into(),
            href: Some(PublicationUrl::parse("index.xhtml").unwrap()),
            children: vec![
                TocEntry {
                    label: "Preface".into(),
                    href: Some(PublicationUrl::parse("preface.xhtml").unwrap()),
                    children: Vec::new(),
                },
                TocEntry {
                    label: "Introduction".into(),
                    href: Some(PublicationUrl::parse("intro.xhtml").unwrap()),
                    children: vec![TocEntry {
                        label: "Rhetoric".into(),
                        href: None,
                        children: Vec::new(),
                    }],
                },
            ],
        }];

        let promoted = promote_single_toc_root(entries);

        assert_eq!(promoted.len(), 2);
        assert_eq!(promoted[0].label, "Preface");
        assert_eq!(promoted[1].label, "Introduction");
        assert_eq!(promoted[1].children[0].label, "Rhetoric");
    }

    #[test]
    fn keeps_multiple_top_level_entries() {
        let entries = vec![
            TocEntry {
                label: "Part One".into(),
                href: None,
                children: Vec::new(),
            },
            TocEntry {
                label: "Part Two".into(),
                href: None,
                children: Vec::new(),
            },
        ];

        let retained = promote_single_toc_root(entries);

        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].label, "Part One");
        assert_eq!(retained[1].label, "Part Two");
    }

    #[test]
    fn recognizes_only_exact_note_navigation_entries() {
        let entries = vec![
            TocEntry {
                label: "Back Matter".into(),
                href: None,
                children: vec![TocEntry {
                    label: " Notes: ".into(),
                    href: Some(PublicationUrl::parse("Text/notes.xhtml#start").unwrap()),
                    children: Vec::new(),
                }],
            },
            TocEntry {
                label: "作者附注".into(),
                href: Some(PublicationUrl::parse("Text/author-notes.xhtml").unwrap()),
                children: Vec::new(),
            },
            TocEntry {
                label: "Notes on the Translation".into(),
                href: Some(PublicationUrl::parse("Text/essay.xhtml").unwrap()),
                children: Vec::new(),
            },
            TocEntry {
                label: "Chapter One".into(),
                href: Some(PublicationUrl::parse("Text/chapter.xhtml#start").unwrap()),
                children: Vec::new(),
            },
            TocEntry {
                label: "Notes".into(),
                href: Some(PublicationUrl::parse("Text/chapter.xhtml#notes").unwrap()),
                children: Vec::new(),
            },
        ];

        let paths = collect_note_section_paths(&entries);
        assert!(paths.contains("Text/notes.xhtml"));
        assert!(paths.contains("Text/author-notes.xhtml"));
        assert!(!paths.contains("Text/essay.xhtml"));
        assert!(!paths.contains("Text/chapter.xhtml"));
    }

    #[test]
    fn marks_a_whole_note_resource_on_the_spine_descriptor() {
        let publication = EpubPublication::open_bytes(note_section_epub()).expect("valid EPUB");

        assert!(!publication.book().sections[0].is_note_section());
        assert!(publication.book().sections[1].is_note_section());
        let notes = publication.parse_section(1).expect("parsed notes section");
        assert!(notes.blocks.iter().all(|block| {
            matches!(
                block,
                Block::Note(note) if note.kind == rebook_publication::NoteBlockKind::Section
            )
        }));
    }

    #[test]
    fn opens_epub3_navigation_and_lazy_resources() {
        let bytes = minimal_epub();
        let publication = EpubPublication::open_bytes(bytes).expect("valid EPUB");

        assert_eq!(publication.book().metadata.title, "原生阅读器");
        assert_eq!(publication.book().metadata.authors, ["Rebook"]);
        assert_eq!(
            publication.book().cover.as_ref().map(PublicationUrl::path),
            Some("OPS/Images/cover.png")
        );
        assert_eq!(
            publication.book().metadata.layout,
            RenditionLayout::Reflowable
        );
        assert_eq!(publication.book().sections.len(), 1);
        assert_eq!(publication.book().table_of_contents[0].label, "第一章");

        let href = PublicationUrl::parse("OPS/Text/chapter.xhtml").expect("valid URL");
        let resource = publication.resource(&href).expect("chapter resource");
        assert!(String::from_utf8_lossy(&resource.bytes).contains("你好，Rust"));

        let section = publication.parse_section(0).expect("reading IR");
        assert!(matches!(section.blocks.first(), Some(Block::Text(_))));
        let cover = publication
            .resource(publication.book().cover.as_ref().expect("EPUB 3 cover"))
            .expect("cover resource");
        assert_eq!(cover.bytes.as_ref(), b"fake-png");
    }

    #[test]
    fn promotes_only_exact_toc_targeted_paragraphs_to_headings() {
        let mut entries = minimal_entries();
        for (name, bytes, _) in &mut entries {
            match *name {
                "OPS/nav.xhtml" => {
                    *bytes = br#"<?xml version="1.0"?>
                    <html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
                      <body><nav epub:type="toc"><ol>
                        <li><a href="Text/chapter.xhtml#alignment">Alignment</a></li>
                        <li><a href="Text/chapter.xhtml#hierarchy">Hierarchy</a></li>
                        <li><a href="Text/chapter.xhtml#native">Native heading</a></li>
                      </ol></nav></body>
                    </html>"#;
                }
                "OPS/Text/chapter.xhtml" => {
                    *bytes = br#"<?xml version="1.0"?>
                    <html xmlns="http://www.w3.org/1999/xhtml"><body>
                      <p id="alignment" style="font-size: 0.8em; font-weight: normal"> ALIGNMENT </p>
                      <p id="hierarchy">Hierarchy in practice</p>
                      <h3 id="native">Native heading</h3>
                      <p>Body text.</p>
                    </body></html>"#;
                }
                _ => {}
            }
        }
        let publication = EpubPublication::open_bytes(zip_entries(&entries)).expect("valid EPUB");
        let section = publication.parse_section(0).expect("reading IR");
        let text_blocks = section
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(text) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(text_blocks[0].kind, TextBlockKind::Heading(1));
        assert_eq!(text_blocks[1].kind, TextBlockKind::Paragraph);
        assert_eq!(text_blocks[2].kind, TextBlockKind::Heading(3));
        let Inline::Text(alignment) = &text_blocks[0].content[0] else {
            panic!("heading should retain authored text");
        };
        assert!((alignment.style.size_scale - 0.8).abs() < 0.001);
        assert!(!alignment.style.bold);
    }

    #[test]
    fn promotes_a_path_only_toc_label_only_near_the_section_start() {
        let mut entries = minimal_entries();
        for (name, bytes, _) in &mut entries {
            match *name {
                "OPS/nav.xhtml" => {
                    *bytes = br#"<?xml version="1.0"?>
                    <html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
                      <body><nav epub:type="toc"><ol>
                        <li><a href="Text/chapter.xhtml">Introduction</a></li>
                      </ol></nav></body>
                    </html>"#;
                }
                "OPS/Text/chapter.xhtml" => {
                    *bytes = br#"<?xml version="1.0"?>
                    <html xmlns="http://www.w3.org/1999/xhtml"><body>
                      <p>Introduction</p><p>Body text.</p>
                    </body></html>"#;
                }
                _ => {}
            }
        }
        let publication = EpubPublication::open_bytes(zip_entries(&entries)).expect("valid EPUB");
        let section = publication.parse_section(0).expect("reading IR");

        assert!(matches!(
            section.blocks.first(),
            Some(Block::Text(text)) if text.kind == TextBlockKind::Heading(1)
        ));
    }

    #[test]
    fn rejects_archive_entries_that_escape_the_root() {
        let bytes = zip_entries(&[("../evil", b"escape", CompressionMethod::Stored)]);
        let error = EpubPublication::open_bytes(bytes).expect_err("unsafe path must fail");
        assert!(matches!(error, EpubError::Publication(_)));
    }

    #[test]
    fn recovers_resources_from_real_local_headers_when_central_offsets_are_wrong() {
        let mut bytes = minimal_epub();
        corrupt_central_offset(&mut bytes, "OPS/Text/chapter.xhtml");

        let publication = EpubPublication::open_bytes(bytes)
            .expect("local-header recovery should open the publication");
        let chapter = PublicationUrl::parse("OPS/Text/chapter.xhtml").expect("chapter URL");
        let resource = publication
            .resource(&chapter)
            .expect("chapter should be recovered by name");

        assert!(!resource.bytes.is_empty());
        assert!(
            !publication
                .parse_section(0)
                .expect("recovered chapter should parse")
                .blocks
                .is_empty()
        );
    }

    #[test]
    fn rejects_declared_uncompressed_size_over_budget() {
        let bytes = minimal_epub();
        let options = EpubOpenOptions {
            limits: EpubLimits {
                total_uncompressed_bytes: 128,
                ..EpubLimits::default()
            },
            strict_container: false,
        };
        let error = EpubPublication::open_bytes_with_options(bytes, options)
            .expect_err("budget must be enforced");
        assert!(matches!(error, EpubError::ResourceLimit(_)));
    }

    #[test]
    fn strict_mode_rejects_a_compressed_mimetype_entry() {
        let entries = minimal_entries();
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes, _) in entries {
            writer
                .start_file(
                    name,
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
                )
                .expect("start entry");
            writer.write_all(bytes).expect("write entry");
        }
        let bytes = writer.finish().expect("finish ZIP").into_inner();
        let error = EpubPublication::open_bytes_with_options(
            bytes,
            EpubOpenOptions {
                strict_container: true,
                ..EpubOpenOptions::default()
            },
        )
        .expect_err("strict mode must reject invalid mimetype placement");
        assert!(matches!(error, EpubError::InvalidArchive(_)));
    }

    #[test]
    fn falls_back_to_epub2_ncx_navigation() {
        let publication = EpubPublication::open_bytes(zip_entries(&epub2_entries()))
            .expect("valid EPUB 2 publication");

        assert_eq!(publication.book().table_of_contents[0].label, "NCX 第一章");
        assert_eq!(
            publication.book().cover.as_ref().map(PublicationUrl::path),
            Some("OPS/Images/cover.jpg")
        );
        assert_eq!(
            publication.book().table_of_contents[0]
                .href
                .as_ref()
                .expect("NCX href")
                .to_string(),
            "OPS/Text/chapter.xhtml#start"
        );
    }

    #[test]
    fn rejects_doctype_internal_subset_before_tree_parsing() {
        let mut entries = minimal_entries();
        entries[1] = (
            "META-INF/container.xml",
            br#"<?xml version="1.0"?><!DOCTYPE container [<!ENTITY injected "unsafe">]><container><rootfiles><rootfile full-path="OPS/package.opf"/></rootfiles></container>"#,
            CompressionMethod::Deflated,
        );
        let error = EpubPublication::open_bytes(zip_entries(&entries))
            .expect_err("DOCTYPE internal subset must fail");

        assert!(matches!(error, EpubError::InvalidXml { .. }));
        assert!(error.to_string().contains("internal subsets"));
    }

    #[test]
    fn allows_plain_html_doctype_for_xhtml_navigation() {
        let mut entries = minimal_entries();
        entries[3] = (
            "OPS/nav.xhtml",
            r#"<?xml version="1.0"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
              <body><nav epub:type="toc"><ol><li><a href="Text/chapter.xhtml#start">第一章</a></li></ol></nav></body>
            </html>"#
                .as_bytes(),
            CompressionMethod::Deflated,
        );

        let publication = EpubPublication::open_bytes(zip_entries(&entries))
            .expect("plain HTML DOCTYPE in XHTML navigation must be accepted");

        assert_eq!(publication.book().table_of_contents[0].label, "第一章");
    }

    #[test]
    fn allows_xhtml_doctype_with_external_identifier_without_loading_it() {
        let mut entries = minimal_entries();
        entries[3] = (
            "OPS/nav.xhtml",
            r#"<?xml version="1.0"?><!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd"><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="Text/chapter.xhtml#start">第一章</a></li></ol></nav></body></html>"#.as_bytes(),
            CompressionMethod::Deflated,
        );
        let publication = EpubPublication::open_bytes(zip_entries(&entries))
            .expect("external XHTML DOCTYPE must be stripped without being loaded");

        assert_eq!(publication.book().table_of_contents[0].label, "第一章");
    }

    #[test]
    fn allows_epub2_ncx_public_doctype_without_loading_it() {
        let mut entries = epub2_entries();
        entries[3] = (
            "OPS/toc.ncx",
            r#"<?xml version="1.0"?><!DOCTYPE ncx PUBLIC "-//NISO//DTD ncx 2005-1//EN" "http://www.daisy.org/z3986/2005/ncx-2005-1.dtd"><ncx><navMap><navPoint><navLabel><text>第一章</text></navLabel><content src="Text/chapter.xhtml#start"/></navPoint></navMap></ncx>"#.as_bytes(),
            CompressionMethod::Deflated,
        );

        let publication = EpubPublication::open_bytes(zip_entries(&entries))
            .expect("EPUB 2 NCX public DOCTYPE must be stripped without being loaded");

        assert_eq!(publication.book().table_of_contents[0].label, "第一章");
    }

    #[test]
    fn escapes_bare_ampersands_in_xhtml_text() {
        let mut entries = minimal_entries();
        entries[5] = (
            "OPS/Text/chapter.xhtml",
            br"<html><body><p>symbols: *&@, valid: &amp;, numeric: &#38;</p></body></html>",
            CompressionMethod::Deflated,
        );

        let publication = EpubPublication::open_bytes(zip_entries(&entries))
            .expect("publication with recoverable XHTML must open");

        publication
            .parse_section(0)
            .expect("bare ampersands in XHTML text must be escaped");
    }

    #[test]
    fn classifies_only_extremely_thin_small_images_as_decorative_separators() {
        let thin = png(78, 6);
        let formula = png(150, 14);
        let mut entries: Vec<(&str, &[u8], CompressionMethod)> = minimal_entries();
        entries.extend([
            (
                "OPS/Text/rule.png",
                thin.as_slice(),
                CompressionMethod::Deflated,
            ),
            (
                "OPS/Text/formula.png",
                formula.as_slice(),
                CompressionMethod::Deflated,
            ),
        ]);
        let publication = EpubPublication::open_bytes(zip_entries(&entries)).unwrap();

        assert!(
            publication.is_decorative_separator_image(
                &PublicationUrl::parse("OPS/Text/rule.png").unwrap()
            )
        );
        assert!(!publication.is_decorative_separator_image(
            &PublicationUrl::parse("OPS/Text/formula.png").unwrap()
        ));
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(width, height)
            .write_to(&mut output, ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    fn minimal_epub() -> Vec<u8> {
        zip_entries(&minimal_entries())
    }

    fn note_section_epub() -> Vec<u8> {
        zip_entries(&[
            ("mimetype", b"application/epub+zip", CompressionMethod::Stored),
            (
                "META-INF/container.xml",
                br#"<?xml version="1.0"?>
                <container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
                  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
                </container>"#,
                CompressionMethod::Deflated,
            ),
            (
                "OPS/package.opf",
                br#"<?xml version="1.0" encoding="UTF-8"?>
                <package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id">
                  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                    <dc:identifier id="book-id">urn:uuid:notes-test</dc:identifier>
                    <dc:title>Notes test</dc:title><dc:language>en</dc:language>
                  </metadata>
                  <manifest>
                    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
                    <item id="chapter" href="Text/chapter.xhtml" media-type="application/xhtml+xml"/>
                    <item id="notes" href="Text/notes.xhtml" media-type="application/xhtml+xml"/>
                  </manifest>
                  <spine><itemref idref="chapter"/><itemref idref="notes"/></spine>
                </package>"#,
                CompressionMethod::Deflated,
            ),
            (
                "OPS/nav.xhtml",
                br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
                  <body><nav epub:type="toc"><ol>
                    <li><a href="Text/chapter.xhtml">Chapter</a></li>
                    <li><a href="Text/notes.xhtml">Notes</a></li>
                  </ol></nav></body>
                </html>"#,
                CompressionMethod::Deflated,
            ),
            (
                "OPS/Text/chapter.xhtml",
                br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Chapter</h1><p>Body.</p></body></html>"#,
                CompressionMethod::Deflated,
            ),
            (
                "OPS/Text/notes.xhtml",
                br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><h1>Notes</h1><p id="note-1">1. A note.</p></body></html>"#,
                CompressionMethod::Deflated,
            ),
        ])
    }

    fn minimal_entries() -> Vec<(&'static str, &'static [u8], CompressionMethod)> {
        vec![
            ("mimetype", b"application/epub+zip", CompressionMethod::Stored),
            (
                "META-INF/container.xml",
                br#"<?xml version="1.0"?>
                <container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
                  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
                </container>"#,
                CompressionMethod::Deflated,
            ),
            (
                "OPS/package.opf",
                r#"<?xml version="1.0" encoding="UTF-8"?>
                <package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id">
                  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
                    <dc:identifier id="book-id">urn:uuid:test</dc:identifier>
                    <dc:title>原生阅读器</dc:title><dc:creator>Rebook</dc:creator><dc:language>zh-CN</dc:language>
                  </metadata>
                  <manifest>
                    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
                    <item id="cover" href="Images/cover.png" media-type="image/png" properties="cover-image"/>
                    <item id="chapter" href="Text/chapter.xhtml" media-type="application/xhtml+xml"/>
                  </manifest>
                  <spine><itemref idref="chapter"/></spine>
                </package>"#
                    .as_bytes(),
                CompressionMethod::Deflated,
            ),
            (
                "OPS/nav.xhtml",
                r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
                  <body><nav epub:type="toc"><ol><li><a href="Text/chapter.xhtml#start">第一章</a></li></ol></nav></body>
                </html>"#
                    .as_bytes(),
                CompressionMethod::Deflated,
            ),
            (
                "OPS/Images/cover.png",
                b"fake-png",
                CompressionMethod::Deflated,
            ),
            (
                "OPS/Text/chapter.xhtml",
                r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="start">第一章</h1><p>你好，Rust。</p></body></html>"#
                    .as_bytes(),
                CompressionMethod::Deflated,
            ),
        ]
    }

    fn epub2_entries() -> Vec<(&'static str, &'static [u8], CompressionMethod)> {
        vec![
            ("mimetype", b"application/epub+zip", CompressionMethod::Stored),
            (
                "META-INF/container.xml",
                br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OPS/package.opf"/></rootfiles></container>"#,
                CompressionMethod::Deflated,
            ),
            (
                "OPS/package.opf",
                br#"<?xml version="1.0"?>
                <package version="2.0">
                  <metadata><title>EPUB 2</title><meta name="cover" content="cover-art"/></metadata>
                  <manifest>
                    <item id="chapter" href="Text/chapter.xhtml" media-type="application/xhtml+xml"/>
                    <item id="cover-art" href="Images/cover.jpg" media-type="image/jpeg"/>
                    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
                  </manifest>
                  <spine toc="ncx"><itemref idref="chapter"/></spine>
                </package>"#,
                CompressionMethod::Deflated,
            ),
            (
                "OPS/toc.ncx",
                r#"<?xml version="1.0"?><ncx><navMap>
                  <navPoint><navLabel><text>NCX 第一章</text></navLabel>
                    <content src="Text/chapter.xhtml#start"/>
                  </navPoint>
                </navMap></ncx>"#
                    .as_bytes(),
                CompressionMethod::Deflated,
            ),
            (
                "OPS/Images/cover.jpg",
                b"fake-jpeg",
                CompressionMethod::Deflated,
            ),
            (
                "OPS/Text/chapter.xhtml",
                br#"<html><body><h1 id="start">Chapter</h1></body></html>"#,
                CompressionMethod::Deflated,
            ),
        ]
    }

    fn zip_entries(entries: &[(&str, &[u8], CompressionMethod)]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes, compression) in entries {
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(*compression),
                )
                .expect("start entry");
            writer.write_all(bytes).expect("write entry");
        }
        writer.finish().expect("finish ZIP").into_inner()
    }

    fn corrupt_central_offset(bytes: &mut [u8], target: &str) {
        for position in 0..bytes.len().saturating_sub(ZIP_CENTRAL_HEADER_SIZE) {
            if bytes[position..position + ZIP_SIGNATURE_SIZE] != ZIP_CENTRAL_HEADER_SIGNATURE {
                continue;
            }
            let Some(name_length) = read_u16(bytes, position + 28).map(usize::from) else {
                continue;
            };
            let name_start = position + ZIP_CENTRAL_HEADER_SIZE;
            let Some(name_end) = name_start.checked_add(name_length) else {
                continue;
            };
            if bytes.get(name_start..name_end) != Some(target.as_bytes()) {
                continue;
            }
            bytes[position + 42..position + 46].copy_from_slice(&123_u32.to_le_bytes());
            return;
        }
        panic!("central directory entry not found: {target}");
    }
}
