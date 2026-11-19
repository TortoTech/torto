use std::{ops::Range, path::Path};

use sha2::{Digest, Sha256};

use encoding_rs::{BIG5, GB18030, UTF_16BE, UTF_16LE, WINDOWS_1252};
use rebook_publication::{
    Block, Book, BookSource, Inline, Metadata, PublicationError, PublicationId, PublicationUrl,
    Resource, Section, SourceAnchor, SourceRange, SpineItem, SpineItemId, TableOfContentsOrigin,
    TextBlock, TextBlockKind, TextRun, TocEntry,
};

use crate::{BookFormat, FormatError, conversion_error};

const MAX_SECTION_BYTES: usize = 96 * 1024;
const MAX_PARAGRAPH_BYTES: usize = 12 * 1024;
const MAX_HEADING_CHARS: usize = 80;
const COMMON_HAN: &str = "的一是了我不在人有他她們们這这个個上中大來来到為为子時时生地國国你說说年著着要那和得出也下後后自以會会家可而過过天去能對对多小然於于心之都好看起當当發发沒没成只如事把用第道想開开從从無无情面女但現现前所同日手行方期它頭头經经長长兒儿回分老因很給给名法間间知什兩两次其此話话常正見见明問问外書书學学章";

pub(crate) struct TxtSource {
    book: Book,
    text: String,
    sections: Vec<Chapter>,
    wrap_width: Option<usize>,
    cjk: bool,
}

#[derive(Debug)]
struct Chapter {
    bytes: Range<usize>,
    heading: Option<Heading>,
}

#[derive(Debug, Clone, Copy)]
struct Heading {
    level: u8,
    numbered: bool,
}

pub(crate) fn open(bytes: &[u8], file_name: &str) -> Result<TxtSource, FormatError> {
    let text = decode(bytes).ok_or_else(|| invalid("无法识别文本编码"))?;
    let text = if text.contains(['\r', '\u{000c}']) {
        text.replace("\r\n", "\n").replace(['\r', '\u{000c}'], "\n")
    } else {
        text
    };
    if text.trim().is_empty() {
        return Err(invalid("没有可阅读的正文"));
    }

    let language = text_language(&text);
    let cjk = language.is_some();
    let wrap_width = hard_wrap_width(&text, cjk);
    let title = metadata_value(&text, &["书名", "書名", "Title"]).unwrap_or_else(|| {
        Path::new(file_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(file_name)
            .to_owned()
    });
    let metadata = Metadata {
        title,
        authors: metadata_value(&text, &["作者", "Author"])
            .into_iter()
            .collect(),
        languages: language.into_iter().map(str::to_owned).collect(),
        ..Default::default()
    };

    let sections = split_sections(&text, wrap_width.is_some() && !cjk);
    let mut spine = Vec::with_capacity(sections.len());
    let mut toc: Vec<TocEntry> = Vec::new();
    let mut inside_volume = false;
    for chapter in &sections {
        let id = SpineItemId::new(format!("txt-{}", chapter.bytes.start))?;
        let href = PublicationUrl::parse(&format!("Text/{}.txt", chapter.bytes.start))?;
        if let Some(heading) = chapter.heading {
            let label = text[chapter.bytes.clone()]
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            let entry = TocEntry {
                label,
                href: Some(href.clone()),
                children: Vec::new(),
            };
            if heading.level == 2
                && let Some(volume) = toc.last_mut()
                && inside_volume
            {
                volume.children.push(entry);
            } else {
                inside_volume = heading.level == 1 && heading.numbered;
                toc.push(entry);
            }
        }
        spine.push(SpineItem {
            id,
            href,
            media_type: "text/plain".into(),
            linear: true,
            properties: Vec::new(),
        });
    }
    if toc.is_empty()
        && let Some(first) = spine.first()
    {
        toc.push(TocEntry {
            label: metadata.title.clone(),
            href: Some(first.href.clone()),
            children: Vec::new(),
        });
    }
    let book = Book {
        id: PublicationId::new(format!("{:x}", Sha256::digest(bytes)))?,
        metadata,
        cover: None,
        sections: spine,
        table_of_contents: toc,
    };
    Ok(TxtSource {
        book,
        text,
        sections,
        wrap_width,
        cjk,
    })
}

fn invalid(message: &str) -> FormatError {
    conversion_error(BookFormat::Txt, message)
}

fn decode(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(b"\xff\xfe\0\0") || bytes.starts_with(b"\0\0\xfe\xff") {
        let little_endian = bytes[0] == 0xff;
        let mut text = String::new();
        for unit in bytes.get(4..)?.as_chunks::<4>().0 {
            let value = if little_endian {
                u32::from_le_bytes(*unit)
            } else {
                u32::from_be_bytes(*unit)
            };
            text.push(char::from_u32(value)?);
        }
        return ((bytes.len() - 4).is_multiple_of(4) && looks_like_text(&text)).then_some(text);
    }
    if let Some((encoding, skip)) = encoding_rs::Encoding::for_bom(bytes) {
        let text = encoding.decode_without_bom_handling_and_without_replacement(&bytes[skip..])?;
        return looks_like_text(&text).then(|| text.into_owned());
    }
    if let Ok(text) = std::str::from_utf8(bytes)
        && looks_like_text(text)
    {
        return Some(text.to_owned());
    }

    // Like huoban, compare the plausible decoded text rather than taking the
    // first legacy decoder that accepts the bytes. Big5 and GB18030 overlap.
    // Two-byte controls/newlines also reveal UTF-16 with a missing BOM.
    let sample = &bytes[..bytes.len().min(32 * 1024)];
    let mut candidates = vec![GB18030, BIG5, WINDOWS_1252];
    if sample.contains(&0) {
        candidates = vec![UTF_16LE, UTF_16BE];
    }
    let mut best = None;
    for encoding in candidates {
        let (text, malformed) = encoding.decode_without_bom_handling(sample);
        if !looks_like_text(&text) {
            continue;
        }
        let score = encoding_score(&text) - if malformed { 20 } else { 0 };
        let Some((_, best_score)) = best else {
            best = Some((encoding, score));
            continue;
        };
        if score > best_score {
            best = Some((encoding, score));
        }
    }
    let text = best?
        .0
        .decode_without_bom_handling_and_without_replacement(bytes)?;
    looks_like_text(&text).then(|| text.into_owned())
}

fn looks_like_text(text: &str) -> bool {
    let mut count = 0;
    let mut unusual = 0;
    for ch in text.chars().take(32 * 1024) {
        count += 1;
        if ch == '\0' {
            return false;
        }
        unusual += usize::from(ch.is_control() && !matches!(ch, '\r' | '\n' | '\t' | '\u{000c}'));
    }
    unusual * 100 <= count
}

fn encoding_score(text: &str) -> i64 {
    let mut total = 0;
    let mut score = 0;
    for ch in text.chars().take(8192) {
        total += 1;
        score += match ch {
            '\u{fffd}' => -30,
            '\u{e000}'..='\u{f8ff}' => -8,
            '\u{4e00}'..='\u{9fff}' if COMMON_HAN.contains(ch) => 8,
            '。' | '，' | '、' | '「' | '」' => 4,
            'Ã' | 'Â' | 'Ð' | '¤' => -4,
            _ if ch.is_control() && !ch.is_whitespace() => -20,
            _ => 0,
        };
    }
    score * 100 / total.max(1)
}

fn text_language(text: &str) -> Option<&'static str> {
    let mut han = 0;
    let mut kana = 0;
    let mut hangul = 0;
    let mut latin = 0;
    for ch in text.chars().take(12_000) {
        match ch {
            '\u{3400}'..='\u{9fff}' => han += 1,
            '\u{3040}'..='\u{30ff}' => kana += 1,
            '\u{ac00}'..='\u{d7af}' => hangul += 1,
            'a'..='z' | 'A'..='Z' => latin += 1,
            _ => {}
        }
    }
    if kana * 4 > han + latin && kana > 0 {
        Some("ja")
    } else if hangul > han + latin {
        Some("ko")
    } else if han > latin && han > 0 {
        Some("zh")
    } else {
        None
    }
}

fn metadata_value(text: &str, labels: &[&str]) -> Option<String> {
    text.lines().take(30).find_map(|line| {
        let (name, value) = line.trim().split_once([':', '：', '】'])?;
        let value = value.trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '：'));
        (labels.iter().any(|label| {
            name.trim_start_matches('【')
                .trim()
                .eq_ignore_ascii_case(label)
        }) && !value.is_empty()
            && value.chars().count() <= MAX_HEADING_CHARS)
            .then(|| value.trim_matches(['《', '》']).to_owned())
    })
}

fn hard_wrap_width(text: &str, cjk: bool) -> Option<usize> {
    let mut lengths = [0usize; 100];
    let mut blanks = 0;
    let mut lines = 0;
    for line in text.lines().take(450) {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            blanks += 1;
        } else {
            lines += 1;
            let width = trimmed.chars().take(lengths.len()).count();
            if let Some(count) = lengths.get_mut(width) {
                *count += 1;
            }
        }
    }
    if lines < 20 || blanks == 0 {
        return None;
    }
    if cjk {
        // Chinese legacy editions usually wrap to the same character column.
        (24..=65)
            .max_by_key(|&width| lengths[width])
            .filter(|&width| lengths[width] * 3 >= lines)
            .map(|width| width - 2)
    } else {
        // Latin text wraps to a word boundary, so line lengths form a band.
        (lengths[55..=85].iter().sum::<usize>() * 2 > lines).then_some(48)
    }
}

fn chinese_number(ch: char) -> bool {
    ch.is_ascii_digit()
        || matches!(ch, '０'..='９')
        || "〇零一二三四五六七八九十百千万萬两兩壹贰貳叁參肆伍陆陸柒捌玖拾佰仟".contains(ch)
}

fn heading(line: &str) -> Option<Heading> {
    let line = line.trim();
    if line.is_empty() || line.chars().take(MAX_HEADING_CHARS + 1).count() > MAX_HEADING_CHARS {
        return None;
    }
    if matches!(
        line,
        "序" | "序言"
            | "前言"
            | "楔子"
            | "引子"
            | "引言"
            | "后记"
            | "後記"
            | "尾声"
            | "尾聲"
            | "终章"
            | "終章"
    ) || ["prologue", "epilogue", "preface", "afterword"]
        .iter()
        .any(|label| line.eq_ignore_ascii_case(label))
    {
        return Some(Heading {
            level: 1,
            numbered: false,
        });
    }
    if let Some(rest) = line.strip_prefix('第') {
        let mut rest = rest.trim_start();
        let ordinal_end = rest
            .find(|ch: char| !chinese_number(ch))
            .unwrap_or(rest.len());
        if ordinal_end == 0 || rest[..ordinal_end].chars().count() > 8 {
            return None;
        }
        rest = rest[ordinal_end..].trim_start();
        let unit = rest.chars().next()?;
        let level = match unit {
            '卷' | '部' | '篇' | '集' => 1,
            '章' | '节' | '節' | '回' | '幕' | '话' | '話' => 2,
            _ => return None,
        };
        let tail = &rest[unit.len_utf8()..];
        let separated = tail.starts_with(|ch: char| ch.is_whitespace() || "：:·—".contains(ch));
        if !separated
            && (tail.chars().count() > 28
                || tail.contains(['。', '，', '；', '！', '？', '“', '”']))
        {
            return None;
        }
        if tail
            .trim_end_matches(['。', '！', '？', '!', '?'])
            .contains('。')
        {
            return None;
        }
        return Some(Heading {
            level,
            numbered: true,
        });
    }
    let (label, tail) = line.split_once(char::is_whitespace)?;
    let level = if label.eq_ignore_ascii_case("chapter") {
        2
    } else if label.eq_ignore_ascii_case("part") || label.eq_ignore_ascii_case("book") {
        1
    } else {
        return None;
    };
    let tail = tail.trim_start();
    let ordinal = tail
        .split(|ch: char| ch.is_whitespace() || ".:—-".contains(ch))
        .next()?;
    let roman = ordinal
        .bytes()
        .all(|ch| b"IVXLCDM".contains(&ch.to_ascii_uppercase()));
    if ordinal.is_empty()
        || ordinal.len() > 8
        || !(roman || ordinal.bytes().all(|ch| ch.is_ascii_digit()))
    {
        return None;
    }
    let rest = &tail[ordinal.len()..];
    let trailing = rest.trim_start_matches(|ch: char| ch.is_whitespace() || ".:—-".contains(ch));
    if rest.starts_with(char::is_whitespace) && trailing.starts_with(|ch: char| ch.is_lowercase()) {
        return None;
    }
    Some(Heading {
        level,
        numbered: true,
    })
}

fn split_sections(text: &str, hard_wrapped: bool) -> Vec<Chapter> {
    let mut position = 0;
    let mut start = 0;
    let mut current_heading = None;
    let mut sections = Vec::new();
    let mut previous_blank = true;
    for line in text.split_inclusive('\n') {
        let inferred = if !hard_wrapped || previous_blank {
            heading(line)
        } else {
            None
        };
        if inferred.is_some() || position - start >= MAX_SECTION_BYTES {
            add_chapter(&mut sections, text, start..position, current_heading);
            start = position;
            current_heading = inferred;
        }
        position += line.len();
        previous_blank = line.trim().is_empty();
    }
    add_chapter(&mut sections, text, start..text.len(), current_heading);
    sections
}

fn add_chapter(
    chapters: &mut Vec<Chapter>,
    text: &str,
    mut bytes: Range<usize>,
    mut heading: Option<Heading>,
) {
    while bytes.len() > MAX_SECTION_BYTES {
        let end = text.floor_char_boundary(bytes.start + MAX_SECTION_BYTES);
        let middle = text.ceil_char_boundary(bytes.start + MAX_SECTION_BYTES / 2);
        let preferred = text[middle..end]
            .rfind('\n')
            .map(|newline| middle + newline + 1);
        let end = preferred.unwrap_or(end);
        if !text[bytes.start..end].trim().is_empty() {
            chapters.push(Chapter {
                bytes: bytes.start..end,
                heading,
            });
        }
        bytes.start = end;
        heading = None;
    }
    if !text[bytes.clone()].trim().is_empty() {
        chapters.push(Chapter { bytes, heading });
    }
}

impl BookSource for TxtSource {
    fn book(&self) -> &Book {
        &self.book
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        TableOfContentsOrigin::Fallback
    }

    fn body_bold_profile(&self) -> Option<bool> {
        Some(false)
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let descriptor = self.book.sections.get(index).ok_or_else(|| {
            PublicationError::InvalidPublication(format!("TXT section index {index}"))
        })?;
        let chapter = &self.sections[index];
        let mut position = chapter.bytes.start;
        let mut start = position;
        let mut paragraph = String::new();
        let mut previous_width = 0;
        let mut blocks = Vec::new();
        for line in self.text[chapter.bytes.clone()].split_inclusive('\n') {
            let trimmed = line.trim();
            if position == chapter.bytes.start
                && let Some(heading) = chapter.heading
            {
                push_block(
                    &mut blocks,
                    descriptor,
                    position,
                    trimmed,
                    TextBlockKind::Heading(heading.level),
                );
            } else {
                let continuation = !trimmed.is_empty()
                    && !line.starts_with(char::is_whitespace)
                    && self.wrap_width.is_some_and(|width| previous_width >= width)
                    && !paragraph.is_empty();
                if !continuation {
                    push_paragraph(&mut blocks, descriptor, start, &paragraph);
                    paragraph.clear();
                    start = position;
                } else if !self.cjk {
                    paragraph.push(' ');
                }
                paragraph.push_str(trimmed);
            }
            previous_width = line.trim_end().chars().count();
            position += line.len();
        }
        push_paragraph(&mut blocks, descriptor, start, &paragraph);
        Ok(Section {
            id: descriptor.id.clone(),
            href: descriptor.href.clone(),
            blocks,
            anchors: Vec::new(),
        })
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        Err(PublicationError::ResourceNotFound(href.to_string()))
    }
}

fn push_paragraph(blocks: &mut Vec<Block>, descriptor: &SpineItem, offset: usize, text: &str) {
    let mut start = 0;
    while start < text.len() {
        let limit = text.floor_char_boundary((start + MAX_PARAGRAPH_BYTES).min(text.len()));
        let end = if limit == text.len() {
            limit
        } else {
            let middle = text.ceil_char_boundary(start + MAX_PARAGRAPH_BYTES / 2);
            text[middle..limit]
                .rfind(char::is_whitespace)
                .map_or(limit, |boundary| middle + boundary)
        };
        push_block(
            blocks,
            descriptor,
            offset + start,
            text[start..end].trim(),
            TextBlockKind::Paragraph,
        );
        start = end;
    }
}

fn push_block(
    blocks: &mut Vec<Block>,
    descriptor: &SpineItem,
    offset: usize,
    text: &str,
    kind: TextBlockKind,
) {
    if text.is_empty() {
        return;
    }
    let start = SourceAnchor {
        spine: descriptor.id.clone(),
        node: format!("p{offset}"),
        text_offset: 0,
    };
    let end = SourceAnchor {
        text_offset: text.chars().count() as u64,
        ..start.clone()
    };
    blocks.push(Block::Text(TextBlock {
        kind,
        content: vec![Inline::Text(TextRun {
            text: text.to_owned(),
            style: Default::default(),
            link: None,
        })],
        style: Default::default(),
        source: Some(SourceRange { start, end }),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_legacy_chinese_without_replacing_text() {
        for (text, expected) in [
            (
                "第一章 这是一个故事\n很久以前，有一个小孩在这里长大。".to_owned(),
                GB18030,
            ),
            (
                "第一章 這是一個故事\n很久以前，有一個小孩在這裡長大。".to_owned(),
                BIG5,
            ),
        ] {
            let (encoded, _, malformed) = expected.encode(&text);
            assert!(!malformed);
            assert_eq!(decode(&encoded), Some(text));
        }
        let utf16: Vec<_> = "第一章 正文\n我要讀書。"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode(&utf16).as_deref(), Some("第一章 正文\n我要讀書。"));
        assert!(decode(b"\0\0\0\0\0\0").is_none());
    }

    #[test]
    fn finds_headings_in_order_without_splitting_prose_or_losing_source_ranges() {
        let input = "書名：測試\n作者：匿名\n\n第一卷\n卷首語。\n第一章 出發\n第一章裡的人仍沒有離開。\n正文。\n第二章 你是誰？\n最後一段。";
        let source = open(input.as_bytes(), "test.txt").unwrap();
        assert_eq!(source.book().metadata.title, "測試");
        assert_eq!(source.book().metadata.authors, ["匿名"]);
        let toc = &source.book().table_of_contents;
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].children.len(), 2);
        assert_eq!(toc[0].children[1].label, "第二章 你是誰？");
        let first = source.parse_section(2).unwrap();
        assert_eq!(first.blocks.len(), 3);
        for block in first.blocks {
            let Block::Text(text) = block else {
                panic!("expected text")
            };
            let range = text.source.unwrap();
            assert_eq!(range.start.spine, first.id);
            assert!(range.end.text_offset > 0);
        }
    }

    #[test]
    fn limits_long_chapterless_text_at_character_boundaries() {
        let text = "字".repeat(100_000);
        let source = open(text.as_bytes(), "no-headings.txt").unwrap();
        assert_eq!(source.book().table_of_contents.len(), 1);
        assert!(source.book().sections.len() > 1);
        let output: String = (0..source.book().sections.len())
            .flat_map(|index| source.parse_section(index).unwrap().blocks)
            .flat_map(|block| match block {
                Block::Text(text) => text.content,
                _ => unreachable!(),
            })
            .map(|inline| match inline {
                Inline::Text(text) => text.text,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(output, text);
    }
}
