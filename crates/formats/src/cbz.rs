use std::io::{Cursor, Read};
use std::path::Path;

use rebook_publication::{Metadata, RenditionLayout};
use roxmltree::Document;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::source::{DirectBookSource, SectionContent, SourceBook, SourceResource, SourceSection};
use crate::xml::decode_xml;
use crate::{BookFormat, FormatError, conversion_error};

const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

struct ImageEntry {
    index: usize,
    name: String,
    extension: &'static str,
    media_type: &'static str,
}

#[derive(Default)]
struct ComicMetadata {
    title: Option<String>,
    authors: Vec<String>,
    language: Option<String>,
}

pub(crate) fn open(bytes: &[u8], file_name: &str) -> Result<DirectBookSource, FormatError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut images = Vec::new();
    let mut comic_info_index = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        if name.eq_ignore_ascii_case("ComicInfo.xml") {
            comic_info_index = Some(index);
        }
        if let Some((extension, media_type)) = image_type_from_name(&name) {
            images.push(ImageEntry {
                index,
                name,
                extension,
                media_type,
            });
        }
    }
    images.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    if images.is_empty() {
        return Err(conversion_error(BookFormat::Cbz, "压缩包中没有支持的图片"));
    }

    let metadata = comic_info_index
        .map(|index| read_comic_info(&mut archive, index))
        .transpose()?
        .unwrap_or_default();
    let mut resources = Vec::with_capacity(images.len());
    let mut sections = Vec::with_capacity(images.len());
    for (page_index, image) in images.into_iter().enumerate() {
        let mut entry = archive.by_index(image.index)?;
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(conversion_error(
                BookFormat::Cbz,
                format_args!("图片 {} 超过 64 MiB 限制", image.name),
            ));
        }
        let mut image_bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry
            .by_ref()
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut image_bytes)?;
        let resource_path = format!("Images/page-{:05}.{}", page_index + 1, image.extension);
        resources.push(SourceResource {
            path: resource_path.clone(),
            media_type: image.media_type.to_owned(),
            bytes: image_bytes,
        });
        sections.push(SourceSection {
            title: image.name.clone(),
            content: SectionContent::Image {
                resource_path,
                alt: image.name.clone(),
            },
            linear: true,
            properties: Vec::new(),
        });
    }
    let cover_path = resources.first().map(|resource| resource.path.clone());
    let title = metadata
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| title_from_file_name(file_name));
    DirectBookSource::open(
        SourceBook {
            id: format!("{:x}", Sha256::digest(bytes)),
            metadata: Metadata {
                title,
                authors: metadata.authors,
                languages: metadata.language.into_iter().collect(),
                layout: RenditionLayout::PrePaginated,
            },
            sections,
            table_of_contents: Vec::new(),
            resources,
            cover_path,
        },
        BookFormat::Cbz,
    )
}

fn read_comic_info(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    index: usize,
) -> Result<ComicMetadata, FormatError> {
    let mut entry = archive.by_index(index)?;
    if entry.size() > MAX_ENTRY_BYTES {
        return Err(conversion_error(BookFormat::Cbz, "ComicInfo.xml 过大"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
    entry.read_to_end(&mut bytes)?;
    let xml = decode_xml(&bytes, BookFormat::Cbz)?;
    let document =
        Document::parse(&xml).map_err(|error| conversion_error(BookFormat::Cbz, error))?;
    let root = document.root_element();
    let field = |name: &str| {
        root.descendants()
            .find(|node| node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name))
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    };
    let authors = field("Writer")
        .map(|writers| {
            writers
                .split([',', ';'])
                .map(str::trim)
                .filter(|writer| !writer.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(ComicMetadata {
        title: field("Title"),
        authors,
        language: field("LanguageISO"),
    })
}

fn image_type_from_name(name: &str) -> Option<(&'static str, &'static str)> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("__macosx/") {
        return None;
    }
    match Path::new(&lower).extension()?.to_str()? {
        "jpg" | "jpeg" => Some(("jpg", "image/jpeg")),
        "png" => Some(("png", "image/png")),
        "gif" => Some(("gif", "image/gif")),
        "webp" => Some(("webp", "image/webp")),
        "bmp" => Some(("bmp", "image/bmp")),
        _ => None,
    }
}

fn title_from_file_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("未命名漫画")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use rebook_publication::{Block, BookSource, RenditionLayout};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    #[test]
    fn converts_comic_info_and_sorted_pages() {
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        archive.start_file("002.png", options).unwrap();
        archive.write_all(b"second").unwrap();
        archive.start_file("001.jpg", options).unwrap();
        archive.write_all(b"first").unwrap();
        archive.start_file("ComicInfo.xml", options).unwrap();
        archive
            .write_all(
                r"<ComicInfo><Title>测试漫画</Title><Writer>甲, 乙</Writer><LanguageISO>zh-CN</LanguageISO></ComicInfo>"
                    .as_bytes(),
            )
            .unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        let publication = open(&bytes, "fallback.cbz").unwrap();
        assert_eq!(publication.book().metadata.title, "测试漫画");
        assert_eq!(publication.book().metadata.authors, ["甲", "乙"]);
        assert_eq!(
            publication.book().metadata.layout,
            RenditionLayout::PrePaginated
        );
        assert_eq!(publication.book().sections.len(), 2);
        let first = publication.parse_section(0).unwrap();
        let Some(Block::Image(image)) = first.blocks.first() else {
            panic!("expected comic page image");
        };
        assert!(image.href.path().ends_with("page-00001.jpg"));
    }
}
