//! AI classification acts on the complete authored container. Text and source
//! boundaries are retained; syntax recognition runs on the composition worker.
use super::*;
use rebook_publication::{BlockStyle, SyntaxColors, TextRun};

mod highlight;
pub(super) const PROMPT: &str = include_str!("code_blocks/prompt.md");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Presentation {
    Code,
    Quote,
}

pub(super) fn body(block: &Block) -> Option<&[TextBlock]> {
    match block {
        Block::Quote(q) if !q.body.is_empty() && anchor(block).is_some() => Some(&q.body),
        Block::Text(t)
            if matches!(
                t.kind,
                TextBlockKind::Blockquote | TextBlockKind::Preformatted
            ) && t.source.is_some() =>
        {
            Some(std::slice::from_ref(t))
        }
        _ => None,
    }
}

pub(super) fn anchor(block: &Block) -> Option<&SourceRange> {
    match block {
        Block::Quote(q) => q
            .source
            .as_ref()
            .or_else(|| q.body.iter().find_map(|t| t.source.as_ref())),
        Block::Text(t)
            if matches!(
                t.kind,
                TextBlockKind::Blockquote | TextBlockKind::Preformatted
            ) =>
        {
            t.source.as_ref()
        }
        _ => None,
    }
}

pub(super) fn input(index: usize, block: &Block) -> Option<Value> {
    let body = body(block)?;
    let missing_credit = !matches!(block, Block::Quote(q) if q.attribution.is_some());
    Some(
        json!({"id":index,"type":if matches!(block,Block::Text(t) if t.kind==TextBlockKind::Preformatted) {"authored_preformatted"} else {"authored_quote"},
        "body":body.iter().enumerate().map(|(i,t)| json!({"index":i,"text":text_block_text(t),
            "attribution_eligible":missing_credit && i>0 && i+1==body.len() && t.source.is_some()})).collect::<Vec<_>>(),
        "attribution":match block {Block::Quote(q)=>q.attribution.as_ref().map(text_block_text),_=>None}}),
    )
}

pub(super) fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "kind":{"type":"string","enum":["code_or_quote"]},
        "block":{"type":"integer","description":"One complete container from targets.classify_code_or_quote."},
        "presentation":{"type":"string","enum":["code","quote"]},
        "language":{"type":["string","null"],"description":"Confident syntax name/extension, e.g. xml, json, css, python, javascript, rust. Null for prose or unknown syntax; never guess."},
        "captions":{"type":"array","items":{"type":"integer"},"description":"Only adjacent explicit caption/credit/source paragraphs, in source order. Empty when absent. Do not include ordinary narrative."},
        "body_index":{"type":["integer","null"],"description":"For prose only: final body paragraph index if it is an explicit credit, otherwise null."}
    },"required":["kind","block","presentation","language","captions","body_index"]})
}

pub(super) fn valid(
    section: &Section,
    block: usize,
    presentation: Presentation,
    language: Option<&str>,
    captions: &[usize],
    credit: Option<usize>,
) -> bool {
    let Some(body) = section.blocks.get(block).and_then(body) else {
        return false;
    };
    if language.is_some_and(|l| {
        presentation != Presentation::Code || l.is_empty() || l.len() > 40 || !l.is_ascii()
    }) {
        return false;
    }
    if credit.is_some_and(|i| {
        presentation != Presentation::Quote
            || matches!(&section.blocks[block],Block::Quote(q) if q.attribution.is_some())
            || i == 0
            || i.checked_add(1) != Some(body.len())
            || body[i].source.is_none()
            || !quote_sources::credit_like(&text_block_text(&body[i]))
    }) {
        return false;
    }
    // Labels may precede the body while a source/credit follows it. The whole
    // group must still be contiguous and captions must retain authored order.
    let mut ids: Vec<_> = std::iter::once(block)
        .chain(captions.iter().copied())
        .collect();
    ids.sort_unstable();
    consecutive(&ids) && captions.windows(2).all(|pair| pair[0] < pair[1])
        && captions.iter().all(|i| section.blocks.get(*i).is_some_and(|b| {
            matches!(b, Block::Text(t) if matches!(t.kind, TextBlockKind::Paragraph | TextBlockKind::Caption | TextBlockKind::QuoteAttribution)
                && t.source.is_some() && !text_block_text(t).trim().is_empty())
        }))
}

pub(super) fn protect(window: &mut WindowResult, citations: &[citations::WindowCandidate]) {
    let code: HashSet<_> = window
        .groups
        .iter()
        .filter_map(|g| match g {
            Proposal::CodeOrQuote {
                block,
                presentation: Presentation::Code,
                ..
            } => Some(*block),
            _ => None,
        })
        .collect();
    window.formulas.retain(|p| !code.contains(&p.block));
    window.citations.retain(|id| {
        !citations
            .iter()
            .any(|c| &c.id == id && code.contains(&c.block))
    });
}

pub(super) fn compose(
    blocks: &mut Vec<Block>,
    range: &SourceRange,
    presentation: Presentation,
    language: Option<&str>,
    captions: &[SourceRange],
    credit: Option<&SourceRange>,
) {
    let Some(index) = blocks.iter().position(|b| anchor(b) == Some(range)) else {
        return;
    };
    let Some(original) = body(&blocks[index]) else {
        return;
    };
    let mut body = original.to_vec();
    let mut attribution = match &blocks[index] {
        Block::Quote(q) => q.attribution.clone(),
        _ => None,
    };
    if let Some(credit) = credit {
        let Some(i) = body.iter().position(|t| t.source.as_ref() == Some(credit)) else {
            return;
        };
        if i == 0 || i + 1 != body.len() || attribution.is_some() {
            return;
        }
        attribution = body.pop();
    }
    // Resolve against ranges after translation. Source-less bilingual companions
    // stay attached to their paragraph; no translated text is cut by guessed offsets.
    let positions: Option<Vec<_>> = captions
        .iter()
        .map(|r| blocks.iter().position(|b| source(b) == Some(r)))
        .collect();
    let Some(positions) = positions else { return };
    let split = positions.partition_point(|i| *i < index);
    let start = positions.first().copied().unwrap_or(index).min(index);
    let mut end = positions.last().copied().unwrap_or(index).max(index) + 1;
    if matches!(blocks.get(end),Some(Block::Text(t)) if t.source.is_none()) {
        end += 1;
    }
    let expected: Vec<_> = captions[..split]
        .iter()
        .chain(std::iter::once(range))
        .chain(&captions[split..])
        .collect();
    let present: Vec<_> = blocks[start..end]
        .iter()
        .filter_map(|b| anchor(b).or_else(|| source(b)))
        .collect();
    if present != expected {
        return;
    }
    let mut before = Vec::new();
    let mut after = Vec::new();
    for (offset, b) in blocks[start..end].iter().enumerate() {
        if start + offset == index {
            continue;
        }
        if let Block::Text(t) = b {
            if t.source.is_none()
                && start + offset == index + 1
                && matches!(blocks[index], Block::Text(_))
            {
                body.push(t.clone());
                continue;
            }
            if captions.iter().any(|r| t.source.as_ref() == Some(r)) || t.source.is_none() {
                let mut t = t.clone();
                t.kind = TextBlockKind::Caption;
                t.style.literal_links = true;
                if start + offset < index {
                    before.push(Block::Text(t));
                } else {
                    after.push(Block::Text(t));
                }
            }
        }
    }
    let mut content = match presentation {
        Presentation::Code => {
            let base_indent = body
                .iter()
                .filter_map(|t| t.source.as_ref().map(|_| t.style.margin_start))
                .reduce(f32::min)
                .unwrap_or(0.0);
            for t in &mut body {
                t.kind = TextBlockKind::Preformatted;
                t.style.literal_links = true;
                t.style.paragraph_group = None;
                t.style.margin_start = (t.style.margin_start - base_indent).max(0.0);
                t.style.margin_start_fraction = 0.0;
                t.style.margin_before = 0.0;
                t.style.margin_after = 0.0;
                t.style.indent = t.style.indent.max(0.0);
                t.style.sentence_indents = false;
                t.style.semantic_alignment = Some(rebook_publication::TextAlignment::Start);
                for inline in &mut t.content {
                    let Inline::Text(run) = inline else { continue };
                    run.style.bold = false;
                    run.style.italic = false;
                    run.style.emphasis = false;
                    run.style.alternate_voice = false;
                    run.style.citation = false;
                    run.style.inline_size_scale = None;
                    run.style.inline_citation = 0;
                }
            }
            if let Some(language) = language {
                highlight::apply(&mut body, language);
            }
            let mut result: Vec<_> = body.into_iter().map(Block::Text).collect();
            if let Some(mut t) = attribution {
                t.kind = TextBlockKind::Caption;
                t.style.literal_links = true;
                result.push(Block::Text(t));
            }
            result
        }
        Presentation::Quote => {
            for t in &mut body {
                t.kind = TextBlockKind::Blockquote;
                t.style.literal_links = true;
            }
            if let Some(t) = &mut attribution {
                t.kind = TextBlockKind::QuoteAttribution;
                t.style.literal_links = true;
            }
            vec![Block::Quote(QuoteBlock {
                body,
                attribution,
                source: Some(range.clone()),
            })]
        }
    };
    before.append(&mut content);
    before.append(&mut after);
    let mut content = before;
    let replacement =
        if presentation == Presentation::Quote && captions.is_empty() && content.len() == 1 {
            content.pop().unwrap()
        } else {
            let spanning = Some(SourceRange {
                start: expected[0].start.clone(),
                end: expected.last().unwrap().end.clone(),
            });
            Block::Figure(FigureBlock {
                body: content,
                images: vec![],
                captions: vec![],
                caption_position: CaptionPosition::After,
                style: BlockStyle::default(),
                source: spanning,
            })
        };
    blocks.splice(start..end, [replacement]);
}

#[cfg(test)]
mod tests;
