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
        let block_index = hint.fragment.as_deref().and_then(|fragment| {
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

        let block_index = block_index.or_else(|| {
            hint.fragment.is_none().then(|| {
                section
                    .blocks
                    .iter()
                    .take(PATH_ONLY_HEADING_SEARCH_BLOCKS)
                    .position(|block| {
                        matches!(
                            block,
                            Block::Text(text)
                                if matches!(
                                    text.kind,
                                    TextBlockKind::Paragraph | TextBlockKind::Heading(_)
                                ) && normalized_text_block(text) == hint.label
                        )
                    })
            })?
        });

        let Some(Block::Text(text)) = block_index.and_then(|index| section.blocks.get_mut(index))
        else {
            continue;
        };
        if text.kind == TextBlockKind::Paragraph && normalized_text_block(text) == hint.label {
            text.kind = TextBlockKind::Heading(hint.level);
        }
    }
}

fn normalized_text_block(block: &TextBlock) -> String {
    let mut text = String::new();
    for inline in &block.content {
        match inline {
            Inline::Text(run) => text.push_str(&run.text),
            Inline::Math(run) => text.push_str(&run.latex),
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
