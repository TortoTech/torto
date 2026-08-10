use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use encoding_rs::{Encoding, UTF_8, WINDOWS_1252};
use libchm::{ChmFile, EntrySel};
use rebook_html::parse_section;
use rebook_publication::{
    Book, BookSource, Metadata, PublicationError, PublicationId, PublicationUrl, RenditionLayout,
    Resource, Section, SpineItem, SpineItemId, TableOfContentsOrigin, TocEntry,
};
use scraper::{ElementRef, Html, Node, Selector};
use sha2::{Digest, Sha256};

use crate::{BookFormat, FormatError, conversion_error};

const MAX_ENTRIES: usize = 20_000;
const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct ChmPublication {
    book: Book,
    table_of_contents_origin: TableOfContentsOrigin,
    resources: HashMap<String, StoredResource>,
}

struct StoredResource {
    href: PublicationUrl,
    media_type: String,
    bytes: Arc<[u8]>,
}

#[derive(Default)]
struct SystemMetadata {
    contents: Option<String>,
    default_topic: Option<String>,
    title: Option<String>,
}

#[derive(Clone)]
struct RawTocEntry {
    label: String,
    target: Option<String>,
    children: Vec<Self>,
}

#[derive(Default)]
struct HtmlMetadata {
    title: Option<String>,
    authors: Vec<String>,
    cover_target: Option<String>,
}

struct NavigationModel {
    table_of_contents: Vec<TocEntry>,
    section_hrefs: Vec<PublicationUrl>,
    default_topic: Option<PublicationUrl>,
    authored: bool,
}

struct PublicationMetadata {
    title: String,
    authors: Vec<String>,
    cover: Option<PublicationUrl>,
}

pub(crate) fn open_path(path: &Path, file_name: &str) -> Result<ChmPublication, FormatError> {
    let digest = format!("{:x}", Sha256::digest(fs::read(path)?));
    open_archive(path, file_name, digest)
}

pub(crate) fn open_bytes(bytes: &[u8], file_name: &str) -> Result<ChmPublication, FormatError> {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let temporary = TemporaryChm::create(bytes)?;
    open_archive(&temporary.path, file_name, digest)
}

fn open_archive(
    path: &Path,
    file_name: &str,
    publication_id: String,
) -> Result<ChmPublication, FormatError> {
    let mut archive = ChmFile::open(path).map_err(chm_error)?;
    let system = archive
        .find("/#SYSTEM")
        .ok()
        .and_then(|entry| archive.read(&entry).ok())
        .map_or_else(SystemMetadata::default, |bytes| {
            parse_system_metadata(&bytes)
        });
    let entries = archive
        .entries(EntrySel::NORMAL | EntrySel::FILES)
        .map_err(chm_error)?;
    if entries.len() > MAX_ENTRIES {
        return Err(chm_error(format!(
            "entry count {} exceeds {MAX_ENTRIES}",
            entries.len()
        )));
    }

    let mut resources = HashMap::with_capacity(entries.len());
    let mut total_bytes = 0_u64;
    for entry in entries {
        if entry.length > MAX_ENTRY_BYTES {
            return Err(chm_error(format!(
                "entry {} exceeds the {} byte limit",
                entry.path, MAX_ENTRY_BYTES
            )));
        }
        total_bytes = total_bytes
            .checked_add(entry.length)
            .ok_or_else(|| chm_error("expanded resource size overflow"))?;
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(chm_error(format!(
                "expanded resources exceed the {MAX_TOTAL_BYTES} byte limit"
            )));
        }
        let Some(href) = archive_entry_url(&entry.path) else {
            continue;
        };
        let bytes = archive.read(&entry).map_err(chm_error)?;
        resources.insert(
            resource_key(&href),
            StoredResource {
                media_type: media_type_for_path(href.path()).into(),
                href,
                bytes: bytes.into(),
            },
        );
    }
    if resources.is_empty() {
        return Err(chm_error("archive contains no readable resources"));
    }

    build_publication(resources, &system, file_name, publication_id)
}

fn build_publication(
    resources: HashMap<String, StoredResource>,
    system: &SystemMetadata,
    file_name: &str,
    publication_id: String,
) -> Result<ChmPublication, FormatError> {
    let navigation = build_navigation(&resources, system)?;
    let metadata = build_metadata(&resources, system, file_name, &navigation);
    let (sections, fallback_toc) = build_sections(
        &resources,
        &navigation.section_hrefs,
        &navigation.table_of_contents,
    )?;
    let table_of_contents_origin = if navigation.authored {
        TableOfContentsOrigin::Embedded
    } else {
        TableOfContentsOrigin::Fallback
    };
    Ok(ChmPublication {
        book: Book {
            id: PublicationId::new(publication_id)?,
            metadata: Metadata {
                title: metadata.title,
                authors: metadata.authors,
                languages: Vec::new(),
                layout: RenditionLayout::Reflowable,
            },
            cover: metadata.cover,
            sections,
            table_of_contents: if navigation.authored {
                navigation.table_of_contents
            } else {
                fallback_toc
            },
        },
        table_of_contents_origin,
        resources,
    })
}

fn build_navigation(
    resources: &HashMap<String, StoredResource>,
    system: &SystemMetadata,
) -> Result<NavigationModel, FormatError> {
    let contents_href = system
        .contents
        .as_deref()
        .and_then(|target| resolve_internal_target(None, target, resources))
        .or_else(|| {
            resources
                .values()
                .find(|resource| {
                    extension(resource.href.path())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("hhc"))
                })
                .map(|resource| resource.href.clone())
        });
    let raw_toc = contents_href
        .as_ref()
        .and_then(|href| resource_bytes(resources, href))
        .map(|bytes| parse_hhc(&decode_text(bytes)))
        .unwrap_or_default();
    let table_of_contents = contents_href.as_ref().map_or_else(Vec::new, |base| {
        resolve_toc_entries(&raw_toc, base, resources)
    });
    let authored = !table_of_contents.is_empty();
    let mut section_hrefs = Vec::new();
    let mut seen = HashSet::new();
    collect_section_hrefs(&table_of_contents, &mut seen, &mut section_hrefs);
    let default_topic = system
        .default_topic
        .as_deref()
        .and_then(|target| resolve_internal_target(None, target, resources));
    if let Some(href) = default_topic.as_ref()
        && is_html_path(href.path())
        && seen.insert(resource_key(href))
    {
        section_hrefs.insert(0, href.resource_url());
    }
    if section_hrefs.is_empty() {
        section_hrefs = resources
            .values()
            .filter(|resource| is_html_path(resource.href.path()))
            .map(|resource| resource.href.clone())
            .collect();
        section_hrefs.sort_by(|left, right| left.path().cmp(right.path()));
    }
    if section_hrefs.is_empty() {
        return Err(chm_error("archive contains no HTML reading sections"));
    }
    Ok(NavigationModel {
        table_of_contents,
        section_hrefs,
        default_topic,
        authored,
    })
}

fn build_metadata(
    resources: &HashMap<String, StoredResource>,
    system: &SystemMetadata,
    file_name: &str,
    navigation: &NavigationModel,
) -> PublicationMetadata {
    let metadata_href = navigation
        .default_topic
        .as_ref()
        .filter(|href| is_html_path(href.path()))
        .unwrap_or(&navigation.section_hrefs[0]);
    let html_metadata = resource_bytes(resources, metadata_href)
        .map(|bytes| inspect_html(&decode_text(bytes)))
        .unwrap_or_default();
    let title = system
        .title
        .as_ref()
        .filter(|title| !title.trim().is_empty())
        .cloned()
        .or_else(|| html_metadata.title.clone())
        .or_else(|| first_toc_label(&navigation.table_of_contents))
        .unwrap_or_else(|| title_from_file_name(file_name));
    let cover = html_metadata
        .cover_target
        .as_deref()
        .and_then(|target| resolve_internal_target(Some(metadata_href), target, resources))
        .or_else(|| {
            resources
                .values()
                .find(|resource| {
                    resource.media_type.starts_with("image/")
                        && resource.href.path().to_ascii_lowercase().contains("cover")
                })
                .map(|resource| resource.href.clone())
        });
    PublicationMetadata {
        title,
        authors: html_metadata.authors,
        cover,
    }
}

fn build_sections(
    resources: &HashMap<String, StoredResource>,
    section_hrefs: &[PublicationUrl],
    table_of_contents: &[TocEntry],
) -> Result<(Vec<SpineItem>, Vec<TocEntry>), PublicationError> {
    let titles = toc_titles_by_path(table_of_contents);
    let sections = section_hrefs
        .iter()
        .enumerate()
        .map(|(index, href)| {
            let title = titles
                .get(&resource_key(href))
                .cloned()
                .or_else(|| {
                    resource_bytes(resources, href)
                        .and_then(|bytes| inspect_html(&decode_text(bytes)).title)
                })
                .unwrap_or_else(|| section_title_from_path(href.path()));
            Ok((
                SpineItem {
                    id: SpineItemId::new(format!("section-{}", index + 1))?,
                    href: href.clone(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                },
                title,
            ))
        })
        .collect::<Result<Vec<_>, PublicationError>>()?;
    let fallback_toc = sections
        .iter()
        .map(|(section, title)| TocEntry {
            label: title.clone(),
            href: Some(section.href.clone()),
            children: Vec::new(),
        })
        .collect();
    Ok((
        sections.into_iter().map(|(section, _)| section).collect(),
        fallback_toc,
    ))
}

impl BookSource for ChmPublication {
    fn book(&self) -> &Book {
        &self.book
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.table_of_contents_origin
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let descriptor = self
            .book
            .sections
            .get(index)
            .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))?;
        let bytes = resource_bytes(&self.resources, &descriptor.href)
            .ok_or_else(|| PublicationError::ResourceNotFound(descriptor.href.to_string()))?;
        let xhtml = html_to_xhtml(&decode_text(bytes));
        parse_section(&xhtml, descriptor, |href| {
            resource_bytes(&self.resources, href).map(decode_text)
        })
        .map_err(|error| PublicationError::InvalidPublication(error.to_string()))
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        let resource = self
            .resources
            .get(&resource_key(href))
            .ok_or_else(|| PublicationError::ResourceNotFound(href.to_string()))?;
        Ok(Resource {
            href: resource.href.clone(),
            media_type: resource.media_type.clone(),
            bytes: Arc::clone(&resource.bytes),
        })
    }
}

fn parse_system_metadata(bytes: &[u8]) -> SystemMetadata {
    let mut metadata = SystemMetadata::default();
    let mut position = 4_usize;
    while position + 4 <= bytes.len() {
        let code = u16::from_le_bytes([bytes[position], bytes[position + 1]]);
        let length = usize::from(u16::from_le_bytes([
            bytes[position + 2],
            bytes[position + 3],
        ]));
        position += 4;
        let Some(end) = position
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
        else {
            break;
        };
        let value = decode_text(
            bytes[position..end]
                .split(|byte| *byte == 0)
                .next()
                .unwrap_or(&[]),
        );
        match code {
            0 if !value.trim().is_empty() => metadata.contents = Some(value),
            2 if !value.trim().is_empty() => metadata.default_topic = Some(value),
            3 if !value.trim().is_empty() => metadata.title = Some(value),
            _ => {}
        }
        position = end;
    }
    metadata
}

fn parse_hhc(source: &str) -> Vec<RawTocEntry> {
    let document = Html::parse_document(source);
    document
        .root_element()
        .descendent_elements()
        .find(|element| element.value().name() == "ul")
        .map_or_else(Vec::new, parse_hhc_list)
}

fn parse_hhc_list(list: ElementRef<'_>) -> Vec<RawTocEntry> {
    let mut entries: Vec<RawTocEntry> = Vec::new();
    for child in list.child_elements() {
        match child.value().name() {
            "li" => {
                let children = child
                    .child_elements()
                    .find(|element| element.value().name() == "ul")
                    .map_or_else(Vec::new, parse_hhc_list);
                if let Some(mut entry) = parse_hhc_item(child) {
                    entry.children.extend(children);
                    entries.push(entry);
                } else {
                    entries.extend(children);
                }
            }
            // Some generated HHC files omit </li>, leaving the nested list as
            // a sibling. Associate it with the preceding navigation item.
            "ul" => {
                let children = parse_hhc_list(child);
                if let Some(previous) = entries.last_mut() {
                    previous.children.extend(children);
                } else {
                    entries.extend(children);
                }
            }
            _ => {}
        }
    }
    entries
}

fn parse_hhc_item(item: ElementRef<'_>) -> Option<RawTocEntry> {
    let object = item
        .child_elements()
        .find(|element| element.value().name() == "object")?;
    let mut label = None;
    let mut target = None;
    for param in object
        .descendent_elements()
        .filter(|element| element.value().name() == "param")
    {
        let name = param.attr("name").unwrap_or_default();
        let value = param.attr("value").unwrap_or_default().trim();
        if name.eq_ignore_ascii_case("name") && !value.is_empty() {
            label = Some(normalize_text(value));
        } else if name.eq_ignore_ascii_case("local") && !value.is_empty() {
            target = Some(value.to_owned());
        }
    }
    let label = label.or_else(|| target.as_deref().map(section_title_from_path))?;
    Some(RawTocEntry {
        label,
        target,
        children: Vec::new(),
    })
}

fn resolve_toc_entries(
    entries: &[RawTocEntry],
    base: &PublicationUrl,
    resources: &HashMap<String, StoredResource>,
) -> Vec<TocEntry> {
    entries
        .iter()
        .filter_map(|entry| {
            let href = entry
                .target
                .as_deref()
                .and_then(|target| resolve_internal_target(Some(base), target, resources));
            let children = resolve_toc_entries(&entry.children, base, resources);
            (href.is_some() || !children.is_empty()).then(|| TocEntry {
                label: entry.label.clone(),
                href,
                children,
            })
        })
        .collect()
}

fn resolve_internal_target(
    base: Option<&PublicationUrl>,
    raw_target: &str,
    resources: &HashMap<String, StoredResource>,
) -> Option<PublicationUrl> {
    let mut target = raw_target.trim().replace('\\', "/");
    if let Some((_, archive_target)) = target.split_once("::/") {
        target = archive_target.to_owned();
    }
    let target = target.trim_start_matches('/');
    if target.is_empty()
        || ["http:", "https:", "mailto:", "javascript:"]
            .iter()
            .any(|scheme| target.to_ascii_lowercase().starts_with(scheme))
    {
        return None;
    }
    let parsed = base
        .map_or_else(
            || PublicationUrl::parse(target),
            |base| base.resolve(target),
        )
        .ok()?;
    let stored = resources.get(&resource_key(&parsed))?;
    let fragment = parsed
        .fragment()
        .map(|fragment| format!("#{fragment}"))
        .unwrap_or_default();
    PublicationUrl::parse(&format!("{}{fragment}", stored.href.path())).ok()
}

fn collect_section_hrefs(
    entries: &[TocEntry],
    seen: &mut HashSet<String>,
    output: &mut Vec<PublicationUrl>,
) {
    for entry in entries {
        if let Some(href) = &entry.href
            && is_html_path(href.path())
            && seen.insert(resource_key(href))
        {
            output.push(href.resource_url());
        }
        collect_section_hrefs(&entry.children, seen, output);
    }
}

fn toc_titles_by_path(entries: &[TocEntry]) -> HashMap<String, String> {
    fn collect(entries: &[TocEntry], output: &mut HashMap<String, String>) {
        for entry in entries {
            if let Some(href) = &entry.href {
                output
                    .entry(resource_key(href))
                    .or_insert_with(|| entry.label.clone());
            }
            collect(&entry.children, output);
        }
    }
    let mut titles = HashMap::new();
    collect(entries, &mut titles);
    titles
}

fn first_toc_label(entries: &[TocEntry]) -> Option<String> {
    entries.first().map(|entry| entry.label.clone())
}

fn inspect_html(source: &str) -> HtmlMetadata {
    let document = Html::parse_document(source);
    let mut metadata = HtmlMetadata::default();
    for element in document.root_element().descendent_elements() {
        match element.value().name() {
            "title" if metadata.title.is_none() => {
                metadata.title = non_empty_text(element_text(element));
            }
            "meta" => {
                let name = element.attr("name").unwrap_or_default();
                if name.eq_ignore_ascii_case("author")
                    && let Some(author) = element.attr("content").and_then(non_empty_text)
                {
                    metadata.authors.push(author);
                }
            }
            "img" if metadata.cover_target.is_none() => {
                let alt = element.attr("alt").unwrap_or_default().to_ascii_lowercase();
                let src = element.attr("src").unwrap_or_default();
                let path = src.to_ascii_lowercase();
                if !src.is_empty() && (alt.contains("cover") || path.contains("cover")) {
                    metadata.cover_target = Some(src.to_owned());
                }
            }
            "td" | "p" | "div" if metadata.authors.is_empty() => {
                let text = element_text(element);
                if let Some(author) = text.strip_prefix("By ").and_then(non_empty_text)
                    && author.chars().count() <= 120
                {
                    metadata.authors.push(author);
                }
            }
            _ => {}
        }
    }
    metadata.authors.sort();
    metadata.authors.dedup();
    metadata
}

fn element_text(element: ElementRef<'_>) -> String {
    normalize_text(&element.text().collect::<Vec<_>>().join(" "))
}

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn non_empty_text(value: impl AsRef<str>) -> Option<String> {
    let value = normalize_text(value.as_ref());
    (!value.is_empty()).then_some(value)
}

fn html_to_xhtml(source: &str) -> String {
    let mut document = Html::parse_document(source);
    normalize_legacy_chm_layout(&mut document);
    // html5ever serializes non-breaking spaces with the HTML-only `&nbsp;`
    // entity. Reading IR is parsed as XML, where numeric character references
    // are portable without loading an external DTD.
    normalize_void_elements(&document.html().replace("&nbsp;", "&#160;"))
}

fn normalize_legacy_chm_layout(document: &mut Html) {
    let layout_selector = Selector::parse("body > table").expect("static selector");
    let image_selector = Selector::parse("img").expect("static selector");

    let layout_tables = document
        .select(&layout_selector)
        .map(|element| element.id())
        .collect::<HashSet<_>>();
    let layout_cells = document
        .tree
        .nodes()
        .filter_map(|node| {
            let Node::Element(element) = node.value() else {
                return None;
            };
            if !matches!(element.name(), "tr" | "td" | "th") {
                return None;
            }
            node.ancestors()
                .skip(1)
                .find(|ancestor| {
                    matches!(ancestor.value(), Node::Element(element) if element.name() == "table")
                })
                .filter(|ancestor| layout_tables.contains(&ancestor.id()))
                .map(|_| node.id())
        })
        .collect::<Vec<_>>();
    let decorative_images = document
        .select(&image_selector)
        .filter(|image| {
            let alt = image.value().attr("alt").unwrap_or_default();
            let width = image
                .value()
                .attr("width")
                .and_then(|value| value.parse::<u32>().ok());
            let height = image
                .value()
                .attr("height")
                .and_then(|value| value.parse::<u32>().ok());
            matches!(
                alt.to_ascii_lowercase().as_str(),
                "previous page" | "next page"
            ) || matches!((width, height), (Some(width), Some(height)) if width <= 1 || height <= 1)
        })
        .map(|image| image.id())
        .collect::<Vec<_>>();

    for id in layout_tables.into_iter().chain(layout_cells) {
        if let Some(mut node) = document.tree.get_mut(id)
            && let Node::Element(element) = node.value()
        {
            element.name.local = "div".into();
        }
    }
    for id in decorative_images {
        if let Some(mut node) = document.tree.get_mut(id) {
            node.detach();
        }
    }
}

fn normalize_void_elements(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let mut output = String::with_capacity(source.len() + 32);
    let mut copied = 0_usize;
    let mut search = 0_usize;
    while let Some(relative_start) = lower[search..].find('<') {
        let start = search + relative_start;
        let Some(relative_end) = lower[start..].find('>') else {
            break;
        };
        let end = start + relative_end + 1;
        let inner = lower[start + 1..end - 1].trim();
        let name = inner
            .trim_start_matches('/')
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if matches!(
            name,
            "area"
                | "base"
                | "br"
                | "col"
                | "embed"
                | "hr"
                | "img"
                | "input"
                | "link"
                | "meta"
                | "param"
                | "source"
                | "track"
                | "wbr"
        ) && !inner.starts_with('/')
            && !inner.ends_with('/')
        {
            output.push_str(&source[copied..end - 1]);
            output.push_str("/>");
            copied = end;
        }
        search = end;
    }
    if copied == 0 {
        source.to_owned()
    } else {
        output.push_str(&source[copied..]);
        output
    }
}

fn decode_text(bytes: &[u8]) -> String {
    let (encoding, bom_len) = Encoding::for_bom(bytes).unwrap_or_else(|| {
        (
            declared_encoding(bytes).unwrap_or_else(|| {
                if std::str::from_utf8(bytes).is_ok() {
                    UTF_8
                } else {
                    WINDOWS_1252
                }
            }),
            0,
        )
    });
    let (decoded, _, _) = encoding.decode(&bytes[bom_len..]);
    decoded.into_owned()
}

fn declared_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let lower = head.to_ascii_lowercase();
    let position = lower.find("charset")? + "charset".len();
    let tail = lower[position..].trim_start();
    let tail = tail.strip_prefix('=').map_or(tail, str::trim_start);
    let label = tail
        .trim_start_matches(['\'', '"'])
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '\'' | '"' | ';' | '>')
        })
        .next()?;
    Encoding::for_label(label.as_bytes())
}

fn archive_entry_url(path: &str) -> Option<PublicationUrl> {
    let path = path.trim_start_matches('/').replace('\\', "/");
    PublicationUrl::parse(&path)
        .ok()
        .map(|href| href.resource_url())
}

fn resource_bytes<'a>(
    resources: &'a HashMap<String, StoredResource>,
    href: &PublicationUrl,
) -> Option<&'a [u8]> {
    resources
        .get(&resource_key(href))
        .map(|resource| resource.bytes.as_ref())
}

fn resource_key(href: &PublicationUrl) -> String {
    href.resource_url().path().to_ascii_lowercase()
}

fn is_html_path(path: &str) -> bool {
    extension(path).is_some_and(|extension| {
        ["html", "htm", "xhtml"]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    })
}

fn extension(path: &str) -> Option<&str> {
    path.rsplit_once('.')
        .map(|(_, extension)| extension)
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
}

fn media_type_for_path(path: &str) -> &'static str {
    match extension(path).map(str::to_ascii_lowercase).as_deref() {
        Some("html" | "htm" | "xhtml") => "application/xhtml+xml",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("otf") => "font/otf",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn section_title_from_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|name| name.to_str())
        .map(|name| name.replace(['_', '-'], " "))
        .and_then(non_empty_text)
        .unwrap_or_else(|| "Untitled section".into())
}

fn title_from_file_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|name| name.to_str())
        .and_then(non_empty_text)
        .unwrap_or_else(|| "Untitled CHM".into())
}

fn chm_error(error: impl std::fmt::Display) -> FormatError {
    conversion_error(BookFormat::Chm, error)
}

struct TemporaryChm {
    path: PathBuf,
}

impl TemporaryChm {
    fn create(bytes: &[u8]) -> Result<Self, std::io::Error> {
        for _ in 0..32 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("torto-chm-{}-{sequence}.chm", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    file.write_all(bytes)?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "unable to allocate a temporary CHM path",
        ))
    }
}

impl Drop for TemporaryChm {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_html_help_contents() {
        let entries = parse_hhc(
            r#"<html><body><ul>
                <li><object type="text/sitemap"><param name="Name" value="Chapter 1"><param name="Local" value="book/ch1.html"></object>
                <ul><li><object type="text/sitemap"><param name="Name" value="Part A"><param name="Local" value="book/ch1.html#a"></object></ul>
                <li><object type="text/sitemap"><param name="Name" value="Chapter 2"><param name="Local" value="book/ch2.html"></object>
            </ul></body></html>"#,
        );

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "Chapter 1");
        assert_eq!(entries[0].children[0].label, "Part A");
        assert_eq!(entries[1].target.as_deref(), Some("book/ch2.html"));
    }

    #[test]
    fn repairs_legacy_html_into_reading_ir_compatible_xhtml() {
        let xhtml = html_to_xhtml(
            r#"<HTML><HEAD><META charset=windows-1252><title>Legacy</title></HEAD><BODY><p>One&nbsp;two<br><img src="images/a.jpg"></BODY></HTML>"#,
        );
        let document = roxmltree::Document::parse(&xhtml).expect("well-formed XHTML");

        assert_eq!(
            document
                .descendants()
                .find(|node| node.has_tag_name("p"))
                .and_then(|node| node.text()),
            Some("One\u{a0}two")
        );
        assert!(document.descendants().any(|node| node.has_tag_name("img")));
    }

    #[test]
    #[ignore = "uses the local CHM fixture supplied for desktop verification"]
    fn opens_local_information_dashboard_fixture() {
        let test_data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-data");
        let path = fs::read_dir(test_data)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "chm"))
            .expect("local CHM fixture");
        let source = open_path(&path, path.file_name().unwrap().to_str().unwrap()).unwrap();

        assert_eq!(source.book().metadata.title, "Information Dashboard Design");
        assert!(
            source
                .book()
                .metadata
                .authors
                .iter()
                .any(|author| author.contains("Stephen Few"))
        );
        assert!(source.book().sections.len() > 50);
        assert!(source.book().table_of_contents.len() > 5);
        assert!(source.book().cover.is_some());

        let mut image_count = 0;
        for index in 0..source.book().sections.len() {
            let section = source
                .parse_section(index)
                .unwrap_or_else(|error| panic!("failed to parse CHM section {index}: {error}"));
            assert!(!section.blocks.is_empty(), "empty CHM section {index}");
            for block in &section.blocks {
                if let rebook_publication::Block::Image(image) = block {
                    source.resource(&image.href).unwrap_or_else(|error| {
                        panic!("failed to load CHM image {}: {error}", image.href)
                    });
                    image_count += 1;
                }
            }
        }
        assert!(
            image_count > 10,
            "expected many CHM images, found {image_count}"
        );
    }
}
