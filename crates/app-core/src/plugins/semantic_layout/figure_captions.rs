use super::*;
use std::ops::Range;

pub(super) type Evidence = Vec<(usize, String)>;

/// Only consider an image run with a possible caption within the window.
pub(super) fn candidates(
    section: &Section,
    target: Range<usize>,
    context: Range<usize>,
) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut index = context.start;
    while index < context.end {
        if !image_needs_caption(section, index) {
            index += 1;
            continue;
        }
        let start = index;
        while index < context.end && image_needs_caption(section, index) {
            index += 1;
        }
        let before = start.checked_sub(1).filter(|id| context.contains(id));
        let after = Some(index).filter(|id| context.contains(id));
        let neighbors: Vec<_> = before
            .into_iter()
            .chain(after)
            .filter(|&id| paragraph(&section.blocks[id]).is_some())
            .collect();
        if !neighbors.is_empty()
            && (start < target.end && target.start < index
                || neighbors.iter().any(|id| target.contains(id)))
        {
            runs.push(start..index);
        }
    }
    runs
}

pub(super) fn evidence(
    source: &dyn BookSource,
    section: &Section,
    target: Range<usize>,
    context: Range<usize>,
) -> Evidence {
    let mut images = Vec::new();
    let mut bytes = 0;
    for run in candidates(section, target, context) {
        if images.len() + run.len() > request_images::MAX_IMAGES {
            continue;
        }
        let remaining = request_images::MAX_BYTES - bytes;
        let mut pending = Vec::new();
        let mut size = 0;
        for id in run.clone() {
            let Block::Image(image) = &section.blocks[id] else {
                break;
            };
            let url = source
                .resource(&image.href)
                .ok()
                .filter(|resource| resource.bytes.len() <= request_images::MAX_RESOURCE_BYTES)
                .and_then(|resource| request_images::decode(&resource.bytes).ok())
                .and_then(|image| request_images::image_url(image).ok());
            let Some(url) = url else { break };
            size += url.len() + 512;
            if size > remaining {
                break;
            }
            pending.push((id, url));
        }
        // Avoid presenting a partial multi-panel illustration as the full run.
        if pending.len() == run.len() {
            bytes += size;
            images.extend(pending);
        }
    }
    images
}

pub(super) fn content(input: &Value, evidence: &[(usize, String)]) -> Value {
    let text = wire::encode(input).to_string();
    if evidence.is_empty() {
        return json!(text);
    }
    let mut parts = vec![json!({"type":"text","text":text})];
    for (id, url) in evidence {
        parts.push(json!({"type":"text","text":format!("Image for book block ID {id}")}));
        parts.push(json!({"type":"image_url","image_url":{"url":url}}));
    }
    json!(parts)
}

/// An early decision is only sufficient when it covers every figure candidate
/// and no competing semantic role needs the combined request.
pub(super) fn decision_pairs(
    section: &Section,
    target: Range<usize>,
    context: Range<usize>,
    evidence: &[(usize, String)],
) -> Option<Vec<(usize, Vec<usize>)>> {
    let runs = candidates(section, target.clone(), context.clone());
    if runs.is_empty() || runs.len() != evidence.len() {
        return None;
    }
    let mut pairs = Vec::new();
    for (run, (id, _)) in runs.into_iter().zip(evidence) {
        if run.len() != 1 || run.start != *id {
            return None;
        }
        let neighbors = [id.checked_sub(1), id.checked_add(1)]
            .into_iter()
            .flatten()
            .filter(|i| context.contains(i))
            .filter(|&i| paragraph(&section.blocks[i]).is_some())
            .collect::<Vec<_>>();
        if neighbors.iter().any(|&i| {
            // A second adjacent paragraph could be a continuation or source line.
            let next = if i < *id {
                i.checked_sub(1)
            } else {
                i.checked_add(1)
            };
            next.filter(|i| context.contains(i))
                .is_some_and(|i| paragraph(&section.blocks[i]).is_some())
        }) {
            return None;
        }
        pairs.push((*id, neighbors));
    }
    Some(pairs)
}
