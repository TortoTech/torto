use super::*;

// Eligibility is deliberately lexical, not a claim that the number is a heading.
// Page numbers pass this filter too; the model must distinguish their context.
pub(super) fn candidate(block: &Block) -> Option<u32> {
    let text = text_block_text(paragraph(block)?);
    let text = text.trim();
    let number = text.strip_suffix(['.', ')', '．', '）']).unwrap_or(text);
    if number.is_empty() || number.len() > 4 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse().ok().filter(|number| *number > 0)
}

pub(super) fn context(section: &Section, start: usize) -> Value {
    let mut candidates = section
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(id, block)| candidate(block).map(|number| (id, number)))
        .collect::<Vec<_>>();
    let total = candidates.len();
    // Bound input size while exposing numbering beyond a single body window.
    candidates.sort_by_key(|(id, _)| id.abs_diff(start));
    candidates.truncate(64);
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
            chars[chars.len().saturating_sub(160)..]
                .iter()
                .collect::<String>()
        } else {
            chars.into_iter().take(160).collect()
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

pub(super) async fn review(
    client: &reqwest::Client,
    endpoint: (&super::super::AiProvider, &str),
    section: &Section,
    proposed: &[usize],
) -> Result<HashSet<usize>, String> {
    let roles = RecognitionRoles {
        quotes: false,
        captions: false,
        headings: true,
    };
    let mut accepted = HashSet::new();
    // Review across recognition-window boundaries; bound unusually dense chapters.
    for batch in proposed.chunks(32) {
        let mut indices = HashSet::new();
        for id in batch {
            indices.extend(id.saturating_sub(2)..(*id + 3).min(section.blocks.len()));
        }
        let mut indices = indices.into_iter().collect::<Vec<_>>();
        indices.sort_unstable();
        let input = json!({
            "target_start":0,"target_end_exclusive":section.blocks.len(),
            "quotes_enabled":false,"captions_enabled":false,"headings_enabled":true,
            "review_only":true,"proposed_headings":batch,
            "numbered_candidates":context(section,batch[0]),
            "blocks":indices.into_iter().map(|id| {
                let mut block = section_input_block(section,id);
                if let Some(text) = block["text"].as_str() {
                    block["text"] = json!(text.chars().take(600).collect::<String>());
                }
                block
            }).collect::<Vec<_>>()
        });
        let result = request_groups(
            client,
            endpoint,
            &input,
            section,
            &roles,
            0..section.blocks.len(),
            0..section.blocks.len(),
        )
        .await?;
        for group in result.groups {
            if let Proposal::SectionHeading { block } = group
                && batch.contains(&block)
            {
                accepted.insert(block);
            }
        }
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests;
