//! Conservative local recognition; no book-wide bibliography scan or network.
use super::*;
use regex::Regex;
use std::sync::LazyLock;

fn normalized(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\u{ff08}' => '(',
            '\u{ff09}' => ')',
            '\u{ff3b}' => '[',
            '\u{ff3d}' => ']',
            '\u{ff0c}' => ',',
            '\u{ff1b}' => ';',
            '\u{ff06}' => '&',
            '\u{ff1a}' => ':',
            '\u{2013}' | '\u{2014}' | '\u{2011}' => '-',
            _ => ch,
        })
        .collect()
}

pub(super) fn author_date(value: &str) -> bool {
    static WORK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"(?x) ^ (?P<authors>.+?) \s* ,? \s+ (?P<years>(?:1[0-9]{3}|20[0-9]{2})[a-z]?(?:\s*,\s*(?:1[0-9]{3}|20[0-9]{2})[a-z]?)*) (?:\s*,\s*(?:pp?\.|pages?|页)\s*[0-9]+(?:\s*-\s*[0-9]+)?)? \s* $"
    ).unwrap()
    });
    static AUTHOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"(?x) ^ (?:\p{Lu}[\p{L}\p{M}'’.-]*|[\p{Han}]{2,6}|van|von|de|der|den|del|da|di|du|la|le|el) (?:\s+(?:\p{Lu}[\p{L}\p{M}'’.-]*|van|von|de|der|den|del|da|di|du|la|le|el)){0,4} $"
    ).unwrap()
    });
    static OXFORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",\s*(?:&|and)\s*").unwrap());
    let normalized = normalized(value);
    let value = normalized.trim();
    let value = value
        .strip_prefix('(')
        .and_then(|v| v.strip_suffix(')'))
        .or_else(|| value.strip_prefix('[').and_then(|v| v.strip_suffix(']')))
        .unwrap_or(value);
    let value = [
        "see ", "See ", "cf. ", "e.g., ", "e.g. ", "参见", "例如", "见",
    ]
    .iter()
    .find_map(|prefix| value.trim().strip_prefix(prefix))
    .unwrap_or(value)
    .trim();
    let value = value.replace(',', ", "); // also accept compact Chinese/English commas.
    value.split(';').all(|work| {
        let Some(captures) = WORK.captures(work.trim()) else {
            return false;
        };
        let mut authors = captures["authors"].trim().trim_end_matches(',').trim();
        for suffix in [" et al.", " et al", "等"] {
            if let Some(head) = authors.strip_suffix(suffix) {
                authors = head.trim();
                break;
            }
        }
        let authors = OXFORD.replace_all(authors, "&");
        let authors = authors.replace(" and ", "&").replace(['、', '与'], "&");
        !authors.is_empty()
            && authors.split(['&', ',']).all(|author| {
                let author = author.trim();
                let head = author
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                !matches!(
                    head.as_str(),
                    "figure"
                        | "fig"
                        | "table"
                        | "eq"
                        | "equation"
                        | "chapter"
                        | "appendix"
                        | "version"
                        | "born"
                        | "updated"
                        | "copyright"
                        | "january"
                        | "february"
                        | "march"
                        | "april"
                        | "may"
                        | "june"
                        | "july"
                        | "august"
                        | "september"
                        | "october"
                        | "november"
                        | "december"
                ) && author
                    .chars()
                    .any(|c| c.is_uppercase() || ('\u{4e00}'..='\u{9fff}').contains(&c))
                    && AUTHOR.is_match(author)
            })
    })
}

fn numeric_reference(block: &TextBlock, citation: &Citation) -> bool {
    static NUMBERS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\[\s*[1-9][0-9]{0,3}(?:\s*-\s*[1-9][0-9]{0,3})?(?:\s*,\s*[1-9][0-9]{0,3}(?:\s*-\s*[1-9][0-9]{0,3})?)*\s*\]$").unwrap()
    });
    let value = normalized(&citation.text);
    if !NUMBERS.is_match(&value) {
        return false;
    }
    if value.trim_matches(['[', ']']).split(',').any(|item| {
        let bounds: Vec<_> = item
            .split('-')
            .filter_map(|n| n.trim().parse::<u32>().ok())
            .collect();
        bounds.len() == 2 && bounds[0] > bounds[1]
    }) {
        return false;
    }
    let text = text_block_text(block);
    let before: String = text.chars().take(citation.start).collect();
    let before = before
        .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '：'))
        .to_lowercase();
    let context: String = before
        .chars()
        .rev()
        .take(48)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if [
        "array", "matrix", "vector", "index", "interval", "数组", "矩阵", "向量", "索引", "区间",
    ]
    .iter()
    .any(|term| context.contains(term))
    {
        return false;
    }
    let cue = [
        "see",
        "cf.",
        "ref.",
        "refs.",
        "reference",
        "references",
        "studies",
        "文献",
        "参见",
        "参考",
        "见",
    ]
    .iter()
    .any(|cue| before.ends_with(cue));
    let mut offset = 0;
    let linked = block.content.iter().any(|inline| {
        let (len, evidence) = match inline {
            Inline::Text(run) => (
                run.text.chars().count(),
                run.link
                    .as_ref()
                    .and_then(|link| link.fragment())
                    .is_some_and(|fragment| {
                        let fragment = fragment.to_ascii_lowercase();
                        [
                            "references",
                            "reference",
                            "bibitem",
                            "refs",
                            "ref",
                            "bib",
                            "cite",
                        ]
                        .iter()
                        .any(|prefix| {
                            fragment.strip_prefix(prefix).is_some_and(|tail| {
                                tail.starts_with(|ch: char| {
                                    ch.is_ascii_digit() || matches!(ch, '_' | '-' | '.' | ':')
                                })
                            })
                        })
                    }),
            ),
            Inline::Break => (1, false),
            Inline::Math(math) => (
                math.original_text()
                    .unwrap_or_else(|| math.latex.clone())
                    .chars()
                    .count(),
                false,
            ),
            Inline::Image(_) => (0, false),
        };
        let overlaps = offset < citation.end && offset + len > citation.start;
        offset += len;
        evidence && overlaps
    });
    cue || linked
}

pub(in crate::plugins::semantic_layout) fn annotations(section: &Section) -> Vec<Annotation> {
    let mut paragraphs = Vec::new();
    texts(&section.blocks, &mut paragraphs);
    paragraphs
        .into_iter()
        .filter_map(|block| {
            let spans: Vec<_> = candidates(block)
                .into_iter()
                .filter(|c| author_date(&c.text) || numeric_reference(block, c))
                .collect();
            (!spans.is_empty()).then(|| Annotation::InlineCitations {
                source: block.source.clone().unwrap(),
                spans,
            })
        })
        .collect()
}

pub(in crate::plugins::semantic_layout) fn apply_display_fallback(section: &mut Section) {
    // Legacy translations may not carry citation placeholders. Only confidently
    // recognizable spans are added; existing restored markers remain protected.
    for block in &mut section.blocks {
        let paragraphs: Vec<&mut TextBlock> = match block {
            Block::Text(text)
                if matches!(
                    text.kind,
                    TextBlockKind::Paragraph
                        | TextBlockKind::Blockquote
                        | TextBlockKind::ListItem { .. }
                ) =>
            {
                vec![text]
            }
            Block::Quote(quote) => quote.body.iter_mut().collect(),
            Block::Table(table) => table.text_blocks_mut().collect(),
            _ => Vec::new(),
        };
        for text in paragraphs {
            let spans: Vec<_> = candidate_spans(text)
                .into_iter()
                .filter(|c| author_date(&c.text) || numeric_reference(text, c))
                .collect();
            apply(text, &spans);
        }
    }
}
