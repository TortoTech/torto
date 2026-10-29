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

// A soft batching budget may be exceeded to keep one run intact. Truly large
// runs remain authored rather than publishing a misleading partial merge.
pub(super) const MAX_RUN_CHARS: usize = 20_000;

pub(super) fn runs(
    section: &Section,
    range: std::ops::Range<usize>,
) -> Vec<std::ops::Range<usize>> {
    let mut result = Vec::new();
    let mut i = range.start;
    while i < range.end {
        if body(&section.blocks[i]).is_none() {
            i += 1;
            continue;
        }
        let start = i;
        while i < range.end && body(&section.blocks[i]).is_some() {
            let closed = matches!(&section.blocks[i],Block::Quote(q) if q.attribution.is_some());
            i += 1;
            if closed {
                break;
            }
        }
        result.push(start..i);
    }
    result
}

pub(super) fn eligible_ids(section: &Section) -> HashSet<usize> {
    let mut eligible = HashSet::new();
    for run in runs(section, 0..section.blocks.len()) {
        let mut count = 0;
        for block in &section.blocks[run.clone()] {
            for text in block.text_blocks() {
                for inline in &text.content {
                    count += match inline {
                        Inline::Break => 1,
                        _ => inline
                            .text_runs()
                            .iter()
                            .map(|r| r.text.chars().count())
                            .sum(),
                    };
                    if count > MAX_RUN_CHARS {
                        break;
                    }
                }
                if count > MAX_RUN_CHARS {
                    break;
                }
            }
            if count > MAX_RUN_CHARS {
                break;
            }
        }
        if count <= MAX_RUN_CHARS {
            eligible.extend(run);
        }
    }
    eligible
}

#[cfg(test)]
pub(super) fn eligible(section: &Section, index: usize) -> bool {
    eligible_ids(section).contains(&index)
}

pub(super) fn attachment_anchor(block: &Block) -> Option<&SourceRange> {
    anchor(block).or_else(|| source(block))
}

pub(super) fn schema() -> Value {
    let ids = json!({"type":"array","items":{"type":"integer"}});
    json!({"type":"object","additionalProperties":false,"properties":{
        "kind":{"type":"string","enum":["code_or_quote"]},
        "body":{"type":"array","minItems":1,"items":{"type":"integer"},"description":"Consecutive complete containers from targets.classify_code_or_quote, in source order. Jointly determine this group's membership and presentation; one member is valid too."},
        "presentation":{"type":"string","enum":["code","quote"]},
        "language":{"type":["string","null"],"description":"Confident syntax name/extension of the whole example, e.g. html (including embedded CSS), xml, json, python. Null for prose or unsupported/unknown syntax."},
        "captions":ids,
        "sources":{"type":"array","items":{"type":"integer"},"description":"Adjacent explicit source/credit containers, outside body. May be paragraphs or authored quotes. Never invent a credit."},
        "body_index":{"type":["integer","null"],"description":"For prose only: final paragraph index inside the LAST body member if that paragraph is an explicit credit. Otherwise null."}
    },"required":["kind","body","presentation","language","captions","sources","body_index"]})
}

pub(super) fn valid(
    section: &Section,
    members: &[usize],
    presentation: Presentation,
    language: Option<&str>,
    captions: &[usize],
    sources: &[usize],
    credit: Option<usize>,
    eligible: &HashSet<usize>,
) -> bool {
    if !consecutive(members) || !members.iter().all(|i| eligible.contains(i)) {
        return false;
    }
    if language.is_some_and(|l| {
        presentation != Presentation::Code || l.is_empty() || l.len() > 40 || !l.is_ascii()
    }) {
        return false;
    }
    // An explicit existing attribution closes its excerpt; never bury it in the
    // middle of another group's body. The final member keeps its attribution.
    if members[..members.len() - 1]
        .iter()
        .any(|i| matches!(&section.blocks[*i],Block::Quote(q) if q.attribution.is_some()))
    {
        return false;
    }
    let last = &section.blocks[*members.last().unwrap()];
    let final_body = body(last).unwrap();
    if credit.is_some_and(|i| {
        presentation != Presentation::Quote
            || matches!(last,Block::Quote(q) if q.attribution.is_some())
            || i == 0
            || i.checked_add(1) != Some(final_body.len())
            || final_body[i].source.is_none()
            || !quote_sources::credit_like(&text_block_text(&final_body[i]))
    }) {
        return false;
    }
    let ordered = |ids: &[usize]| ids.windows(2).all(|p| p[0] < p[1]);
    let mut all: Vec<_> = members
        .iter()
        .chain(captions)
        .chain(sources)
        .copied()
        .collect();
    all.sort_unstable();
    consecutive(&all) && ordered(captions) && ordered(sources)
        && captions.iter().all(|i| matches!(section.blocks.get(*i),Some(Block::Text(t))
            if matches!(t.kind,TextBlockKind::Paragraph|TextBlockKind::Caption|TextBlockKind::QuoteAttribution)
            && t.source.is_some() && !text_block_text(t).trim().is_empty()))
        && sources.iter().all(|i| section.blocks.get(*i).is_some_and(|b|
            attachment_anchor(b).is_some()
            && (matches!(b, Block::Quote(_)) || matches!(b, Block::Text(t) if matches!(t.kind,
                    TextBlockKind::Paragraph | TextBlockKind::Blockquote | TextBlockKind::Caption | TextBlockKind::QuoteAttribution)))
            && b.text_blocks().any(|t| !text_block_text(t).trim().is_empty())))
}

pub(super) fn protect(window: &mut WindowResult, citations: &[citations::WindowCandidate]) {
    let code: HashSet<_> = window
        .groups
        .iter()
        .filter_map(|g| match g {
            Proposal::CodeOrQuote {
                body,
                presentation: Presentation::Code,
                ..
            } => Some(body.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    window.formulas.retain(|p| !code.contains(&p.block));
    window.citations.retain(|id| {
        !citations
            .iter()
            .any(|c| &c.id == id && code.contains(&c.block))
    });
}

#[derive(Clone, Copy)]
enum Role {
    Body,
    Caption,
    Source,
}

pub(super) fn compose(
    blocks: &mut Vec<Block>,
    members: &[SourceRange],
    presentation: Presentation,
    language: Option<&str>,
    captions: &[SourceRange],
    sources: &[SourceRange],
    credit: Option<&SourceRange>,
) {
    if members.is_empty() {
        return;
    }
    let mut entries = Vec::new();
    for (ranges, role) in [
        (members, Role::Body),
        (captions, Role::Caption),
        (sources, Role::Source),
    ] {
        for range in ranges {
            let Some(index) = blocks
                .iter()
                .position(|b| attachment_anchor(b) == Some(range))
            else {
                return;
            };
            entries.push((index, range, role));
        }
    }
    entries.sort_by_key(|e| e.0);
    if entries.windows(2).any(|p| p[0].0 == p[1].0) {
        return;
    }
    let start = entries[0].0;
    let mut end = entries.last().unwrap().0 + 1;
    while matches!(blocks.get(end),Some(Block::Text(t)) if t.source.is_none()) {
        end += 1;
    }
    let expected: Vec<_> = entries.iter().map(|e| e.1).collect();
    let present: Vec<_> = blocks[start..end]
        .iter()
        .filter_map(attachment_anchor)
        .collect();
    if present != expected {
        return;
    }
    let first_body = entries
        .iter()
        .find(|e| matches!(e.2, Role::Body))
        .unwrap()
        .0;
    let mut body_text = Vec::new();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut credits = Vec::new();
    let mut role = Role::Body;
    for (index, block) in blocks.iter().enumerate().take(end).skip(start) {
        if let Some(entry) = entries.iter().find(|e| e.0 == index) {
            role = entry.2;
        }
        match role {
            Role::Body => match block {
                Block::Quote(q) => {
                    body_text.extend(q.body.iter().cloned());
                    credits.extend(q.attribution.iter().cloned());
                }
                Block::Text(t) => body_text.push(t.clone()),
                _ => return,
            },
            Role::Caption | Role::Source => {
                let texts: Vec<_> = block
                    .text_blocks()
                    .cloned()
                    .map(|mut t| {
                        t.kind = if matches!(role, Role::Source) {
                            TextBlockKind::QuoteAttribution
                        } else {
                            TextBlockKind::Caption
                        };
                        t.style.literal_links = true;
                        t
                    })
                    .collect();
                let destination = if index < first_body {
                    &mut before
                } else {
                    &mut after
                };
                destination.extend(texts);
            }
        }
    }
    if let Some(range) = credit {
        let Some(i) = body_text
            .iter()
            .position(|t| t.source.as_ref() == Some(range))
        else {
            return;
        };
        if i == 0 || !credits.is_empty() || body_text[i + 1..].iter().any(|t| t.source.is_some()) {
            return;
        }
        credits.extend(body_text.split_off(i));
    }
    if body_text.is_empty() {
        return;
    }
    for t in &mut credits {
        t.kind = TextBlockKind::QuoteAttribution;
        t.style.literal_links = true;
    }
    // Existing and model-selected internal credits retain their own source ranges.
    credits.append(&mut after);
    let mut after = credits;
    let spanning = |ranges: &[SourceRange]| {
        Some(SourceRange {
            start: ranges.first().unwrap().start.clone(),
            end: ranges.last().unwrap().end.clone(),
        })
    };
    let mut content = match presentation {
        Presentation::Code => {
            prepare_code(&mut body_text, language);
            body_text.into_iter().map(Block::Text).collect::<Vec<_>>()
        }
        Presentation::Quote => {
            for t in &mut body_text {
                t.kind = TextBlockKind::Blockquote;
                t.style.literal_links = true;
            }
            let attribution = if before.is_empty()
                && after.len() == 1
                && after[0].kind == TextBlockKind::QuoteAttribution
            {
                after.pop()
            } else {
                None
            };
            vec![Block::Quote(QuoteBlock {
                body: body_text,
                attribution,
                source: spanning(members),
            })]
        }
    };
    let plain_quote = presentation == Presentation::Quote && before.is_empty() && after.is_empty();
    let replacement = if plain_quote {
        content.pop().unwrap()
    } else {
        let mut body: Vec<_> = before.into_iter().map(Block::Text).collect();
        body.append(&mut content);
        body.extend(after.into_iter().map(Block::Text));
        Block::Figure(FigureBlock {
            body,
            images: vec![],
            captions: vec![],
            caption_position: CaptionPosition::After,
            style: BlockStyle::default(),
            source: Some(SourceRange {
                start: expected[0].start.clone(),
                end: expected.last().unwrap().end.clone(),
            }),
        })
    };
    blocks.splice(start..end, [replacement]);
}

fn prepare_code(body: &mut [TextBlock], language: Option<&str>) {
    let base_indent = body
        .iter()
        .filter(|t| t.source.is_some())
        .map(|t| t.style.margin_start)
        .reduce(f32::min)
        .unwrap_or(0.0);
    for t in body.iter_mut() {
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
        highlight::apply(body, language);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod group_tests;
