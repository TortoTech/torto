use super::*;
use crate::plugins::semantic_layout::{fixed_batches, semantic_units};
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Batch {
    pub index: usize,
    pub target: Range<usize>,
    pub context: Range<usize>,
    pub subsection: Range<usize>,
    pub visible: bool,
}

struct SubsectionPlan {
    range: Range<usize>,
    batches: Vec<Batch>,
}

pub(super) struct SectionPlan {
    original: Arc<Section>,
    sources: Vec<Vec<SourceRange>>,
    nodes: HashMap<(rebook_publication::SpineItemId, String), Vec<usize>>,
    subsections: Vec<SubsectionPlan>,
}

pub(super) fn trim_plans(plans: &mut HashMap<usize, Arc<SectionPlan>>, retained: &HashSet<usize>) {
    while plans.len() > 16 {
        let Some(index) = plans
            .keys()
            .find(|index| !retained.contains(index))
            .copied()
        else {
            break;
        };
        plans.remove(&index);
    }
}

impl SectionPlan {
    pub(super) fn new(
        index: usize,
        original: Arc<Section>,
        toc: &[rebook_reader::TocViewItem],
    ) -> Self {
        let sources: Vec<_> = original.blocks.iter().map(block_ranges).collect();
        let mut nodes: HashMap<_, Vec<usize>> = HashMap::new();
        let mut starts = HashMap::new();
        for (block, ranges) in sources.iter().enumerate() {
            for source in ranges {
                starts.entry(source.start.node.as_str()).or_insert(block);
                for anchor in [&source.start, &source.end] {
                    let entries = nodes
                        .entry((anchor.spine.clone(), anchor.node.clone()))
                        .or_default();
                    if entries.last() != Some(&block) {
                        entries.push(block);
                    }
                }
            }
        }
        let mut anchors = HashMap::new();
        for anchor in &original.anchors {
            anchors
                .entry(anchor.fragment.as_str())
                .or_insert(&anchor.source);
        }
        let mut boundaries = vec![0, original.blocks.len()];
        boundaries.extend(toc.iter().filter_map(|item| {
            let target = item.target.as_ref()?;
            if target.path() != original.href.path() {
                return None;
            }
            let anchor = anchors.get(target.fragment()?)?;
            starts.get(anchor.node.as_str()).copied()
        }));
        boundaries.sort_unstable();
        boundaries.dedup();
        let subsections = boundaries
            .windows(2)
            .map(|bounds| {
                let range = bounds[0]..bounds[1];
                let units = semantic_units(&original, range.clone());
                let batches = fixed_batches(&original, range.clone())
                    .into_iter()
                    .map(|target| {
                        let lo = units
                            .iter()
                            .rev()
                            .find(|r| r.end == target.start)
                            .map_or(target.start, |r| r.start);
                        let hi = units
                            .iter()
                            .find(|r| r.start == target.end)
                            .map_or(target.end, |r| r.end);
                        Batch {
                            index,
                            target,
                            context: lo..hi,
                            subsection: range.clone(),
                            visible: false,
                        }
                    })
                    .collect();
                SubsectionPlan { range, batches }
            })
            .collect();
        Self {
            original,
            sources,
            nodes,
            subsections,
        }
    }

    pub(super) fn matches(&self, original: &Arc<Section>) -> bool {
        Arc::ptr_eq(&self.original, original)
    }

    fn select(&self, ranges: &[SourceRange]) -> Vec<Batch> {
        let mut touched = std::collections::BTreeSet::new();
        for range in ranges {
            for anchor in [&range.start, &range.end] {
                if let Some(blocks) = self.nodes.get(&(anchor.spine.clone(), anchor.node.clone())) {
                    for &block in blocks {
                        if self.sources[block]
                            .iter()
                            .any(|source| overlap(source, range))
                        {
                            touched.insert(block);
                        }
                    }
                }
            }
        }
        let touches = |range: &Range<usize>| touched.range(range.clone()).next().is_some();
        let mut out = Vec::new();
        for subsection in &self.subsections {
            if touches(&subsection.range) {
                out.extend(subsection.batches.iter().cloned().map(|mut batch| {
                    batch.visible = touches(&batch.target);
                    batch
                }));
            }
        }
        out.sort_by_key(|b| !b.visible);
        out
    }
}

#[cfg(test)]
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
            let mut plans = self.semantic_layout.batch_plans.borrow_mut();
            let cached = plans.entry(*index).or_insert_with(|| {
                Arc::new(SectionPlan::new(
                    *index,
                    Arc::clone(section),
                    self.reader.toc_items(),
                ))
            });
            if !cached.matches(section) {
                *cached = Arc::new(SectionPlan::new(
                    *index,
                    Arc::clone(section),
                    self.reader.toc_items(),
                ));
            }
            out.extend(cached.select(&ranges));
        }
        trim_plans(&mut self.semantic_layout.batch_plans.borrow_mut(), &seen);
        out.sort_by_key(|b| !b.visible);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::Block;

    #[test]
    fn indexed_plan_preserves_boundaries_visibility_and_context() {
        let (_, mut section, _) = super::super::tests::fixture();
        let template = section.blocks[0].clone();
        section.blocks = (0..12)
            .map(|index| {
                let mut block = template.clone();
                let Block::Text(text) = &mut block else {
                    unreachable!()
                };
                text.content = vec![rebook_publication::Inline::Text(
                    rebook_publication::TextRun {
                        text: "word ".repeat(500),
                        style: Default::default(),
                        link: None,
                    },
                )];
                let range = text.source.as_mut().unwrap();
                range.start.node = index.to_string();
                range.end.node = index.to_string();
                range.end.text_offset = 2500;
                block
            })
            .collect();
        let toc = [0, 4, 9]
            .into_iter()
            .map(|index| {
                let source = block_ranges(&section.blocks[index])[0].start.clone();
                section.anchors.push(rebook_publication::SectionAnchor {
                    fragment: format!("a{index}"),
                    source,
                });
                rebook_reader::TocViewItem {
                    id: format!("toc{index}"),
                    label: index.to_string(),
                    target: Some(
                        rebook_publication::PublicationUrl::parse(&format!(
                            "{}#a{index}",
                            section.href.path()
                        ))
                        .unwrap(),
                    ),
                    depth: 0,
                    ancestors: Vec::new(),
                    has_children: false,
                }
            })
            .collect::<Vec<_>>();
        let section = Arc::new(section);
        let indexed = SectionPlan::new(0, section.clone(), &toc);
        for visible in [vec![0], vec![3], vec![4, 5], vec![2, 10], vec![11]] {
            let ranges = visible
                .into_iter()
                .flat_map(|index| block_ranges(&section.blocks[index]))
                .collect::<Vec<_>>();
            assert_eq!(
                indexed.select(&ranges),
                plan(0, &section, vec![0, 4, 9], &ranges)
            );
        }
        assert!(indexed.matches(&section));
        assert!(!indexed.matches(&Arc::new((*section).clone())));
    }

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
