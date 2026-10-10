use std::collections::HashMap;
use std::sync::Arc;

use rebook_html::parse_section_with_hints_and_image_classifier;
use rebook_publication::{
    Block, Book, BookSource, ImageBlock, ImageStyle, Inline, Metadata, PublicationError,
    PublicationId, PublicationUrl, RenditionLayout, Resource, Section, SpineItem, SpineItemId,
    TableOfContentsOrigin, TextBlock, TextBlockKind, TocEntry, heading_ordinal_key,
    promote_single_toc_root,
};

use crate::{BookFormat, FormatError, conversion_error};

pub(crate) struct SourceBook {
    pub id: String,
    pub metadata: Metadata,
    pub sections: Vec<SourceSection>,
    pub table_of_contents: Vec<SourceTocEntry>,
    pub resources: Vec<SourceResource>,
    pub cover_path: Option<String>,
}

pub(crate) struct SourceSection {
    pub title: String,
    pub content: SectionContent,
    pub linear: bool,
    pub properties: Vec<String>,
}

pub(crate) enum SectionContent {
    Html(String),
    Image { resource_path: String, alt: String },
}

pub(crate) struct SourceResource {
    pub path: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct SourceTocEntry {
    pub label: String,
    pub href: String,
    pub children: Vec<SourceTocEntry>,
}

pub(crate) struct DirectBookSource {
    book: Book,
    table_of_contents_origin: TableOfContentsOrigin,
    sections: Vec<SectionContent>,
    resources: HashMap<String, StoredResource>,
    toc_heading_hints: HashMap<String, Vec<TocHeadingHint>>,
    html_context: crate::HtmlContext,
    cover_page: Option<PublicationUrl>,
    fragment_sections: HashMap<String, Option<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TocHeadingHint {
    label: String,
    fragment: Option<String>,
    level: u8,
}

const PATH_ONLY_HEADING_SEARCH_BLOCKS: usize = 8;

pub(crate) fn collect_toc_heading_hints(
    entries: &[TocEntry],
) -> HashMap<String, Vec<TocHeadingHint>> {
    fn visit(entries: &[TocEntry], depth: u8, hints: &mut HashMap<String, Vec<TocHeadingHint>>) {
        for entry in entries {
            if let Some(href) = &entry.href {
                let label = normalize_heading_text(&entry.label);
                if !label.is_empty() {
                    hints
                        .entry(href.path().to_owned())
                        .or_default()
                        .push(TocHeadingHint {
                            label,
                            fragment: href.fragment().map(str::to_owned),
                            level: depth.clamp(1, 6),
                        });
                }
            }
            visit(&entry.children, depth.saturating_add(1), hints);
        }
    }

    let mut hints = HashMap::new();
    visit(entries, 1, &mut hints);
    hints
}

pub(crate) fn promote_toc_headings(section: &mut Section, hints: &[TocHeadingHint]) {
    for hint in hints {
        let anchored_index = hint.fragment.as_deref().and_then(|fragment| {
            let node = section
                .anchors
                .iter()
                .find(|anchor| anchor.fragment == fragment)?
                .source
                .node
                .as_str();
            section.blocks.iter().position(|block| {
                matches!(
                    block,
                    Block::Text(text)
                        if text.source.as_ref().is_some_and(|source| source.start.node == node)
                )
            })
        });

        let search_range = heading_search_range(section.blocks.len(), anchored_index);
        let mut matches = search_range
            .clone()
            .filter_map(|index| heading_candidate(&section.blocks, index, hint))
            .collect::<Vec<_>>();
        // A title-only TOC label can identify the second half of a split
        // heading. Prefer its validated pair over that same title on its own.
        let paired_titles = matches
            .iter()
            .filter_map(|candidate| match candidate {
                HeadingCandidate::Split { title, .. } => Some(*title),
                HeadingCandidate::Single(_) => None,
            })
            .collect::<Vec<_>>();
        matches.retain(|candidate| !matches!(candidate, HeadingCandidate::Single(index) if paired_titles.contains(index)));
        let [candidate] = matches.as_slice() else {
            continue;
        };
        match *candidate {
            HeadingCandidate::Single(index) => {
                if let Some(Block::Text(text)) = section.blocks.get_mut(index)
                    && text.kind == TextBlockKind::Paragraph
                {
                    text.kind = TextBlockKind::Heading(hint.level);
                }
            }
            HeadingCandidate::Split { ordinal, title } => {
                let Ok([Block::Text(ordinal), Block::Text(title)]) =
                    section.blocks.get_disjoint_mut([ordinal, title])
                else {
                    continue;
                };
                ordinal.kind = TextBlockKind::HeadingOrdinal(hint.level);
                title.kind = TextBlockKind::Heading(hint.level);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadingCandidate {
    Single(usize),
    Split { ordinal: usize, title: usize },
}

fn heading_search_range(
    block_count: usize,
    anchored_index: Option<usize>,
) -> std::ops::Range<usize> {
    if let Some(index) = anchored_index {
        index.saturating_sub(1)..index.saturating_add(2).min(block_count)
    } else {
        0..PATH_ONLY_HEADING_SEARCH_BLOCKS.min(block_count)
    }
}

fn heading_candidate(
    blocks: &[Block],
    index: usize,
    hint: &TocHeadingHint,
) -> Option<HeadingCandidate> {
    let block = heading_text_block(blocks.get(index)?)?;
    if heading_labels_match(&normalized_text_block(block), &hint.label) {
        return Some(HeadingCandidate::Single(index));
    }

    let title_index = index.checked_add(1)?;
    let title = heading_text_block(blocks.get(title_index)?)?;
    let ordinal = normalized_text_block(block);
    let title_text = normalized_text_block(title);
    (heading_ordinal_key(&ordinal).is_some()
        && title_text.chars().any(char::is_alphabetic)
        && (split_heading_matches(&ordinal, &title_text, &hint.label)
            || block.kind.is_heading()
                && title.kind.is_heading()
                && heading_labels_match(&title_text, &hint.label)))
    .then_some(HeadingCandidate::Split {
        ordinal: index,
        title: title_index,
    })
}

fn heading_text_block(block: &Block) -> Option<&TextBlock> {
    match block {
        Block::Text(text)
            if matches!(
                text.kind,
                TextBlockKind::Paragraph
                    | TextBlockKind::Heading(_)
                    | TextBlockKind::HeadingOrdinal(_)
            ) =>
        {
            Some(text)
        }
        _ => None,
    }
}

fn split_heading_matches(ordinal: &str, title: &str, hint: &str) -> bool {
    if title.is_empty() {
        return false;
    }
    let Some(ordinal_key) = heading_ordinal_key(ordinal) else {
        return false;
    };
    let Some((hint_ordinal, hint_title)) = split_heading_label(hint) else {
        return false;
    };
    ordinal_key == hint_ordinal && heading_labels_match(title, hint_title)
}

fn heading_labels_match(left: &str, right: &str) -> bool {
    heading_match_key(left) == heading_match_key(right)
}

fn heading_match_key(text: &str) -> String {
    text.chars()
        .filter(|character| {
            !character.is_whitespace()
                && !matches!(
                    character,
                    ':' | '.'
                        | '-'
                        | '\u{2010}'
                        | '\u{2011}'
                        | '\u{2012}'
                        | '\u{2013}'
                        | '\u{2014}'
                )
        })
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn split_heading_label(label: &str) -> Option<(String, &str)> {
    let trimmed = label.trim();
    // Prefer the longest valid prefix: dotted ordinals and number words can
    // contain punctuation/spaces themselves. Require a textual title after it.
    trimmed
        .char_indices()
        .skip(1)
        .take(96)
        .filter_map(|(index, _)| {
            let ordinal = heading_ordinal_key(&trimmed[..index])?;
            let title = trimmed[index..].trim_start_matches(|ch: char| {
                ch.is_whitespace() || matches!(ch, ':' | '.' | '-' | '–' | '—')
            });
            (!title.is_empty() && title.chars().any(char::is_alphabetic))
                .then_some((ordinal, title))
        })
        .last()
}

fn normalized_text_block(block: &TextBlock) -> String {
    let mut text = String::new();
    for inline in &block.content {
        match inline {
            Inline::Ruby(run) => run.base.iter().for_each(|r| text.push_str(&r.text)),
            Inline::Text(run) => text.push_str(&run.text),
            Inline::Math(run) => text.push_str(&run.latex),
            Inline::Image(_) => {}
            Inline::Break => text.push(' '),
        }
    }
    normalize_heading_text(&text)
}

fn normalize_heading_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

struct StoredResource {
    href: PublicationUrl,
    media_type: String,
    bytes: Arc<[u8]>,
}

impl DirectBookSource {
    pub(crate) fn open(book: SourceBook, format: BookFormat) -> Result<Self, FormatError> {
        let mut descriptors = Vec::with_capacity(book.sections.len());
        let mut sections = Vec::with_capacity(book.sections.len());
        let mut fallback_toc = Vec::new();
        for (index, section) in book.sections.into_iter().enumerate() {
            let number = index + 1;
            let id = SpineItemId::new(format!("section-{number}"))?;
            let href = PublicationUrl::parse(&format!("Text/section-{number}.xhtml"))?;
            if section.linear {
                fallback_toc.push(TocEntry {
                    label: section.title.clone(),
                    href: Some(href.clone()),
                    children: Vec::new(),
                });
            }
            descriptors.push(SpineItem {
                id,
                href,
                media_type: "application/xhtml+xml".into(),
                linear: section.linear,
                properties: section.properties,
            });
            sections.push(section.content);
        }
        if descriptors.is_empty() {
            return Err(conversion_error(format, "没有可阅读的正文"));
        }

        let table_of_contents_origin = if book.table_of_contents.is_empty() {
            TableOfContentsOrigin::Fallback
        } else {
            TableOfContentsOrigin::Embedded
        };
        let table_of_contents = if table_of_contents_origin == TableOfContentsOrigin::Fallback {
            fallback_toc
        } else {
            book.table_of_contents
                .into_iter()
                .map(parse_toc_entry)
                .collect::<Result<Vec<_>, _>>()?
        };
        let table_of_contents = promote_single_toc_root(table_of_contents);
        let toc_heading_hints = if book.metadata.layout == RenditionLayout::Reflowable {
            collect_toc_heading_hints(&table_of_contents)
        } else {
            HashMap::new()
        };
        let mut navigation_documents = Vec::new();
        let mut cover_page = None;
        let mut fragment_sections = HashMap::new();
        for (index, content) in sections.iter().enumerate() {
            let SectionContent::Html(content) = content else {
                continue;
            };
            collect_fragment_sections(content, index, &mut fragment_sections);
            // Parse only metadata candidates; normal chapters remain lazy.
            if content.contains("<nav")
                || content.contains("<guide")
                || content.contains("doc-cover")
                || content.contains("\"cover\"")
                || content.contains("'cover'")
            {
                if let Ok(xml) = html_document(content) {
                    navigation_documents.extend(crate::html_context::navigation_documents(
                        &xml,
                        &descriptors[index].href,
                    ));
                    if cover_page.is_none() {
                        cover_page =
                            crate::html_context::cover_page(&xml, &descriptors[index].href);
                    }
                }
            }
        }
        let html_context = crate::HtmlContext::new(&table_of_contents, navigation_documents);
        if book.metadata.layout == RenditionLayout::Reflowable {
            html_context.mark_note_sections(&mut descriptors);
            crate::continuations::mark_quote_continuations(
                &mut descriptors,
                &table_of_contents,
                |href| {
                    let index = descriptors_index(href, sections.len())?;
                    let SectionContent::Html(content) = &sections[index] else {
                        return None;
                    };
                    html_document(content).ok()
                },
            );
        }
        let cover = book
            .cover_path
            .as_deref()
            .map(PublicationUrl::parse)
            .transpose()?;
        let resources = book
            .resources
            .into_iter()
            .map(|resource| {
                let href = PublicationUrl::parse(&resource.path)?;
                Ok((
                    href.path().to_owned(),
                    StoredResource {
                        href,
                        media_type: resource.media_type,
                        bytes: resource.bytes.into(),
                    },
                ))
            })
            .collect::<Result<HashMap<_, _>, PublicationError>>()?;
        Ok(Self {
            book: Book {
                id: PublicationId::new(book.id)?,
                metadata: book.metadata,
                cover,
                sections: descriptors,
                table_of_contents,
            },
            table_of_contents_origin,
            sections,
            resources,
            toc_heading_hints,
            html_context,
            cover_page,
            fragment_sections,
        })
    }
}

impl BookSource for DirectBookSource {
    fn book(&self) -> &Book {
        &self.book
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.table_of_contents_origin
    }

    fn cover_section(&self) -> Result<Option<Section>, PublicationError> {
        let Some(href) = &self.cover_page else {
            return Ok(None);
        };
        let Some(index) = descriptors_index(href, self.sections.len()) else {
            return Ok(None);
        };
        self.parse_section(index).map(Some)
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let descriptor = self
            .book
            .sections
            .get(index)
            .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))?;
        let content = self
            .sections
            .get(index)
            .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))?;
        match content {
            SectionContent::Html(body) => {
                let document = html_document(body).map_err(PublicationError::InvalidPublication)?;
                let document = resolve_fragment_links(document, index, &self.fragment_sections)
                    .map_err(PublicationError::InvalidPublication)?;
                let mut section = parse_section_with_hints_and_image_classifier(
                    &document,
                    descriptor,
                    |href| {
                        self.resources
                            .get(href.path())
                            .map(|resource| String::from_utf8_lossy(&resource.bytes).into_owned())
                    },
                    |href| {
                        self.html_context.is_separator_image(href, || {
                            self.resources
                                .get(href.path())
                                .map(|resource| Arc::clone(&resource.bytes))
                        })
                    },
                    self.html_context.hints(descriptor),
                )
                .map_err(|error| PublicationError::InvalidPublication(error.to_string()))?;
                if let Some(hints) = self.toc_heading_hints.get(descriptor.href.path()) {
                    promote_toc_headings(&mut section, hints);
                }
                Ok(section)
            }
            SectionContent::Image { resource_path, alt } => {
                let href = PublicationUrl::parse(resource_path)?;
                Ok(Section {
                    id: descriptor.id.clone(),
                    href: descriptor.href.clone(),
                    blocks: vec![Block::Image(ImageBlock {
                        formula_image: false,
                        formula: None,
                        href,
                        alt: alt.clone(),
                        style: ImageStyle::default(),
                        source: None,
                        text_layer: None,
                    })],
                    anchors: Vec::new(),
                })
            }
        }
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        let resource = self
            .resources
            .get(href.resource_url().path())
            .ok_or_else(|| PublicationError::ResourceNotFound(href.to_string()))?;
        Ok(Resource {
            href: resource.href.clone(),
            media_type: resource.media_type.clone(),
            bytes: Arc::clone(&resource.bytes),
        })
    }
}

pub(crate) fn html_document(content: &str) -> Result<String, String> {
    if content.to_ascii_lowercase().contains("<html") {
        crate::markup::html(content, crate::markup::Limits::default())
            .map(std::borrow::Cow::into_owned)
    } else {
        let document = format!(
            "<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xmlns:mbp=\"http://mobipocket.com/ns/mbp\"><head></head><body>{content}</body></html>"
        );
        crate::markup::html_fragment(&document, crate::markup::Limits::default())
            .map(std::borrow::Cow::into_owned)
    }
}

fn descriptors_index(href: &PublicationUrl, section_count: usize) -> Option<usize> {
    let index = href
        .path()
        .strip_prefix("Text/section-")?
        .strip_suffix(".xhtml")?
        .parse::<usize>()
        .ok()?
        .checked_sub(1)?;
    (index < section_count).then_some(index)
}

fn collect_fragment_sections(
    content: &str,
    index: usize,
    fragments: &mut HashMap<String, Option<usize>>,
) {
    let mut reader = quick_xml::Reader::from_str(content);
    loop {
        match reader.read_event() {
            Ok(
                quick_xml::events::Event::Start(element) | quick_xml::events::Event::Empty(element),
            ) => {
                for attribute in element
                    .attributes()
                    .flatten()
                    .filter(|attribute| matches!(attribute.key.as_ref(), b"id" | b"name" | b"aid"))
                {
                    if let Ok(id) = attribute.decoded_and_normalized_value(
                        quick_xml::XmlVersion::Implicit1_0,
                        reader.decoder(),
                    ) {
                        fragments
                            .entry(id.into_owned())
                            .and_modify(|section| {
                                if *section != Some(index) {
                                    *section = None;
                                }
                            })
                            .or_insert(Some(index));
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn resolve_fragment_links(
    document: String,
    index: usize,
    fragments: &HashMap<String, Option<usize>>,
) -> Result<String, String> {
    use quick_xml::events::{BytesStart, Event};
    if !document.contains('#') {
        return Ok(document);
    }
    let mut reader = quick_xml::Reader::from_str(&document);
    let mut writer = quick_xml::Writer::new(Vec::new());
    loop {
        let event = reader.read_event().map_err(|error| error.to_string())?;
        let empty = matches!(event, Event::Empty(_));
        let event = match event {
            Event::Eof => break,
            Event::Start(element) | Event::Empty(element) => {
                let target = element
                    .attributes()
                    .flatten()
                    .find(|attribute| attribute.key.as_ref() == b"href")
                    .and_then(|attribute| {
                        attribute
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .ok()
                            .map(|value| value.into_owned())
                    })
                    .and_then(|href| {
                        let fragment = href.strip_prefix('#')?;
                        let section = fragments.get(fragment).copied().flatten()?;
                        (section != index)
                            .then(|| format!("section-{}.xhtml#{fragment}", section + 1))
                    });
                let element = if let Some(target) = target {
                    let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                    let mut rewritten = BytesStart::new(name);
                    for attribute in element
                        .attributes()
                        .flatten()
                        .filter(|attribute| attribute.key.as_ref() != b"href")
                    {
                        rewritten.push_attribute(attribute);
                    }
                    rewritten.push_attribute(("href", target.as_str()));
                    rewritten
                } else {
                    element
                };
                if empty {
                    Event::Empty(element)
                } else {
                    Event::Start(element)
                }
            }
            event => event,
        };
        writer
            .write_event(event)
            .map_err(|error| error.to_string())?;
    }
    String::from_utf8(writer.into_inner()).map_err(|error| error.to_string())
}

fn parse_toc_entry(entry: SourceTocEntry) -> Result<TocEntry, PublicationError> {
    Ok(TocEntry {
        label: entry.label,
        href: Some(PublicationUrl::parse(&entry.href)?),
        children: entry
            .children
            .into_iter()
            .map(parse_toc_entry)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

pub(crate) fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(crate) fn escape_attribute(value: &str) -> String {
    escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use rebook_publication::{BookSource, RenditionLayout, TextBlockKind};

    use super::*;

    fn heading_fixture(xml: &str, label: &str) -> Section {
        DirectBookSource::open(
            SourceBook {
                id: "heading-recovery-test".into(),
                metadata: Metadata::default(),
                sections: vec![SourceSection {
                    title: "Chapter".into(),
                    content: SectionContent::Html(xml.into()),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: label.into(),
                    href: "Text/section-1.xhtml#chapter".into(),
                    children: vec![],
                }],
                resources: vec![],
                cover_path: None,
            },
            BookFormat::Epub,
        )
        .unwrap()
        .parse_section(0)
        .unwrap()
    }

    #[test]
    fn split_heading_recovery_handles_appendices_dotted_ordinals_and_title_only_toc_labels() {
        for (xml, label) in [
            (
                "<p id='chapter'>Appendix C</p><p>CONNECTIONIST MODELS</p>",
                "Appendix C—Connectionist Models",
            ),
            (
                "<h1 id='chapter'>1</h1><h1>Thinking with Sensations</h1>",
                "Thinking with Sensations",
            ),
            ("<h2>1.1</h2><h2 id='chapter'>Models</h2>", "1.1 Models"),
            (
                "<h1 id='chapter'>第1章</h1><h1>社会工程初探</h1>",
                "第1章 社会工程初探",
            ),
        ] {
            let section = heading_fixture(xml, label);
            assert!(
                matches!(&section.blocks[0], Block::Text(t) if t.kind == TextBlockKind::HeadingOrdinal(1)),
                "{label}"
            );
            assert!(
                matches!(&section.blocks[1], Block::Text(t) if t.kind == TextBlockKind::Heading(1)),
                "{label}"
            );
            let ranges: Vec<_> = section
                .blocks
                .iter()
                .filter_map(|b| {
                    if let Block::Text(t) = b {
                        t.source.as_ref()
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(ranges.len(), 2);
            assert_ne!(ranges[0].start.node, ranges[1].start.node);
            assert_eq!(section.anchors.len(), 1);
        }
    }

    #[test]
    fn split_heading_recovery_rejects_page_numbers_formulas_and_ordinal_collisions() {
        for (xml, label) in [
            (
                "<h2 id='chapter'>CHAPTER 6</h2><h3>81</h3><p>Through the Looking Glass</p>",
                "CHAPTER 6, 81",
            ),
            (
                "<p id='chapter'>0.9206</p><h2>Gaps between Primes</h2>",
                "Gaps between Primes",
            ),
            ("<h2 id='chapter'>1.1</h2><h2>Models</h2>", "11 Models"),
            ("<h2 id='chapter'>A</h2><p>Abrahamson, Dor</p>", "A"),
        ] {
            let section = heading_fixture(xml, label);
            assert!(!section.blocks.iter().any(|b| matches!(b, Block::Text(t) if matches!(t.kind, TextBlockKind::HeadingOrdinal(_)))), "{label}");
        }
    }

    #[test]
    fn direct_source_promotes_a_single_toc_root() {
        let source = DirectBookSource::open(
            SourceBook {
                id: "wrapped-toc-test".into(),
                metadata: Metadata {
                    title: "Sample Book".into(),
                    authors: Vec::new(),
                    languages: Vec::new(),
                    layout: RenditionLayout::PrePaginated,
                },
                sections: vec![SourceSection {
                    title: "Page 1".into(),
                    content: SectionContent::Html("<p>Page 1</p>".into()),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: "目 录".into(),
                    href: "Text/section-1.xhtml".into(),
                    children: vec![
                        SourceTocEntry {
                            label: "Preface".into(),
                            href: "Text/section-1.xhtml#preface".into(),
                            children: Vec::new(),
                        },
                        SourceTocEntry {
                            label: "Chapter One".into(),
                            href: "Text/section-1.xhtml#chapter-one".into(),
                            children: Vec::new(),
                        },
                    ],
                }],
                resources: Vec::new(),
                cover_path: None,
            },
            BookFormat::Pdf,
        )
        .unwrap();

        assert_eq!(source.book().table_of_contents.len(), 2);
        assert_eq!(source.book().table_of_contents[0].label, "Preface");
        assert_eq!(source.book().table_of_contents[1].label, "Chapter One");
    }

    #[test]
    fn direct_source_parses_lazy_html_toc_fragments_and_resources() {
        let source = DirectBookSource::open(
            SourceBook {
                id: "direct-source-test".into(),
                metadata: Metadata {
                    title: "Direct".into(),
                    authors: Vec::new(),
                    languages: Vec::new(),
                    layout: RenditionLayout::Reflowable,
                },
                sections: vec![SourceSection {
                    title: "Chapter".into(),
                    content: SectionContent::Html(
                        "<h1 id=\"chapter\">Chapter</h1><img src=\"../Images/cover.png\"/>".into(),
                    ),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: "Chapter".into(),
                    href: "Text/section-1.xhtml#chapter".into(),
                    children: Vec::new(),
                }],
                resources: vec![SourceResource {
                    path: "Images/cover.png".into(),
                    media_type: "image/png".into(),
                    bytes: vec![1, 2, 3],
                }],
                cover_path: Some("Images/cover.png".into()),
            },
            BookFormat::Fb2,
        )
        .unwrap();

        assert_eq!(
            source.book().table_of_contents[0]
                .href
                .as_ref()
                .and_then(PublicationUrl::fragment),
            Some("chapter")
        );
        let section = source.parse_section(0).unwrap();
        assert_eq!(section.anchors[0].fragment, "chapter");
        let cover = source
            .resource(source.book().cover.as_ref().unwrap())
            .unwrap();
        assert_eq!(cover.bytes.as_ref(), [1, 2, 3]);
    }

    #[test]
    fn reflowable_direct_source_promotes_an_exact_toc_paragraph() {
        let source = DirectBookSource::open(
            SourceBook {
                id: "toc-heading-test".into(),
                metadata: Metadata {
                    title: "Direct".into(),
                    authors: Vec::new(),
                    languages: Vec::new(),
                    layout: RenditionLayout::Reflowable,
                },
                sections: vec![SourceSection {
                    title: "Chapter".into(),
                    content: SectionContent::Html(
                        "<p id=\"alignment\" style=\"font-size: 0.8em\">ALIGNMENT</p><p>Body.</p>"
                            .into(),
                    ),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: "Alignment".into(),
                    href: "Text/section-1.xhtml#alignment".into(),
                    children: Vec::new(),
                }],
                resources: Vec::new(),
                cover_path: None,
            },
            BookFormat::Azw3,
        )
        .unwrap();

        let section = source.parse_section(0).unwrap();
        let Some(Block::Text(heading)) = section.blocks.first() else {
            panic!("first block should be text");
        };
        assert_eq!(heading.kind, TextBlockKind::Heading(1));
        let Some(Inline::Text(run)) = heading.content.first() else {
            panic!("heading should retain authored text");
        };
        assert!((run.style.size_scale - 0.8).abs() < 0.001);
    }

    #[test]
    fn reflowable_direct_source_promotes_a_split_number_and_title() {
        let source = DirectBookSource::open(
            SourceBook {
                id: "split-toc-heading-test".into(),
                metadata: Metadata {
                    title: "Direct".into(),
                    authors: Vec::new(),
                    languages: vec!["en".into()],
                    layout: RenditionLayout::Reflowable,
                },
                sections: vec![SourceSection {
                    title: "Chapter".into(),
                    content: SectionContent::Html(
                        "<p id=\"chapter-1\">1</p><p>Why Goal Setting Is Broken</p><p>Body.</p>"
                            .into(),
                    ),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: "Chapter 1: Why Goal Setting Is Broken".into(),
                    href: "Text/section-1.xhtml#chapter-1".into(),
                    children: Vec::new(),
                }],
                resources: Vec::new(),
                cover_path: None,
            },
            BookFormat::Epub,
        )
        .unwrap();

        let section = source.parse_section(0).unwrap();
        assert!(matches!(
            section.blocks.first(),
            Some(Block::Text(text)) if text.kind == TextBlockKind::HeadingOrdinal(1)
        ));
        assert!(matches!(
            section.blocks.get(1),
            Some(Block::Text(text)) if text.kind == TextBlockKind::Heading(1)
        ));
    }

    #[test]
    fn reflowable_direct_source_groups_an_authored_part_heading() {
        let source = DirectBookSource::open(
            SourceBook {
                id: "split-part-heading-test".into(),
                metadata: Metadata {
                    title: "Direct".into(),
                    authors: Vec::new(),
                    languages: vec!["en".into()],
                    layout: RenditionLayout::Reflowable,
                },
                sections: vec![SourceSection {
                    title: "Part".into(),
                    content: SectionContent::Html(
                        "<h2 id=\"part-one\">Part I</h2><h2>Introduction</h2><p>Body.</p>".into(),
                    ),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: "Part I Introduction".into(),
                    href: "Text/section-1.xhtml#part-one".into(),
                    children: Vec::new(),
                }],
                resources: Vec::new(),
                cover_path: None,
            },
            BookFormat::Epub,
        )
        .unwrap();

        let section = source.parse_section(0).unwrap();
        assert!(matches!(
            section.blocks.first(),
            Some(Block::Text(text)) if text.kind == TextBlockKind::HeadingOrdinal(1)
        ));
        assert!(matches!(
            section.blocks.get(1),
            Some(Block::Text(text)) if text.kind == TextBlockKind::Heading(1)
        ));
    }

    #[test]
    fn reflowable_direct_source_does_not_guess_a_split_heading_without_a_toc_match() {
        let source = DirectBookSource::open(
            SourceBook {
                id: "split-toc-heading-negative-test".into(),
                metadata: Metadata {
                    title: "Direct".into(),
                    authors: Vec::new(),
                    languages: vec!["en".into()],
                    layout: RenditionLayout::Reflowable,
                },
                sections: vec![SourceSection {
                    title: "Chapter".into(),
                    content: SectionContent::Html(
                        "<p id=\"chapter-1\">1</p><p>A numbered body paragraph.</p>".into(),
                    ),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: "Chapter 1: A Different Title".into(),
                    href: "Text/section-1.xhtml#chapter-1".into(),
                    children: Vec::new(),
                }],
                resources: Vec::new(),
                cover_path: None,
            },
            BookFormat::Epub,
        )
        .unwrap();

        let section = source.parse_section(0).unwrap();
        assert!(matches!(
            section.blocks.first(),
            Some(Block::Text(text)) if text.kind == TextBlockKind::Paragraph
        ));
        assert!(matches!(
            section.blocks.get(1),
            Some(Block::Text(text)) if text.kind == TextBlockKind::Paragraph
        ));
    }

    #[test]
    fn pre_paginated_direct_source_does_not_infer_toc_headings() {
        let source = DirectBookSource::open(
            SourceBook {
                id: "fixed-toc-heading-test".into(),
                metadata: Metadata {
                    title: "Fixed".into(),
                    authors: Vec::new(),
                    languages: Vec::new(),
                    layout: RenditionLayout::PrePaginated,
                },
                sections: vec![SourceSection {
                    title: "Page 1".into(),
                    content: SectionContent::Html("<p id=\"title\">Title</p>".into()),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: vec![SourceTocEntry {
                    label: "Title".into(),
                    href: "Text/section-1.xhtml#title".into(),
                    children: Vec::new(),
                }],
                resources: Vec::new(),
                cover_path: None,
            },
            BookFormat::Pdf,
        )
        .unwrap();

        let section = source.parse_section(0).unwrap();
        assert!(matches!(
            section.blocks.first(),
            Some(Block::Text(text)) if text.kind == TextBlockKind::Paragraph
        ));
    }
}
