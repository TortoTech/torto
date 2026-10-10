use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::{Reader, Writer};
use rebook_publication::{Metadata, RenditionLayout};
use sha2::{Digest, Sha256};

use crate::source::{DirectBookSource, SectionContent, SourceBook, SourceSection, html_document};
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
                properties: Vec::new(),
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
                    properties: Vec::new(),
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

pub(crate) fn normalize_chapter(
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
    let document = html_document(&source).map_err(|error| conversion_error(format, error))?;
    let mut reader = Reader::from_str(&document);
    let mut writer = Writer::new(Vec::new());
    loop {
        let event = reader
            .read_event()
            .map_err(|error| conversion_error(format, error))?;
        let event = match event {
            Event::Eof => break,
            Event::Start(element) => Event::Start(normalize_element(&element, reader.decoder())),
            Event::Empty(element) => Event::Empty(normalize_element(&element, reader.decoder())),
            Event::End(element) => {
                Event::End(BytesEnd::new(normalized_tag(element.name().as_ref())))
            }
            event => event,
        };
        writer
            .write_event(event)
            .map_err(|error| conversion_error(format, error))?;
    }
    String::from_utf8(writer.into_inner()).map_err(|error| conversion_error(format, error))
}

fn normalize_element(
    element: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> BytesStart<'static> {
    let mut normalized = BytesStart::new(normalized_tag(element.name().as_ref()));
    for attribute in element.attributes().flatten() {
        let authored_name = String::from_utf8_lossy(attribute.key.as_ref());
        let lower = authored_name.to_ascii_lowercase();
        let name = if matches!(
            lower.as_str(),
            "id" | "aid"
                | "name"
                | "class"
                | "style"
                | "src"
                | "href"
                | "alt"
                | "width"
                | "height"
                | "colspan"
                | "rowspan"
                | "align"
                | "valign"
                | "role"
                | "type"
                | "rel"
                | "lang"
                | "dir"
                | "size"
                | "color"
                | "face"
        ) {
            lower.as_str()
        } else {
            authored_name.as_ref()
        };
        if let Ok(value) =
            attribute.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
        {
            normalized.push_attribute((name, value.as_ref()));
        }
    }
    let has_id = normalized
        .attributes()
        .flatten()
        .any(|attribute| attribute.key.as_ref() == b"id");
    if !has_id {
        let id = normalized
            .attributes()
            .flatten()
            .find(|attribute| matches!(attribute.key.as_ref(), b"aid" | b"name"))
            .and_then(|attribute| {
                attribute
                    .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
                    .ok()
                    .map(std::borrow::Cow::into_owned)
            });
        if let Some(id) = id {
            normalized.push_attribute(("id", id.as_str()));
        }
    }
    normalized
}

fn normalized_tag(name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "mbp:pagebreak" => "br".into(),
        "html" | "head" | "body" | "title" | "meta" | "link" | "style" | "p" | "h1" | "h2"
        | "h3" | "h4" | "h5" | "h6" | "a" | "img" | "figure" | "figcaption" | "table"
        | "caption" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" | "col" | "colgroup"
        | "div" | "span" | "section" | "article" | "header" | "footer" | "nav" | "aside"
        | "blockquote" | "ul" | "ol" | "li" | "dl" | "dt" | "dd" | "pre" | "code" | "b"
        | "strong" | "i" | "em" | "u" | "s" | "sup" | "sub" | "cite" | "font" | "br" | "hr"
        | "guide" | "reference" => lower,
        _ => name.into_owned(),
    }
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
        assert!(body.contains("id=\"kindle-heading\""), "{body}");
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
