use std::collections::{HashMap, HashSet};
use std::ops::Range;

use super::{IndexData, decode_html_entities};
use crate::source::SourceTocEntry;

struct Entry {
    original_index: usize,
    label: String,
    section: usize,
    position: usize,
    parent: Option<usize>,
}

pub(super) fn resolve(
    index: &IndexData,
    sections: &[Range<usize>],
) -> Result<Vec<SourceTocEntry>, String> {
    let entries = index
        .entries
        .iter()
        .enumerate()
        .filter_map(|(original_index, entry)| {
            let position = *entry.tags.get(&1)?.first()?;
            let label = index.cncx.get(entry.tags.get(&3)?.first()?)?;
            let label = decode_html_entities(label)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let section = sections.partition_point(|range| range.end <= position);
            if label.is_empty() || !sections.get(section)?.contains(&position) {
                return None;
            }
            Some(Entry {
                original_index,
                label,
                section,
                position,
                parent: entry
                    .tags
                    .get(&21)
                    .and_then(|values| values.first())
                    .copied(),
            })
        })
        .collect::<Vec<_>>();
    let indices = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.original_index, i))
        .collect::<HashMap<_, _>>();
    let mut children = vec![Vec::new(); entries.len()];
    let mut roots = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        // Reject cycles and pathological depth before constructing the tree.
        // A missing/invalid parent's surviving children become roots.
        let mut parent = Some(entry.original_index);
        let mut seen = HashSet::new();
        while let Some(ancestor) = parent.and_then(|id| indices.get(&id).copied()) {
            if !seen.insert(ancestor) || seen.len() > 64 {
                return Err("cyclic or excessively deep MOBI6 table of contents".into());
            }
            parent = entries[ancestor].parent;
        }
        if let Some(parent) = entry.parent.and_then(|id| indices.get(&id).copied()) {
            children[parent].push(i);
        } else {
            roots.push(i);
        }
    }
    fn build(i: usize, entries: &[Entry], children: &[Vec<usize>]) -> SourceTocEntry {
        let entry = &entries[i];
        SourceTocEntry {
            label: entry.label.clone(),
            href: format!(
                "Text/section-{}.xhtml#filepos{}",
                entry.section + 1,
                entry.position
            ),
            children: children[i]
                .iter()
                .map(|&child| build(child, entries, children))
                .collect(),
        }
    }
    Ok(roots
        .into_iter()
        .map(|i| build(i, &entries, &children))
        .collect())
}
