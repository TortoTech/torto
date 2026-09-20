//! Bounded, deterministic recovery of publication content, never package metadata.
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write as _;

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use scraper::{ElementRef, Html, Node};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub bytes: usize,
    pub depth: usize,
    pub nodes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 64 * 1024 * 1024,
            depth: 512,
            nodes: 500_000,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Html,
    Fb2,
}

pub(crate) fn html(source: &str, limits: Limits) -> Result<Cow<'_, str>, String> {
    normalize(source, limits, Mode::Html)
}
pub(crate) fn fb2(source: &str) -> Result<Cow<'_, str>, String> {
    normalize(source, Limits::default(), Mode::Fb2)
}

fn normalize(source: &str, limits: Limits, mode: Mode) -> Result<Cow<'_, str>, String> {
    preflight(source, limits, mode)?;
    if valid_xml(source, limits).is_ok() {
        return Ok(Cow::Borrowed(source));
    }
    if let Ok(repaired) = repair_xml(source, limits, mode)
        && valid_xml(&repaired, limits).is_ok()
    {
        eprintln!(
            "publication markup: recovered malformed {} content",
            if mode == Mode::Html { "HTML" } else { "FB2" }
        );
        return Ok(Cow::Owned(repaired));
    }
    if mode == Mode::Fb2 {
        return Err("FB2 XML cannot be recovered without guessing its structure".into());
    }
    let document = Html::parse_document(source);
    let body = document
        .select(&scraper::Selector::parse("body").unwrap())
        .next()
        .ok_or("recovery produced no body")?;
    if !body.descendants().any(|node| match node.value() {
        Node::Text(text) => !text.trim().is_empty() && !node.ancestors().any(|ancestor| matches!(ancestor.value(), Node::Element(element) if matches!(element.name(), "script" | "style"))),
        Node::Element(element) => matches!(element.name(), "img" | "svg" | "math" | "hr"),
        _ => false,
    }) { return Err("HTML recovery produced no readable content".into()); }
    let repaired = serialize_html(&document, limits)?;
    valid_xml(&repaired, limits)?;
    eprintln!("publication markup: recovered malformed HTML using HTML5 parsing");
    Ok(Cow::Owned(repaired))
}

// Check before building a DOM, including when tokenization itself fails.
fn preflight(source: &str, limits: Limits, mode: Mode) -> Result<(), String> {
    if source.len() > limits.bytes {
        return Err("markup byte limit exceeded".into());
    }
    if source.bytes().filter(|byte| *byte == b'<').count() > limits.nodes {
        return Err("markup token limit exceeded".into());
    }
    check_declarations(source)?;
    let mut reader = Reader::from_str(source);
    reader.config_mut().check_end_names = false;
    let mut stack = Vec::<String>::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = String::from_utf8_lossy(start.name().as_ref()).to_ascii_lowercase();
                if mode == Mode::Html
                    && let Some(position) = optional_end(&stack, &name)
                {
                    stack.truncate(position);
                }
                if mode != Mode::Html || !is_void(name.as_bytes()) {
                    stack.push(name);
                }
                if stack.len() > limits.depth {
                    return Err("markup depth limit exceeded".into());
                }
            }
            Ok(Event::End(end)) => {
                let name = String::from_utf8_lossy(end.name().as_ref()).to_ascii_lowercase();
                if let Some(position) = stack.iter().rposition(|open| open == &name) {
                    stack.truncate(position);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    Ok(())
}

fn check_declarations(source: &str) -> Result<(), String> {
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find('<') {
        let start = cursor + relative;
        let tail = &source[start..];
        if tail.starts_with("<!--") {
            cursor = start + tail.find("-->").ok_or("unterminated comment")? + 3;
            continue;
        }
        if tail.starts_with("<![CDATA[") {
            cursor = start + tail.find("]]>").ok_or("unterminated CDATA")? + 3;
            continue;
        }
        if tail
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("<!entity"))
        {
            return Err("entity declarations are disabled".into());
        }
        let doctype = tail
            .get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("<!doctype"));
        let mut quote = None;
        let mut end = None;
        for (index, byte) in tail.bytes().enumerate() {
            if matches!(byte, b'\'' | b'"') {
                if quote == Some(byte) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(byte);
                }
            }
            if doctype && quote.is_none() && byte == b'[' {
                return Err("DOCTYPE internal subsets are disabled".into());
            }
            if quote.is_none() && byte == b'>' {
                end = Some(start + index + 1);
                break;
            }
        }
        cursor = match end {
            Some(end) => end,
            None if doctype => return Err("unterminated DOCTYPE".into()),
            None => break,
        };
    }
    Ok(())
}

fn optional_end(stack: &[String], name: &str) -> Option<usize> {
    let barrier: &[&str] = match name {
        "p" | "tr" => &["table"],
        "li" => &["ul", "ol"],
        "dt" | "dd" => &["dl"],
        "td" | "th" => &["tr", "table"],
        _ => return None,
    };
    let position = stack.iter().rposition(|open| {
        open == name
            || matches!(
                (name, open.as_str()),
                ("dt", "dd") | ("dd", "dt") | ("td", "th") | ("th", "td")
            )
    })?;
    (!stack[position + 1..]
        .iter()
        .any(|open| barrier.contains(&open.as_str())))
    .then_some(position)
}

fn valid_xml(source: &str, limits: Limits) -> Result<(), String> {
    if source.len() > limits.bytes {
        return Err("recovered markup byte limit exceeded".into());
    }
    let doc = roxmltree::Document::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            nodes_limit: u32::try_from(limits.nodes).unwrap_or(u32::MAX),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    drop(doc);
    let mut reader = Reader::from_str(source);
    let mut depth = 0usize;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(_) => {
                depth += 1;
                if depth > limits.depth {
                    return Err("markup depth limit exceeded".into());
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}

fn is_void(name: &[u8]) -> bool {
    matches!(
        name,
        b"area"
            | b"base"
            | b"br"
            | b"col"
            | b"embed"
            | b"hr"
            | b"img"
            | b"input"
            | b"link"
            | b"meta"
            | b"param"
            | b"source"
            | b"track"
            | b"wbr"
    )
}

fn recoverable(name: &str, mode: Mode) -> bool {
    match mode {
        Mode::Html => matches!(
            name,
            "div"
                | "span"
                | "p"
                | "section"
                | "article"
                | "blockquote"
                | "b"
                | "i"
                | "em"
                | "strong"
                | "a"
                | "sup"
                | "sub"
                | "ul"
                | "ol"
                | "li"
                | "dl"
                | "dt"
                | "dd"
                | "table"
                | "thead"
                | "tbody"
                | "tfoot"
                | "tr"
                | "td"
                | "th"
        ),
        Mode::Fb2 => matches!(
            name,
            "p" | "emphasis" | "strong" | "strikethrough" | "sub" | "sup" | "code" | "style"
        ),
    }
}

fn repair_xml(source: &str, limits: Limits, mode: Mode) -> Result<String, String> {
    let prepared = if mode == Mode::Fb2 {
        Cow::Owned(fb2_text_entities(source))
    } else {
        Cow::Borrowed(source)
    };
    let source = prepared.as_ref();
    let mut reader = Reader::from_str(source);
    reader.config_mut().check_end_names = false;
    let mut writer = Writer::new(Vec::new());
    let mut stack = Vec::<String>::new();
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let name =
                    String::from_utf8(start.name().as_ref().to_vec()).map_err(|e| e.to_string())?;
                if mode == Mode::Html && optional_end(&stack, &name).is_some() {
                    return Err("HTML optional end tags need HTML5 recovery".into());
                }
                let mut cleaned = BytesStart::new(name.as_str());
                let mut seen = HashSet::new();
                for attribute in start.attributes().with_checks(false) {
                    let attribute = attribute.map_err(|e| e.to_string())?;
                    if seen.insert(attribute.key.as_ref().to_vec()) {
                        cleaned.push_attribute(attribute);
                    } else if mode == Mode::Fb2 {
                        return Err("ambiguous duplicate FB2 attribute".into());
                    }
                }
                let empty = matches!(event, Event::Empty(_))
                    || (mode == Mode::Html && is_void(name.as_bytes()));
                if !empty {
                    stack.push(name.clone());
                }
                writer
                    .write_event(if empty {
                        Event::Empty(cleaned)
                    } else {
                        Event::Start(cleaned)
                    })
                    .map_err(|e| e.to_string())?;
            }
            Event::End(end) => {
                let name =
                    String::from_utf8(end.name().as_ref().to_vec()).map_err(|e| e.to_string())?;
                let ancestor = stack
                    .iter()
                    .rposition(|open| open == &name)
                    .ok_or("unmatched closing tag")?;
                if !stack[ancestor + 1..]
                    .iter()
                    .all(|open| recoverable(open, mode))
                {
                    return Err("ambiguous closing tags".into());
                }
                if mode == Mode::Fb2
                    && ancestor + 1 < stack.len()
                    && !stack.iter().any(|open| open == "body")
                {
                    return Err("FB2 metadata recovery is disabled".into());
                }
                for open in stack[ancestor..].iter().rev() {
                    writer
                        .write_event(Event::End(BytesEnd::new(open.as_str())))
                        .map_err(|e| e.to_string())?;
                }
                stack.truncate(ancestor);
            }
            Event::DocType(_) if mode == Mode::Html => {}
            Event::Text(text) if mode == Mode::Fb2 => {
                let text = std::str::from_utf8(text.as_ref()).map_err(|e| e.to_string())?;
                writer
                    .write_event(Event::Text(BytesText::from_escaped(xml_text_references(
                        text,
                    ))))
                    .map_err(|e| e.to_string())?;
            }
            Event::DocType(_) => return Err("FB2 DOCTYPE is disabled".into()),
            Event::Eof => {
                if !stack.is_empty() {
                    return Err("truncated XML".into());
                }
                break;
            }
            other => writer.write_event(other).map_err(|e| e.to_string())?,
        }
        if writer.get_ref().len() > limits.bytes {
            return Err("recovered markup byte limit exceeded".into());
        }
    }
    String::from_utf8(writer.into_inner()).map_err(|e| e.to_string())
}

fn xml_text_references(text: &str) -> String {
    let mut out = String::new();
    let mut copied = 0;
    for (index, _) in text.match_indices('&') {
        if index < copied {
            continue;
        }
        out.push_str(&text[copied..index]);
        let tail = &text[index + 1..];
        if let Some(end) = tail.find(';').filter(|end| *end < 32) {
            let name = &tail[..end];
            let replacement = match name {
                "nbsp" => Some("&#160;"),
                "mdash" => Some("&#8212;"),
                "ndash" => Some("&#8211;"),
                "hellip" => Some("&#8230;"),
                _ => None,
            };
            if let Some(replacement) = replacement {
                out.push_str(replacement);
                copied = index + end + 2;
                continue;
            }
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '#') && !name.is_empty() {
                out.push('&');
                copied = index + 1;
                continue;
            }
        }
        out.push_str("&amp;");
        copied = index + 1;
    }
    out.push_str(&text[copied..]);
    out
}

fn fb2_text_entities(source: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;
    while cursor < source.len() {
        let Some(relative) = source[cursor..].find('<') else {
            out.push_str(&xml_text_references(&source[cursor..]));
            break;
        };
        let start = cursor + relative;
        out.push_str(&xml_text_references(&source[cursor..start]));
        let tail = &source[start..];
        let end = if tail.starts_with("<![CDATA[") {
            tail.find("]]>").map(|end| start + end + 3)
        } else if tail.starts_with("<!--") {
            tail.find("-->").map(|end| start + end + 3)
        } else {
            let mut quote = None;
            tail.bytes().enumerate().find_map(|(index, byte)| {
                if matches!(byte, b'\'' | b'"') {
                    if quote == Some(byte) {
                        quote = None;
                    } else if quote.is_none() {
                        quote = Some(byte);
                    }
                }
                (byte == b'>' && quote.is_none()).then_some(start + index + 1)
            })
        };
        let end = end.unwrap_or(source.len());
        out.push_str(&source[start..end]);
        cursor = end;
    }
    out
}

// Serialize the HTML5 tree as XML, including foreign namespaces. HTML's own
// serializer leaves void tags and raw-text ampersands unsuitable for roxmltree.
pub(crate) fn serialize_html(document: &Html, limits: Limits) -> Result<String, String> {
    fn element(
        node: ElementRef<'_>,
        out: &mut String,
        parent_ns: &str,
        depth: usize,
        limits: Limits,
    ) -> Result<(), String> {
        if depth > limits.depth {
            return Err("HTML5 tree depth limit exceeded".into());
        }
        let value = node.value();
        let name = value.name();
        let ns = value.name.ns.as_ref();
        out.push('<');
        out.push_str(name);
        let mut attrs = HashSet::new();
        for (key, value) in &value.attrs {
            let key = key.prefix.as_ref().map_or_else(
                || key.local.to_string(),
                |prefix| format!("{prefix}:{}", key.local),
            );
            if !attrs.insert(key.clone()) {
                continue;
            }
            out.push(' ');
            out.push_str(&key);
            out.push_str("=\"");
            out.push_str(&crate::source::escape_attribute(value));
            out.push('"');
        }
        if ns != parent_ns && !attrs.contains("xmlns") {
            out.push_str(" xmlns=\"");
            out.push_str(&crate::source::escape_attribute(ns));
            out.push('"');
        }
        if depth == 1 {
            for (prefix, uri) in [
                ("epub", "http://www.idpf.org/2007/ops"),
                ("xlink", "http://www.w3.org/1999/xlink"),
            ] {
                if !attrs.contains(&format!("xmlns:{prefix}")) {
                    let _ = write!(out, " xmlns:{prefix}=\"{uri}\"");
                }
            }
        }
        if ns == "http://www.w3.org/1999/xhtml" && is_void(name.as_bytes()) {
            out.push_str("/>");
            return Ok(());
        }
        out.push('>');
        for child in node.children() {
            if let Some(child) = ElementRef::wrap(child) {
                element(child, out, ns, depth + 1, limits)?;
            } else if let Node::Text(text) = child.value() {
                out.push_str(&crate::source::escape_text(text));
            }
            if out.len() > limits.bytes {
                return Err("HTML5 output byte limit exceeded".into());
            }
        }
        out.push_str("</");
        out.push_str(name);
        out.push('>');
        Ok(())
    }
    let mut output = String::new();
    element(document.root_element(), &mut output, "", 1, limits)?;
    if output.len() > limits.bytes {
        return Err("HTML5 output byte limit exceeded".into());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(xml: &str) -> String {
        roxmltree::Document::parse(xml)
            .unwrap()
            .descendants()
            .filter(|node| node.is_text())
            .filter_map(|node| node.text())
            .collect()
    }

    #[test]
    fn valid_content_is_byte_identical_for_stable_source_locations() {
        let source = r##"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><p id="a" class="quote">A &amp; B<br/><a epub:type="noteref" href="#note">1</a></p><aside id="note">Note</aside></body></html>"##;
        assert!(matches!(
            html(source, Limits::default()).unwrap(),
            Cow::Borrowed(_)
        ));
        let example = "<html><body><p><![CDATA[<!ENTITY example 'text'>]]></p><!-- <!DOCTYPE sample [ignored]> --></body></html>";
        assert!(matches!(
            html(example, Limits::default()).unwrap(),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn repairs_containers_void_tags_and_duplicate_attributes_without_losing_anchors() {
        let source = r##"<html><body><section id="chapter"><div class="a" class="b"><p id="p">A<br>B<img src="a.png"><span>C</p><a href="#note">1</a><p id="note">Note</body></html>"##;
        let output = html(source, Limits::default()).unwrap();
        let doc = roxmltree::Document::parse(&output).unwrap();
        assert_eq!(text(&output), "ABC1Note");
        for id in ["chapter", "p", "note"] {
            assert!(
                doc.descendants()
                    .any(|node| node.attribute("id") == Some(id))
            );
        }
        assert!(
            doc.descendants()
                .any(|node| node.attribute("href") == Some("#note"))
        );
        assert!(
            doc.descendants()
                .any(|node| node.attribute("src") == Some("a.png"))
        );
        assert!(
            doc.descendants()
                .any(|node| node.attribute("class") == Some("a"))
        );
        assert_eq!(output, html(source, Limits::default()).unwrap());
    }

    #[test]
    fn html5_recovers_optional_ends_crossed_formatting_and_entities() {
        let source = "<html><body><p>First &nbsp; &copy; & raw<p>Second <b><i>bold</b> italic</i><ul><li>One<li>Two</ul><table><tr><td>A<td>B</table></body></html>";
        let output = html(source, Limits::default()).unwrap();
        let doc = roxmltree::Document::parse(&output).unwrap();
        assert_eq!(
            doc.descendants()
                .filter(|node| node.has_tag_name("li"))
                .count(),
            2
        );
        assert_eq!(
            doc.descendants()
                .filter(|node| node.has_tag_name("td"))
                .count(),
            2
        );
        assert!(text(&output).contains("First \u{a0} © & raw"));
        assert!(text(&output).contains("Second bold italic"));
    }

    #[test]
    fn foreign_content_and_style_survive_html5_serialization() {
        let source = r##"<html xmlns:epub="http://www.idpf.org/2007/ops"><head><style>p > a { color: red; }</style></head><body><p>&nbsp;<a epub:type="noteref" href="#n">N</a><svg xmlns="http://www.w3.org/2000/svg"><image xmlns:xlink="http://www.w3.org/1999/xlink" xlink:href="a.png"/></svg><math xmlns="http://www.w3.org/1998/Math/MathML"><mi>x</mi></math><p id="n">Note</body></html>"##;
        let output = html(source, Limits::default()).unwrap();
        let doc = roxmltree::Document::parse(&output).unwrap();
        assert!(
            doc.descendants()
                .any(|node| node.has_tag_name(("http://www.w3.org/2000/svg", "svg")))
        );
        assert!(
            doc.descendants()
                .any(|node| node.has_tag_name(("http://www.w3.org/1998/Math/MathML", "mi")))
        );
        assert!(
            doc.descendants().any(
                |node| node.attribute(("http://www.w3.org/1999/xlink", "href")) == Some("a.png")
            )
        );
        assert!(doc.descendants().any(|node| {
            node.attribute(("http://www.idpf.org/2007/ops", "type")) == Some("noteref")
        }));
        assert!(text(&output).contains("p > a"));
    }

    #[test]
    fn fb2_repairs_body_without_reinterpreting_metadata_binary_or_cdata() {
        let source = r##"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink"><description><title-info><book-title>Book</book-title></title-info></description><body><section id="s"><p>A & B&nbsp;C<emphasis>italic</section></body><binary id="pic" content-type="image/png">YWJj</binary></FictionBook>"##;
        let output = fb2(source).unwrap();
        assert!(text(&output).contains("A & B\u{a0}Citalic"));
        assert!(output.contains("YWJj"));
        assert_eq!(
            fb2_text_entities("<p><![CDATA[A & B]]></p>"),
            "<p><![CDATA[A & B]]></p>"
        );
        assert!(fb2("<FictionBook><description><title-info><book-title>Book</description><body/></FictionBook>").is_err());
        assert!(
            fb2("<FictionBook><body><section><title>Title<p>Text</section></body></FictionBook>")
                .is_err()
        );
    }

    #[test]
    fn recovery_does_not_bypass_resource_limits_or_return_an_empty_success() {
        for source in [
            "<!DOCTYPE html [<!ENTITY x 'value'>]><p>&x;",
            "<!ENTITY x SYSTEM 'file:///etc/passwd'><p>x",
            "<html><head><title>Only metadata",
        ] {
            assert!(html(source, Limits::default()).is_err(), "{source}");
        }
        let limits = Limits {
            bytes: 80,
            depth: 4,
            nodes: 20,
        };
        assert!(html(&"x".repeat(81), limits).is_err());
        assert!(html("<html><body><div><span><em>x", limits).is_err());
        let list = format!(
            "<html><body><ul>{}</ul></body></html>",
            "<li>x".repeat(1000)
        );
        let output = html(&list, Limits::default()).unwrap();
        assert_eq!(text(&output).chars().count(), 1000);
    }
}
