//! Recover unmarked subheadings after the source's other block heuristics.
use super::{Block, Inline, Section, TextAlignment, TextBlock, TextBlockKind, body};
use rebook_publication::{InlineRole, LinkRole, SeparatorKind};

fn uppercase_label(block: &TextBlock) -> Option<String> {
    let mut value = String::new();
    for inline in &block.content {
        let Inline::Text(run) = inline else {
            return None;
        };
        if run.style.inline_role == InlineRole::PageMarker {
            continue;
        }
        if run.link.is_some()
            || run.style.inline_role != InlineRole::Normal
            || run.style.link_role != LinkRole::Normal
            || run.style.inline_citation != 0
            || run.style.citation
            || value.len() + run.text.len() > 384
        {
            return None;
        }
        value.push_str(&run.text);
    }
    let value = value.trim();
    let mut letters = 0;
    for ch in value.chars().filter(|ch| ch.is_alphabetic()) {
        if !ch.is_uppercase() {
            return None;
        }
        letters += 1;
    }
    let words = value.split_whitespace();
    if !(6..=72).contains(&letters)
        || !(2..=12).contains(&words.clone().count())
        || words
            .filter(|word| word.chars().filter(|ch| ch.is_uppercase()).count() >= 3)
            .count()
            < 2
        || value.ends_with(['.', '。', '．', '!', '！', '…', ';', '；'])
        || value.contains([
            '=', '∑', '∫', '≤', '≥', '→', '@', '/', '\\', '{', '}', '<', '>',
        ])
    {
        return None;
    }
    Some(value.to_owned())
}

fn other_block_candidate(block: &TextBlock, value: &str) -> bool {
    let style = block.style;
    if style.list_group.is_some()
        || style.authored_list_marker.is_some()
        || style.list_marker_format.is_some()
        || style.semantic_alignment.is_some()
        || style.literal_links
        || style.indent < -0.5
        || style.margin_start >= 20.0
        || style.margin_start_fraction >= 0.08
    {
        return true;
    }
    if value.starts_with([
        '“', '‘', '"', '「', '『', '©', '—', '–', '-', '•', '●', '▪', '*', '†', '[', '(', '【',
    ]) || rebook_publication::media_references::caption_label(value).is_some()
        || rebook_publication::media_notes::is_annotation(value)
    {
        return true;
    }
    let first = value.split_whitespace().next().unwrap_or_default();
    let ordinal = first.trim_end_matches(['.', ')', ':', '．', '）', '、', '：']);
    (!ordinal.is_empty()
        && (ordinal.chars().all(|ch| ch.is_ascii_digit())
            || ordinal != first
                && (ordinal.chars().count() == 1
                    || ordinal
                        .chars()
                        .all(|ch| matches!(ch, 'I' | 'V' | 'X' | 'L' | 'C' | 'D' | 'M')))))
        || [
            "FIGURE ",
            "FIG.",
            "TABLE ",
            "PLATE ",
            "ILLUSTRATION ",
            "EXHIBIT ",
            "SOURCE ",
            "CREDIT ",
            "COPYRIGHT",
            "ISBN",
            "WWW.",
            "BY ",
            "LIST OF ",
        ]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        || matches!(value, "TABLE OF CONTENTS" | "ALL RIGHTS RESERVED")
}

fn spacer(block: &Block) -> bool {
    matches!(block, Block::LineBreak)
        || matches!(block, Block::Separator(s) if !s.in_quote
            && matches!(s.kind, SeparatorKind::Spacing | SeparatorKind::AuthoredSpacing))
}

fn prose(block: &Block) -> Option<&TextBlock> {
    let Block::Text(text) = block else {
        return None;
    };
    (body(text)
        && text.style.list_group.is_none()
        && text.style.authored_list_marker.is_none()
        && text
            .content
            .iter()
            .flat_map(Inline::text_runs)
            .flat_map(|run| run.text.chars())
            .filter(|ch| !ch.is_whitespace())
            .take(60)
            .count()
            == 60)
        .then_some(text)
}

fn next_content<'a>(blocks: impl Iterator<Item = &'a Block>) -> Option<&'a Block> {
    blocks.take(4).find(|b| !spacer(b))
}

fn typography_sample(block: &TextBlock) -> (f32, bool) {
    let mut n = 0usize;
    let mut sum = 0.;
    let mut bold = 0;
    for run in block.content.iter().flat_map(Inline::text_runs) {
        let count = run.text.chars().take(512 - n).count();
        n += count;
        sum += count as f32 * run.style.size_scale;
        if run.style.bold {
            bold += count;
        }
        if n == 512 {
            break;
        }
    }
    (sum / n.max(1) as f32, bold * 2 >= n)
}

pub(super) fn recover(section: &mut Section, bold_baseline: bool) {
    struct Match {
        index: usize,
        value: String,
        between_prose: bool,
        styled: bool,
    }
    fn apply(blocks: &mut [Block], matches: &mut Vec<Match>, level: u8) {
        let repeating = matches
            .iter()
            .find(|m| m.between_prose)
            .is_some_and(|first| {
                matches
                    .iter()
                    .any(|m| m.between_prose && m.value != first.value)
            });
        for m in matches
            .drain(..)
            .filter(|m| m.styled || repeating && m.between_prose)
        {
            if let Block::Text(t) = &mut blocks[m.index] {
                t.kind = TextBlockKind::Heading(level);
            }
        }
    }
    let mut matches = Vec::new();
    let mut level = 3;
    for i in 0..section.blocks.len() {
        let Block::Text(t) = &section.blocks[i] else {
            continue;
        };
        if let TextBlockKind::Heading(parent) = t.kind {
            apply(&mut section.blocks, &mut matches, level);
            level = parent.saturating_add(1).min(6);
            continue;
        }
        if !body(t) {
            continue;
        }
        let Some(value) = uppercase_label(t) else {
            continue;
        };
        if other_block_candidate(t, &value) {
            continue;
        }
        let Some(after) = next_content(section.blocks[i + 1..].iter()).and_then(prose) else {
            continue;
        };
        let before = next_content(section.blocks[..i].iter().rev());
        if before.is_some_and(|b| !matches!(b, Block::Text(t) if body(t) || t.kind.is_heading())) {
            continue;
        }
        // Unclassified captions and credits can still be adjacent to a figure.
        // Stop at real prose, but don't skip an intervening short label.
        if section.blocks[..i]
            .iter()
            .rev()
            .take(3)
            .take_while(|b| prose(b).is_none())
            .any(|b| {
                matches!(
                    b,
                    Block::Image(_)
                        | Block::Figure(_)
                        | Block::Table(_)
                        | Block::Quote(_)
                        | Block::Note(_)
                ) || matches!(b, Block::Text(t) if !body(t) && !t.kind.is_heading())
            })
        {
            continue;
        }
        let (n, bold) = super::counts(t);
        let (after_size, after_bold) = typography_sample(after);
        let factor = typography_sample(t).0 / after_size.max(0.1);
        let styled = factor >= 1.12
            || t.style.authored_alignment == Some(TextAlignment::Center)
            || !bold_baseline && n > 0 && bold * 10 >= n * 9 && !after_bold;
        matches.push(Match {
            index: i,
            value,
            between_prose: before.and_then(prose).is_some(),
            styled,
        });
    }
    apply(&mut section.blocks, &mut matches, level);
}
