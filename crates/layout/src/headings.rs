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
    let next = text(blocks.get(1)?)?;
    let translated_ordinal = companion(ordinal, next).then_some(next);
    let title_index = 1 + usize::from(translated_ordinal.is_some());
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

/// Soften just the ordinal-to-title boundary, retaining all source characters
/// and subtitle breaks. The display/copy offset mapping remains one-to-one.
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
    let Some(start) = text.find(['\n', '\r']) else {
        return;
    };
    let tail = &text[start..];
    let end = start + tail.len() - tail.trim_start().len();
    if heading_ordinal_key(&text[..start]).is_none()
        || !text[end..].chars().any(char::is_alphabetic)
    {
        return;
    }
    let mut byte = 0;
    for inline in &mut block.content {
        match inline {
            Inline::Text(run) => {
                run.text = run
                    .text
                    .chars()
                    .map(|ch| {
                        let at = byte;
                        byte += ch.len_utf8();
                        if (start..end).contains(&at) && matches!(ch, '\n' | '\r') {
                            ' '
                        } else {
                            ch
                        }
                    })
                    .collect();
            }
            Inline::Break => {
                if (start..end).contains(&byte) {
                    *inline = Inline::Text(TextRun {
                        text: " ".into(),
                        style: Default::default(),
                        link: None,
                    });
                }
                byte += 1;
            }
            _ => unreachable!(),
        }
    }
    clear_keyword_sizes(block);
}

#[cfg(test)]
mod tests;
