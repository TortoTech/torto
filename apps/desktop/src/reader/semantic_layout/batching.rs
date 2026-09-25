use super::*;
use crate::plugins::semantic_layout::{fixed_batches, semantic_units};
use std::ops::Range;

pub(super) struct Batch {
    pub index: usize,
    pub target: Range<usize>,
    pub context: Range<usize>,
    pub subsection: Range<usize>,
    pub visible: bool,
}

pub(super) fn plan(
    index: usize,
    section: &Section,
    mut boundaries: Vec<usize>,
    ranges: &[SourceRange],
) -> Vec<Batch> {
    boundaries.extend([0, section.blocks.len()]);
    boundaries.sort_unstable();
    boundaries.dedup();
    let touches = |range: Range<usize>| {
        section.blocks[range]
            .iter()
            .flat_map(block_ranges)
            .any(|s| ranges.iter().any(|r| overlap(&s, r)))
    };
    let mut out = Vec::new();
    for bounds in boundaries.windows(2) {
        let subsection = bounds[0]..bounds[1];
        if !touches(subsection.clone()) {
            continue;
        }
        let units = semantic_units(section, subsection.clone());
        for target in fixed_batches(section, subsection.clone()) {
            let lo = units
                .iter()
                .rev()
                .find(|r| r.end == target.start)
                .map_or(target.start, |r| r.start);
            let hi = units
                .iter()
                .find(|r| r.start == target.end)
                .map_or(target.end, |r| r.end);
            out.push(Batch {
                index,
                visible: touches(target.clone()),
                target,
                context: lo..hi,
                subsection: subsection.clone(),
            });
        }
    }
    out.sort_by_key(|b| !b.visible);
    out
}

impl DesktopReader {
    pub(super) fn semantic_batch_plan(&self, demand: &Demand) -> Vec<Batch> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (index, _) in demand {
            if !seen.insert(*index) {
                continue;
            }
            let Some(section) = self.semantic_layout.originals.get(index) else {
                continue;
            };
            let ranges: Vec<_> = demand
                .iter()
                .filter(|(i, _)| i == index)
                .flat_map(|(_, r)| r.iter().cloned())
                .collect();
            let boundaries = self
                .reader
                .toc_items()
                .iter()
                .filter_map(|item| {
                    let target = item.target.as_ref()?;
                    if target.path() != section.href.path() {
                        return None;
                    }
                    let fragment = target.fragment()?;
                    let anchor = section.anchors.iter().find(|a| a.fragment == fragment)?;
                    section.blocks.iter().position(|b| {
                        block_ranges(b)
                            .iter()
                            .any(|r| r.start.node == anchor.source.node)
                    })
                })
                .collect();
            out.extend(plan(*index, section, boundaries, &ranges));
        }
        out.sort_by_key(|b| !b.visible);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsection_batches_are_stable_prioritized_and_do_not_prefetch_other_subsections() {
        let (_, mut s, _) = super::super::tests::fixture();
        let template = s.blocks[0].clone();
        s.blocks = (0..8)
            .map(|i| {
                let mut b = template.clone();
                let rebook_publication::Block::Text(t) = &mut b else {
                    unreachable!()
                };
                let rebook_publication::Inline::Text(run) = &mut t.content[0] else {
                    unreachable!()
                };
                run.text = "x".repeat(2000);
                let source = t.source.as_mut().unwrap();
                source.start.node = i.to_string();
                source.end.node = i.to_string();
                b
            })
            .collect();
        let visible = block_ranges(&s.blocks[3]);
        let a = plan(0, &s, vec![0, 6, 8], &visible);
        assert_eq!(
            a.iter().map(|b| b.target.clone()).collect::<Vec<_>>(),
            vec![2..4, 0..2, 4..6]
        );
        assert_eq!(a[0].context, 1..5);
        assert!(a[0].visible);
        assert!(!a[1].visible);
        let b = plan(0, &s, vec![0, 6, 8], &block_ranges(&s.blocks[2]));
        assert_eq!(
            a.iter()
                .map(|b| (&b.target, &b.context))
                .collect::<Vec<_>>(),
            b.iter()
                .map(|b| (&b.target, &b.context))
                .collect::<Vec<_>>()
        );
        let c = plan(0, &s, vec![0, 6, 8], &block_ranges(&s.blocks[7]));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].target, 6..8);
        assert_eq!(c[0].context, 6..8);
    }
}
