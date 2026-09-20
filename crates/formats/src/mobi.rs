use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use rebook_publication::{Metadata, RenditionLayout};
use sha2::{Digest, Sha256};

use crate::source::{
    DirectBookSource, SectionContent, SourceBook, SourceSection, escape_attribute, escape_text,
};
use crate::{BookFormat, FormatError, conversion_error, kf8};

pub(crate) fn open(
    bytes: &[u8],
    file_name: &str,
    format: BookFormat,
) -> Result<DirectBookSource, FormatError> {
    catch_unwind(AssertUnwindSafe(|| convert(bytes, file_name, format))).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("解析器意外终止");
        Err(conversion_error(format, message))
    })
}

#[allow(clippy::too_many_lines)]
fn convert(
    bytes: &[u8],
    file_name: &str,
    format: BookFormat,
) -> Result<DirectBookSource, FormatError> {
    let kf8::MobiMetadata {
        title: metadata_title,
        authors,
        languages,
        cover_path: metadata_cover_path,
    } = kf8::metadata(bytes, format)?;

    let mut sections = Vec::new();
    let mut table_of_contents = Vec::new();
    let resources;
    if kf8::is_kf8(bytes) {
        let parsed = kf8::parse(bytes, format)?;
        let kf8::Kf8Book {
            sections: kf8_sections,
            table_of_contents: kf8_toc,
            resources: kf8_resources,
        } = parsed;
        resources = kf8_resources;
        table_of_contents = kf8_toc;
        for section in kf8_sections {
            let body = normalize_chapter(&section.html, &HashMap::new(), format)?;
            sections.push(SourceSection {
                title: section.title,
                content: SectionContent::Html(body),
                linear: true,
            });
        }
    } else {
        let kf8::Mobi6Book {
            sections: legacy_sections,
            resources: legacy_resources,
            image_sources,
        } = kf8::parse_mobi6(bytes, format)?;
        resources = legacy_resources;
        for (index, chapter) in legacy_sections.into_iter().enumerate() {
            let title = if chapter.title.trim().is_empty() {
                format!("第 {} 节", index + 1)
            } else {
                chapter.title.trim().to_owned()
            };
            let body = normalize_chapter(&chapter.html, &image_sources, format)?;
            if !body.trim().is_empty() {
                sections.push(SourceSection {
                    title,
                    content: SectionContent::Html(body),
                    linear: true,
                });
            }
        }
    }
    if sections.is_empty() {
        return Err(conversion_error(format, "没有可阅读的正文"));
    }
    let title = if metadata_title
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        Path::new(file_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("未命名书籍")
            .to_owned()
    } else {
        metadata_title
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let cover_path = metadata_cover_path
        .filter(|cover| resources.iter().any(|resource| resource.path == *cover));
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
            table_of_contents,
            resources,
            cover_path,
        },
        format,
    )
}

#[derive(Clone)]
enum ContentType {
    Paragraph,
    Heading(u8),
    Image,
    Link,
    ListItem,
    BlockQuote,
    CodeBlock,
    HorizontalRule,
    Text,
    Other(String),
}

struct ContentItem {
    content_type: ContentType,
    text: String,
    attributes: Vec<(String, String)>,
    children: Vec<Self>,
}

impl ContentItem {
    fn new(content_type: ContentType) -> Self {
        Self {
            content_type,
            text: String::new(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }
}

#[derive(Default)]
struct HtmlParser {
    items: Vec<ContentItem>,
}

impl HtmlParser {
    fn new() -> Self {
        Self::default()
    }

    fn parse(&mut self, html: &str) -> Result<(), quick_xml::Error> {
        let mut reader = Reader::from_str(html);
        reader.config_mut().trim_text(false);
        reader.config_mut().expand_empty_elements = true;
        reader.config_mut().check_end_names = false;
        let mut stack = Vec::new();
        let mut in_body = false;
        let mut has_body = false;
        loop {
            match reader.read_event()? {
                Event::Eof => break,
                Event::Start(element) => {
                    let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                    if name.eq_ignore_ascii_case("body") {
                        in_body = true;
                        has_body = true;
                        continue;
                    }
                    if !has_body && !is_document_wrapper(&name) {
                        in_body = true;
                    }
                    if !in_body {
                        continue;
                    }
                    let mut item = ContentItem::new(content_type(&name));
                    item.attributes = element
                        .attributes()
                        .flatten()
                        .map(|attribute| {
                            (
                                String::from_utf8_lossy(attribute.key.as_ref()).into_owned(),
                                attribute
                                    .decoded_and_normalized_value(
                                        quick_xml::XmlVersion::Implicit1_0,
                                        reader.decoder(),
                                    )
                                    .map(|value| decode_entities(&value))
                                    .unwrap_or_default(),
                            )
                        })
                        .collect();
                    stack.push(item);
                }
                Event::End(element) => {
                    let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                    if name.eq_ignore_ascii_case("body") {
                        in_body = false;
                        continue;
                    }
                    if in_body && let Some(item) = stack.pop() {
                        push_item(&mut self.items, &mut stack, item);
                    }
                }
                Event::Text(text) if in_body => {
                    let value = String::from_utf8_lossy(text.as_ref());
                    if !value.trim().is_empty() {
                        if let Some(item) = stack.last_mut() {
                            item.text.push_str(&value);
                        } else {
                            let mut item = ContentItem::new(ContentType::Text);
                            item.text.push_str(&value);
                            self.items.push(item);
                        }
                    }
                }
                Event::GeneralRef(reference) if in_body => {
                    let value = reference
                        .resolve_char_ref()?
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| {
                            format!("&{};", String::from_utf8_lossy(reference.as_ref()))
                        });
                    if let Some(item) = stack.last_mut() {
                        item.text.push_str(&value);
                    } else {
                        let mut item = ContentItem::new(ContentType::Text);
                        item.text = value;
                        self.items.push(item);
                    }
                }
                Event::CData(text) if in_body => {
                    if let Some(item) = stack.last_mut() {
                        item.text.push_str(&String::from_utf8_lossy(text.as_ref()));
                    }
                }
                _ => {}
            }
        }
        while let Some(item) = stack.pop() {
            push_item(&mut self.items, &mut stack, item);
        }
        Ok(())
    }
}

fn push_item(items: &mut Vec<ContentItem>, stack: &mut [ContentItem], item: ContentItem) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(item);
    } else {
        items.push(item);
    }
}

fn is_document_wrapper(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "html" | "head" | "meta" | "title" | "link" | "style"
    )
}

fn content_type(name: &str) -> ContentType {
    match name.to_ascii_lowercase().as_str() {
        "p" => ContentType::Paragraph,
        "h1" => ContentType::Heading(1),
        "h2" => ContentType::Heading(2),
        "h3" => ContentType::Heading(3),
        "h4" => ContentType::Heading(4),
        "h5" => ContentType::Heading(5),
        "h6" => ContentType::Heading(6),
        "img" => ContentType::Image,
        "a" => ContentType::Link,
        "li" => ContentType::ListItem,
        "blockquote" => ContentType::BlockQuote,
        "pre" | "code" => ContentType::CodeBlock,
        "hr" => ContentType::HorizontalRule,
        _ => ContentType::Other(name.to_owned()),
    }
}

fn normalize_chapter(
    source: &str,
    images: &HashMap<usize, String>,
    format: BookFormat,
) -> Result<String, FormatError> {
    let mut source = source.to_owned();
    for (index, path) in images {
        let replacement = format!("src=\"../{path}\"");
        for recindex in [format!("{index:05}"), index.to_string()] {
            source = source
                .replace(&format!("recindex=\"{recindex}\""), &replacement)
                .replace(&format!("recindex='{recindex}'"), &replacement)
                .replace(&format!("recindex={recindex}"), &replacement);
        }
    }
    let source = rewrite_numeric_attributes(&source, "recindex", |value| {
        images.get(&value).map(|path| format!("src=\"../{path}\""))
    });
    let source = rewrite_numeric_attributes(&source, "filepos", |value| {
        Some(format!("href=\"#filepos{value}\""))
    });
    let source = strip_document_wrappers(&source);
    let document = format!(
        "<html xmlns:mbp=\"http://mobipocket.com/ns/mbp\"><head></head><body>{source}</body></html>"
    );
    let source = crate::markup::html(&document, Default::default())
        .map_err(|error| conversion_error(format, error))?;
    let source = protect_entities(&source);
    let mut parser = HtmlParser::new();
    parser
        .parse(&source)
        .map_err(|error| conversion_error(format, error))?;
    let body = parser.items.iter().map(render_item).collect::<String>();
    if body.trim().is_empty() {
        let plain = strip_markup(&source);
        return if plain.trim().is_empty() {
            Ok(String::new())
        } else {
            Ok(format!("<p>{}</p>", escape_text(plain.trim())))
        };
    }
    Ok(body)
}

fn rewrite_numeric_attributes(
    source: &str,
    name: &str,
    mut replacement: impl FnMut(usize) -> Option<String>,
) -> String {
    let bytes = source.as_bytes();
    let lower = bytes.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    let needle = name.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut copied = 0usize;
    let mut search = 0usize;
    while search + needle.len() <= lower.len() {
        let Some(relative) = lower[search..]
            .windows(needle.len())
            .position(|window| window == needle)
        else {
            break;
        };
        let start = search + relative;
        search = start + needle.len();
        let within_tag = lower[..start].iter().rposition(|byte| *byte == b'<')
            > lower[..start].iter().rposition(|byte| *byte == b'>');
        if !within_tag || start > 0 && lower[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut position = search;
        while lower.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if lower.get(position) != Some(&b'=') {
            continue;
        }
        position += 1;
        while lower.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        let quote = lower
            .get(position)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        if quote.is_some() {
            position += 1;
        }
        let value_start = position;
        while lower.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == value_start || quote.is_some_and(|quote| lower.get(position) != Some(&quote))
        {
            continue;
        }
        let end = position + usize::from(quote.is_some());
        let Ok(value) = source[value_start..position].parse::<usize>() else {
            continue;
        };
        let replacement = replacement(value)
            .unwrap_or_else(|| format!("{name}=\"{}\"", &source[value_start..position]));
        output.push_str(&source[copied..start]);
        output.push_str(&replacement);
        copied = end;
        search = end;
    }
    if copied == 0 {
        source.to_owned()
    } else {
        output.push_str(&source[copied..]);
        output
    }
}

fn strip_document_wrappers(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let mut output = String::with_capacity(source.len());
    let mut copied = 0usize;
    let mut search = 0usize;
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
        if inner.starts_with("!doctype") || matches!(name, "html" | "body") {
            output.push_str(&source[copied..start]);
            copied = end;
        } else if name == "head" && !inner.starts_with('/') {
            output.push_str(&source[copied..start]);
            let block_end = lower[end..]
                .find("</head>")
                .map_or(end, |relative| end + relative + "</head>".len());
            copied = block_end;
            search = block_end;
            continue;
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

fn render_item(item: &ContentItem) -> String {
    let content = format!(
        "{}{}",
        escape_text(&decode_entities(item.text.trim())),
        item.children.iter().map(render_item).collect::<String>()
    );
    match &item.content_type {
        ContentType::Paragraph => wrap("p", item, &content),
        ContentType::Heading(level) => wrap(&format!("h{}", (*level).clamp(1, 6)), item, &content),
        ContentType::Image => {
            let Some(src) = attribute(item, "src") else {
                return String::new();
            };
            if !src.starts_with("../Images/") {
                return String::new();
            }
            let alt = attribute(item, "alt").unwrap_or_default();
            format!(
                "<img src=\"{}\" alt=\"{}\"/>",
                escape_attribute(src),
                escape_attribute(alt)
            )
        }
        ContentType::Link => {
            let href = attribute(item, "href").unwrap_or_default();
            let id = authored_identifier(item);
            if !href.starts_with('#') && id.is_none() {
                content
            } else {
                let id = id
                    .map(|id| format!(" id=\"{}\"", escape_attribute(id)))
                    .unwrap_or_default();
                let href = if href.starts_with('#') {
                    format!(" href=\"{}\"", escape_attribute(href))
                } else {
                    String::new()
                };
                format!("<a{id}{href}>{content}</a>")
            }
        }
        ContentType::ListItem => wrap("li", item, &content),
        ContentType::BlockQuote => wrap("blockquote", item, &content),
        ContentType::CodeBlock => wrap("pre", item, &content),
        ContentType::HorizontalRule => "<hr/>".to_owned(),
        ContentType::Text => content,
        ContentType::Other(tag) => match tag.to_ascii_lowercase().as_str() {
            "br" | "mbp:pagebreak" => format!("<br/>{content}"),
            "div" | "section" | "article" => wrap("div", item, &content),
            "ul" => wrap("ul", item, &content),
            "ol" => wrap("ol", item, &content),
            "strong" | "b" => wrap("strong", item, &content),
            "em" | "i" => wrap("em", item, &content),
            "sup" => wrap("sup", item, &content),
            "sub" => wrap("sub", item, &content),
            _ => content,
        },
    }
}

fn wrap(tag: &str, item: &ContentItem, content: &str) -> String {
    let id = authored_identifier(item)
        .map(|id| format!(" id=\"{}\"", escape_attribute(id)))
        .unwrap_or_default();
    format!("<{tag}{id}>{content}</{tag}>")
}

fn authored_identifier(item: &ContentItem) -> Option<&str> {
    attribute(item, "id")
        .or_else(|| attribute(item, "name"))
        .or_else(|| attribute(item, "aid"))
}

fn attribute<'a>(item: &'a ContentItem, name: &str) -> Option<&'a str> {
    item.attributes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn decode_entities(value: &str) -> String {
    value
        .replace('\u{e000}', "&")
        .replace('\u{e001}', "<")
        .replace('\u{e002}', ">")
        .replace('\u{e003}', "\"")
        .replace('\u{e004}', "'")
        .replace('\u{e005}', " ")
        .replace("&nbsp;", " ")
        .replace("&#160;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn protect_entities(value: &str) -> String {
    value
        .replace("&amp;", "\u{e000}")
        .replace("&lt;", "\u{e001}")
        .replace("&gt;", "\u{e002}")
        .replace("&quot;", "\u{e003}")
        .replace("&apos;", "\u{e004}")
        .replace("&nbsp;", "\u{e005}")
        .replace("&#160;", "\u{e005}")
}

fn strip_markup(value: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                output.push(' ');
            }
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    decode_entities(&output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::BookSource as _;

    #[test]
    fn normalizes_mobi_html_and_embedded_images() {
        let images = HashMap::from([(1, "Images/image-1.jpg".to_owned())]);
        let body = normalize_chapter(
            "<a id=\"chapter-start\"></a><h1 aid=\"kindle-heading\">Title</h1><p>Hello &amp; world</p><img recindex=\"00001\">",
            &images,
            BookFormat::Mobi,
        )
        .unwrap();
        assert!(body.contains("<h1 id=\"kindle-heading\">Title</h1>"));
        assert!(body.contains("<a id=\"chapter-start\"></a>"), "{body}");
        assert!(body.contains("Hello &amp; world"), "{body}");
        assert!(body.contains("src=\"../Images/image-1.jpg\""));
    }

    #[test]
    fn malformed_mobi_preserves_entities_images_and_page_break_content() {
        let images = HashMap::from([(1, "Images/image-1.jpg".to_owned())]);
        let body = normalize_chapter(
            r#"<p>A & raw &#9731; &copy;<p>B<img recindex="1" alt="a > b"><mbp:pagebreak/><p>C"#,
            &images,
            BookFormat::Azw3,
        )
        .unwrap();
        assert!(body.contains("A &amp; raw ☃ ©"), "{body}");
        assert!(body.contains("a &gt; b"), "{body}");
        assert!(body.contains("Images/image-1.jpg"), "{body}");
        assert!(body.contains('C'), "{body}");
    }

    #[test]
    fn opens_mobi6_with_metadata_sections_and_cover_without_iepub() {
        let bytes = mobi6_fixture();
        let source = open(&bytes, "fixture.mobi", BookFormat::Mobi).unwrap();
        assert_eq!(source.book().metadata.title, "Native MOBI");
        assert_eq!(source.book().metadata.authors, ["Test Author"]);
        assert_eq!(source.book().metadata.languages, ["en"]);
        assert_eq!(
            source
                .book()
                .cover
                .as_ref()
                .map(rebook_publication::PublicationUrl::path),
            Some("Images/kindle-1.png")
        );
        assert_eq!(source.book().sections.len(), 2);
        assert!(source.parse_section(0).unwrap().blocks.len() >= 2);
        assert!(!source.parse_section(1).unwrap().blocks.is_empty());
    }

    fn mobi6_fixture() -> Vec<u8> {
        let title = b"Native MOBI";
        let text = b"<html><body><h1>One</h1><p>Hello &amp; world.</p><img recindex=00001></body></html><mbp:pagebreak/><html><body><h1>Two</h1><p>Second.</p></body></html>";
        let exth = exth(&[
            (100, b"Test Author".as_slice()),
            (524, b"en".as_slice()),
            (201, 0_u32.to_be_bytes().as_slice()),
        ]);
        let mobi_header_length = 232usize;
        let exth_offset = 16 + mobi_header_length;
        let title_offset = exth_offset + exth.len();
        let mut record_zero = vec![0; title_offset + title.len()];
        put_u16(&mut record_zero, 0, 1);
        put_u32(&mut record_zero, 4, u32::try_from(text.len()).unwrap());
        put_u16(&mut record_zero, 8, 1);
        put_u16(&mut record_zero, 10, 4_096);
        record_zero[16..20].copy_from_slice(b"MOBI");
        put_u32(
            &mut record_zero,
            20,
            u32::try_from(mobi_header_length).unwrap(),
        );
        put_u32(&mut record_zero, 24, 2);
        put_u32(&mut record_zero, 28, 65_001);
        put_u32(&mut record_zero, 32, 42);
        put_u32(&mut record_zero, 36, 6);
        put_u32(&mut record_zero, 84, u32::try_from(title_offset).unwrap());
        put_u32(&mut record_zero, 88, u32::try_from(title.len()).unwrap());
        record_zero[95] = 9;
        put_u32(&mut record_zero, 108, 2);
        put_u32(&mut record_zero, 112, u32::MAX);
        put_u32(&mut record_zero, 128, 0x40);
        put_u32(&mut record_zero, 244, u32::MAX);
        record_zero[exth_offset..title_offset].copy_from_slice(&exth);
        record_zero[title_offset..title_offset + title.len()].copy_from_slice(title);

        let cover = b"\x89PNG\r\n\x1a\n";
        let records = [record_zero.as_slice(), text.as_slice(), cover.as_slice()];
        let header_length = 78 + records.len() * 8;
        let mut output = vec![0; header_length];
        output[..11].copy_from_slice(title);
        output[60..68].copy_from_slice(b"BOOKMOBI");
        put_u16(&mut output, 76, u16::try_from(records.len()).unwrap());
        let mut offset = header_length;
        for (index, record) in records.iter().enumerate() {
            put_u32(&mut output, 78 + index * 8, u32::try_from(offset).unwrap());
            output.extend_from_slice(record);
            offset += record.len();
        }
        output
    }

    fn exth(entries: &[(u32, &[u8])]) -> Vec<u8> {
        let length = 12
            + entries
                .iter()
                .map(|(_, data)| 8 + data.len())
                .sum::<usize>();
        let padded = length.next_multiple_of(4);
        let mut output = vec![0; padded];
        output[..4].copy_from_slice(b"EXTH");
        put_u32(&mut output, 4, u32::try_from(padded).unwrap());
        put_u32(&mut output, 8, u32::try_from(entries.len()).unwrap());
        let mut position = 12usize;
        for &(kind, data) in entries {
            put_u32(&mut output, position, kind);
            put_u32(
                &mut output,
                position + 4,
                u32::try_from(8 + data.len()).unwrap(),
            );
            output[position + 8..position + 8 + data.len()].copy_from_slice(data);
            position += 8 + data.len();
        }
        output
    }

    fn put_u16(output: &mut [u8], offset: usize, value: u16) {
        output[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u32(output: &mut [u8], offset: usize, value: u32) {
        output[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
}
