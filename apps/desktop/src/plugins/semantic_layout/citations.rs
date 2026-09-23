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
            Block::Table(t) => out.extend(t.rows.iter().flat_map(|r| &r.cells).map(|c| &c.text)),
            _ => {}
        }
    }
}

pub(super) fn candidates(block: &TextBlock) -> Vec<Citation> {
    if block.source.is_none() {
        return vec![];
    }
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

fn apply(text: &mut TextBlock, spans: &[Citation]) {
    let value = text_block_text(text);
    let chars: Vec<_> = value.chars().collect();
    // Translated-only text may not preserve a citation verbatim. Never guess a
    // replacement range or alter unrelated translated text.
    let mut located = Vec::new();
    for c in spans {
        let mut c = c.clone();
        if chars
            .get(c.start..c.end)
            .map(|s| s.iter().collect::<String>())
            .as_deref()
            != Some(&c.text)
        {
            let matches: Vec<_> = value.match_indices(&c.text).collect();
            if matches.len() != 1 {
                return;
            }
            c.start = value[..matches[0].0].chars().count();
            c.end = c.start + c.text.chars().count();
        }
        located.push(c);
    }
    if !located.windows(2).all(|p| p[0].end <= p[1].start) {
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
                for cell in t.rows.iter_mut().flat_map(|r| &mut r.cells) {
                    if cell.text.source.as_ref() == Some(source) {
                        apply(&mut cell.text, spans);
                    }
                }
            }
            _ => {
                companion = false;
            }
        }
    }
}
