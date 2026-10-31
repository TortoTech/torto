//! Source-based reading text and linked notes shared by native hosts.
use rebook_publication::{
    Block, Inline, InlineRole, LinkRole, PublicationUrl, Section, SourceAnchor, SourceRange,
    TextBlock,
};

pub enum FocusFootnoteSource {
    Citation {
        text: String,
        source: SourceRange,
        number: u32,
        display_number: u32,
    },
    Inline(String, u32),
    Reference {
        number: u32,
        marker: String,
        target: PublicationUrl,
    },
}

/// Resolve an assistant's durable node, including figure captions and table cells.
pub fn source_range_for_node(
    source: &dyn rebook_publication::BookSource,
    section_index: usize,
    node: &str,
) -> Option<SourceRange> {
    source
        .parse_section(section_index)
        .ok()?
        .blocks
        .iter()
        .find_map(|block| {
            block_source_range(block)
                .filter(|r| r.start.node == node)
                .cloned()
                .or_else(|| {
                    block
                        .text_blocks()
                        .filter_map(|t| t.source.clone())
                        .find(|r| r.start.node == node)
                })
        })
}

pub fn block_source_range(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Text(block) => block.source.as_ref(),
        Block::Quote(quote) => quote.source.as_ref(),
        Block::Table(table) => table.source.as_ref(),
        Block::Image(image) => image.source.as_ref(),
        Block::Figure(figure) => figure.source.as_ref(),
        Block::Note(note) => note.source.as_ref(),
        Block::Separator(_) | Block::LineBreak | Block::PageBreak => None,
    }
}

pub fn text_block_focus_footnotes(block: &TextBlock) -> Vec<FocusFootnoteSource> {
    let mut notes = Vec::new();
    let mut footnotes = rebook_layout::paragraph_footnotes(block)
        .into_iter()
        .enumerate()
        .peekable();
    let mut index = 0;
    // Walk the inlines once so references, inline notes and citations stay
    // interleaved in their authored order, independently of their numbering.
    while index < block.content.len() {
        if footnotes
            .peek()
            .is_some_and(|(_, range)| range.start == index)
        {
            let (ordinal, range) = footnotes.next().unwrap();
            let Inline::Text(first) = &block.content[range.start] else {
                unreachable!()
            };
            index = range.end;
            let text = block.content[range]
                .iter()
                .flat_map(Inline::text_runs)
                .map(|run| run.text.as_str())
                .collect::<String>();
            let number = if first.style.footnote_number == 0 {
                ordinal as u32 + 1
            } else {
                first.style.footnote_number
            };
            if first.style.inline_role == InlineRole::Footnote {
                notes.push(FocusFootnoteSource::Inline(text.trim().to_owned(), number));
            } else if let Some(target) = &first.link {
                notes.push(FocusFootnoteSource::Reference {
                    marker: text.trim().to_owned(),
                    target: target.clone(),
                    number,
                });
            }
            continue;
        }
        let Inline::Text(run) = &block.content[index] else {
            index += 1;
            continue;
        };
        let number = run.style.inline_citation;
        let display_number = if run.style.citation_number == 0 {
            number
        } else {
            run.style.citation_number
        };
        let Some(source) = block.source.as_ref().filter(|_| number != 0) else {
            index += 1;
            continue;
        };
        let mut text = String::new();
        while let Some(Inline::Text(run)) = block.content.get(index) {
            if run.style.inline_citation != number {
                break;
            }
            text.push_str(&run.text);
            index += 1;
        }
        notes.push(FocusFootnoteSource::Citation {
            text,
            source: source.clone(),
            number,
            display_number,
        });
    }
    notes
}

pub fn block_focus_footnotes(block: &Block) -> Vec<FocusFootnoteSource> {
    if let Block::Text(text) = block {
        return text_block_focus_footnotes(text);
    }
    let numbered = rebook_layout::numbered_semantic_footnotes(block);
    match numbered.as_ref() {
        Block::Text(block) => text_block_focus_footnotes(block),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(quote.attribution.iter())
            .flat_map(text_block_focus_footnotes)
            .collect(),
        Block::Table(table) => table
            .text_blocks()
            .flat_map(text_block_focus_footnotes)
            .collect(),
        Block::Figure(figure) => figure
            .text_blocks()
            .flat_map(text_block_focus_footnotes)
            .collect(),
        Block::Note(_) => Vec::new(),
        Block::Image(_) | Block::Separator(_) | Block::LineBreak | Block::PageBreak => Vec::new(),
    }
}

pub fn focus_footnote_text_in_section(
    section: &Section,
    target: &PublicationUrl,
    marker: &str,
) -> Option<String> {
    let fragment = target.fragment()?;
    let anchor = section
        .anchors
        .iter()
        .find(|anchor| anchor.fragment == fragment)?
        .source
        .clone();
    if let Some(note) = section
        .blocks
        .iter()
        .find_map(|block| note_definition_containing_anchor(block, &anchor))
    {
        return normalize_resolved_footnote_text(&block_focus_text(note), marker);
    }
    let block = section
        .blocks
        .iter()
        .find_map(|block| find_block_containing_anchor(block, &anchor))?;
    normalize_resolved_footnote_text(&block_focus_text(block), marker)
}

pub fn find_block_containing_anchor<'a>(
    block: &'a Block,
    anchor: &SourceAnchor,
) -> Option<&'a Block> {
    if let Block::Note(note) = block {
        return note
            .blocks
            .iter()
            .find_map(|child| find_block_containing_anchor(child, anchor));
    }
    (block_source_range(block).is_some_and(|range| source_range_contains_anchor(range, anchor))
        || block_source_range(block).is_some_and(|range| {
            focus_block_paint_ranges(block, range)
                .iter()
                .any(|range| source_range_contains_anchor(range, anchor))
        }))
    .then_some(block)
}

pub fn note_definition_containing_anchor<'a>(
    block: &'a Block,
    anchor: &SourceAnchor,
) -> Option<&'a Block> {
    let Block::Note(note) = block else {
        return None;
    };
    for child in &note.blocks {
        if let Some(definition) = note_definition_containing_anchor(child, anchor) {
            return Some(definition);
        }
    }
    note.blocks
        .iter()
        .any(|child| find_block_containing_anchor(child, anchor).is_some())
        .then_some(block)
}

pub fn normalize_resolved_footnote_text(text: &str, marker: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let without_marker = text.strip_prefix(marker.trim()).map_or(text, |rest| {
        rest.trim_start_matches(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '.' | '．' | '，' | '、' | ')' | '）' | ']' | '】'
                )
        })
    });
    Some(without_marker.to_owned())
}

pub fn text_block_focus_text(block: &TextBlock) -> String {
    let mut leading_backlink = false;
    let mut started = false;
    block
        .content
        .iter()
        .filter_map(|inline| match inline {
            Inline::Ruby(run) => Some(run.base_text()),
            Inline::Text(run)
                if matches!(
                    run.style.inline_role,
                    InlineRole::Footnote | InlineRole::PageMarker
                ) =>
            {
                None
            }
            Inline::Text(run) if run.style.link_role == LinkRole::FootnoteBacklink => {
                leading_backlink |= !started;
                None
            }
            Inline::Text(run) => {
                let text = if leading_backlink && !started {
                    run.text.trim_start_matches(|ch: char| {
                        ch.is_whitespace()
                            || matches!(ch, '.' | '．' | ')' | '）' | ']' | '】' | '、')
                    })
                } else {
                    run.text.as_str()
                };
                started |= !text.trim().is_empty();
                Some(text.to_owned())
            }
            Inline::Math(run) => Some(run.latex.clone()),
            Inline::Image(run) => run
                .image
                .ocr_text
                .as_ref()
                .map(|text| text.display_text().to_owned())
                .or_else(|| {
                    run.image
                        .formula
                        .as_ref()
                        .map(|f| format!(r"\({}\)", f.latex))
                }),
            Inline::Break => Some("\n".to_owned()),
        })
        .collect()
}

pub fn block_focus_text(block: &Block) -> String {
    match block {
        Block::Text(block) => text_block_focus_text(block),
        Block::Quote(quote) => quote
            .body
            .iter()
            .chain(quote.attribution.iter())
            .map(text_block_focus_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Block::Table(table) => table
            .text_blocks()
            .map(text_block_focus_text)
            .collect::<Vec<_>>()
            .join(" "),
        Block::Image(image) => image
            .formula
            .as_ref()
            .map_or_else(|| image.alt.clone(), |f| format!(r"\({}\)", f.latex)),
        Block::Figure(figure) => figure
            .text_blocks()
            .map(text_block_focus_text)
            .collect::<Vec<_>>()
            .join(" "),
        Block::Note(note) => note
            .blocks
            .iter()
            .map(block_focus_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Block::Separator(_) | Block::LineBreak | Block::PageBreak => String::new(),
    }
}

pub fn focus_block_paint_ranges(block: &Block, range: &SourceRange) -> Vec<SourceRange> {
    if let Block::Quote(quote) = block {
        let ranges = quote
            .body
            .iter()
            .filter_map(|block| block.source.clone())
            .chain(
                quote
                    .attribution
                    .iter()
                    .filter_map(|block| block.source.clone()),
            )
            .collect::<Vec<_>>();
        if !ranges.is_empty() {
            return ranges;
        }
    }
    if let Block::Table(table) = block {
        let mut cell_ranges = table
            .text_blocks()
            .filter_map(|text| text.source.clone())
            .collect::<Vec<_>>();
        if table.visualization.as_ref().is_some_and(|v| v.active) {
            cell_ranges.push(range.clone());
        }
        if !cell_ranges.is_empty() {
            return cell_ranges;
        }
    }
    if let Block::Figure(figure) = block {
        let figure_ranges = figure
            .images
            .iter()
            .filter_map(|image| image.source.clone())
            .chain(
                figure
                    .captions
                    .iter()
                    .filter_map(|caption| caption.source.clone()),
            )
            .chain(figure.body.iter().flat_map(|child| {
                focus_block_paint_ranges(child, block_source_range(child).unwrap_or(range))
            }))
            .collect::<Vec<_>>();
        if !figure_ranges.is_empty() {
            return figure_ranges;
        }
    }
    vec![range.clone()]
}

pub fn source_range_contains_anchor(range: &SourceRange, anchor: &SourceAnchor) -> bool {
    if range.start.spine != anchor.spine || range.start.node != anchor.node {
        return false;
    }
    if range.start.spine != range.end.spine || range.start.node != range.end.node {
        return range.start == *anchor;
    }
    anchor.text_offset >= range.start.text_offset
        && (anchor.text_offset < range.end.text_offset
            || (range.start.text_offset == range.end.text_offset
                && anchor.text_offset == range.start.text_offset))
}

/// Portable popup content: keep publication emphasis, links and authored formulas.
/// Markdown is an interchange view only; durable positions remain in the source IR.
pub fn note_inline_markdown(inlines: &[Inline]) -> String {
    fn escape(s: &str) -> String {
        s.chars()
            .flat_map(|c| {
                if matches!(
                    c,
                    '\\' | '*' | '_' | '`' | '[' | ']' | '$' | '<' | '>' | '#' | '|'
                ) {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect()
    }
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(r) => {
                let mut value = escape(&r.text);
                if r.style.bold {
                    value = format!("**{value}**");
                }
                if r.style.italic {
                    value = format!("*{value}*");
                }
                if let Some(url) = &r.link {
                    value = format!("[{value}](<{}>)", url.to_string().replace('>', "%3E"));
                }
                out.push_str(&value);
            }
            Inline::Math(m) => {
                if m.display {
                    out.push_str(&format!("\n\n$${}$$\n\n", m.latex));
                } else {
                    out.push_str(&format!("${}$", m.latex));
                }
            }
            Inline::Ruby(r) => out.push_str(&escape(&r.base_text())),
            Inline::Image(i) => {
                if let Some(m) = &i.image.formula {
                    out.push_str(&format!("${}$", m.latex));
                } else {
                    out.push_str(&escape(&i.image.alt));
                }
            }
            Inline::Break => out.push_str("  \n"),
        }
    }
    out
}
pub fn note_block_markdown(block: &Block) -> String {
    if let Block::Note(n) = block {
        return n
            .blocks
            .iter()
            .map(note_block_markdown)
            .collect::<Vec<_>>()
            .join("\n\n");
    }
    block
        .text_blocks()
        .map(|b| note_inline_markdown(&b.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}
pub fn focus_footnote_markdown_in_section(
    section: &Section,
    target: &PublicationUrl,
) -> Option<String> {
    let fragment = target.fragment()?;
    let anchor = &section
        .anchors
        .iter()
        .find(|a| a.fragment == fragment)?
        .source;
    let block = section
        .blocks
        .iter()
        .find_map(|b| note_definition_containing_anchor(b, anchor))
        .or_else(|| {
            section
                .blocks
                .iter()
                .find_map(|b| find_block_containing_anchor(b, anchor))
        })?;
    Some(note_block_markdown(block))
}
#[cfg(test)]
mod popup_tests {
    use super::*;
    #[test]
    fn popup_keeps_emphasis_links_math_and_literal_punctuation() {
        let inline = vec![
            Inline::Text(rebook_publication::TextRun {
                text: "Bold [literal]".into(),
                style: rebook_publication::TextStyle {
                    bold: true,
                    ..Default::default()
                },
                link: Some(PublicationUrl::website("https://example.com/note").unwrap()),
            }),
            Inline::Math(rebook_publication::MathRun {
                latex: r"\frac{1}{2}".into(),
                display: false,
                size_scale: 1.0,
                original: None,
            }),
        ];
        let value = note_inline_markdown(&inline);
        assert!(value.contains("**Bold \\[literal\\]**"));
        assert!(value.contains("https://example.com/note"));
        assert!(value.contains(r"$\frac{1}{2}$"));
    }
}
