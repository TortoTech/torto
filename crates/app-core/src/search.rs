use rebook_publication::{
    Block, BookSource, Inline, RenditionLayout, SourceAnchor, SourceRange, TextBlock,
    TextBlockKind, TocEntry,
};
use regex::RegexBuilder;

const DEFAULT_CONTEXT_CHARS: usize = 72;

#[derive(Clone, Debug, PartialEq)]
pub struct BookSearchResult {
    pub section_index: usize,
    pub section_title: String,
    pub excerpt: String,
    /// UTF-8 byte range of this occurrence within `excerpt`, not source offsets.
    pub excerpt_match: std::ops::Range<usize>,
    pub matched_text: String,
    pub block_kind: String,
    pub range: SourceRange,
}

pub fn search_book(
    source: &dyn BookSource,
    query: &str,
    max_results: usize,
) -> Result<Vec<BookSearchResult>, String> {
    search_sections(source, query, max_results, 0..source.book().sections.len())
}

pub fn search_section(
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
    for section_index in sections {
        let section = source
            .parse_section(section_index)
            .map_err(|error| format!("解析第 {} 节失败：{error}", section_index + 1))?;
        let section_title = section_title(source, section_index, &section.blocks);
        for block in &section.blocks {
            if let Block::Table(table) = block {
                for cell in table.text_blocks() {
                    let Some(_source_range) = &cell.source else {
                        continue;
                    };
                    let text = text_block_text(cell);
                    for found in matcher.find_iter(&text) {
                        let range = text_block_match_range(cell, found.start(), found.end());
                        let (excerpt, excerpt_match) =
                            excerpt(&text, found.start(), found.end(), DEFAULT_CONTEXT_CHARS);
                        results.push(BookSearchResult {
                            section_index,
                            section_title: section_title.clone(),
                            excerpt,
                            excerpt_match,
                            matched_text: found.as_str().to_owned(),
                            block_kind: "table-cell".into(),
                            range,
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
                Block::Quote(quote) => {
                    for child in quote.body.iter().chain(quote.attribution.iter()) {
                        let Some(_source_range) = &child.source else {
                            continue;
                        };
                        let text = text_block_text(child);
                        for found in matcher.find_iter(&text) {
                            let (excerpt, excerpt_match) =
                                excerpt(&text, found.start(), found.end(), DEFAULT_CONTEXT_CHARS);
                            results.push(BookSearchResult {
                                section_index,
                                section_title: section_title.clone(),
                                excerpt,
                                excerpt_match,
                                matched_text: found.as_str().to_owned(),
                                block_kind: text_block_kind(child).to_owned(),
                                range: text_block_match_range(child, found.start(), found.end()),
                            });
                            if results.len() >= max_results {
                                return Ok(results);
                            }
                        }
                    }
                    continue;
                }
                Block::Image(image) => {
                    let (Some(layer), Some(source_range)) = (&image.text_layer, &image.source)
                    else {
                        continue;
                    };
                    (layer.text.clone(), source_range, "image-text".into())
                }
                Block::Figure(figure) => {
                    for child in figure.text_blocks() {
                        if child.source.is_none() {
                            continue;
                        }
                        let text = text_block_text(child);
                        for found in matcher.find_iter(&text) {
                            let (excerpt, excerpt_match) =
                                excerpt(&text, found.start(), found.end(), DEFAULT_CONTEXT_CHARS);
                            results.push(BookSearchResult {
                                section_index,
                                section_title: section_title.clone(),
                                excerpt,
                                excerpt_match,
                                matched_text: found.as_str().to_owned(),
                                block_kind: text_block_kind(child).to_owned(),
                                range: text_block_match_range(child, found.start(), found.end()),
                            });
                            if results.len() >= max_results {
                                return Ok(results);
                            }
                        }
                    }
                    continue;
                }
                Block::Table(_)
                | Block::Note(_)
                | Block::Separator(_)
                | Block::LineBreak
                | Block::PageBreak => {
                    continue;
                }
            };
            for found in matcher.find_iter(&text) {
                let range = if let Block::Text(block) = block {
                    text_block_match_range(block, found.start(), found.end())
                } else {
                    source_range_for_match(source_range, &text, found.start(), found.end())
                };
                let (excerpt, excerpt_match) =
                    excerpt(&text, found.start(), found.end(), DEFAULT_CONTEXT_CHARS);
                results.push(BookSearchResult {
                    section_index,
                    section_title: section_title.clone(),
                    excerpt,
                    excerpt_match,
                    matched_text: found.as_str().to_owned(),
                    block_kind: block_kind.clone(),
                    range,
                });
                if results.len() >= max_results {
                    return Ok(results);
                }
            }
        }
    }
    Ok(results)
}

pub fn text_block_kind(block: &TextBlock) -> &'static str {
    match block.kind {
        TextBlockKind::Paragraph => "paragraph",
        TextBlockKind::Heading(_) | TextBlockKind::HeadingOrdinal(_) => "heading",
        TextBlockKind::Blockquote => "blockquote",
        TextBlockKind::QuoteAttribution => "quote-attribution",
        TextBlockKind::Preformatted => "preformatted",
        TextBlockKind::ListItem { .. } => "list-item",
        TextBlockKind::DefinitionTerm { .. } => "definition-term",
        TextBlockKind::DefinitionDescription { .. } => "definition-description",
        TextBlockKind::Caption => "caption",
        TextBlockKind::FootnoteDefinition => "footnote-definition",
    }
}

pub fn text_block_text(block: &TextBlock) -> String {
    block
        .content
        .iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.clone(),
            Inline::Ruby(run) => run.base_text(),
            Inline::Math(run) => run.original_text().unwrap_or_else(|| run.latex.clone()),
            Inline::Image(image) => image
                .image
                .ocr_text
                .as_ref()
                .map(|text| text.display_text().to_owned())
                .unwrap_or_default(),
            Inline::Break => "\n".to_owned(),
        })
        .collect()
}

pub fn section_title(source: &dyn BookSource, section_index: usize, blocks: &[Block]) -> String {
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
    rebook_publication::toc_entry_for_href(entries, href, rebook_publication::TocMatch::Section)
        .map(|entry| entry.label.clone())
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

fn text_block_match_range(block: &TextBlock, byte_start: usize, byte_end: usize) -> SourceRange {
    let source = block.source.as_ref().expect("search requires a source");
    let text = text_block_text(block);
    if !block
        .content
        .iter()
        .any(|i| matches!(i,Inline::Image(r) if r.image.ocr_text.is_some()))
    {
        return source_range_for_match(source, &text, byte_start, byte_end);
    }
    if source.start.spine != source.end.spine || source.start.node != source.end.node {
        return source.clone();
    }
    let segments = block
        .content
        .iter()
        .map(|i| match i {
            Inline::Text(r) => (r.text.chars().count(), r.text.chars().count()),
            Inline::Ruby(r) => (r.source_char_len(), r.source_char_len()),
            Inline::Math(r) => (
                r.original_text()
                    .unwrap_or_else(|| r.latex.clone())
                    .chars()
                    .count(),
                r.source_char_len(),
            ),
            Inline::Image(r) => (
                r.image
                    .ocr_text
                    .as_ref()
                    .map_or(0, |t| t.display_text().chars().count()),
                0,
            ),
            Inline::Break => (1, 1),
        })
        .collect::<Vec<_>>();
    let total = segments
        .iter()
        .map(|(_, canonical)| canonical)
        .sum::<usize>();
    let canonical = |at: usize| {
        let mut display = 0;
        let mut source = 0;
        for (len, original) in &segments {
            if at <= display + len {
                return source + (at - display).min(*original);
            }
            display += len;
            source += original;
        }
        source
    };
    let raw_length = source
        .end
        .text_offset
        .saturating_sub(source.start.text_offset);
    let offset = |at: usize| {
        source.start.text_offset
            + if total == 0 {
                0
            } else {
                (canonical(at) as u128 * raw_length as u128 / total as u128) as u64
            }
    };
    SourceRange {
        start: SourceAnchor {
            text_offset: offset(text[..byte_start].chars().count()),
            ..source.start.clone()
        },
        end: SourceAnchor {
            text_offset: offset(text[..byte_end].chars().count()),
            ..source.end.clone()
        },
    }
}

fn excerpt(
    text: &str,
    start: usize,
    end: usize,
    context_chars: usize,
) -> (String, std::ops::Range<usize>) {
    let context_start = text[..start]
        .char_indices()
        .rev()
        .nth(context_chars.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    let context_end = text[end..]
        .char_indices()
        .nth(context_chars)
        .map_or(text.len(), |(index, _)| end + index);
    // Only inspect bounded context around the occurrence, even for very large
    // OCR paragraphs. Do not split decimal numbers or common abbreviations.
    let sentence_start = text[context_start..start]
        .char_indices()
        .rev()
        .find(|(at, ch)| sentence_end(text, context_start + at, *ch))
        .map(|(at, ch)| context_start + at + ch.len_utf8());
    let sentence_end = text[end..context_end]
        .char_indices()
        .find(|(at, ch)| sentence_end(text, end + at, *ch))
        .map(|(at, ch)| end + at + ch.len_utf8());
    let context_start = sentence_start.unwrap_or(context_start);
    let context_end = sentence_end.unwrap_or(context_end);
    let fragment = &text[context_start..context_end];
    let trimmed = fragment.trim();
    let leading_space = fragment.len() - fragment.trim_start().len();
    let prefix = if context_start > 0 && sentence_start.is_none() {
        "…"
    } else {
        ""
    };
    let excerpt = format!(
        "{}{}{}",
        prefix,
        trimmed,
        if context_end < text.len() && sentence_end.is_none() {
            "…"
        } else {
            ""
        }
    );
    let match_start = prefix.len() + start - context_start - leading_space;
    (excerpt, match_start..match_start + end - start)
}

fn sentence_end(text: &str, at: usize, ch: char) -> bool {
    if matches!(
        ch,
        '。' | '！' | '？' | '!' | '?' | '\n' | '\r' | ';' | '；'
    ) {
        return true;
    }
    if ch != '.' {
        return false;
    }
    let next = text[at + 1..].chars().next();
    if next.is_some_and(|ch| !ch.is_whitespace() && !matches!(ch, '"' | '\'' | '”' | '’' | ')'))
    {
        return false;
    }
    let token: String = text[..at]
        .chars()
        .rev()
        .take(12)
        .take_while(|ch| ch.is_ascii_alphabetic() || *ch == '.')
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if token.len() == 1 && token.chars().all(|ch| ch.is_ascii_uppercase()) {
        return false;
    }
    !matches!(
        token.to_ascii_lowercase().as_str(),
        "mr" | "mrs"
            | "ms"
            | "dr"
            | "prof"
            | "fig"
            | "eq"
            | "e.g"
            | "i.e"
            | "etc"
            | "vs"
            | "no"
            | "st"
            | "al"
            | "vol"
            | "pp"
            | "p"
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

    #[test]
    fn recognized_text_search_keeps_following_source_offsets_and_image_slot() {
        let mut block = TextBlock {
            kind: TextBlockKind::Paragraph,
            content: vec![
                Inline::Text(TextRun {
                    text: "A".into(),
                    style: Default::default(),
                    link: None,
                }),
                Inline::Image(Box::new(rebook_publication::InlineImageRun {
                    image: ImageBlock {
                        ocr_text: Some(rebook_publication::ImageText {
                            text: "か".into(),
                            literal: true,
                            translated: None,
                        }),
                        formula_image: false,
                        formula: None,
                        href: PublicationUrl::parse("kana.png").unwrap(),
                        alt: String::new(),
                        style: Default::default(),
                        source: None,
                        text_layer: None,
                    },
                    size_scale: 1.0,
                    intrinsic_sizing: false,
                    vertical_align: rebook_publication::InlineImageAlignment::Baseline,
                    presentation: false,
                })),
                Inline::Text(TextRun {
                    text: " B".into(),
                    style: Default::default(),
                    link: None,
                }),
            ],
            style: Default::default(),
            source: None,
        };
        let start = SourceAnchor {
            spine: SpineItemId::new("s").unwrap(),
            node: "p".into(),
            text_offset: 0,
        };
        block.source = Some(SourceRange {
            start: start.clone(),
            end: SourceAnchor {
                text_offset: 3,
                ..start
            },
        });
        assert_eq!(text_block_text(&block), "Aか B");
        let kana = text_block_match_range(&block, 1, 4);
        assert_eq!((kana.start.text_offset, kana.end.text_offset), (1, 1));
        let following = text_block_match_range(&block, 5, 6);
        assert_eq!(
            (following.start.text_offset, following.end.text_offset),
            (2, 3)
        );
    }

    struct SearchSource {
        book: Book,
        sections: Vec<Section>,
    }

    #[test]
    fn search_inside_structured_figure_returns_the_child_source() {
        let descriptor = SpineItem {
            id: SpineItemId::new("s").unwrap(),
            href: PublicationUrl::parse("s.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: vec![],
        };
        let section=rebook_html::parse_section("<html><body><div><p>Example needle text.</p><ol><li><p>Another needle.</p></li></ol></div><p>Figure 4.3 Example</p></body></html>",&descriptor,|_|None).unwrap();
        let expected: Vec<_> = section.blocks[0]
            .text_blocks()
            .take(2)
            .map(|t| t.source.as_ref().unwrap().start.node.clone())
            .collect();
        let source = SearchSource {
            book: Book {
                id: PublicationId::new("figure-search").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: vec![descriptor],
                table_of_contents: vec![],
            },
            sections: vec![section],
        };
        let results = search_book(&source, "needle", 10).unwrap();
        assert_eq!(results.len(), 2);
        for (result, node) in results.iter().zip(expected) {
            assert_eq!(result.range.start.node, node);
            assert!(result.excerpt.contains("needle"));
        }
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
        assert_eq!(
            &results[0].excerpt[results[0].excerpt_match.clone()],
            "Systems"
        );
        assert_eq!(
            &results[1].excerpt[results[1].excerpt_match.clone()],
            "systems"
        );
        assert!(results[1].excerpt_match.start > results[0].excerpt_match.start);
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
                    ocr_text: None,
                    formula_image: false,
                    formula: None,
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
        assert_eq!(
            excerpt("alpha beta gamma", 6, 10, 2),
            ("…a beta g…".into(), 5..9)
        );
    }

    #[test]
    fn preview_prefers_the_sentence_containing_this_occurrence() {
        let text = "人们会产生五花八门的联想。人工纤维织物只能承受低温清洗。下一句不应该出现。";
        let start = text.find("纤维").unwrap();
        let (preview, matched) = excerpt(text, start, start + "纤维".len(), 72);
        assert_eq!(preview, "人工纤维织物只能承受低温清洗。");
        assert_eq!(&preview[matched], "纤维");
    }

    #[test]
    fn preview_keeps_abbreviations_decimals_and_exact_repeated_occurrences() {
        let text = "Previous sentence. Dr. Myers uses Fig. 4.4 and e.g. a value of 3.14 to describe reading. Next sentence.";
        let start = text.find("reading").unwrap();
        let (preview, matched) = excerpt(text, start, start + 7, 100);
        assert_eq!(
            preview,
            "Dr. Myers uses Fig. 4.4 and e.g. a value of 3.14 to describe reading."
        );
        assert_eq!(&preview[matched], "reading");
        let text = "纤维和纤维有区别。";
        let start = text.rfind("纤维").unwrap();
        let (preview, matched) = excerpt(text, start, start + "纤维".len(), 72);
        assert_eq!(matched.start, "纤维和".len());
        assert_eq!(&preview[matched], "纤维");
    }

    #[test]
    fn preview_context_is_bounded_and_utf8_safe_for_large_paragraphs() {
        let text = format!("{}纤维{}", "长🙂".repeat(10_000), "字👩‍💻".repeat(10_000));
        let start = text.find("纤维").unwrap();
        let (preview, matched) = excerpt(&text, start, start + "纤维".len(), 72);
        assert!(preview.chars().count() <= 72 * 2 + 4);
        assert!(preview.starts_with('…') && preview.ends_with('…'));
        assert_eq!(&preview[matched], "纤维");
        let (preview, matched) = excerpt("  words\tkeyword\twords  ", 8, 15, 72);
        assert_eq!(&preview[matched], "keyword");
    }
}

#[cfg(test)]
mod library_regressions;
