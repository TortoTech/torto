//! Source-based reading text and linked notes shared by native hosts.
use rebook_publication::{
    Block, Inline, InlineRole, LinkRole, PublicationUrl, Section, SourceAnchor, SourceRange,
    TextBlock, TextBlockKind,
};

/// Authored text eligible for the focused paragraph structure action on every host.
pub fn focus_block_structure_ranges(block: &Block) -> Vec<SourceRange> {
    match block {
        Block::Text(text)
            if matches!(
                text.kind,
                TextBlockKind::Paragraph
                    | TextBlockKind::Blockquote
                    | TextBlockKind::Caption
                    | TextBlockKind::ListItem { .. }
            ) =>
        {
            text.source.iter().cloned().collect()
        }
        Block::Figure(figure) => figure.text_blocks()
            .filter_map(|caption| caption.source.clone())
            .collect(),
        Block::Quote(quote) => quote
            .body
            .iter()
            .filter(|body| {
                matches!(
                    body.kind,
                    TextBlockKind::Paragraph | TextBlockKind::Blockquote
                )
            })
            .filter_map(|body| body.source.clone())
            .collect(),
        Block::Text(_)
        | Block::Table(_)
        | Block::Image(_)
        | Block::Note(_)
        | Block::Separator(_)
        | Block::LineBreak
        | Block::PageBreak => Vec::new(),
    }
}


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
    let block = resolved_footnote_block(section, target)?;
    normalize_resolved_footnote_text(&block_focus_text(block), marker)
}

fn resolved_footnote_block<'a>(section: &'a Section, target: &PublicationUrl) -> Option<&'a Block> {
    let fragment = target.fragment()?;
    let anchor = &section.anchors.iter().find(|a| a.fragment == fragment)?.source;
    section.blocks.iter()
        .find_map(|block| note_definition_containing_anchor(block, anchor))
        .or_else(|| section.blocks.iter().find_map(|block| find_block_containing_anchor(block, anchor)))
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
    let body = strip_note_marker(text, marker).unwrap_or(text);
    (!body.is_empty()).then(|| body.to_owned())
}

fn note_marker_value(marker: &str) -> &str {
    marker.trim().trim_matches(['[', ']', '(', ')', '（', '）', '【', '】', '.', '．']).trim()
}

fn after_note_marker(text: &str) -> &str {
    text.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '.' | '．' | '，' | '、' | ')' | '）' | ']' | '】')
    })
}

fn strip_note_marker<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let value = note_marker_value(marker);
    if value.is_empty() {
        return None;
    }
    let text = text.trim_start();
    if let Some((open, close)) = [('[' , ']'), ('(', ')'), ('（', '）'), ('【', '】')]
        .into_iter()
        .find(|(open, _)| text.starts_with(*open))
    {
        let (label, rest) = text.strip_prefix(open)?.split_once(close)?;
        return (label.trim() == value).then(|| after_note_marker(rest));
    }
    let rest = text.strip_prefix(value)?;
    (rest.is_empty() || after_note_marker(rest).len() < rest.len())
        .then(|| after_note_marker(rest))
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
    marker: &str,
    unified: bool,
) -> Option<String> {
    let block = resolved_footnote_block(section, target)?;
    let mut paragraphs = Vec::new();
    let mut removed_marker = false;
    for text in block.text_blocks() {
        let mut content = text.content.clone();
        if paragraphs.is_empty() {
            // Remove the leading backlink before escaping Markdown or introducing
            // emphasis syntax. A normal link whose label is the marker is a
            // legacy backlink; other links in the note body remain selectable.
            let first = content.iter_mut().position(|inline| match inline {
                Inline::Text(run) => {
                    if run.text.trim().is_empty() {
                        return false;
                    }
                    if run.style.link_role == LinkRole::FootnoteBacklink
                        || (!removed_marker && run.link.is_some()
                            && !note_marker_value(marker).is_empty()
                            && note_marker_value(&run.text) == note_marker_value(marker))
                    {
                        removed_marker = true;
                        return false;
                    }
                    if !removed_marker && let Some(body) = strip_note_marker(&run.text, marker) {
                        run.text = body.to_owned();
                        removed_marker = true;
                    } else if removed_marker {
                        run.text = after_note_marker(&run.text).to_owned();
                    }
                    !run.text.is_empty()
                }
                Inline::Break => false,
                _ => true,
            });
            let Some(first) = first else { continue };
            content.drain(..first);
        }
        if unified {
            rebook_layout::apply_translated_note_emphasis(&mut content);
        }
        let markup = note_inline_markdown(&content);
        if !markup.trim().is_empty() {
            paragraphs.push(markup);
        }
    }
    (!paragraphs.is_empty()).then(|| paragraphs.join("\n\n"))
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

    #[test]
    fn popup_removes_linked_marker_and_applies_unified_translation_emphasis() {
        let href = PublicationUrl::parse("notes.xhtml").unwrap();
        let descriptor = rebook_publication::SpineItem {
            id: rebook_publication::SpineItemId::new("notes").unwrap(),
            href: href.clone(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let mut section = rebook_html::parse_section(
            r#"<html><body><p id="note"><a href="chapter.xhtml#ref"><b>11</b></a>. <em>译文</em> <a href="https://example.com">来源</a></p></body></html>"#,
            &descriptor, |_| None,
        ).unwrap();
        let target = href.resolve("#note").unwrap();
        let original = focus_footnote_markdown_in_section(&section, &target, "[11]", true).unwrap();
        assert!(original.starts_with("*译文*"), "{original}");
        if let Block::Text(block) = &mut section.blocks[0] {
            for inline in &mut block.content {
                if let Inline::Text(run) = inline {
                    run.style.display_writing_system = Some(rebook_publication::WritingSystem::Cjk);
                }
            }
        }
        let unified = focus_footnote_markdown_in_section(&section, &target, "[11]", true).unwrap();
        assert!(unified.starts_with("**译文**"), "{unified}");
        assert!(unified.contains("https://example.com"));
        assert!(!unified.contains("chapter.xhtml"));
        assert_eq!(normalize_resolved_footnote_text("11", "11"), None);
        assert_eq!(normalize_resolved_footnote_text("110 is the total", "11").as_deref(), Some("110 is the total"));
    }
}

/// Extend visible body demand with only its linked footnote definitions.
pub fn append_linked_footnote_translation_ranges(
    source: &dyn rebook_publication::BookSource,
    sections: &mut Vec<(usize, Vec<SourceRange>)>,
) {
    let visible_sections = sections.clone();
    let mut parsed_sections = std::collections::HashMap::new();
    let mut footnote_targets = Vec::new();
    for (section_index, ranges) in &visible_sections {
        let Ok(section) = source.parse_section(*section_index) else {
            continue;
        };
        for block in &section.blocks {
            let visible = block_source_range(block).is_some_and(|source| {
                ranges
                    .iter()
                    .any(|range| translation_ranges_overlap(source, range))
            });
            if !visible {
                continue;
            }
            for footnote in block_focus_footnotes(block) {
                if let FocusFootnoteSource::Reference { target, .. } = footnote
                    && !footnote_targets.contains(&target)
                {
                    footnote_targets.push(target);
                }
            }
        }
        parsed_sections.insert(*section_index, section);
    }
    for target in footnote_targets {
        let Some(section_index) = source
            .book()
            .sections
            .iter()
            .position(|section| section.href.path() == target.path())
        else {
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(entry) =
            parsed_sections.entry(section_index)
        {
            let Ok(section) = source.parse_section(section_index) else {
                continue;
            };
            entry.insert(section);
        }
        let Some(section) = parsed_sections.get(&section_index) else {
            continue;
        };
        let footnote_ranges = focus_footnote_translation_ranges_in_section(section, &target);
        if footnote_ranges.is_empty() {
            continue;
        }
        if let Some((_, ranges)) = sections
            .iter_mut()
            .find(|(candidate, _)| *candidate == section_index)
        {
            for range in footnote_ranges {
                if !ranges.contains(&range) {
                    ranges.push(range);
                }
            }
        } else {
            sections.push((section_index, footnote_ranges));
        }
    }
}

fn translation_ranges_overlap(left: &SourceRange, right: &SourceRange) -> bool {
    left.start.spine == right.start.spine
        && (left.start.node == right.start.node
            || left.start.node == right.end.node
            || left.end.node == right.start.node
            || left.end.node == right.end.node)
}


pub fn focus_footnote_translation_ranges_in_section(
    section: &Section,
    target: &PublicationUrl,
) -> Vec<SourceRange> {
    let Some(target_block) = resolved_footnote_block(section, target) else {
        return Vec::new();
    };
    let mut ranges = Vec::new();
    collect_focus_footnote_translation_ranges(target_block, &mut ranges);
    ranges
}

fn collect_focus_footnote_translation_ranges(block: &Block, ranges: &mut Vec<SourceRange>) {
    let mut push_text = |text: &TextBlock| {
        if let Some(range) = &text.source
            && !ranges.contains(range)
        {
            ranges.push(range.clone());
        }
    };
    match block {
        Block::Text(text) => push_text(text),
        Block::Quote(quote) => {
            for text in quote.body.iter().chain(quote.attribution.iter()) {
                push_text(text);
            }
        }
        Block::Table(table) => {
            for text in table.text_blocks() {
                push_text(text);
            }
        }
        Block::Figure(figure) => {
            for caption in figure.text_blocks() {
                push_text(caption);
            }
        }
        Block::Note(note) => {
            for child in &note.blocks {
                collect_focus_footnote_translation_ranges(child, ranges);
            }
        }
        Block::Image(image) => {
            if let Some(range) = &image.source
                && !ranges.contains(range)
            {
                ranges.push(range.clone());
            }
        }
        Block::Separator(_) | Block::LineBreak | Block::PageBreak => {}
    }
}
