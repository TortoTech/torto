//! Shared assistant Markdown normalization; native UIs own presentation.
use crate::plugins::CHAT_CITATION_PREFIX;
use std::borrow::Cow;

pub fn markdown_fence(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start();
    let marker = *trimmed.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = trimmed.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some((marker, length))
}

pub fn normalize_loose_strong_delimiters(source: &str) -> Cow<'_, str> {
    let mut output = String::with_capacity(source.len());
    let mut fence = None;
    let mut changed = false;
    for line in source.split_inclusive('\n') {
        let line_content = line.trim_end_matches(['\r', '\n']);
        let line_fence = markdown_fence(line_content).map(|(marker, _)| marker);
        if let Some(active) = fence {
            output.push_str(line);
            if line_fence == Some(active) {
                fence = None;
            }
        } else if let Some(opening) = line_fence {
            fence = Some(opening);
            output.push_str(line);
        } else {
            let normalized = normalize_loose_strong_line(line);
            changed |= matches!(normalized, Cow::Owned(_));
            let cjk = normalize_cjk_strong_boundaries(normalized.as_ref());
            changed |= matches!(cjk, Cow::Owned(_));
            output.push_str(cjk.as_ref());
        }
    }
    if changed {
        Cow::Owned(output)
    } else {
        Cow::Borrowed(source)
    }
}

pub fn strong_delimiter_offsets(line: &str) -> Vec<usize> {
    let bytes = line.as_bytes();
    let mut delimiters = Vec::new();
    let mut code_ticks = None;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let count = bytes[index..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            if code_ticks == Some(count) {
                code_ticks = None;
            } else if code_ticks.is_none() {
                code_ticks = Some(count);
            }
            index += count;
            continue;
        }
        if code_ticks.is_none()
            && bytes[index..].starts_with(b"**")
            && bytes.get(index.wrapping_sub(1)) != Some(&b'*')
            && bytes.get(index + 2) != Some(&b'*')
            && !is_escaped(bytes, index)
        {
            delimiters.push(index);
            index += 2;
            continue;
        }
        index += 1;
    }

    delimiters
}

// CommonMark treats punctuation next to a Han character differently from
// punctuation next to whitespace. Encode only the outside Han boundary as a
// character reference: the parser sees a punctuation boundary but emits the
// exact original character, without adding visible or invisible whitespace.
pub fn normalize_cjk_strong_boundaries(line: &str) -> Cow<'_, str> {
    let is_han = |c: char| matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af);
    let punctuation = |c: char| !c.is_alphanumeric() && !c.is_whitespace();
    let mut positions = std::collections::BTreeMap::new();
    for pair in strong_delimiter_offsets(line).chunks_exact(2) {
        let (open, close) = (pair[0], pair[1]);
        let inner = &line[open + 2..close];
        if inner.chars().next().is_some_and(punctuation) {
            if let Some((index, c)) = line[..open]
                .char_indices()
                .next_back()
                .filter(|(_, c)| is_han(*c))
            {
                positions.insert(index, c);
            }
        }
        if inner.chars().next_back().is_some_and(punctuation) {
            if let Some(c) = line[close + 2..].chars().next().filter(|c| is_han(*c)) {
                positions.insert(close + 2, c);
            }
        }
    }
    if positions.is_empty() {
        return Cow::Borrowed(line);
    }
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    for (index, c) in positions {
        output.push_str(&line[cursor..index]);
        output.push_str(&format!("&#x{:x};", c as u32));
        cursor = index + c.len_utf8();
    }
    output.push_str(&line[cursor..]);
    Cow::Owned(output)
}

pub fn normalize_loose_strong_line(line: &str) -> Cow<'_, str> {
    let delimiters = strong_delimiter_offsets(line);
    let mut replacements = Vec::new();
    for pair in delimiters.chunks_exact(2) {
        let open = pair[0];
        let close = pair[1];
        let inner = &line[open + 2..close];
        let trimmed_start = inner.trim_start_matches(char::is_whitespace);
        let leading_len = inner.len() - trimmed_start.len();
        let trimmed = trimmed_start.trim_end_matches(char::is_whitespace);
        let trailing_len = trimmed_start.len() - trimmed.len();
        if trimmed.is_empty() || (leading_len == 0 && trailing_len == 0) {
            continue;
        }
        replacements.push((open, close + 2, leading_len, trailing_len));
    }
    if replacements.is_empty() {
        return Cow::Borrowed(line);
    }

    let mut output = String::with_capacity(line.len());
    let mut cursor = 0;
    for (open, end, leading_len, trailing_len) in replacements {
        let before = &line[cursor..open];
        output.push_str(before);
        let inner = &line[open + 2..end - 2];
        let leading = &inner[..leading_len];
        let content_end = inner.len().saturating_sub(trailing_len);
        let content = &inner[leading_len..content_end];
        let trailing = &inner[content_end..];
        if !leading.is_empty() && !output.chars().next_back().is_some_and(char::is_whitespace) {
            output.push_str(leading);
        }
        output.push_str("**");
        output.push_str(content);
        output.push_str("**");
        if !trailing.is_empty() && !line[end..].chars().next().is_some_and(char::is_whitespace) {
            output.push_str(trailing);
        }
        cursor = end;
    }
    output.push_str(&line[cursor..]);
    Cow::Owned(output)
}

pub fn normalize_math_delimiters(source: &str) -> Cow<'_, str> {
    let mut output = String::with_capacity(source.len());
    let mut fence = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let line_fence = if trimmed.starts_with("```") {
            Some(b'`')
        } else if trimmed.starts_with("~~~") {
            Some(b'~')
        } else {
            None
        };
        if let Some(active) = fence {
            output.push_str(line);
            if line_fence == Some(active) {
                fence = None;
            }
        } else if let Some(opening) = line_fence {
            fence = Some(opening);
            output.push_str(line);
        } else {
            normalize_math_line(line, &mut output);
        }
    }
    if output == source {
        Cow::Borrowed(source)
    } else {
        Cow::Owned(output)
    }
}

pub fn normalize_math_line(line: &str, output: &mut String) {
    let mut converted = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut in_code = false;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let start = index;
            while index < bytes.len() && bytes[index] == b'`' {
                index += 1;
            }
            converted.push_str(&line[start..index]);
            in_code = !in_code;
            continue;
        }
        if !in_code && bytes[index] == b'\\' && index + 1 < bytes.len() {
            let replacement = match bytes[index + 1] {
                b'(' | b')' => Some("$"),
                b'[' | b']' => Some("$$"),
                _ => None,
            };
            if let Some(replacement) = replacement {
                converted.push_str(replacement);
                index += 2;
                continue;
            }
        }
        let next = line[index..]
            .char_indices()
            .nth(1)
            .map_or(bytes.len(), |(offset, _)| index + offset);
        converted.push_str(&line[index..next]);
        index = next;
    }
    normalize_loose_math_spacing(&converted, output);
}

pub fn normalize_loose_math_spacing(line: &str, output: &mut String) {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut in_code = false;
    while index < bytes.len() {
        if bytes[index] == b'`' {
            let start = index;
            while index < bytes.len() && bytes[index] == b'`' {
                index += 1;
            }
            output.push_str(&line[start..index]);
            in_code = !in_code;
            continue;
        }
        if !in_code && bytes[index] == b'$' && !is_escaped(bytes, index) {
            let delimiter_len = if bytes.get(index + 1) == Some(&b'$') {
                2
            } else {
                1
            };
            if let Some(end) =
                find_closing_math_delimiter(bytes, index + delimiter_len, delimiter_len)
            {
                let inner = &line[index + delimiter_len..end];
                let trimmed = inner.trim_matches([' ', '\t']);
                if !trimmed.is_empty() && trimmed.len() != inner.len() {
                    let delimiter = if delimiter_len == 2 { "$$" } else { "$" };
                    output.push_str(delimiter);
                    output.push_str(trimmed);
                    output.push_str(delimiter);
                    index = end + delimiter_len;
                    continue;
                }
            }
        }
        let next = line[index..]
            .char_indices()
            .nth(1)
            .map_or(bytes.len(), |(offset, _)| index + offset);
        output.push_str(&line[index..next]);
        index = next;
    }
}

pub fn find_closing_math_delimiter(
    bytes: &[u8],
    mut index: usize,
    delimiter_len: usize,
) -> Option<usize> {
    while index + delimiter_len <= bytes.len() {
        if bytes[index] == b'$'
            && !is_escaped(bytes, index)
            && (delimiter_len == 2) == (bytes.get(index + 1) == Some(&b'$'))
            && (delimiter_len == 2
                || (bytes.get(index.wrapping_sub(1)) != Some(&b'$')
                    && bytes.get(index + 1) != Some(&b'$')))
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

pub fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = index;
    while cursor > 0 && bytes[cursor - 1] == b'\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

pub fn normalize_internal_citations(source: &str) -> Cow<'_, str> {
    match normalize_openai_citation_markers(source) {
        Cow::Borrowed(source) => normalize_legacy_internal_citations(source),
        Cow::Owned(source) => Cow::Owned(
            normalize_legacy_internal_citations(&source)
                .as_ref()
                .to_owned(),
        ),
    }
}

pub fn normalize_legacy_internal_citations(source: &str) -> Cow<'_, str> {
    match normalize_ref_citations(source) {
        Cow::Borrowed(source) => normalize_malformed_internal_citations(source),
        Cow::Owned(source) => Cow::Owned(
            normalize_malformed_internal_citations(&source)
                .as_ref()
                .to_owned(),
        ),
    }
}

pub fn normalize_openai_citation_markers(source: &str) -> Cow<'_, str> {
    const OPEN: &str = "【";
    const CLOSE: &str = "】";
    let mut search_start = 0;
    let mut copy_start = 0;
    let mut output = None::<String>;

    while let Some(relative_open) = source[search_start..].find(OPEN) {
        let open = search_start + relative_open;
        let content_start = open + OPEN.len();
        let Some(relative_close) = source[content_start..].find(CLOSE) else {
            break;
        };
        let close = content_start + relative_close;
        // Never search past this marker's closing bracket for a valid suffix:
        // malformed model output must not consume prose or a later citation.
        let next = close + CLOSE.len();
        let marker = &source[content_start..close];
        if let Some(nested_open) = marker.rfind(OPEN) {
            search_start = content_start + nested_open;
            continue;
        }
        let Some(body) = marker.strip_suffix("†source") else {
            search_start = next;
            continue;
        };
        let body = body.trim();
        let locator = body
            .strip_prefix(CHAT_CITATION_PREFIX)
            .map_or_else(|| format!("{CHAT_CITATION_PREFIX}{body}"), str::to_owned);
        if !is_internal_citation_locator(&locator) {
            search_start = next;
            continue;
        }

        let output = output.get_or_insert_with(|| String::with_capacity(source.len()));
        output.push_str(&source[copy_start..open]);
        output.push_str("[source](<");
        output.push_str(&locator);
        output.push_str(">)");
        copy_start = next;
        search_start = next;
    }

    output.map_or(Cow::Borrowed(source), |mut output| {
        output.push_str(&source[copy_start..]);
        Cow::Owned(output)
    })
}

pub fn normalize_ref_citations(source: &str) -> Cow<'_, str> {
    const OPEN: &str = "<ref>";
    const CLOSE: &str = "</ref>";
    let mut search_start = 0;
    let mut copy_start = 0;
    let mut output = None::<String>;

    while let Some(relative_open) = source[search_start..].find(OPEN) {
        let open = search_start + relative_open;
        let content_start = open + OPEN.len();
        let Some(relative_close) = source[content_start..].find(CLOSE) else {
            break;
        };
        let close = content_start + relative_close;
        let locator = source[content_start..close].trim();
        let next = close + CLOSE.len();
        if !is_internal_citation_locator(locator)
            || locator
                .chars()
                .any(|character| character.is_whitespace() || matches!(character, '<' | '>'))
        {
            search_start = next;
            continue;
        }

        let output = output.get_or_insert_with(|| String::with_capacity(source.len()));
        output.push_str(&source[copy_start..open]);
        output.push_str("[ref](<");
        output.push_str(locator);
        output.push_str(">)");
        copy_start = next;
        search_start = next;
    }

    output.map_or(Cow::Borrowed(source), |mut output| {
        output.push_str(&source[copy_start..]);
        Cow::Owned(output)
    })
}

pub fn normalize_malformed_internal_citations(source: &str) -> Cow<'_, str> {
    const LINK_OPEN: &str = "](link://j/";
    let mut search_start = 0;
    let mut copy_start = 0;
    let mut output = None::<String>;
    while let Some(relative_start) = source[search_start..].find(LINK_OPEN) {
        let link_start = search_start + relative_start + 2;
        let mut malformed_close = None;
        for (offset, character) in source[link_start..].char_indices() {
            match character {
                ')' => break,
                ']' => {
                    malformed_close = Some(link_start + offset);
                    break;
                }
                character if character.is_whitespace() || matches!(character, '(' | '<' | '>') => {
                    break;
                }
                _ => {}
            }
        }
        let Some(close) = malformed_close else {
            search_start = link_start;
            continue;
        };
        let locator = &source[link_start..close];
        if !is_internal_citation_locator(locator) {
            search_start = close + 1;
            continue;
        }
        let output = output.get_or_insert_with(|| String::with_capacity(source.len()));
        output.push_str(&source[copy_start..close]);
        output.push(')');
        copy_start = close + 1;
        search_start = copy_start;
    }
    output.map_or(Cow::Borrowed(source), |mut output| {
        output.push_str(&source[copy_start..]);
        Cow::Owned(output)
    })
}

pub fn is_internal_citation_locator(locator: &str) -> bool {
    let Some(remainder) = locator.strip_prefix(CHAT_CITATION_PREFIX) else {
        return false;
    };
    let (section, node) = remainder
        .split_once('/')
        .map_or((remainder, None), |(section, node)| (section, Some(node)));
    !section.is_empty()
        && section.bytes().all(|byte| byte.is_ascii_digit())
        && node.is_none_or(|node| {
            !node.is_empty()
                && !node.chars().any(|c| {
                    c.is_whitespace()
                        || matches!(c, '【' | '】' | '†' | '<' | '>' | '[' | ']' | '(' | ')')
                })
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatCitation {
    pub section_index: usize,
    pub node: Option<String>,
}

pub fn parse_chat_citation(locator: &str) -> Option<ChatCitation> {
    if !is_internal_citation_locator(locator) {
        return None;
    }
    let remainder = locator.strip_prefix(CHAT_CITATION_PREFIX)?;
    let (section, node) = remainder
        .split_once('/')
        .map_or((remainder, None), |(section, node)| (section, Some(node)));
    let section_index = section.parse().ok()?;
    let node = node
        .filter(|node| !node.is_empty())
        .and_then(|node| {
            percent_encoding::percent_decode_str(node)
                .decode_utf8()
                .ok()
        })
        .map(std::borrow::Cow::into_owned);
    Some(ChatCitation {
        section_index,
        node,
    })
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MarkdownRun {
    pub text: String,
    pub style: u8,
    pub link: String,
    pub math: bool,
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MarkdownBlock {
    pub kind: String,
    pub level: usize,
    pub prefix: String,
    pub runs: Vec<MarkdownRun>,
    pub rows: Vec<Vec<Vec<MarkdownRun>>>,
}
/// Parse once on a worker. The result contains no executable HTML or remote assets.
pub fn parse(source: &str) -> Vec<MarkdownBlock> {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
    let strong = normalize_loose_strong_delimiters(source);
    let math = normalize_math_delimiters(&strong);
    let normalized = normalize_internal_citations(&math);
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH;
    let mut blocks = Vec::new();
    let mut current = MarkdownBlock::default();
    let mut table = None::<MarkdownBlock>;
    let mut row = Vec::new();
    let mut style = [0u32; 3];
    let mut links = Vec::<String>::new();
    let mut lists = Vec::<Option<u64>>::new();
    let mut prefix = String::new();
    let mut quotes = 0usize;
    fn flush(current: &mut MarkdownBlock, blocks: &mut Vec<MarkdownBlock>) {
        if !current.runs.is_empty() || current.kind == "rule" {
            if current.kind.is_empty() {
                current.kind = "paragraph".into();
            }
            blocks.push(std::mem::take(current));
        } else {
            *current = MarkdownBlock::default();
        }
    }
    for event in Parser::new_ext(&normalized, options) {
        let mut text = None;
        let mut is_math = false;
        let mut extra = 0;
        match event {
            Event::Start(Tag::Paragraph) => {
                flush(&mut current, &mut blocks);
                current.kind = if quotes > 0 { "quote" } else { "paragraph" }.into();
                current.level = lists.len().saturating_sub(1);
                current.prefix = std::mem::take(&mut prefix);
            }
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut current, &mut blocks);
                current.kind = "heading".into();
                current.level = level as usize;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut current, &mut blocks);
                current.kind = "code".into();
                current.prefix = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    _ => String::new(),
                };
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush(&mut current, &mut blocks);
                quotes += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut current, &mut blocks);
                quotes = quotes.saturating_sub(1);
            }
            Event::Start(Tag::List(start)) => {
                flush(&mut current, &mut blocks);
                lists.push(start);
            }
            Event::Start(Tag::Item) => {
                flush(&mut current, &mut blocks);
                prefix = lists
                    .last()
                    .and_then(|v| *v)
                    .map_or_else(|| "• ".into(), |n| format!("{n}. "));
                current.prefix = prefix.clone();
                current.level = lists.len().saturating_sub(1);
                current.kind = if quotes > 0 { "quote" } else { "paragraph" }.into();
            }
            Event::End(TagEnd::Item) => {
                flush(&mut current, &mut blocks);
                prefix.clear();
                if let Some(Some(n)) = lists.last_mut() {
                    *n += 1;
                }
            }
            Event::End(TagEnd::List(_)) => {
                flush(&mut current, &mut blocks);
                lists.pop();
            }
            Event::Start(Tag::Strong) => style[0] += 1,
            Event::End(TagEnd::Strong) => style[0] = style[0].saturating_sub(1),
            Event::Start(Tag::Emphasis) => style[1] += 1,
            Event::End(TagEnd::Emphasis) => style[1] = style[1].saturating_sub(1),
            Event::Start(Tag::Strikethrough) => style[2] += 1,
            Event::End(TagEnd::Strikethrough) => style[2] = style[2].saturating_sub(1),
            Event::Start(Tag::Link { dest_url, .. }) => {
                let link = dest_url.to_string();
                let safe = is_internal_citation_locator(&link)
                    || reqwest::Url::parse(&link)
                        .is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
                links.push(if safe { link } else { String::new() });
            }
            Event::End(TagEnd::Link) => {
                links.pop();
            }
            Event::Start(Tag::Table(_)) => {
                flush(&mut current, &mut blocks);
                table = Some(MarkdownBlock {
                    kind: "table".into(),
                    ..Default::default()
                });
            }
            Event::Start(Tag::TableHead | Tag::TableRow) => row.clear(),
            Event::Start(Tag::TableCell) => current = MarkdownBlock::default(),
            Event::End(TagEnd::TableCell) => row.push(std::mem::take(&mut current.runs)),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if let Some(table) = &mut table {
                    table.rows.push(std::mem::take(&mut row));
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = table.take() {
                    blocks.push(table);
                }
                current = MarkdownBlock::default();
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock) => {
                flush(&mut current, &mut blocks)
            }
            Event::Text(value) => text = Some(value.to_string()),
            Event::Code(value) => {
                text = Some(value.to_string());
                extra = 8;
            }
            Event::InlineMath(value) => {
                text = Some(value.to_string());
                is_math = true;
            }
            Event::DisplayMath(value) => {
                flush(&mut current, &mut blocks);
                current.kind = "formula".into();
                text = Some(value.to_string());
                is_math = true;
            }
            Event::SoftBreak | Event::HardBreak => text = Some("\n".into()),
            Event::TaskListMarker(checked) => text = Some(if checked { "☑ " } else { "☐ " }.into()),
            Event::Rule => {
                flush(&mut current, &mut blocks);
                current.kind = "rule".into();
                flush(&mut current, &mut blocks);
            }
            Event::Html(value) | Event::InlineHtml(value) => {
                text = Some(value.to_string());
                extra = 8;
            }
            _ => {}
        }
        if let Some(mut text) = text {
            let link = links.last().cloned().unwrap_or_default();
            if is_internal_citation_locator(&link) {
                text = "↗".into();
            }
            let flags = u8::from(style[0] > 0)
                | (u8::from(style[1] > 0) << 1)
                | (u8::from(style[2] > 0) << 2)
                | extra
                | if current.kind == "code" { 8 } else { 0 };
            current.runs.push(MarkdownRun {
                text,
                style: flags,
                link,
                math: is_math,
            });
            if current.kind == "formula" {
                flush(&mut current, &mut blocks);
            }
        }
    }
    flush(&mut current, &mut blocks);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_document_preserves_math_tables_and_source_links() {
        let parsed = parse(
            "## 标题\n\n**重点** *强调* `x` $x^2$【2/n17†source】\n\n> 引用\n\n- 一\n- 二\n\n|A|B|\n|-|-|\n|1|2|\n\n```rust\nlet x = \"$literal$\";\n```\n\n[bad](javascript:alert) [ok](https://example.com)",
        );
        assert_eq!(parsed[0].kind, "heading");
        let runs = &parsed[1].runs;
        assert!(runs.iter().any(|r| r.style & 1 != 0 && r.text == "重点"));
        assert!(runs.iter().any(|r| r.math && r.text == "x^2"));
        assert!(
            runs.iter()
                .any(|r| r.link == "link://j/2/n17" && r.text == "↗")
        );
        assert!(parsed.iter().any(|b| b.kind == "quote"));
        assert_eq!(parsed.iter().filter(|b| b.prefix == "• ").count(), 2);
        let table = parsed.iter().find(|b| b.kind == "table").unwrap();
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1][1][0].text, "2");
        let code = parsed.iter().find(|b| b.kind == "code").unwrap();
        assert!(code.runs.iter().all(|r| !r.math));
        assert!(
            parsed
                .iter()
                .flat_map(|b| &b.runs)
                .all(|r| !r.link.starts_with("javascript:"))
        );
    }
}
