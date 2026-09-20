use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Cursor, Read};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rebook_publication::{Metadata, RenditionLayout};
use roxmltree::{Document, Node};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::source::{
    DirectBookSource, SectionContent, SourceBook, SourceResource, SourceSection, escape_attribute,
    escape_text,
};
use crate::xml::decode_xml;
use crate::{BookFormat, FormatError, conversion_error};

const MAX_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

struct ImageReference {
    path: String,
}

pub(crate) fn open(bytes: &[u8], file_name: &str) -> Result<DirectBookSource, FormatError> {
    let xml_bytes = extract_xml(bytes)?;
    let xml = decode_xml(&xml_bytes, BookFormat::Fb2)?;
    let xml = crate::markup::fb2(&xml).map_err(|error| conversion_error(BookFormat::Fb2, error))?;
    let document =
        Document::parse(&xml).map_err(|error| conversion_error(BookFormat::Fb2, error))?;
    let root = document.root_element();
    if local_name(root) != "fictionbook" {
        return Err(conversion_error(BookFormat::Fb2, "根元素不是 FictionBook"));
    }

    let title_info = root
        .descendants()
        .find(|node| is_element(*node, "title-info"));
    let title = title_info
        .and_then(|node| first_descendant_text(node, "book-title"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| title_from_file_name(file_name));
    let authors = title_info.map(extract_authors).unwrap_or_default();
    let languages = title_info
        .and_then(|node| first_descendant_text(node, "lang"))
        .filter(|value| !value.is_empty())
        .into_iter()
        .collect();

    let (resources, image_references) = extract_images(root)?;
    let cover_path = title_info
        .and_then(|node| {
            node.descendants()
                .find(|child| is_element(*child, "coverpage"))
        })
        .and_then(|cover| {
            cover
                .descendants()
                .find(|child| is_element(*child, "image"))
        })
        .and_then(image_identifier)
        .and_then(|id| image_references.get(id))
        .map(|image| image.path.clone());
    let sections = extract_sections(root, &image_references);
    if sections.is_empty() {
        return Err(conversion_error(BookFormat::Fb2, "没有可阅读的正文"));
    }

    DirectBookSource::open(
        SourceBook {
            id: format!("{:x}", Sha256::digest(bytes)),
            metadata: Metadata {
                title,
                authors,
                languages,
                layout: RenditionLayout::Reflowable,
            },
            sections,
            table_of_contents: Vec::new(),
            resources,
            cover_path,
        },
        BookFormat::Fb2,
    )
}

fn extract_xml(bytes: &[u8]) -> Result<Vec<u8>, FormatError> {
    if !bytes.starts_with(b"PK") {
        return Ok(bytes.to_vec());
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let entry_index = (0..archive.len()).find(|index| {
        archive
            .by_index(*index)
            .is_ok_and(|entry| entry.name().to_ascii_lowercase().ends_with(".fb2"))
    });
    let Some(entry_index) = entry_index else {
        return Err(conversion_error(BookFormat::Fbz, "压缩包中没有 .fb2 文件"));
    };
    let mut entry = archive.by_index(entry_index)?;
    if entry.size() > MAX_ENTRY_BYTES {
        return Err(conversion_error(
            BookFormat::Fbz,
            "FB2 文件超过 64 MiB 限制",
        ));
    }
    let mut output = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
    entry
        .by_ref()
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut output)?;
    if u64::try_from(output.len()).unwrap_or(u64::MAX) > MAX_ENTRY_BYTES {
        return Err(conversion_error(
            BookFormat::Fbz,
            "FB2 文件超过 64 MiB 限制",
        ));
    }
    Ok(output)
}

fn extract_images(
    root: Node<'_, '_>,
) -> Result<(Vec<SourceResource>, HashMap<String, ImageReference>), FormatError> {
    let mut resources = Vec::new();
    let mut references = HashMap::new();
    for binary in root
        .descendants()
        .filter(|node| is_element(*node, "binary"))
    {
        let Some(id) = attribute_local(binary, "id").filter(|id| !id.trim().is_empty()) else {
            continue;
        };
        let encoded = binary
            .text()
            .unwrap_or_default()
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let decoded = BASE64
            .decode(encoded.as_bytes())
            .map_err(|error| conversion_error(BookFormat::Fb2, error))?;
        let declared = attribute_local(binary, "content-type").unwrap_or_default();
        let Some((extension, media_type)) = image_type(declared, &decoded) else {
            continue;
        };
        let path = format!("Images/image-{}.{}", resources.len() + 1, extension);
        resources.push(SourceResource {
            path: path.clone(),
            media_type: media_type.to_owned(),
            bytes: decoded,
        });
        references.insert(id.to_owned(), ImageReference { path });
    }
    Ok((resources, references))
}

fn extract_sections(
    root: Node<'_, '_>,
    images: &HashMap<String, ImageReference>,
) -> Vec<SourceSection> {
    let mut sections = Vec::new();
    for (body_index, body) in root
        .children()
        .filter(|node| is_element(*node, "body"))
        .enumerate()
    {
        let linear = body_index == 0 && attribute_local(body, "name").is_none();
        let top_level_sections = body
            .children()
            .filter(|node| is_element(*node, "section"))
            .collect::<Vec<_>>();
        if top_level_sections.is_empty() {
            let body_markup = render_children(body, images);
            if !body_markup.trim().is_empty() {
                sections.push(SourceSection {
                    title: section_title(body, sections.len()),
                    content: SectionContent::Html(body_markup),
                    linear,
                });
            }
            continue;
        }

        let preface = body
            .children()
            .filter(|node| !is_element(*node, "section"))
            .map(|node| render_node(node, images))
            .collect::<String>();
        if !preface.trim().is_empty() {
            sections.push(SourceSection {
                title: section_title(body, sections.len()),
                content: SectionContent::Html(preface),
                linear,
            });
        }
        for section in top_level_sections {
            sections.push(SourceSection {
                title: section_title(section, sections.len()),
                content: SectionContent::Html(render_node(section, images)),
                linear,
            });
        }
    }
    sections
}

fn render_children(node: Node<'_, '_>, images: &HashMap<String, ImageReference>) -> String {
    node.children()
        .map(|child| render_node(child, images))
        .collect()
}

fn render_node(node: Node<'_, '_>, images: &HashMap<String, ImageReference>) -> String {
    if node.is_text() {
        return escape_text(node.text().unwrap_or_default());
    }
    if !node.is_element() {
        return String::new();
    }
    let name = local_name(node);
    if name == "binary" {
        return String::new();
    }
    if name == "image" {
        return image_identifier(node)
            .and_then(|id| images.get(id))
            .map(|image| format!("<img src=\"../{}\" alt=\"\"/>", image.path))
            .unwrap_or_default();
    }
    if name == "empty-line" {
        return "<br/>".to_owned();
    }

    let tag = match name.as_str() {
        "section" => "section",
        "title" => "header",
        "p" if node
            .parent()
            .is_some_and(|parent| is_element(parent, "title")) =>
        {
            "h1"
        }
        "p" | "v" | "text-author" | "date" => "p",
        "subtitle" => "h2",
        "epigraph" | "poem" | "cite" => "blockquote",
        "stanza" => "div",
        "annotation" => "aside",
        "strong" => "strong",
        "emphasis" => "em",
        "strikethrough" => "s",
        "sub" => "sub",
        "sup" => "sup",
        "code" => "code",
        "table" => "table",
        "tr" => "tr",
        "th" => "th",
        "td" => "td",
        "a" => "a",
        _ => return render_children(node, images),
    };
    let mut attributes = String::new();
    if let Some(id) = attribute_local(node, "id") {
        write!(attributes, " id=\"{}\"", escape_attribute(id)).unwrap();
    }
    if name == "a"
        && let Some(href) = attribute_local(node, "href")
    {
        write!(attributes, " href=\"{}\"", escape_attribute(href)).unwrap();
    }
    format!(
        "<{tag}{attributes}>{}</{tag}>",
        render_children(node, images)
    )
}

fn section_title(node: Node<'_, '_>, index: usize) -> String {
    node.descendants()
        .find(|child| is_element(*child, "title"))
        .map(normalized_text)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| format!("第 {} 节", index + 1))
}

fn extract_authors(title_info: Node<'_, '_>) -> Vec<String> {
    title_info
        .children()
        .filter(|node| is_element(*node, "author"))
        .filter_map(|author| {
            let nickname = first_descendant_text(author, "nickname").unwrap_or_default();
            if !nickname.is_empty() {
                return Some(nickname);
            }
            let name = ["first-name", "middle-name", "last-name"]
                .into_iter()
                .filter_map(|part| first_descendant_text(author, part))
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

fn first_descendant_text(node: Node<'_, '_>, name: &str) -> Option<String> {
    node.descendants()
        .find(|child| is_element(*child, name))
        .map(normalized_text)
}

fn normalized_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|child| child.text())
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn image_identifier<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    attribute_local(node, "href").map(|href| href.trim_start_matches('#'))
}

fn attribute_local<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name().eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value())
}

fn local_name(node: Node<'_, '_>) -> String {
    node.tag_name().name().to_ascii_lowercase()
}

fn is_element(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
}

fn title_from_file_name(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    if lower.ends_with(".fb2.zip") {
        return file_name[..file_name.len() - ".fb2.zip".len()].to_owned();
    }
    std::path::Path::new(file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("未命名书籍")
        .to_owned()
}

fn image_type(declared: &str, bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    match declared.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => return Some(("jpg", "image/jpeg")),
        "image/png" => return Some(("png", "image/png")),
        "image/gif" => return Some(("gif", "image/gif")),
        "image/webp" => return Some(("webp", "image/webp")),
        "image/bmp" | "image/x-ms-bmp" => return Some(("bmp", "image/bmp")),
        _ => {}
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("png", "image/png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("jpg", "image/jpeg"))
    } else if bytes.starts_with(b"GIF8") {
        Some(("gif", "image/gif"))
    } else if bytes.starts_with(b"BM") {
        Some(("bmp", "image/bmp"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use rebook_publication::{Block, BookSource};

    use super::*;

    #[test]
    fn converts_metadata_cover_and_sections() {
        let source = r##"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
  <description><title-info><book-title>FB2 测试书</book-title><author><first-name>三</first-name><last-name>张</last-name></author><lang>zh-CN</lang><coverpage><image l:href="#cover"/></coverpage></title-info></description>
  <body><section id="one"><title><p>第一章</p></title><p>正文内容</p><image l:href="#cover"/></section></body>
  <binary id="cover" content-type="image/png">iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=</binary>
</FictionBook>"##;
        let publication = open(source.as_bytes(), "fixture.fb2").unwrap();
        assert_eq!(publication.book().metadata.title, "FB2 测试书");
        assert_eq!(publication.book().metadata.authors, ["三 张"]);
        assert_eq!(publication.book().metadata.languages, ["zh-CN"]);
        assert!(publication.book().cover.is_some());
        let section = publication.parse_section(0).unwrap();
        assert!(
            section
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Text(_)))
        );
        assert!(
            section
                .blocks
                .iter()
                .any(|block| matches!(block, Block::Image(_)))
        );
    }
}
