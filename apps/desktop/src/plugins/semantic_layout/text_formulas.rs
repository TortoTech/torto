use super::*;
use rebook_publication::{InlineRole, LinkRole, MathRun, TextBaseline, TextRun};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Proposal {
    pub block: usize,
    pub paragraph: usize,
    pub original: String,
    pub latex: String,
    #[serde(default)]
    pub before: String,
    #[serde(default)]
    pub after: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Span {
    pub start: usize,
    pub end: usize,
    pub original: String,
    pub latex: String,
}

pub(super) fn texts<'a>(blocks: &'a [Block], out: &mut Vec<&'a TextBlock>) {
    for block in blocks {
        match block {
            Block::Text(t)
                if !t.kind.is_heading()
                    && t.kind != TextBlockKind::QuoteAttribution
                    && t.source.is_some() =>
            {
                out.push(t)
            }
            Block::Quote(q) => out.extend(q.body.iter().filter(|t| t.source.is_some())),
            Block::Figure(f) => out.extend(f.captions.iter().filter(|t| t.source.is_some())),
            Block::Table(t) => out.extend(t.text_blocks().filter(|t| t.source.is_some())),
            Block::Note(n) => texts(&n.blocks, out),
            _ => {}
        }
    }
}

struct Encoded {
    value: String,
    boundaries: HashMap<usize, usize>,
    protected: Vec<std::ops::Range<usize>>,
}
fn encode(text: &TextBlock) -> Encoded {
    let mut out = Encoded {
        value: String::new(),
        boundaries: HashMap::new(),
        protected: Vec::new(),
    };
    let mut offset = 0;
    for inline in &text.content {
        let begin = out.value.len();
        out.boundaries.insert(begin, offset);
        match inline {
            Inline::Text(run)
                if run.link.is_none()
                    && run.style.inline_role == InlineRole::Normal
                    && run.style.link_role == LinkRole::Normal
                    && run.style.inline_citation == 0 =>
            {
                let mut close = Vec::new();
                for (enabled, open, end) in [
                    (run.style.bold, "<b>", "</b>"),
                    (run.style.italic, "<i>", "</i>"),
                    (
                        run.style.baseline == TextBaseline::Superscript,
                        "<sup>",
                        "</sup>",
                    ),
                    (
                        run.style.baseline == TextBaseline::Subscript,
                        "<sub>",
                        "</sub>",
                    ),
                ] {
                    if enabled {
                        out.value.push_str(open);
                        close.push(end);
                        out.boundaries.insert(out.value.len(), offset);
                    }
                }
                for ch in run.text.chars() {
                    match ch {
                        '&' => out.value.push_str("&amp;"),
                        '<' => out.value.push_str("&lt;"),
                        '>' => out.value.push_str("&gt;"),
                        _ => out.value.push(ch),
                    }
                    offset += 1;
                    out.boundaries.insert(out.value.len(), offset);
                }
                for tag in close.into_iter().rev() {
                    out.value.push_str(tag);
                    out.boundaries.insert(out.value.len(), offset);
                }
            }
            other => {
                out.value.push_str("<protected/>");
                offset += match other {
                    Inline::Text(r) => r.text.chars().count(),
                    Inline::Math(m) => m.source_char_len(),
                    Inline::Break => 1,
                    _ => 0,
                };
                out.protected.push(begin..out.value.len());
            }
        }
        out.boundaries.insert(out.value.len(), offset);
    }
    out
}

pub(super) fn input(block: &Block) -> Vec<Value> {
    let mut paragraphs = Vec::new();
    texts(std::slice::from_ref(block), &mut paragraphs);
    paragraphs
        .into_iter()
        .enumerate()
        .filter(|(_, text)| text.content.iter().any(|inline| matches!(inline, Inline::Text(run) if !run.text.trim().is_empty() && run.link.is_none() && run.style.inline_citation==0 && run.style.inline_role==InlineRole::Normal && run.style.link_role==LinkRole::Normal)))
        .map(|(paragraph, text)| json!({"paragraph":paragraph,"text":encode(text).value}))
        .collect()
}

pub(super) fn attach_input(value: &mut Value, block: &Block) {
    let paragraphs = input(block);
    // One canonical representation per paragraph, shared by all recognition roles.
    if !paragraphs.is_empty() {
        if matches!(block, Block::Text(_)) {
            value.as_object_mut().unwrap().remove("text");
        }
        if let Block::Quote(quote) = block
            && let Some(body) = value.get_mut("body").and_then(Value::as_array_mut)
        {
            let mut paragraph = 0;
            for (item, text) in body.iter_mut().zip(&quote.body) {
                if text.source.is_none() {
                    continue;
                }
                if paragraphs.iter().any(|p| p["paragraph"] == paragraph) {
                    item.as_object_mut().unwrap().remove("text");
                    item["paragraph"] = json!(paragraph);
                }
                paragraph += 1;
            }
        }
    }
    value["math_texts"] = json!(paragraphs);
}

// Validate model-selected ranges, without generating formula candidates locally.
fn complete_script_span(text: &TextBlock, start: usize, end: usize) -> bool {
    let mut offset = 0;
    for inline in &text.content {
        let len = match inline {
            Inline::Text(run) => {
                let len = run.text.chars().count();
                if len > 0 && run.style.baseline != TextBaseline::Normal {
                    // A script cannot be selected without its base, cut in half,
                    // or left behind when the preceding base is selected.
                    if (start >= offset && start < offset + len)
                        || (end >= offset && end < offset + len)
                    {
                        return false;
                    }
                }
                len
            }
            Inline::Math(m) => m.source_char_len(),
            Inline::Break => 1,
            _ => 0,
        };
        offset += len;
    }
    true
}

fn locate(text: &TextBlock, original: &str, before: &str, after: &str) -> Option<(usize, usize)> {
    if original.trim().is_empty() {
        return None;
    }
    let encoded = encode(text);
    let occurrences: Vec<_> = encoded.value.match_indices(original).collect();
    let unique = occurrences.len() == 1;
    let mut matches = occurrences
        .into_iter()
        .filter(|(start, _)| {
            // Context is only a disambiguator. Models can omit a style boundary
            // from otherwise redundant context around a unique exact span.
            unique
                || (encoded.value[..*start].ends_with(before)
                    && encoded.value[start + original.len()..].starts_with(after))
        })
        .filter_map(|(start, _)| {
            let end = start + original.len();
            if encoded
                .protected
                .iter()
                .any(|range| range.start < end && start < range.end)
            {
                return None;
            }
            let a = *encoded.boundaries.get(&start)?;
            let b = *encoded.boundaries.get(&end)?;
            (a < b && complete_script_span(text, a, b)).then_some((a, b))
        });
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
}

pub(super) fn resolve(
    section: &Section,
    target: std::ops::Range<usize>,
    proposals: &[Proposal],
) -> (Vec<Annotation>, usize) {
    let mut accepted: Vec<(SourceRange, Span)> = Vec::new();
    let mut skipped = 0;
    for proposal in proposals {
        let resolve = || {
            if !target.contains(&proposal.block) {
                return None;
            }
            let mut paragraphs = Vec::new();
            texts(
                std::slice::from_ref(section.blocks.get(proposal.block)?),
                &mut paragraphs,
            );
            let text = *paragraphs.get(proposal.paragraph)?;
            let (start, end) = locate(text, &proposal.original, &proposal.before, &proposal.after)?;
            formulas::validate_formula(&rebook_publication::ImageFormula {
                latex: proposal.latex.clone(),
                equation_number: None,
            })
            .ok()?;
            Some((
                text.source.clone()?,
                Span {
                    start,
                    end,
                    original: proposal.original.clone(),
                    latex: proposal.latex.clone(),
                },
            ))
        };
        if let Some(span) = resolve() {
            accepted.push(span);
        } else {
            skipped += 1;
        }
    }
    let overlapping: HashSet<usize> = accepted
        .iter()
        .enumerate()
        .filter_map(|(i, (source, span))| {
            accepted
                .iter()
                .enumerate()
                .any(|(j, (other, next))| {
                    i != j && source == other && span.start < next.end && next.start < span.end
                })
                .then_some(i)
        })
        .collect();
    skipped += overlapping.len();
    let mut groups: Vec<(SourceRange, Vec<Span>)> = Vec::new();
    for (i, (source, span)) in accepted.into_iter().enumerate() {
        if overlapping.contains(&i) {
            continue;
        }
        if let Some((_, spans)) = groups.iter_mut().find(|(s, _)| *s == source) {
            spans.push(span);
        } else {
            groups.push((source, vec![span]));
        }
    }
    (
        groups
            .into_iter()
            .map(|(source, mut spans)| {
                spans.sort_by_key(|s| s.start);
                Annotation::TextFormulas { source, spans }
            })
            .collect(),
        skipped,
    )
}

fn original_runs(text: &TextBlock, start: usize, end: usize) -> Option<Vec<TextRun>> {
    let mut offset = 0;
    let mut out = Vec::new();
    for inline in &text.content {
        let len = match inline {
            Inline::Text(r) => r.text.chars().count(),
            Inline::Math(m) => m.source_char_len(),
            Inline::Break => 1,
            _ => 0,
        };
        if offset < end && start < offset + len {
            let Inline::Text(run) = inline else {
                return None;
            };
            if run.link.is_some()
                || run.style.inline_citation != 0
                || run.style.inline_role != InlineRole::Normal
                || run.style.link_role != LinkRole::Normal
            {
                return None;
            }
            let mut run = run.clone();
            run.text = run
                .text
                .chars()
                .skip(start.saturating_sub(offset))
                .take(end.min(offset + len) - start.max(offset))
                .collect();
            out.push(run);
        }
        offset += len;
    }
    (out.iter().map(|r| r.text.chars().count()).sum::<usize>() == end - start).then_some(out)
}

fn apply(text: &mut TextBlock, spans: &[Span]) {
    let mut located = Vec::new();
    for span in spans {
        // Canonical source positions are usable only if the exact original
        // formatted slice still occurs there; translated prose is never indexed
        // using original offsets.
        let encoded = encode(text);
        let exact = encoded
            .value
            .match_indices(&span.original)
            .find_map(|(begin, _)| {
                let end = begin + span.original.len();
                (encoded.boundaries.get(&begin) == Some(&span.start)
                    && encoded.boundaries.get(&end) == Some(&span.end)
                    && !encoded
                        .protected
                        .iter()
                        .any(|r| r.start < end && begin < r.end))
                .then_some((span.start, span.end))
            });
        let Some((start, end)) = exact.or_else(|| locate(text, &span.original, "", "")) else {
            continue;
        };
        let Some(original) = original_runs(text, start, end) else {
            continue;
        };
        located.push((
            start,
            end,
            MathRun {
                original: Some(original),
                latex: span.latex.clone(),
                display: false,
                size_scale: 1.0,
            },
        ));
    }
    located.sort_by_key(|(start, _, _)| *start);
    if located.windows(2).any(|p| p[0].1 > p[1].0) {
        return;
    }
    let total: usize = text
        .content
        .iter()
        .map(|i| match i {
            Inline::Text(r) => r.text.chars().count(),
            Inline::Math(m) => m.source_char_len(),
            Inline::Break => 1,
            _ => 0,
        })
        .sum();
    let mut result = Vec::new();
    let mut offset = 0;
    for inline in &text.content {
        let Inline::Text(run) = inline else {
            offset += match inline {
                Inline::Math(m) => m.source_char_len(),
                Inline::Break => 1,
                _ => 0,
            };
            result.push(inline.clone());
            continue;
        };
        let chars: Vec<_> = run.text.chars().collect();
        let mut at = 0;
        while at < chars.len() {
            let pos = offset + at;
            if let Some((start, end, formula)) =
                located.iter().find(|(a, b, _)| *a <= pos && pos < *b)
            {
                if pos == *start {
                    let mut formula = formula.clone();
                    formula.display = *start == 0 && *end == total;
                    result.push(Inline::Math(formula));
                }
                at = (*end - offset).min(chars.len());
            } else {
                let end = located
                    .iter()
                    .filter(|(a, _, _)| *a > pos)
                    .map(|(a, _, _)| *a - offset)
                    .min()
                    .unwrap_or(chars.len())
                    .min(chars.len());
                let mut piece = run.clone();
                piece.text = chars[at..end].iter().collect();
                result.push(Inline::Text(piece));
                at = end;
            }
        }
        offset += chars.len();
    }
    text.content = result;
}

pub(super) fn compose(blocks: &mut [Block], source: &SourceRange, spans: &[Span]) {
    visit(blocks, &mut |text| {
        if text.source.as_ref() == Some(source) {
            apply(text, spans);
        }
    });
}
pub(super) fn visit(blocks: &mut [Block], f: &mut impl FnMut(&mut TextBlock)) {
    for block in blocks {
        match block {
            Block::Text(t) => f(t),
            Block::Quote(q) => {
                for t in &mut q.body {
                    f(t);
                }
            }
            Block::Figure(g) => {
                for t in &mut g.captions {
                    f(t);
                }
            }
            Block::Table(t) => {
                for text in t.text_blocks_mut() {
                    f(text);
                }
            }
            Block::Note(n) => visit(&mut n.blocks, f),
            _ => {}
        }
    }
}
pub(super) fn restore_originals(blocks: &mut [Block]) {
    visit(blocks, &mut |text| {
        text.content = std::mem::take(&mut text.content)
            .into_iter()
            .flat_map(|inline| match inline {
                Inline::Math(MathRun {
                    original: Some(runs),
                    ..
                }) => runs.into_iter().map(Inline::Text).collect(),
                other => vec![other],
            })
            .collect();
    });
}

pub(super) fn validate_annotations(section: &Section, annotations: &[Annotation]) -> bool {
    let mut paragraphs = Vec::new();
    texts(&section.blocks, &mut paragraphs);
    annotations.iter().all(|a| match a {
        Annotation::TextFormulas { source, spans } => paragraphs
            .iter()
            .find(|text| text.source.as_ref() == Some(source))
            .is_some_and(|text| {
                let encoded = encode(text);
                spans.windows(2).all(|p| p[0].end <= p[1].start)
                    && spans.iter().all(|span| {
                        !span.original.trim().is_empty()
                            && complete_script_span(text, span.start, span.end)
                            && original_runs(text, span.start, span.end).is_some()
                            && encoded.value.match_indices(&span.original).any(|(at, _)| {
                                encoded.boundaries.get(&at) == Some(&span.start)
                                    && encoded.boundaries.get(&(at + span.original.len()))
                                        == Some(&span.end)
                            })
                            && formulas::validate_formula(&rebook_publication::ImageFormula {
                                latex: span.latex.clone(),
                                equation_number: None,
                            })
                            .is_ok()
                    })
            }),
        _ => true,
    })
}

#[cfg(test)]
mod tests;
