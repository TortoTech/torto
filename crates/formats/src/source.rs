use std::collections::HashMap;
use std::sync::Arc;

use rebook_html::parse_section;
use rebook_publication::{
    Block, Book, BookSource, ImageBlock, ImageStyle, Inline, Metadata, PublicationError,
    PublicationId, PublicationUrl, RenditionLayout, Resource, Section, SpineItem, SpineItemId,
    TableOfContentsOrigin, TextBlock, TextBlockKind, TocEntry, promote_single_toc_root,
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
        let matches = search_range
            .clone()
            .filter_map(|index| heading_candidate(&section.blocks, index, hint))
            .collect::<Vec<_>>();
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
    let title = normalized_text_block(title);
    (is_heading_ordinal(&ordinal) && split_heading_matches(&ordinal, &title, &hint.label))
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
    let combined = format!("{ordinal} {title}");
    if heading_labels_match(&combined, hint) {
        return true;
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

fn split_heading_label(label: &str) -> Option<(String, &str)> {
    let trimmed = label.trim();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in ["chapter", "part", "book"] {
        if lower == prefix || !lower.starts_with(prefix) {
            continue;
        }
        let rest = trimmed.get(prefix.len()..)?.trim_start();
        let ordinal_end = rest
            .char_indices()
            .find_map(|(index, character)| {
                (character.is_whitespace()
                    || matches!(character, ':' | '.' | '-' | '\u{2013}' | '\u{2014}'))
                .then_some(index)
            })
            .unwrap_or(rest.len());
        let ordinal = rest.get(..ordinal_end)?.trim();
        let title = rest
            .get(ordinal_end..)?
            .trim_start_matches(|character: char| {
                character.is_whitespace()
                    || matches!(character, ':' | '.' | '-' | '\u{2013}' | '\u{2014}')
            });
        if !ordinal.is_empty() && !title.is_empty() {
            return Some((heading_match_key(ordinal), title));
        }
    }

    let ordinal_end = trimmed.char_indices().find_map(|(index, character)| {
        (character.is_whitespace()
            || matches!(character, ':' | '.' | '-' | '\u{2013}' | '\u{2014}'))
        .then_some(index)
    })?;
    let ordinal = trimmed.get(..ordinal_end)?.trim();
    let title = trimmed
        .get(ordinal_end..)?
        .trim_start_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, ':' | '.' | '-' | '\u{2013}' | '\u{2014}')
        });
    (!ordinal.is_empty() && !title.is_empty()).then(|| (heading_match_key(ordinal), title))
}

fn heading_ordinal_key(text: &str) -> Option<String> {
    let mut normalized = text.trim().to_ascii_lowercase();
    for prefix in ["chapter", "part", "book"] {
        if normalized.starts_with(prefix) {
            normalized = normalized.get(prefix.len()..)?.trim_start().to_owned();
            break;
        }
    }
    let normalized = normalized.trim_matches(|character: char| {
        character.is_whitespace() || matches!(character, ':' | '.' | '-' | '\u{2013}' | '\u{2014}')
    });
    is_bare_heading_ordinal(normalized).then(|| heading_match_key(normalized))
}

fn is_heading_ordinal(text: &str) -> bool {
    heading_ordinal_key(text).is_some()
}

fn is_bare_heading_ordinal(text: &str) -> bool {
    !text.is_empty()
        && (text.chars().all(|character| character.is_ascii_digit())
            || is_roman_numeral(text)
            || is_english_number_word(text))
}

fn is_roman_numeral(text: &str) -> bool {
    text.len() <= 12
        && text
            .chars()
            .all(|character| matches!(character, 'i' | 'v' | 'x' | 'l' | 'c' | 'd' | 'm'))
}

fn is_english_number_word(text: &str) -> bool {
    text.split([' ', '-']).all(|word| {
        matches!(
            word,
            "one"
                | "two"
                | "three"
                | "four"
                | "five"
                | "six"
                | "seven"
                | "eight"
                | "nine"
                | "ten"
                | "eleven"
                | "twelve"
                | "thirteen"
                | "fourteen"
                | "fifteen"
                | "sixteen"
                | "seventeen"
                | "eighteen"
                | "nineteen"
                | "twenty"
                | "thirty"
                | "forty"
                | "fifty"
                | "sixty"
                | "seventy"
                | "eighty"
                | "ninety"
                | "hundred"
        )
    })
}

fn normalized_text_block(block: &TextBlock) -> String {
    let mut text = String::new();
    for inline in &block.content {
        match inline {
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
                properties: Vec::new(),
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
                let document = format!(
                    "<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title></title></head><body>{body}</body></html>"
                );
                let mut section = parse_section(&document, descriptor, |_| None)
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
