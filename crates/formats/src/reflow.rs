//! Source-independent reflow rules. Geometry and provider roles are evidence,
//! not substitutes for semantic barriers or stable source positions.
use std::collections::{HashMap, HashSet};

use rebook_publication::{
    Block, CaptionPosition, FigureBlock, Inline, Section, SourceAnchor, TextBlock, TextBlockKind,
    TocEntry,
};

/// Increment when OCR normalization changes source identities or offsets.
pub const VERSION: u32 = 2;

/// Directory validation shared with EPUB/CHM and native PDF outline recovery.
pub struct HeadingHints(HashMap<String, Vec<crate::source::TocHeadingHint>>);

impl HeadingHints {
    pub fn new(entries: &[TocEntry]) -> Self {
        Self(crate::source::collect_toc_heading_hints(entries))
    }

    pub fn apply(&self, section: &mut Section) {
        if let Some(hints) = self.0.get(section.href.path()) {
            crate::source::promote_toc_headings(section, hints);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ContinuationEvidence {
    /// Native geometry or an explicit provider continuation relationship.
    Geometry,
    /// Adjacent physical pages, with no intervening structural block.
    PageBoundary,
    /// A captioned image interrupts prose, but geometry is unavailable.
    CaptionedFloat,
}

pub fn text(block: &TextBlock) -> String {
    block
        .content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|r| r.text.as_str())
        .collect()
}

/// A hard hyphen is removable only with an independent complete spelling.
/// This rule is shared with native captions; an authored compound stays intact.
pub fn proven_hyphen_join(left: &str, right: &str, words: &HashSet<String>) -> bool {
    let Some(left) = left.strip_suffix('-') else {
        return false;
    };
    let prefix: String = left
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let suffix: String = right.chars().take_while(|c| c.is_alphabetic()).collect();
    !prefix.is_empty()
        && suffix.chars().next().is_some_and(char::is_lowercase)
        && words.contains(&format!("{prefix}{suffix}").to_lowercase())
}

fn recover_ocr_hyphen(left: &mut TextBlock, right: &TextBlock, words: &HashSet<String>) -> bool {
    let tail = text(left);
    if !tail.ends_with('\u{ad}') && !proven_hyphen_join(&tail, &text(right), words) {
        return false;
    }
    if let Some(Inline::Text(run)) = left.content.last_mut() {
        run.text.pop();
        if let Some(source) = &mut left.source {
            source.end.text_offset = source.end.text_offset.saturating_sub(1);
        }
        return true;
    }
    false
}

/// No length threshold. Sentence ends, numbered items and new headings remain
/// barriers. Without geometry, floating-image joins need Latin lowercase proof.
pub fn continues(left: &str, right: &str, evidence: ContinuationEvidence) -> bool {
    let left = left
        .trim_end()
        .trim_end_matches(['"', '\'', '”', '’', ')', ']', '}', '」', '』', '）', '】']);
    if left.is_empty() || left.ends_with(['.', '!', '?', '。', '！', '？', ':', '：', ';', '；'])
    {
        return false;
    }
    let Some(first) = right.trim_start().chars().next() else {
        return false;
    };
    if first.is_numeric() || matches!(first, '•' | '●' | '▪' | '◦' | '–' | '—') {
        return false;
    }
    if first.is_ascii() && !first.is_ascii_lowercase() {
        return false;
    }
    evidence != ContinuationEvidence::CaptionedFloat || first.is_ascii_lowercase()
}

/// Mapping for the removed node, usable by anchors and native glyph provenance.
pub struct SourceMove {
    pub from: SourceAnchor,
    pub to: SourceAnchor,
}

impl SourceMove {
    pub fn apply(&self, anchor: &mut SourceAnchor) {
        if anchor.spine == self.from.spine && anchor.node == self.from.node {
            anchor.text_offset =
                self.to.text_offset + anchor.text_offset.saturating_sub(self.from.text_offset);
            anchor.node.clone_from(&self.to.node);
            anchor.spine.clone_from(&self.to.spine);
        }
    }
}

/// Retain runs (links, math and emphasis) and return the exact scalar-offset map.
pub fn append_text(left: &mut TextBlock, right: TextBlock) -> Option<SourceMove> {
    append_text_inner(left, right, false)
}

fn append_text_inner(
    left: &mut TextBlock,
    mut right: TextBlock,
    word_join: bool,
) -> Option<SourceMove> {
    let a = left.source.as_ref()?;
    let b = right.source.as_ref()?;
    let space = !word_join
        && text(left)
            .chars()
            .next_back()
            .zip(text(&right).chars().next())
            .is_some_and(|(a, b)| a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric());
    let mut to = a.end.clone();
    to.node.clone_from(&a.start.node);
    to.text_offset += u64::from(space);
    let movement = SourceMove {
        from: b.start.clone(),
        to,
    };
    left.source.as_mut()?.end.text_offset =
        movement.to.text_offset + b.end.text_offset.saturating_sub(b.start.text_offset);
    if space {
        // Copying a normal text run preserves language, but the inserted space
        // is not part of a link or footnote marker.
        let mut run = left
            .content
            .iter()
            .flat_map(Inline::text_runs)
            .next()?
            .clone();
        run.text = " ".into();
        run.link = None;
        run.style.link_role = Default::default();
        left.content.push(Inline::Text(run));
    }
    left.content.append(&mut right.content);
    Some(movement)
}

fn source_node(block: &Block) -> Option<&str> {
    match block {
        Block::Text(t) => t.source.as_ref(),
        Block::Image(i) => i.source.as_ref(),
        Block::Figure(f) => f.source.as_ref(),
        Block::Table(t) => t.source.as_ref(),
        _ => None,
    }
    .map(|s| s.start.node.as_str())
}

fn paragraph(block: &Block) -> Option<&TextBlock> {
    match block {
        Block::Text(t) if t.kind == TextBlockKind::Paragraph => Some(t),
        _ => None,
    }
}

fn float(block: &Block) -> bool {
    matches!(block, Block::Figure(f) if !f.captions.is_empty() && !f.images.is_empty()
        && f.images.iter().all(|i| !i.formula_image))
}

/// Normalize an OCR section on its parsing worker, without retaining another
/// document/DOM. The caller supplies physical page starts from existing anchors.
/// Special-page images, tables, lists, formulas and titles remain barriers.
pub fn normalize_ocr(section: &mut Section, headings: &HeadingHints, page_prefix: &str) {
    normalize_ocr_with_continuations(section, headings, page_prefix, &HashSet::new());
}

/// `continuations` contains only source nodes backed by explicit provider data.
pub fn normalize_ocr_with_continuations(
    section: &mut Section,
    headings: &HeadingHints,
    page_prefix: &str,
    continuations: &HashSet<String>,
) {
    headings.apply(section);
    let words = section
        .blocks
        .iter()
        .flat_map(|block| match block {
            Block::Text(t) => vec![text(t)],
            Block::Figure(f) => f.captions.iter().map(text).collect(),
            _ => vec![],
        })
        .flat_map(|text| {
            text.split(|c: char| !c.is_alphabetic())
                .filter(|word| !word.is_empty())
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .collect::<HashSet<_>>();
    let mut starts = HashMap::new();
    for anchor in &section.anchors {
        if let Some(page) = anchor
            .fragment
            .strip_prefix(page_prefix)
            .and_then(|n| n.parse::<usize>().ok())
        {
            // Empty physical pages can bind several markers to the next node.
            // Use its actual (last) page, so blank pages cannot authorize a join.
            starts
                .entry(anchor.source.node.clone())
                .and_modify(|p: &mut usize| *p = (*p).max(page))
                .or_insert(page);
        }
    }
    let mut page = 0;
    let mut pages = HashMap::new();
    for block in &section.blocks {
        if let Some(node) = source_node(block) {
            if let Some(start) = starts.get(node) {
                page = *start;
            }
            pages.insert(node.to_owned(), page);
        }
    }

    // The HTML parser already identifies numbered captions. OCR HTML commonly
    // puts image and caption in separate divs, so associate adjacent IR blocks.
    let mut i = 0;
    while i < section.blocks.len() {
        if !matches!(&section.blocks[i], Block::Image(image) if !image.formula_image) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while matches!(section.blocks.get(j), Some(Block::Image(image)) if !image.formula_image) {
            j += 1;
        }
        let caption = j;
        while matches!(section.blocks.get(j), Some(Block::Text(t)) if t.kind == TextBlockKind::Caption)
        {
            j += 1;
        }
        if j == caption {
            i = j;
            continue;
        }
        let source = match &section.blocks[i] {
            Block::Image(image) => image.source.clone(),
            _ => unreachable!(),
        };
        let mut images = Vec::new();
        let mut captions = Vec::new();
        for block in section.blocks.drain(i..j) {
            match block {
                Block::Image(image) => images.push(image),
                Block::Text(t) => captions.push(t),
                _ => unreachable!(),
            }
        }
        section.blocks.insert(
            i,
            Block::Figure(FigureBlock {
                images,
                captions,
                caption_position: CaptionPosition::After,
                style: Default::default(),
                source,
            }),
        );
        i += 1;
    }

    // A caption can itself be interrupted at a line/page boundary. Require an
    // unfinished caption and a lowercase continuation; never absorb a completed
    // caption's following body paragraph merely because it is adjacent.
    let mut i = 0;
    while i + 1 < section.blocks.len() {
        let good = match (&section.blocks[i], &section.blocks[i + 1]) {
            (Block::Figure(f), Block::Text(t))
                if matches!(t.kind, TextBlockKind::Caption | TextBlockKind::Paragraph) =>
            {
                f.captions.last().is_some_and(|c| {
                    continues(&text(c), &text(t), ContinuationEvidence::CaptionedFloat)
                })
            }
            _ => false,
        };
        if !good {
            i += 1;
            continue;
        }
        let Block::Text(next) = section.blocks.remove(i + 1) else {
            unreachable!()
        };
        let Block::Figure(f) = &mut section.blocks[i] else {
            unreachable!()
        };
        let word_join = recover_ocr_hyphen(f.captions.last_mut().unwrap(), &next, &words);
        if let Some(movement) = append_text_inner(f.captions.last_mut().unwrap(), next, word_join) {
            for anchor in &mut section.anchors {
                movement.apply(&mut anchor.source);
            }
        }
    }

    let mut i = 0;
    while i < section.blocks.len() {
        let Some(left) = paragraph(&section.blocks[i]) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while section.blocks.get(j).is_some_and(float) {
            j += 1;
        }
        let Some(right) = section.blocks.get(j).and_then(paragraph) else {
            i += 1;
            continue;
        };
        let (Some(a), Some(b)) = (&left.source, &right.source) else {
            i += 1;
            continue;
        };
        let evidence = if continuations.contains(&b.start.node) {
            ContinuationEvidence::Geometry
        } else if j > i + 1
            && pages
                .get(&a.start.node)
                .zip(pages.get(&b.start.node))
                .is_none_or(|(a, b)| *a == 0 || (*b >= *a && *b <= *a + 1))
        {
            ContinuationEvidence::CaptionedFloat
        } else if pages
            .get(&a.start.node)
            .zip(pages.get(&b.start.node))
            .is_some_and(|(a, b)| *a > 0 && *b == *a + 1)
        {
            ContinuationEvidence::PageBoundary
        } else {
            i += 1;
            continue;
        };
        if left.style.hard_break_after || !continues(&text(left), &text(right), evidence) {
            i += 1;
            continue;
        }
        let Block::Text(right) = section.blocks.remove(j) else {
            unreachable!()
        };
        let Block::Text(left) = &mut section.blocks[i] else {
            unreachable!()
        };
        let word_join = recover_ocr_hyphen(left, &right, &words);
        if let Some(movement) = append_text_inner(left, right, word_join) {
            for anchor in &mut section.anchors {
                movement.apply(&mut anchor.source);
            }
            // Track the tail page after a join, so consecutive page boundaries
            // can continue without swallowing independent same-page paragraphs.
            if let Some(page) = pages.get(&movement.from.node).copied() {
                pages.insert(movement.to.node, page);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{PublicationUrl, SpineItem, SpineItemId};

    fn parse(html: &str) -> Section {
        rebook_html::parse_section(
            &format!("<html><body>{html}</body></html>"),
            &SpineItem {
                id: SpineItemId::new("test").unwrap(),
                href: PublicationUrl::parse("Text/test.xhtml").unwrap(),
                media_type: "application/xhtml+xml".into(),
                linear: true,
                properties: vec![],
            },
            |_| None,
        )
        .unwrap()
    }

    #[test]
    fn float_join_keeps_caption_links_unicode_offsets_and_page_anchors() {
        let mut section = parse(
            r#"<div id="pdf-page-97"></div><p>Émile created the user interface</p><div id="pdf-page-98"></div><div><img src="sketch.png"/></div><div>Figure 2.8 Sketchpad.</div><p id="continuation">for the TX-2 <a href="https://example.com">here</a>.</p><p>Independent paragraph.</p>"#,
        );
        normalize_ocr(&mut section, &HeadingHints::new(&[]), "pdf-page-");
        let [Block::Text(body), Block::Figure(figure), Block::Text(_)] = &section.blocks[..] else {
            panic!("{:?}", section.blocks)
        };
        assert_eq!(
            text(body),
            "Émile created the user interface for the TX-2 here."
        );
        assert_eq!(text(&figure.captions[0]), "Figure 2.8 Sketchpad.");
        assert!(
            body.content
                .iter()
                .flat_map(Inline::text_runs)
                .any(|r| r.link.is_some())
        );
        let anchor = section
            .anchors
            .iter()
            .find(|a| a.fragment == "continuation")
            .unwrap();
        assert_eq!(anchor.source.node, body.source.as_ref().unwrap().start.node);
        assert_eq!(
            anchor.source.text_offset,
            "Émile created the user interface ".chars().count() as u64
        );
        assert!(section.anchors.iter().any(|a| a.fragment == "pdf-page-98"));
    }

    #[test]
    fn blank_pages_are_barriers_and_provider_proof_allows_chinese_floats() {
        let mut section = parse(
            "<div id='pdf-page-1'></div><p>未完正文</p><div id='pdf-page-2'></div><div id='pdf-page-3'></div><p>新的正文。</p>",
        );
        normalize_ocr(&mut section, &HeadingHints::new(&[]), "pdf-page-");
        assert_eq!(section.blocks.len(), 2);
        let mut section =
            parse("<p>未完正文</p><img src='a.png'/><p>图 1 图注。</p><p>后续内容。</p>");
        let Some(Block::Text(last)) = section.blocks.last() else {
            panic!()
        };
        let continuations = HashSet::from([last.source.as_ref().unwrap().start.node.clone()]);
        normalize_ocr_with_continuations(
            &mut section,
            &HeadingHints::new(&[]),
            "pdf-page-",
            &continuations,
        );
        assert!(matches!(&section.blocks[0], Block::Text(t) if text(t) == "未完正文后续内容。"));
        assert_eq!(section.blocks.len(), 2);
    }

    #[test]
    fn physical_page_joins_keep_chinese_offsets_and_compound_hyphens() {
        let mut section = parse(
            "<div id='pdf-page-1'></div><p>我担任</p><div id='pdf-page-2'></div><p id='second'>课程助教。</p><p>完整句子。</p><div id='pdf-page-3'></div><p>下一段。</p>",
        );
        normalize_ocr(&mut section, &HeadingHints::new(&[]), "pdf-page-");
        let Block::Text(first) = &section.blocks[0] else {
            panic!()
        };
        assert_eq!(text(first), "我担任课程助教。");
        let second = section
            .anchors
            .iter()
            .find(|a| a.fragment == "second")
            .unwrap();
        assert_eq!(second.source.text_offset, 3);
        assert_eq!(
            second.source.node,
            first.source.as_ref().unwrap().start.node
        );
        assert_eq!(section.blocks.len(), 3);
        for (left, expected) in [
            ("An inter-", "An international example."),
            ("A well-", "A well-known example."),
        ] {
            let mut section = parse(&format!("<p>Another international mention.</p><div id='pdf-page-1'></div><p>{left}</p><div id='pdf-page-2'></div><p>\u{200b}{}</p>", if left.starts_with("An") { "national example." } else { "known example." }).replace('\u{200b}', ""));
            normalize_ocr(&mut section, &HeadingHints::new(&[]), "pdf-page-");
            let Block::Text(joined) = &section.blocks[1] else {
                panic!()
            };
            assert_eq!(text(joined), expected);
            assert_eq!(
                joined.source.as_ref().unwrap().end.text_offset,
                expected.chars().count() as u64
            );
        }
    }

    #[test]
    fn toc_validation_recovers_split_ocr_heading_without_guessing_page_numbers() {
        let mut section = parse(
            "<h3>2.1</h3><h2 id='title'>Introduction</h2><h2>49</h2><p>Ordinary paragraph.</p>",
        );
        let hints = HeadingHints::new(&[TocEntry {
            label: "2.1 Introduction".into(),
            href: Some(section.href.resolve("#title").unwrap()),
            children: vec![],
        }]);
        normalize_ocr(&mut section, &hints, "pdf-page-");
        assert!(
            matches!(&section.blocks[..2], [Block::Text(a), Block::Text(b)] if a.kind == TextBlockKind::HeadingOrdinal(1) && b.kind == TextBlockKind::Heading(1))
        );
        assert!(
            matches!(&section.blocks[2], Block::Text(t) if !matches!(t.kind, TextBlockKind::HeadingOrdinal(_)))
        );
    }

    #[test]
    fn uncertain_chinese_floats_and_semantic_barriers_stay_separate() {
        for html in [
            "<p>尚未完成</p><img src='a.png'/><p>图 1 图注。</p><p>后续内容。</p>",
            "<p>unfinished prose</p><h2>New section</h2><p>continues here.</p>",
            "<p>Complete sentence.</p><img src='a.png'/><p>Figure 1 Caption.</p><p>next paragraph.</p>",
        ] {
            let mut section = parse(html);
            let text_count = section
                .blocks
                .iter()
                .filter(|b| paragraph(b).is_some())
                .count();
            normalize_ocr(&mut section, &HeadingHints::new(&[]), "pdf-page-");
            assert_eq!(
                section
                    .blocks
                    .iter()
                    .filter(|b| paragraph(b).is_some())
                    .count(),
                text_count
            );
        }
    }
}
