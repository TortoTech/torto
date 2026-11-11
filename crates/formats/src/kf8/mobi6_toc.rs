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

/// Flat NCX indexes can coexist with an explicitly nested HTML contents page.
/// Borrow only its hierarchy: original NCX order and target offsets stay intact.
pub(super) fn recover_structure(entries: Vec<SourceTocEntry>, html: &str) -> Vec<SourceTocEntry> {
    if entries.len() < 2 || entries.iter().any(|e| !e.children.is_empty()) {
        return entries;
    }
    let Ok(xml) = crate::source::html_document(html) else {
        return entries;
    };
    let Ok(doc) = roxmltree::Document::parse(&xml) else {
        return entries;
    };
    let key = |s: &str| {
        decode_html_entities(s)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let mut labels = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if labels.insert(key(&e.label), i).is_some() {
            return entries;
        }
    }
    let mut found = Vec::new();
    let mut ordered = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("a")) {
        let Some(position) = node
            .attribute("filepos")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            continue;
        };
        let value: String = node
            .descendants()
            .filter_map(|n| n.text().filter(|_| n.is_text()))
            .collect();
        let Some(&i) = labels.get(&key(&value)) else {
            continue;
        };
        let Some(target) = entries[i]
            .href
            .rsplit("#filepos")
            .next()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            continue;
        };
        if target.abs_diff(position) > 64 {
            continue;
        }
        let depth = node
            .ancestors()
            .filter(|n| n.has_tag_name("blockquote") || n.has_tag_name("li"))
            .count();
        if i == 0 {
            ordered.clear();
        }
        if i != ordered.len() {
            ordered.clear();
            continue;
        }
        ordered.push((i, depth));
        if ordered.len() == entries.len() {
            // Some MOBIs include both a flat generated contents page and a
            // nested authored one. Prefer complete structural evidence.
            if ordered.iter().any(|(_, d)| *d != ordered[0].1) {
                if !found.is_empty() && found != ordered {
                    return entries;
                }
                found.clone_from(&ordered);
            }
            ordered.clear();
        }
    }
    // Complete, unambiguous correspondence prevents a stray body link from
    // reparenting a partial index or moving a target backwards.
    if found.len() != entries.len() || found.iter().enumerate().any(|(i, (at, _))| i != *at) {
        return entries;
    }
    let mut levels: Vec<_> = found.iter().map(|(_, d)| *d).collect();
    levels.sort_unstable();
    levels.dedup();
    if levels.len() < 2 || levels.len() > 8 {
        return entries;
    }
    let mut children = vec![Vec::new(); entries.len()];
    let mut roots = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (i, depth) in found {
        while stack.last().is_some_and(|&(_, d)| d >= depth) {
            stack.pop();
        }
        if let Some(&(parent, _)) = stack.last() {
            children[parent].push(i);
        } else {
            roots.push(i);
        }
        stack.push((i, depth));
    }
    fn build(i: usize, entries: &[SourceTocEntry], children: &[Vec<usize>]) -> SourceTocEntry {
        let e = &entries[i];
        SourceTocEntry {
            label: e.label.clone(),
            href: e.href.clone(),
            children: children[i]
                .iter()
                .map(|&i| build(i, entries, children))
                .collect(),
        }
    }
    roots
        .into_iter()
        .map(|i| build(i, &entries, &children))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entries() -> Vec<SourceTocEntry> {
        ["Chapter", "Article", "Appendix", "Next"]
            .into_iter()
            .enumerate()
            .map(|(i, label)| SourceTocEntry {
                label: label.into(),
                href: format!("Text/section-1.xhtml#filepos{}", 100 + i * 100),
                children: vec![],
            })
            .collect()
    }
    fn snapshot(entries: &[SourceTocEntry]) -> Vec<(String, String, usize)> {
        fn visit(entries: &[SourceTocEntry], depth: usize, out: &mut Vec<(String, String, usize)>) {
            for e in entries {
                out.push((e.label.clone(), e.href.clone(), depth));
                visit(&e.children, depth + 1, out)
            }
        }
        let mut out = vec![];
        visit(entries, 0, &mut out);
        out
    }
    const CONTENTS: &str = "<html><body><p><a filepos='102'>Chapter</a></p><blockquote><p><a filepos='202'>Article</a></p><blockquote><p><a filepos='302'>Appendix</a></p></blockquote></blockquote><p><a filepos='402'>Next</a></p></body></html>";
    #[test]
    #[ignore = "requires TORTO_MOBI_TOC_BOOK with the reported flat-NCX MOBI"]
    fn local_html_contents_restores_the_reported_ncx() {
        let bytes = std::fs::read(std::env::var("TORTO_MOBI_TOC_BOOK").unwrap()).unwrap();
        let book = super::super::parse_mobi6_inner(&bytes).unwrap();
        assert!(
            book.table_of_contents
                .iter()
                .any(|e| !e.children.is_empty())
        );
    }
    #[test]
    fn html_contents_restores_hierarchy_without_rewriting_ncx_targets() {
        let original = entries();
        let restored = recover_structure(original.clone(), CONTENTS);
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].href, original[0].href);
        assert_eq!(restored[0].children[0].href, original[1].href);
        assert_eq!(restored[0].children[0].children[0].href, original[2].href);
        assert_eq!(restored[1].href, original[3].href);
        // Existing NCX hierarchy always takes precedence.
        assert_eq!(
            snapshot(&recover_structure(restored.clone(), CONTENTS)),
            snapshot(&restored)
        );
    }
    #[test]
    fn incomplete_or_reversed_contents_cannot_reparent_the_index() {
        let original = entries();
        for html in [
            CONTENTS.replace("filepos='202'", "href='other'"),
            CONTENTS
                .replace("filepos='202'>Article", "filepos='302'>Appendix")
                .replace(
                    "filepos='302'>Appendix</a></p></blockquote>",
                    "filepos='202'>Article</a></p></blockquote>",
                ),
        ] {
            assert_eq!(
                snapshot(&recover_structure(original.clone(), &html)),
                snapshot(&original)
            );
        }
    }
}
