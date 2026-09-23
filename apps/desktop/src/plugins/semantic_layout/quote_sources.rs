use super::*;

/// Structural guard, not author identification. The model must still establish
/// that this is an explicit credit rather than a short narrative paragraph.
pub(super) fn credit_like(text: &str) -> bool {
    let text = text.trim();
    // A sentence that closes the quotation is body continuation, not a credit.
    // Explicit signature/citation delimiters still allow quoted work titles.
    let delimited = text.starts_with(['—', '–', '-', '(', '（', '[', '【']);
    if !delimited && text.ends_with(['"', '”', '’', '」', '』']) {
        return false;
    }
    let lower = text.to_lowercase();
    if !delimited
        && [
            "i ", "we ", "he ", "she ", "they ", "nothing ", "this ", "that ", "it ",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return false;
    }
    !text.is_empty()
        && text.chars().any(char::is_alphabetic)
        && text.chars().count() <= 300
        && text.split_whitespace().count() <= 40
        && !text.contains(['?', '!', '？', '！'])
        && !text.to_lowercase().starts_with("someone")
        && !text.starts_with("有人")
}

pub(super) fn introduces_quote(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_lowercase();
    let tail = lower.trim_end_matches(['.', '。']);
    (text.ends_with(':')
        || text.ends_with('：')
        || [
            " writes",
            " wrote",
            " says",
            " said",
            " as follows",
            " the following",
            " put it",
            "写道",
            "说道",
            "指出",
            "说",
        ]
        .iter()
        .any(|ending| tail.ends_with(ending)))
        && text.chars().count() > 3
        && !["someone", "somebody", "people say", "有人", "研究表明"]
            .iter()
            .any(|word| text.to_lowercase().contains(word))
}

/// Returns source-backed pieces without rewriting text. A source offset counts
/// characters in text runs and one position per explicit line break.
pub(super) fn split_credit(text: &TextBlock, credit: &str) -> Option<(TextBlock, TextBlock)> {
    if !credit_like(credit) {
        return None;
    }
    let whole = text_block_text(text);
    let prefix = whole.trim_end().strip_suffix(credit.trim_end())?;
    if prefix.trim().is_empty() {
        return None;
    }
    let delimiter = credit.trim_start();
    if !delimiter.starts_with(['—', '–', '-', '(', '（', '[', '【'])
        && !credit.starts_with('\n')
        && !prefix.ends_with('\n')
    {
        return None;
    }
    let range = text.source.as_ref()?;
    if range.start.node != range.end.node || range.start.spine != range.end.spine {
        return None;
    }
    let mut remaining = prefix.chars().count();
    let mut source_offset = 0_u64;
    let mut body = text.clone();
    body.content.clear();
    let mut attribution = text.clone();
    attribution.content.clear();
    for inline in &text.content {
        match inline {
            Inline::Text(run) => {
                let len = run.text.chars().count();
                let take = remaining.min(len);
                let byte = run
                    .text
                    .char_indices()
                    .nth(take)
                    .map_or(run.text.len(), |(i, _)| i);
                if take > 0 {
                    let mut part = run.clone();
                    part.text = run.text[..byte].into();
                    body.content.push(Inline::Text(part));
                }
                if take < len {
                    let mut part = run.clone();
                    part.text = run.text[byte..].into();
                    attribution.content.push(Inline::Text(part));
                }
                source_offset += take as u64;
                remaining -= take;
            }
            Inline::Break => {
                if remaining > 0 {
                    body.content.push(Inline::Break);
                    remaining -= 1;
                    source_offset += 1;
                } else {
                    attribution.content.push(Inline::Break);
                }
            }
            // Do not guess offsets through formulas or image-based text.
            _ => return None,
        }
    }
    if remaining != 0 {
        return None;
    }
    let mut boundary = range.start.clone();
    boundary.text_offset = boundary.text_offset.checked_add(source_offset)?;
    if boundary.text_offset >= range.end.text_offset {
        return None;
    }
    body.source.as_mut()?.end = boundary.clone();
    attribution.source.as_mut()?.start = boundary;
    body.kind = TextBlockKind::Blockquote;
    attribution.kind = TextBlockKind::QuoteAttribution;
    // The paragraph boundary replaces the separating line break visually.
    while matches!(body.content.last(), Some(Inline::Break)) {
        body.content.pop();
        body.source.as_mut()?.end.text_offset -= 1;
    }
    while matches!(attribution.content.first(), Some(Inline::Break)) {
        attribution.content.remove(0);
        attribution.source.as_mut()?.start.text_offset += 1;
    }
    Some((body, attribution))
}

pub(super) fn compose(blocks: &mut Vec<Block>, annotation: &Annotation) -> bool {
    match annotation {
        Annotation::QuoteBefore {
            body,
            attribution,
            alignment,
        } => {
            // Keep the introducing paragraph in place; its source relationship
            // remains recorded in the annotation rather than duplicating prose.
            if blocks.iter().any(|b| source(b) == Some(attribution)) {
                super::compose(
                    blocks,
                    &Annotation::Quote {
                        body: body.clone(),
                        attribution: None,
                        alignment: *alignment,
                    },
                );
            }
            true
        }
        Annotation::QuoteInline {
            body,
            credit,
            alignment,
        } => {
            let Some(last) = body.last() else {
                return true;
            };
            if body.len() == 1
                && let Some(index) = blocks.iter().position(|b| {
                    quote_anchor(b) == Some(last) && unattributed_quote_body(b).is_some()
                })
            {
                let mut quote = match &blocks[index] {
                    Block::Quote(q) => q.clone(),
                    Block::Text(t) => QuoteBlock {
                        body: vec![t.clone()],
                        attribution: None,
                        source: t.source.clone(),
                    },
                    _ => unreachable!(),
                };
                // Translated companions remain intact; split only the source-backed
                // final paragraph when its exact suffix is still available.
                let Some(part) = quote.body.iter().rposition(|t| t.source.is_some()) else {
                    return true;
                };
                if let Some((text, attribution)) = split_credit(&quote.body[part], credit) {
                    quote.body[part] = text;
                    quote.attribution = Some(attribution);
                    blocks[index] = Block::Quote(quote);
                }
                return true;
            }
            let Some(index) = blocks.iter().position(|b| source(b) == Some(last)) else {
                return true;
            };
            let Block::Text(text) = &blocks[index] else {
                return true;
            };
            let Some((quote_text, attribution)) = split_credit(text, credit) else {
                // A translated-only paragraph cannot safely be cut using source
                // text offsets. Keep its content intact inside the quotation.
                super::compose(
                    blocks,
                    &Annotation::Quote {
                        body: body.clone(),
                        attribution: None,
                        alignment: *alignment,
                    },
                );
                return true;
            };
            let mut ranges = body.clone();
            *ranges.last_mut().unwrap() = quote_text.source.clone().unwrap();
            let credit_range = attribution.source.clone();
            // Preserve a bilingual companion as part of the body, never as a
            // guessed translation of the shorter attribution fragment.
            let companion =
                matches!(blocks.get(index+1), Some(Block::Text(t)) if t.source.is_none());
            let mut candidate = blocks.clone();
            candidate[index] = Block::Text(quote_text);
            candidate.insert(index + 1 + usize::from(companion), Block::Text(attribution));
            super::compose(
                &mut candidate,
                &Annotation::Quote {
                    body: ranges,
                    attribution: credit_range.clone(),
                    alignment: *alignment,
                },
            );
            if candidate.iter().any(|block| matches!(block, Block::Quote(q) if q.attribution.as_ref().and_then(|t| t.source.as_ref()) == credit_range.as_ref())) {
                *blocks = candidate;
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests;
