use super::*;
#[cfg(test)]
use rebook_app_core::reading_ai::batching::plan;
pub(super) use rebook_app_core::reading_ai::batching::{Batch, SectionPlan, trim_plans};
use std::sync::Arc;

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
                    page_label: None,
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
