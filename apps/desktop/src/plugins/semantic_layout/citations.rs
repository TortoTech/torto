use super::*;
use rebook_publication::{InlineRole, LinkRole};

#[cfg(test)]
mod tests;

pub(super) const PROMPT: &str = include_str!("citations/prompt.md");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Citation {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

#[cfg(test)]
pub(super) fn options() -> Value {
    json!({"temperature":0.0,"response_format":{"type":"json_schema","json_schema":{
        "name":"inline_bibliographic_citations","strict":true,"schema":{
            "type":"object","additionalProperties":false,
            "properties":{"citations":{"type":"array","items":{"type":"integer"}}},
            "required":["citations"]
        }
    }}})
}

fn texts<'a>(blocks: &'a [Block], out: &mut Vec<&'a TextBlock>) {
    for b in blocks {
        match b {
            Block::Text(t)
                if matches!(
                    t.kind,
                    TextBlockKind::Paragraph
                        | TextBlockKind::Blockquote
                        | TextBlockKind::ListItem { .. }
                ) =>
            {
                out.push(t)
            }
            Block::Quote(q) => out.extend(q.body.iter()),
            Block::Table(t) => out.extend(t.text_blocks()),
            _ => {}
        }
    }
}

pub(super) fn candidates(block: &TextBlock) -> Vec<Citation> {
    if block.source.is_none() {
        return vec![];
    }
    candidate_spans(block)
}

// Also used for translated companion blocks, which intentionally have no source.
fn candidate_spans(block: &TextBlock) -> Vec<Citation> {
    let text = text_block_text(block);
    let chars: Vec<_> = text.chars().collect();
    let mut forbidden = Vec::new();
    let mut offset = 0;
    for inline in &block.content {
        let (len, protected) = match inline {
            Inline::Text(r) => (
                r.text.chars().count(),
                r.style.inline_citation != 0
                    || r.style.inline_role != InlineRole::Normal
                    || r.style.link_role != LinkRole::Normal
                    || (r.link.is_some()
                        && r.style.baseline == rebook_publication::TextBaseline::Superscript),
            ),
            Inline::Break => (1, true),
            Inline::Math(m) => (m.latex.chars().count(), true),
            Inline::Image(_) => (0, true),
        };
        if protected {
            forbidden.push(offset..offset + len.max(1));
        }
        offset += len;
    }
    let mut stack = Vec::new();
    let mut start = 0;
    let mut out = Vec::new();
    for (i, &ch) in chars.iter().enumerate() {
        if let Some(close) = match ch {
            '(' => Some(')'),
            '（' => Some('）'),
            '[' => Some(']'),
            '［' => Some('］'),
            _ => None,
        } {
            if stack.is_empty() {
                start = i;
            }
            stack.push(close);
        } else if stack.last() == Some(&ch) {
            stack.pop();
            if !stack.is_empty() {
                continue;
            }
            let end = i + 1;
            if end - start > 600 || forbidden.iter().any(|r| r.start < end && r.end > start) {
                continue;
            }
            let value: String = chars[start..end].iter().collect();
            let digits = value.chars().filter(char::is_ascii_digit).count();
            let letters = value.chars().filter(|c| c.is_alphabetic()).count();
            // Year-only parentheses after an author remain part of the sentence.
            let square = matches!(chars[start], '[' | '［');
            if digits == 0 || (!square && letters < 2) {
                continue;
            }
            out.push(Citation {
                start,
                end,
                text: value,
            });
        }
    }
    out
}

#[cfg(test)]
pub(super) async fn recognize(
    client: &reqwest::Client,
    provider: &super::super::AiProvider,
    model: &str,
    section: &Section,
) -> Result<Vec<Annotation>, String> {
    let mut paragraphs = Vec::new();
    texts(&section.blocks, &mut paragraphs);
    let all: Vec<_> = paragraphs
        .iter()
        .flat_map(|t| candidates(t).into_iter().map(move |c| (*t, c)))
        .collect();
    let mut accepted: HashMap<String, Vec<Citation>> = HashMap::new();
    for chunk in all.chunks(24) {
        let input: Vec<_> = chunk.iter().enumerate().map(|(id,(t,c))| {
            let chars: Vec<_> = text_block_text(t).chars().collect();
            json!({"id":id,"text":c.text,"before":chars[c.start.saturating_sub(240)..c.start].iter().collect::<String>(),"after":chars[c.end..(c.end+160).min(chars.len())].iter().collect::<String>()})
        }).collect();
        let mut messages = vec![
            json!({"role":"system","content":PROMPT}),
            json!({"role":"user","content":json!({"candidates":input}).to_string()}),
        ];
        let mut selected = None;
        for _ in 0..2 {
            let response = ai::request_completion(
                client,
                provider,
                model,
                &messages,
                None,
                Some(2048),
                ReasoningEffort::Default,
                Some(&options()),
            )
            .await?;
            let content = ai::message_content(&response).ok_or("Empty citation response")?;
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Response {
                citations: Vec<usize>,
            }
            if let Ok(result) = llm_json::parse::<Response>(&content)
                && result.citations.iter().all(|id| *id < chunk.len())
            {
                selected = Some(result.citations);
                break;
            }
            messages.push(json!({"role":"assistant","content":content}));
            messages.push(json!({"role":"user","content":"# Correct the response\nReturn only existing candidate IDs in the required JSON schema; omit uncertain candidates."}));
        }
        let ids = selected.ok_or("Invalid inline citation response")?;
        log::event(
            provider,
            model,
            "citations.result",
            json!({"section":section.id,"candidates":chunk.len(),"accepted":ids.len()}),
        );
        for id in ids {
            let (t, c) = &chunk[id];
            let spans = accepted
                .entry(serde_json::to_string(t.source.as_ref().unwrap()).unwrap())
                .or_default();
            if !spans.contains(c) {
                spans.push(c.clone());
            }
        }
    }
    let mut result = Vec::new();
    for t in paragraphs {
        if let Some(source) = &t.source
            && let Some(mut spans) = accepted.remove(&serde_json::to_string(source).unwrap())
        {
            spans.sort_by_key(|c| c.start);
            result.push(Annotation::InlineCitations {
                source: source.clone(),
                spans,
            });
        }
    }
    Ok(result)
}

pub(super) fn validate_annotations(section: &Section, annotations: &[Annotation]) -> bool {
    let mut paragraphs = Vec::new();
    texts(&section.blocks, &mut paragraphs);
    annotations.iter().all(|a| match a {
        Annotation::InlineCitations { source, spans } => paragraphs
            .iter()
            .find(|t| t.source.as_ref() == Some(source))
            .is_some_and(|t| {
                let available = candidates(t);
                !spans.is_empty()
                    && spans.iter().all(|c| available.contains(c))
                    && spans.windows(2).all(|p| p[0].end <= p[1].start)
            }),
        _ => true,
    })
}

// Normalize presentation only: author names, years, order and punctuation
// boundaries remain significant. Never use fuzzy author/year matching.
fn citation_key(value: &str) -> String {
    let value = value.trim();
    let inner = value
        .chars()
        .next()
        .and_then(|first| {
            let last = value.chars().next_back()?;
            matches!(
                (first, last),
                ('(', ')') | ('\u{ff08}', '\u{ff09}') | ('[', ']') | ('\u{ff3b}', '\u{ff3d}')
            )
            .then(|| &value[first.len_utf8()..value.len() - last.len_utf8()])
        })
        .unwrap_or(value)
        .trim();
    let inner = ["e.g.,", "e.g.", "\u{4f8b}\u{5982}", "\u{4f8b}"]
        .iter()
        .find_map(|prefix| inner.strip_prefix(prefix))
        .unwrap_or(inner)
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ',' | '\u{ff0c}' | ':' | '\u{ff1a}')
        });
    inner
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            '\u{ff0c}' => ',',
            '\u{ff1b}' => ';',
            '\u{ff1a}' => ':',
            '\u{ff06}' => '&',
            '\u{2013}' | '\u{2011}' => '-',
            _ => c,
        })
        .collect()
}

fn apply(text: &mut TextBlock, spans: &[Citation]) {
    // Explicit translation markup is authoritative; legacy text matching must
    // not renumber or overwrite already restored citations.
    if text
        .content
        .iter()
        .any(|inline| matches!(inline, Inline::Text(run) if run.style.inline_citation > 0))
    {
        return;
    }
    let available = candidate_spans(text);
    let mut located = Vec::new();
    for original in spans {
        let exact: Vec<_> = available
            .iter()
            .filter(|c| c.text == original.text)
            .collect();
        let selected = exact
            .iter()
            .copied()
            .find(|c| c.start == original.start && c.end == original.end)
            .or_else(|| (exact.len() == 1).then(|| exact[0]));
        let selected = selected.or_else(|| {
            let key = citation_key(&original.text);
            let mut matches = available.iter().filter(|c| citation_key(&c.text) == key);
            let first = matches.next()?;
            matches.next().is_none().then_some(first)
        });
        if let Some(found) = selected {
            located.push(found.clone());
        }
    }
    // Conflicting mappings are ambiguous. Leave only those spans untouched;
    // all other citations still receive consecutive numbers in display order.
    let located: Vec<_> = located
        .iter()
        .enumerate()
        .filter(|(index, c)| {
            !located.iter().enumerate().any(|(other, candidate)| {
                other != *index && c.start < candidate.end && candidate.start < c.end
            })
        })
        .map(|(_, c)| c.clone())
        .collect();
    let mut located = located;
    located.sort_by_key(|c| c.start);
    if located.is_empty() {
        return;
    }
    let mut result = Vec::new();
    let mut offset = 0;
    for inline in &text.content {
        match inline {
            Inline::Text(run) => {
                let len = run.text.chars().count();
                let mut at = 0;
                while at < len {
                    let pos = offset + at;
                    let selected = located
                        .iter()
                        .enumerate()
                        .find(|(_, c)| c.start <= pos && pos < c.end);
                    let end = selected
                        .map_or_else(
                            || {
                                located
                                    .iter()
                                    .filter(|c| c.start > pos)
                                    .map(|c| c.start)
                                    .min()
                                    .unwrap_or(offset + len)
                            },
                            |(_, c)| c.end,
                        )
                        .min(offset + len);
                    let mut piece = run.clone();
                    piece.text = run.text.chars().skip(at).take(end - pos).collect();
                    piece.style.inline_citation = selected.map_or(0, |(i, _)| (i + 1) as u32);
                    result.push(Inline::Text(piece));
                    at = end - offset;
                }
                offset += len;
            }
            Inline::Break => {
                offset += 1;
                result.push(inline.clone());
            }
            Inline::Math(m) => {
                offset += m.latex.chars().count();
                result.push(inline.clone());
            }
            _ => result.push(inline.clone()),
        }
    }
    text.content = result;
}

pub(super) fn compose(blocks: &mut [Block], source: &SourceRange, spans: &[Citation]) {
    let mut companion = false;
    for b in blocks {
        match b {
            Block::Text(t) => {
                let matches = t.source.as_ref() == Some(source);
                if matches || (companion && t.source.is_none()) {
                    apply(t, spans);
                }
                companion = matches;
            }
            Block::Quote(q) => {
                for t in &mut q.body {
                    if t.source.as_ref() == Some(source) {
                        apply(t, spans);
                    }
                }
            }
            Block::Table(t) => {
                for cell in t.text_blocks_mut() {
                    if cell.source.as_ref() == Some(source) {
                        apply(cell, spans);
                    }
                }
            }
            _ => {
                companion = false;
            }
        }
    }
}

pub(super) fn clear_markers(blocks: &mut [Block]) {
    fn clear(text: &mut TextBlock) {
        for inline in &mut text.content {
            if let Inline::Text(run) = inline {
                run.style.inline_citation = 0;
            }
        }
    }
    for block in blocks {
        match block {
            Block::Text(text) => clear(text),
            Block::Quote(quote) => {
                for text in &mut quote.body {
                    clear(text);
                }
                if let Some(text) = &mut quote.attribution {
                    clear(text);
                }
            }
            Block::Table(table) => {
                for text in table.text_blocks_mut() {
                    clear(text);
                }
            }
            Block::Figure(figure) => {
                for text in &mut figure.captions {
                    clear(text);
                }
            }
            Block::Note(note) => clear_markers(&mut note.blocks),
            _ => {}
        }
    }
}

#[derive(Clone)]
pub(super) struct WindowCandidate {
    pub id: String,
    pub block: usize,
    pub paragraph: usize,
    pub source: SourceRange,
    pub span: Citation,
    pub paragraph_text: String,
}

pub(super) fn window_candidates(
    section: &Section,
    target: std::ops::Range<usize>,
) -> Vec<WindowCandidate> {
    let mut out = Vec::new();
    for block in target {
        let mut paragraphs = Vec::new();
        texts(&section.blocks[block..block + 1], &mut paragraphs);
        for (paragraph, text) in paragraphs.into_iter().enumerate() {
            for (index, span) in candidates(text).into_iter().enumerate() {
                out.push(WindowCandidate {
                    id: format!("c{block}_{paragraph}_{index}"),
                    block,
                    paragraph,
                    source: text.source.clone().unwrap(),
                    span,
                    paragraph_text: text_block_text(text),
                });
            }
        }
    }
    out
}

pub(super) fn window_annotations(
    candidates: &[WindowCandidate],
    ids: &[String],
) -> Vec<Annotation> {
    let mut out: Vec<Annotation> = Vec::new();
    for candidate in candidates.iter().filter(|c| ids.contains(&c.id)) {
        if let Some(Annotation::InlineCitations { spans, .. }) = out.iter_mut().find(
            |a| matches!(a,Annotation::InlineCitations {source,..} if source==&candidate.source),
        ) {
            spans.push(candidate.span.clone());
        } else {
            out.push(Annotation::InlineCitations {
                source: candidate.source.clone(),
                spans: vec![candidate.span.clone()],
            });
        }
    }
    out
}
