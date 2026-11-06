use super::overlap;
use crate::plugins::semantic_layout::block_ranges;
use crate::plugins::semantic_layout::{fixed_batches, semantic_units};
use rebook_publication::{Section, SourceRange};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::Mutex;

pub struct PreparedSection {
    pub section: Arc<Section>,
    pub hash: String,
    pub plan: SectionPlan,
    bytes: usize,
}
#[derive(Default)]
pub struct Snapshots(Mutex<std::collections::BTreeMap<(usize, u64), Arc<PreparedSection>>>);
impl Snapshots {
    pub fn load(
        &self,
        index: usize,
        revision: u64,
        source: &dyn rebook_publication::BookSource,
        toc: &[rebook_reader::TocViewItem],
    ) -> Result<Arc<PreparedSection>, rebook_publication::PublicationError> {
        if let Some(found) = self.0.lock().unwrap().get(&(index, revision)).cloned() {
            return Ok(found);
        }
        // Parsing and fingerprinting are worker work, outside the cache lock.
        let section = Arc::new(source.parse_section(index)?);
        let bytes = serde_json::to_vec(section.as_ref()).unwrap();
        let prepared = Arc::new(PreparedSection {
            hash: super::super::plugins::semantic_layout::fingerprint(&section),
            plan: SectionPlan::new(index, section.clone(), toc),
            section,
            bytes: bytes.len(),
        });
        let mut entries = self.0.lock().unwrap();
        entries.retain(|&(i, r), _| i != index || r == revision);
        let found = entries.entry((index, revision)).or_insert(prepared).clone();
        while entries.len() > 16
            || entries.values().map(|s| s.bytes).sum::<usize>() > 8 * 1024 * 1024
        {
            let Some(key) = entries
                .keys()
                .find(|&&key| key != (index, revision))
                .copied()
            else {
                break;
            };
            entries.remove(&key);
        }
        Ok(found)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
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

pub struct SectionPlan {
    original: Arc<Section>,
    sources: Vec<Vec<SourceRange>>,
    nodes: HashMap<(rebook_publication::SpineItemId, String), Vec<usize>>,
    subsections: Vec<SubsectionPlan>,
}

pub fn trim_plans(plans: &mut HashMap<usize, Arc<SectionPlan>>, retained: &HashSet<usize>) {
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
    pub fn new(index: usize, original: Arc<Section>, toc: &[rebook_reader::TocViewItem]) -> Self {
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

    pub fn matches(&self, original: &Arc<Section>) -> bool {
        Arc::ptr_eq(&self.original, original)
    }

    pub fn select(&self, ranges: &[SourceRange]) -> Vec<Batch> {
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

#[cfg(any(test, feature = "test-support"))]
pub fn plan(
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
