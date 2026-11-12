//! Presentation-only heading composition. Authored blocks and sources stay intact.
use super::TextSourceSpan;
use rebook_publication::{
    Block, Inline, TextBlock, TextBlockKind, TextRun, heading_ordinal_key, source_block_identity,
};

pub(super) struct Group<'a> {
    pub ordinal: &'a TextBlock,
    pub title: &'a TextBlock,
    pub translated_ordinal: Option<&'a TextBlock>,
    pub translated_title: Option<&'a TextBlock>,
    pub consumed: usize,
}

pub(super) fn group<'a>(blocks: &[&'a Block]) -> Option<Group<'a>> {
    fn text<'b>(block: &&'b Block) -> Option<&'b TextBlock> {
        if let Block::Text(text) = block {
            Some(text)
        } else {
            None
        }
    }
    fn companion(original: &TextBlock, candidate: &TextBlock) -> bool {
        candidate.source.is_none()
            && candidate.kind == original.kind
            && original.source.as_ref().is_some_and(|source| {
                candidate.style.reference_owner == Some(source_block_identity(source))
            })
    }
    let ordinal = text(blocks.first()?)?;
    let TextBlockKind::HeadingOrdinal(level) = ordinal.kind else {
        return None;
    };
    let next = blocks.get(1).and_then(text);
    let translated_ordinal = next.filter(|next| companion(ordinal, next));
    let start = 1 + usize::from(translated_ordinal.is_some());
    let title_index = (start..blocks.len().min(start + 4))
        .find(|&i| !rebook_publication::is_heading_separator(blocks[i]))?;
    let title = text(blocks.get(title_index)?)?;
    if title.kind != TextBlockKind::Heading(level) {
        return None;
    }
    let translated_title = blocks
        .get(title_index + 1)
        .and_then(text)
        .filter(|candidate| companion(title, candidate));
    Some(Group {
        ordinal,
        title,
        translated_ordinal,
        translated_title,
        consumed: title_index + 1 + usize::from(translated_title.is_some()),
    })
}

pub(super) fn joined(
    ordinal: &TextBlock,
    title: &TextBlock,
    translated: bool,
) -> (TextBlock, Vec<TextSourceSpan>) {
    let mut combined = title.clone();
    combined.content = super::table_captions::join_content(&ordinal.content, &title.content);
    let title_start = ordinal.content.len() + 1;
    let mut spans = Vec::with_capacity(2);
    if !translated {
        for (range, source) in [
            (0..ordinal.content.len(), &ordinal.source),
            (title_start..combined.content.len(), &title.source),
        ] {
            if let Some(source) = source {
                spans.push(TextSourceSpan {
                    range,
                    source: source.clone(),
                });
            }
        }
    } else {
        // A reused numeric label on the bilingual companion must not create
        // another selectable original-source region over the translated line.
        combined.source = None;
    }
    clear_keyword_sizes(&mut combined);
    (combined, spans)
}

fn clear_keyword_sizes(block: &mut TextBlock) {
    for inline in &mut block.content {
        match inline {
            Inline::Text(run) => run.style.keyword_size_scale = None,
            Inline::Ruby(ruby) => {
                for run in ruby.base.iter_mut().chain(&mut ruby.annotation) {
                    run.style.keyword_size_scale = None;
                }
            }
            _ => {}
        }
    }
}

/// Normalize heading gaps and soften the ordinal-to-title break, retaining
/// source character counts and intentional subtitle breaks.
pub(super) fn normalize_internal(block: &mut TextBlock) {
    if !matches!(block.kind, TextBlockKind::Heading(_))
        || block
            .content
            .iter()
            .any(|inline| !matches!(inline, Inline::Text(_) | Inline::Break))
    {
        return;
    }
    let text: String = block
        .content
        .iter()
        .map(|inline| match inline {
            Inline::Text(run) => run.text.as_str(),
            Inline::Break => "\n",
            _ => unreachable!(),
        })
        .collect();
    let softened = text.find(['\n', '\r']).and_then(|start| {
        let tail = &text[start..];
        let end = start + tail.len() - tail.trim_start().len();
        (heading_ordinal_key(&text[..start]).is_some()
            && text[end..].chars().any(char::is_alphabetic))
            .then_some(start..end)
    });
    let mut chars: Vec<_> = text
        .char_indices()
        .map(|(at, ch)| {
            if softened.as_ref().is_some_and(|range| range.contains(&at)) {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let mut start = 0;
    while start < chars.len() {
        if !chars[start].is_whitespace() || matches!(chars[start], '\n' | '\r') {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < chars.len()
            && chars[end].is_whitespace()
            && !matches!(chars[end], '\n' | '\r')
        {
            end += 1;
        }
        // Keep a single normal space, with zero-width padding before it so
        // line wrapping and every following source offset remain intact.
        chars[start..end - 1].fill('\u{2060}');
        chars[end - 1] = ' ';
        start = end;
    }
    if chars.iter().copied().eq(text.chars()) {
        return;
    }
    let mut chars = chars.into_iter();
    for inline in &mut block.content {
        match inline {
            Inline::Text(run) => {
                run.text = chars.by_ref().take(run.text.chars().count()).collect();
            }
            Inline::Break => {
                let ch = chars.next().unwrap();
                if ch != '\n' {
                    *inline = Inline::Text(TextRun {
                        text: ch.to_string(),
                        style: Default::default(),
                        link: None,
                    });
                }
            }
            _ => unreachable!(),
        }
    }
    clear_keyword_sizes(block);
}

#[cfg(test)]
mod tests;
