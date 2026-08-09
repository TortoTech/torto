use rebook_publication::{
    Block, BookSource, Inline, RenditionLayout, SourceAnchor, SourceRange, TextBlock,
    TextBlockKind, TocEntry,
};
use regex::RegexBuilder;

const DEFAULT_CONTEXT_CHARS: usize = 72;

#[derive(Clone, Debug, PartialEq)]
pub struct BookSearchResult {
    pub book_id: String,
    pub book_title: String,
    pub section_index: usize,
    pub section_title: String,
    pub excerpt: String,
    pub matched_text: String,
    pub block_kind: String,
    pub range: SourceRange,
    pub similarity: Option<f32>,
    pub is_image: bool,
    pub image_href: Option<String>,
    pub image_preview: Option<Vec<u8>>,
}

pub fn search_book(
    source: &dyn BookSource,
    query: &str,
    max_results: usize,
) -> Result<Vec<BookSearchResult>, String> {
    search_sections(source, query, max_results, 0..source.book().sections.len())
}

pub(crate) fn search_section(
    source: &dyn BookSource,
    query: &str,
    section_index: usize,
    max_results: usize,
) -> Result<Vec<BookSearchResult>, String> {
    if section_index >= source.book().sections.len() {
        return Err(format!("章节索引超出范围：{section_index}"));
    }
    search_sections(source, query, max_results, section_index..=section_index)
}

fn search_sections(
    source: &dyn BookSource,
    query: &str,
    max_results: usize,
    sections: impl IntoIterator<Item = usize>,
) -> Result<Vec<BookSearchResult>, String> {
    let query = query.trim();
    if query.is_empty() || max_results == 0 {
        return Ok(Vec::new());
    }
    let matcher = RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .unicode(true)
        .build()
        .map_err(|error| format!("搜索表达式无效：{error}"))?;
    let mut results = Vec::new();
    let book_id = source.book().id.to_string();
    let book_title = source.book().metadata.title.clone();

    for section_index in sections {
        let section = source
            .parse_section(section_index)
            .map_err(|error| format!("解析第 {} 节失败：{error}", section_index + 1))?;
        let section_title = section_title(source, section_index, &section.blocks);
        for block in &section.blocks {
            if let Block::Table(table) = block {
                for cell in table.rows.iter().flat_map(|row| &row.cells) {
                    let Some(source_range) = &cell.text.source else {
                        continue;
                    };
                    let text = text_block_text(&cell.text);
                    for found in matcher.find_iter(&text) {
                        let range =
                            source_range_for_match(source_range, &text, found.start(), found.end());
                        results.push(BookSearchResult {
                            book_id: book_id.clone(),
                            book_title: book_title.clone(),
                            section_index,
                            section_title: section_title.clone(),
                            excerpt: excerpt(
                                &text,
                                found.start(),
                                found.end(),
                                DEFAULT_CONTEXT_CHARS,
                            ),
                            matched_text: found.as_str().to_owned(),
                            block_kind: "table-cell".into(),
                            range,
                            similarity: None,
                            is_image: false,
                            image_href: None,
                            image_preview: None,
                        });
                        if results.len() >= max_results {
                            return Ok(results);
                        }
                    }
                }
                continue;
            }
            let (text, source_range, block_kind) = match block {
                Block::Text(block) => {
                    let Some(source_range) = &block.source else {
                        continue;
                    };
                    (
                        text_block_text(block),
                        source_range,
                        text_block_kind(block).to_owned(),
                    )
                }
                Block::Image(image) => {
                    let (Some(layer), Some(source_range)) = (&image.text_layer, &image.source)
                    else {
                        continue;
                    };
                    (layer.text.clone(), source_range, "image-text".into())
                }
                Block::Table(_) | Block::Separator | Block::PageBreak => continue,
            };
            for found in matcher.find_iter(&text) {
                let range = source_range_for_match(source_range, &text, found.start(), found.end());
                results.push(BookSearchResult {
                    book_id: book_id.clone(),
                    book_title: book_title.clone(),
                    section_index,
                    section_title: section_title.clone(),
                    excerpt: excerpt(&text, found.start(), found.end(), DEFAULT_CONTEXT_CHARS),
                    matched_text: found.as_str().to_owned(),
                    block_kind: block_kind.clone(),
                    range,
                    similarity: None,
                    is_image: false,
                    image_href: None,
                    image_preview: None,
                });
                if results.len() >= max_results {
                    return Ok(results);
                }
            }
        }
    }
    Ok(results)
}

pub(crate) fn text_block_kind(block: &TextBlock) -> &'static str {
    match block.kind {
        TextBlockKind::Paragraph => "paragraph",
        TextBlockKind::Heading(_) => "heading",
        TextBlockKind::Blockquote => "blockquote",
        TextBlockKind::Preformatted => "preformatted",
        TextBlockKind::ListItem { .. } => "list-item",
    }
}

pub(crate) fn text_block_text(block: &TextBlock) -> String {
    block
        .content
        .iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.as_str(),
            Inline::Math(run) => run.latex.as_str(),
            Inline::Break => "\n",
        })
        .collect()
}

pub(crate) fn section_title(
    source: &dyn BookSource,
    section_index: usize,
    blocks: &[Block],
) -> String {
    let book = source.book();
    if book.metadata.layout == RenditionLayout::PrePaginated {
        let href = &book.sections[section_index].href;
        return toc_label_for_href(&book.table_of_contents, href)
            .unwrap_or_else(|| format!("第 {} 页", section_index + 1));
    }
    if let Some(title) = blocks.iter().find_map(|block| match block {
        Block::Text(block) if matches!(block.kind, TextBlockKind::Heading(_)) => {
            let text = text_block_text(block);
            (!text.trim().is_empty()).then(|| text.trim().to_owned())
        }
        _ => None,
    }) {
        return title;
    }
    let href = &book.sections[section_index].href;
    toc_label_for_href(&book.table_of_contents, href)
        .unwrap_or_else(|| format!("第 {} 节", section_index + 1))
}

fn toc_label_for_href(
    entries: &[TocEntry],
    href: &rebook_publication::PublicationUrl,
) -> Option<String> {
    for entry in entries {
        if entry
            .href
            .as_ref()
            .is_some_and(|target| target.resource_url() == href.resource_url())
        {
            return Some(entry.label.clone());
        }
        if let Some(label) = toc_label_for_href(&entry.children, href) {
            return Some(label);
        }
    }
    None
}

fn source_range_for_match(
    source: &SourceRange,
    text: &str,
    byte_start: usize,
    byte_end: usize,
) -> SourceRange {
    if source.start.spine != source.end.spine || source.start.node != source.end.node {
        return source.clone();
    }
    let start_offset = source.start.text_offset
        + u64::try_from(text[..byte_start].chars().count()).unwrap_or(u64::MAX);
    let end_offset = source.start.text_offset
        + u64::try_from(text[..byte_end].chars().count()).unwrap_or(u64::MAX);
    if start_offset >= end_offset || end_offset > source.end.text_offset {
        return source.clone();
    }
    SourceRange {
        start: SourceAnchor {
            spine: source.start.spine.clone(),
            node: source.start.node.clone(),
            text_offset: start_offset,
        },
        end: SourceAnchor {
            spine: source.end.spine.clone(),
            node: source.end.node.clone(),
            text_offset: end_offset,
        },
    }
}

fn excerpt(text: &str, start: usize, end: usize, context_chars: usize) -> String {
    let context_start = text[..start]
        .char_indices()
        .rev()
        .nth(context_chars.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    let context_end = text[end..]
        .char_indices()
        .nth(context_chars)
        .map_or(text.len(), |(index, _)| end + index);
    format!(
        "{}{}{}",
        if context_start > 0 { "…" } else { "" },
        text[context_start..context_end].trim(),
        if context_end < text.len() { "…" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use rebook_publication::{
        BlockStyle, Book, FixedPageTextLayer, FixedPageTextRect, FixedPageTextSpan, ImageBlock,
        ImageStyle, Metadata, PublicationError, PublicationId, PublicationUrl, Resource, Section,
        SpineItem, SpineItemId, TextRun, TextStyle,
    };

    use super::*;

    struct SearchSource {
        book: Book,
        sections: Vec<Section>,
    }

    impl BookSource for SearchSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            Ok(self.sections[index].clone())
        }

        fn resource(&self, _href: &PublicationUrl) -> Result<Resource, PublicationError> {
            unreachable!()
        }
    }

    #[test]
    fn search_returns_source_backed_unicode_matches() {
        let spine = SpineItemId::new("chapter-1").unwrap();
        let href = PublicationUrl::parse("chapter-1.xhtml").unwrap();
        let text = "Systems thinking helps us see systems.";
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 4,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "paragraph-1".into(),
                text_offset: 4 + u64::try_from(text.chars().count()).unwrap(),
            },
        };
        let search_source = SearchSource {
            book: Book {
                id: PublicationId::new("search-book").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: vec![SpineItem {
                    id: spine.clone(),
                    href: href.clone(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            sections: vec![Section {
                id: spine,
                href,
                blocks: vec![Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: text.into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(source),
                })],
                anchors: Vec::new(),
            }],
        };

        let results = search_book(&search_source, "SYSTEMS", 10).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].matched_text, "Systems");
        assert_eq!(results[0].block_kind, "paragraph");
        assert_eq!(results[0].range.start.text_offset, 4);
        assert_eq!(results[0].range.end.text_offset, 11);
        assert_eq!(results[1].range.start.text_offset, 34);
    }

    #[test]
    fn search_returns_source_ranges_from_fixed_page_text() {
        let spine = SpineItemId::new("pdf-page-1").unwrap();
        let href = PublicationUrl::parse("page-1.xhtml").unwrap();
        let text = "可搜索的 PDF 页面";
        let char_count = u64::try_from(text.chars().count()).unwrap();
        let source = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "pdf-page-text".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "pdf-page-text".into(),
                text_offset: char_count,
            },
        };
        let search_source = SearchSource {
            book: Book {
                id: PublicationId::new("searchable-pdf").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: vec![SpineItem {
                    id: spine.clone(),
                    href: href.clone(),
                    media_type: "application/pdf".into(),
                    linear: true,
                    properties: Vec::new(),
                }],
                table_of_contents: Vec::new(),
            },
            sections: vec![Section {
                id: spine,
                href,
                blocks: vec![Block::Image(ImageBlock {
                    href: PublicationUrl::parse("Pages/page-00001.png").unwrap(),
                    alt: "PDF page 1".into(),
                    style: ImageStyle::default(),
                    source: Some(source),
                    text_layer: Some(FixedPageTextLayer {
                        width: 100.0,
                        height: 100.0,
                        text: text.into(),
                        spans: vec![FixedPageTextSpan {
                            char_range: 0..char_count,
                            rect: FixedPageTextRect {
                                x: 0.0,
                                y: 0.0,
                                width: 100.0,
                                height: 20.0,
                            },
                        }],
                        replacement: None,
                    }),
                })],
                anchors: Vec::new(),
            }],
        };

        let results = search_book(&search_source, "PDF", 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matched_text, "PDF");
        assert_eq!(results[0].block_kind, "image-text");
        assert_eq!(results[0].range.start.text_offset, 5);
        assert_eq!(results[0].range.end.text_offset, 8);
    }

    #[test]
    fn excerpt_keeps_unicode_boundaries_and_context() {
        assert_eq!(excerpt("alpha beta gamma", 6, 10, 2), "…a beta g…");
    }
}
