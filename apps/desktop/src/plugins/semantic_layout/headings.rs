use super::*;

pub(super) const MAX_CHARS: usize = 120;

// Eligibility is deliberately lexical, not a claim that the number is a heading.
// Page numbers pass this filter too; the model must distinguish their context.
pub(super) fn number(block: &Block) -> Option<u32> {
    let text = text_block_text(paragraph(block)?);
    let text = text.trim();
    let number = text.strip_suffix(['.', ')', '．', '）']).unwrap_or(text);
    if number.is_empty() || number.len() > 4 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse().ok().filter(|number| *number > 0)
}

fn numbered_prefix(value: &str) -> bool {
    static PREFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(concat!(
            r"^(?:",
            r"[0-9０-９]{1,4}(?:[.．][0-9０-９]{1,4})*(?:\s|[.)．）、:：]|\p{Han})",
            r"|[（(][0-9０-９]{1,4}(?:[.．][0-9０-９]{1,4})*[）)]",
            r"|(?i:chapter|part|section|book|volume|chap\.|sec\.|vol\.)\s+",
            r"(?:[0-9０-９]{1,4}(?:[.．][0-9０-９]{1,4})*|(?i:[ivxlcdm]+))(?:$|\s|[.:：、)）-])",
            r"|第[零〇一二三四五六七八九十百千两0-9０-９]+[章节篇部卷回])"
        ))
        .expect("heading prefix regex")
    });
    PREFIX.is_match(value)
}

// Eligibility only: numbered prose still needs contextual rejection by the model.
pub(super) fn candidate(block: &Block) -> Option<()> {
    let numeric = number(block).is_some();
    let block = paragraph(block)?;
    let value = text_block_text(block);
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_CHARS {
        return None;
    }
    if block.content.iter().any(|inline| match inline {
        Inline::Text(run) => {
            run.style.inline_role != rebook_publication::InlineRole::Normal
                || run.style.link_role != rebook_publication::LinkRole::Normal
        }
        Inline::Math(_) | Inline::Image(_) => true,
        _ => false,
    }) {
        return None;
    }
    (numeric || (numbered_prefix(value) && value.chars().any(char::is_alphabetic))).then_some(())
}

pub(super) fn annotation_eligible(section: &Section, annotation: &Annotation) -> bool {
    match annotation {
        Annotation::SectionHeading { source: range } => section
            .blocks
            .iter()
            .any(|block| source(block) == Some(range) && candidate(block).is_some()),
        _ => true,
    }
}

pub(super) fn style(block: &TextBlock) -> Value {
    let mut total = 0.0_f64;
    let mut bold = 0.0;
    let mut italic = 0.0;
    let mut size = 0.0;
    for inline in &block.content {
        for run in inline.text_runs() {
            let count = run.text.chars().filter(|c| !c.is_whitespace()).count() as f64;
            total += count;
            if run.style.bold {
                bold += count;
            }
            if run.style.italic {
                italic += count;
            }
            size += count * f64::from(run.style.size_scale);
        }
    }
    let total = total.max(1.0);
    use super::super::numbers::round_request_number as rounded;
    json!({"bold_ratio":rounded(bold/total),"italic_ratio":rounded(italic/total),"relative_font_size":rounded(size/total),
        "align":format!("{:?}",block.style.align),"margin_before":rounded(f64::from(block.style.margin_before)),"margin_after":rounded(f64::from(block.style.margin_after))})
}

pub(super) fn context(section: &Section, start: usize) -> Value {
    let mut candidates = section
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(id, block)| number(block).map(|number| (id, number)))
        .collect::<Vec<_>>();
    let total = candidates.len();
    // Bound input size while exposing numbering beyond a single body window.
    candidates.sort_by_key(|(id, _)| id.abs_diff(start));
    candidates.truncate(8);
    candidates.sort_unstable();
    let excerpt = |index: Option<usize>, tail: bool| {
        let text = index
            .and_then(|i| section.blocks.get(i))
            .and_then(|b| match b {
                Block::Text(text) => Some(text_block_text(text)),
                _ => None,
            })
            .unwrap_or_default();
        let chars = text.chars().collect::<Vec<_>>();
        if tail {
            chars[chars.len().saturating_sub(100)..]
                .iter()
                .collect::<String>()
        } else {
            chars.into_iter().take(100).collect()
        }
    };
    json!({"total":total,"items":candidates.into_iter().map(|(id,number)| json!({
        "id":id,"number":number,
        "before":excerpt(id.checked_sub(1),true),
        "after":excerpt(id.checked_add(1),false)
    })).collect::<Vec<_>>()})
}

pub(super) fn compose(blocks: &mut [Block], range: &SourceRange) {
    let Some(index) = blocks.iter().position(|block| source(block) == Some(range)) else {
        return;
    };
    let Block::Text(text) = &mut blocks[index] else {
        return;
    };
    if text.kind != TextBlockKind::Paragraph {
        return;
    }
    text.kind = TextBlockKind::Heading(3);
    // Translation companions have no source; preserve their content and anchors.
    if let Some(Block::Text(companion)) = blocks.get_mut(index + 1)
        && companion.source.is_none()
        && companion.kind == TextBlockKind::Paragraph
    {
        companion.kind = TextBlockKind::Heading(3);
    }
}

#[cfg(test)]
mod tests;
