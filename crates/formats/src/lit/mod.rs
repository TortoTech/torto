mod container;
mod lzx;
mod maps;
mod tokens;

use std::{
    collections::{HashMap, HashSet},
    fmt::{Display, Write as _},
    io::{Cursor, Write},
    path::Path,
};

use quick_xml::escape::escape;
use rebook_publication::{PublicationId, PublicationUrl};
use sha2::{Digest, Sha256};
use zip::{ZipWriter, write::SimpleFileOptions};

use self::{
    container::Container,
    maps::{HTML_MAP, OPF_MAP},
    tokens::Atoms,
};
use crate::{BookFormat, FormatError, conversion_error, epub::EpubPublication};

const MAX_PAYLOAD_BYTES: usize = 128 * 1024 * 1024;
const NAV_PATH: &str = "__torto_lit_nav.xhtml";
const CONTAINER: &str = r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;

pub(crate) fn open(bytes: &[u8], name: &str) -> Result<EpubPublication, FormatError> {
    let mut container = Container::open(bytes)?;
    let items = container.manifest()?;
    if !items.iter().any(|item| item.linear) {
        return Err(invalid("没有可阅读的正文"));
    }

    let mut names = HashSet::from([
        "mimetype",
        "META-INF/container.xml",
        "package.opf",
        NAV_PATH,
    ]);
    for item in &items {
        let url = PublicationUrl::from_archive_path(&item.path).map_err(invalid)?;
        if url.path() != item.path || !names.insert(&item.path) {
            return Err(invalid("有重复或无效的资源路径"));
        }
    }
    let paths: HashMap<_, _> = items
        .iter()
        .map(|item| (item.id.as_str(), item.path.as_str()))
        .collect();

    let opf = if container.has("/meta") {
        let data = container.read("/meta")?;
        tokens::decode(data, "package.opf", &paths, &OPF_MAP, &Atoms::default()).ok()
    } else {
        None
    };
    let metadata = opf.as_deref().and_then(parse_metadata).unwrap_or_default();
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let mut payload_bytes = 0;
    write_entry(
        &mut archive,
        "mimetype",
        b"application/epub+zip",
        &mut payload_bytes,
    )?;
    write_entry(
        &mut archive,
        "META-INF/container.xml",
        CONTAINER.as_bytes(),
        &mut payload_bytes,
    )?;

    let mut manifest = String::new();
    let mut spine = String::new();
    let mut guide = String::new();
    let mut cover_found = false;
    let mut navigation = None;

    for (index, item) in items.iter().enumerate() {
        let id = format!("lit-{index}");
        let url = PublicationUrl::from_archive_path(&item.path).map_err(invalid)?;
        let href = escape(url.to_string());
        let mut item_properties = "";
        if item.is_document {
            let data_path = format!("/data/{}/content", item.id);
            let atoms = container.atoms(item)?;
            let raw = container.read(&data_path)?;
            let html = tokens::decode(raw, &item.path, &paths, &HTML_MAP, &atoms)?;
            if navigation.is_none() {
                navigation = navigation_document(&html, &url);
            }
            write_entry(
                &mut archive,
                &item.path,
                html.as_bytes(),
                &mut payload_bytes,
            )?;
            if item.linear {
                write!(spine, r#"<itemref idref="{id}"/>"#).unwrap();
            }
            if item.id.eq_ignore_ascii_case("contents") {
                write!(
                    guide,
                    r#"<reference type="toc" title="Contents" href="{href}"/>"#
                )
                .unwrap();
            }
            write!(
                manifest,
                r#"<item id="{id}" href="{href}" media-type="application/xhtml+xml"/>"#
            )
            .unwrap();
        } else {
            let resource = container.read(&format!("/data/{}", item.id))?;
            write_entry(&mut archive, &item.path, resource, &mut payload_bytes)?;
            if !cover_found
                && item.media.starts_with("image/")
                && metadata.cover_id.as_deref().map_or_else(
                    || {
                        item.id.eq_ignore_ascii_case("cover")
                            || item.path.to_ascii_lowercase().contains("cover.")
                    },
                    |id| id == item.id,
                )
            {
                item_properties = r#" properties="cover-image""#;
                cover_found = true;
            }
            let media = escape(match item.media.as_str() {
                "" => "application/octet-stream",
                "text/x-oeb1-css" => "text/css",
                media => media,
            });
            write!(
                manifest,
                r#"<item id="{id}" href="{href}" media-type="{media}"{item_properties}/>"#
            )
            .unwrap();
        }
    }
    if let Some(navigation) = navigation {
        write_entry(
            &mut archive,
            NAV_PATH,
            navigation.as_bytes(),
            &mut payload_bytes,
        )?;
        write!(manifest, r#"<item id="lit-nav" href="{NAV_PATH}" media-type="application/xhtml+xml" properties="nav"/>"#).unwrap();
    }
    let digest = format!("{:x}", Sha256::digest(bytes));
    let metadata = metadata.xml;
    let fallback_title = escape(
        Path::new(name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(name),
    );
    let title = if metadata.contains("<dc:title>") {
        String::new()
    } else {
        format!("<dc:title>{fallback_title}</dc:title>")
    };
    let package = format!(
        r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="lit-id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="lit-id">urn:sha256:{digest}</dc:identifier>{metadata}{title}</metadata><manifest>{manifest}</manifest><spine>{spine}</spine><guide>{guide}</guide></package>"#,
    );
    write_entry(
        &mut archive,
        "package.opf",
        package.as_bytes(),
        &mut payload_bytes,
    )?;
    let archive = archive.finish()?.into_inner();
    let id = PublicationId::new(digest)?;
    EpubPublication::open_bytes_with_id(archive.into(), id).map_err(invalid)
}

#[derive(Default)]
struct PackageMetadata {
    xml: String,
    cover_id: Option<String>,
}

fn parse_metadata(xml: &str) -> Option<PackageMetadata> {
    // Older packages declare the Dublin Core prefix on individual nodes or
    // use it without declaring it. The temporary root covers the latter.
    let wrapped = format!(
        r#"<lit xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:oebpackage="http://openebook.org/namespaces/oeb-package/1.0/">{xml}</lit>"#
    );
    let doc = roxmltree::Document::parse(&wrapped).ok()?;
    let mut result = PackageMetadata::default();
    let metadata = doc
        .descendants()
        .find(|node| node.has_tag_name("metadata"))?;
    for element in metadata.descendants().filter(|node| node.is_element()) {
        if element.has_tag_name("meta") && element.attribute("name") == Some("cover") {
            result.cover_id = element.attribute("content").map(str::to_owned);
        }
        let name = element.tag_name().name().to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "title"
                | "creator"
                | "language"
                | "publisher"
                | "date"
                | "description"
                | "subject"
                | "rights"
        ) {
            continue;
        }
        let Some(value) = element
            .text()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        write!(result.xml, "<dc:{name}>{}</dc:{name}>", escape(value)).unwrap();
    }
    Some(result)
}

fn navigation_document(html: &str, source: &PublicationUrl) -> Option<String> {
    const LABELS: &[&str] = &[
        "contents",
        "table of contents",
        "document outline",
        "目录",
        "目錄",
    ];
    if html.len() > 512 * 1024 || !html.contains("<li") {
        return None;
    }
    let document = roxmltree::Document::parse(html).ok()?;
    let body = document
        .descendants()
        .find(|node| node.has_tag_name("body"))?;
    let heading = body
        .descendants()
        .find(|node| matches!(node.tag_name().name(), "h1" | "h2" | "h3"))?;
    let title = heading
        .descendants()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect::<String>();
    if !LABELS.contains(&title.trim().to_lowercase().as_str()) {
        return None;
    }
    let list = heading
        .next_siblings()
        .find(|node| matches!(node.tag_name().name(), "ol" | "ul"))?;
    let mut xml = String::from(
        r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc">"#,
    );
    if !write_nav_list(list, source, 0, &mut xml) {
        return None;
    }
    xml.push_str("</nav></body></html>");
    Some(xml)
}

fn write_nav_list(
    list: roxmltree::Node<'_, '_>,
    source: &PublicationUrl,
    depth: usize,
    xml: &mut String,
) -> bool {
    if depth >= 24 {
        return false;
    }
    let start = xml.len();
    xml.push_str("<ol>");
    let mut usable = false;
    for node in list.children().filter(|node| node.has_tag_name("li")) {
        let Some(anchor) = node.children().find(|node| node.has_tag_name("a")) else {
            continue;
        };
        let href = anchor
            .attribute("href")
            .and_then(|href| source.resolve(href).ok());
        let Some(href) = href else { continue };
        let label = anchor
            .descendants()
            .filter(|node| node.is_text())
            .filter_map(|node| node.text())
            .collect::<String>();
        if label.trim().is_empty() {
            continue;
        }
        write!(
            xml,
            r#"<li><a href="{}">{}</a>"#,
            escape(href.to_string()),
            escape(label.trim())
        )
        .unwrap();
        if let Some(children) = node
            .children()
            .find(|node| matches!(node.tag_name().name(), "ol" | "ul"))
        {
            write_nav_list(children, source, depth + 1, xml);
        }
        xml.push_str("</li>");
        usable = true;
    }
    if usable {
        xml.push_str("</ol>");
    } else {
        xml.truncate(start);
    }
    usable
}

fn write_entry(
    archive: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    bytes: &[u8],
    total: &mut usize,
) -> Result<(), FormatError> {
    *total += bytes.len();
    if *total > MAX_PAYLOAD_BYTES {
        return Err(invalid("电子书资源超过大小限制"));
    }
    archive.start_file(
        path,
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )?;
    archive.write_all(bytes)?;
    Ok(())
}

fn invalid(reason: impl Display) -> FormatError {
    conversion_error(BookFormat::Lit, reason)
}

#[cfg(test)]
mod tests {
    use rebook_publication::{Block, BookSource, Inline};

    use super::*;

    #[test]
    fn opens_lit_container_and_reads_binary_body() {
        let mut manifest = vec![1, b'/'];
        manifest.extend_from_slice(&1u32.to_le_bytes());
        manifest.extend_from_slice(&0u32.to_le_bytes());
        for value in ["chapter", "chapter.html", "text/x-oeb1-document"] {
            manifest.push(value.len() as u8);
            manifest.extend_from_slice(value.as_bytes());
        }
        manifest.extend_from_slice(&[0; 14]); // MIME terminator, three empty groups and the end marker.
        let content = b"\0\x01\x10\0\0\x01J\0A sentence from a LIT book.\0\x02\0\0\x02\0";
        let book = open(&container_fixture(&manifest, content), "sample.LIT").unwrap();
        assert_eq!(book.book().metadata.title, "sample");
        let section = book.parse_section(0).unwrap();
        let [Block::Text(text)] = section.blocks.as_slice() else {
            panic!("expected one paragraph")
        };
        let [Inline::Text(run)] = text.content.as_slice() else {
            panic!("expected one text run")
        };
        assert_eq!(run.text, "A sentence from a LIT book.");
        assert!(open(b"ITOLITLS", "broken.lit").is_err());
    }

    #[test]
    fn lit_outline_preserves_nested_entries_and_canonical_fragments() {
        let html = r#"<html><body><h2>Document Outline</h2><ul><li><a href="chapter.html#one">Part A</a><ul><li><a href="../other.html#two">A child chapter</a></li></ul></li></ul></body></html>"#;
        let base = PublicationUrl::parse("Text/contents.html").unwrap();
        let navigation = navigation_document(html, &base).unwrap();
        assert!(navigation.contains(r#"<ol><li><a href="Text/chapter.html#one">Part A</a><ol><li><a href="other.html#two">A child chapter</a>"#));
    }

    fn container_fixture(manifest: &[u8], content: &[u8]) -> Vec<u8> {
        const DIRECTORY_OFFSET: usize = 160;
        const CHUNK_SIZE: usize = 512;
        const CHUNK_OFFSET: usize = DIRECTORY_OFFSET + 32;
        const CONTENT_OFFSET: usize = CHUNK_OFFSET + CHUNK_SIZE;
        let mut file = vec![0; CONTENT_OFFSET];
        file[..8].copy_from_slice(b"ITOLITLS");
        for (offset, value) in [
            (8, 1),
            (12, 40),
            (16, 2),
            (20, 88),
            (56, DIRECTORY_OFFSET),
            (64, 32 + CHUNK_SIZE),
            (76, 8),
            (144, CONTENT_OFFSET),
            (DIRECTORY_OFFSET + 8, CHUNK_SIZE),
            (DIRECTORY_OFFSET + 24, 1),
        ] {
            file[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
        }
        file[80..84].copy_from_slice(b"CAOL");
        file[128..132].copy_from_slice(b"ITSF");
        file[DIRECTORY_OFFSET..DIRECTORY_OFFSET + 4].copy_from_slice(b"IFCM");
        file[CHUNK_OFFSET..CHUNK_OFFSET + 4].copy_from_slice(b"AOLL");
        let mut record = Vec::new();
        for (name, offset, len) in [
            ("/manifest", 0, manifest.len()),
            ("/data/chapter/content", manifest.len(), content.len()),
        ] {
            record.push(name.len() as u8);
            record.extend_from_slice(name.as_bytes());
            record.extend([0, offset as u8, len as u8]);
        }
        let free_bytes = CHUNK_SIZE - 48 - record.len();
        file[CHUNK_OFFSET + 4..CHUNK_OFFSET + 8]
            .copy_from_slice(&(free_bytes as u32).to_le_bytes());
        file[CHUNK_OFFSET + 48..CHUNK_OFFSET + 48 + record.len()].copy_from_slice(&record);
        file.extend_from_slice(manifest);
        file.extend_from_slice(content);
        file
    }
}
