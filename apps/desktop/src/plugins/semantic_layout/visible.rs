use super::*;
use std::ops::Range;

pub(crate) fn block_ranges(block: &Block) -> Vec<SourceRange> {
    let mut ranges = source(block).cloned().into_iter().collect::<Vec<_>>();
    match block {
        Block::Quote(q) => ranges.extend(
            q.body
                .iter()
                .chain(q.attribution.iter())
                .filter_map(|text| text.source.clone()),
        ),
        Block::Table(t) => ranges.extend(t.text_blocks().filter_map(|text| text.source.clone())),
        Block::Figure(f) => {
            ranges.extend(f.images.iter().filter_map(|image| image.source.clone()));
            ranges.extend(f.captions.iter().filter_map(|text| text.source.clone()));
        }
        Block::Note(n) => ranges.extend(n.blocks.iter().flat_map(block_ranges)),
        _ => {}
    }
    ranges
}

fn annotation_blocks(section: &Section, annotation: &Annotation) -> Vec<usize> {
    let ranges = match annotation {
        Annotation::InlineCitations { source, .. } | Annotation::SectionHeading { source } => {
            vec![source]
        }
        Annotation::QuoteBefore {
            body, attribution, ..
        } => body.iter().chain(std::iter::once(attribution)).collect(),
        Annotation::QuoteInline { body, .. } => body.iter().collect(),
        Annotation::Quote {
            body, attribution, ..
        } => body.iter().chain(attribution.iter()).collect(),
        Annotation::Figure {
            images, captions, ..
        } => images.iter().chain(captions).collect(),
        Annotation::QuoteAttribution {
            quote, attribution, ..
        } => vec![quote, attribution],
        Annotation::ImageFormula { href, .. } | Annotation::UnreadableFormula { href } => {
            return section
                .blocks
                .iter()
                .enumerate()
                .filter_map(|(index, _)| {
                    formulas::candidates(&scope_section(section, index..index + 1))
                        .iter()
                        .any(|candidate| &candidate.image.href == href)
                        .then_some(index)
                })
                .collect();
        }
    };
    section
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let sources = block_ranges(block);
            ranges
                .iter()
                .any(|range| {
                    sources.iter().any(|source| {
                        source.start.spine == range.start.spine
                            && source.start.node == range.start.node
                    })
                })
                .then_some(index)
        })
        .collect()
}

pub(crate) fn empty_recognition(section: &Section) -> Recognition {
    Recognition {
        formulas_checked: true,
        fingerprint: fingerprint(section),
        annotations: Vec::new(),
        skipped_groups: 0,
    }
}

pub(crate) fn merge_recognition(
    section: &Section,
    range: Range<usize>,
    results: Vec<Recognition>,
) -> Recognition {
    let mut merged = empty_recognition(&scope_section(section, range));
    for result in results {
        for annotation in result.annotations {
            if !merged.annotations.contains(&annotation) {
                merged.annotations.push(annotation);
            }
        }
    }
    merged
}

/// Independent paragraphs remain independent; references returned by the model
/// join a body/credit or image/caption into one atomic publication group.
pub(crate) fn recognition_groups(
    section: &Section,
    target: Range<usize>,
    result: Recognition,
) -> Vec<(Range<usize>, Recognition)> {
    let entries = result
        .annotations
        .into_iter()
        .map(|annotation| {
            let mut ids = annotation_blocks(section, &annotation);
            if matches!(
                annotation,
                Annotation::ImageFormula { .. } | Annotation::UnreadableFormula { .. }
            ) {
                ids.retain(|id| target.contains(id));
            }
            (annotation, ids)
        })
        .filter(|(_, ids)| ids.iter().any(|id| target.contains(id)))
        .collect::<Vec<_>>();
    let mut groups = target.clone().map(|id| vec![id]).collect::<Vec<_>>();
    for (_, ids) in &entries {
        let mut joined = ids.clone();
        groups.retain(|group| {
            if group.iter().any(|id| ids.contains(id)) {
                joined.extend(group);
                false
            } else {
                true
            }
        });
        joined.sort_unstable();
        joined.dedup();
        groups.push(joined);
    }
    groups.sort_by_key(|ids| ids[0]);
    groups
        .into_iter()
        .map(|ids| {
            let range = ids[0]..ids[ids.len() - 1] + 1;
            let mut recognition = empty_recognition(&scope_section(section, range.clone()));
            recognition.annotations = entries
                .iter()
                .filter(|(_, affected)| affected.iter().any(|id| ids.contains(id)))
                .map(|(annotation, _)| annotation.clone())
                .collect();
            (range, recognition)
        })
        .collect()
}

pub(crate) fn needs_recognition(section: &Section, target: Range<usize>) -> bool {
    let target = scope_section(section, target);
    if matches!(target.blocks.as_slice(), [Block::Note(note)] if note.kind == rebook_publication::NoteBlockKind::Section)
    {
        return false;
    }
    if !formulas::candidates(&target).is_empty() {
        return true;
    }
    target.blocks.iter().any(|block| {
        if paragraph(block).is_some()
            || unattributed_quote_body(block).is_some()
            || headings::candidate(block).is_some()
        {
            return true;
        }
        match block {
            Block::Quote(q) => q.body.iter().any(|t| !citations::candidates(t).is_empty()),
            Block::Table(t) => t
                .text_blocks()
                .any(|t| !citations::candidates(t).is_empty()),
            _ => false,
        }
    })
}
