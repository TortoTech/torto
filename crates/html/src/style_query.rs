//! Document-local CSS lookup. Indexes only narrow the candidate set: the
//! existing selector matcher and stable cascade order remain authoritative.
use std::collections::HashMap;
use std::mem::size_of;
use std::rc::Rc;

use roxmltree::{Node, NodeId};

use super::{SimpleSelector, StyleRule, attribute_local};

#[derive(Default)]
pub(super) struct RuleIndex {
    ids: HashMap<String, Vec<usize>>,
    classes: HashMap<String, Vec<usize>>,
    tags: HashMap<String, Vec<usize>>,
    unkeyed: Vec<usize>,
}

impl RuleIndex {
    pub(super) fn insert(&mut self, index: usize, selector: &SimpleSelector) {
        // Every selector has exactly one bucket. Compound selectors are still
        // checked in full, including all classes, the tag, and the ID.
        let bucket = if let Some(id) = &selector.id {
            self.ids.entry(id.clone()).or_default()
        } else if let Some(class) = selector.classes.first() {
            self.classes.entry(class.clone()).or_default()
        } else if let Some(tag) = &selector.tag {
            self.tags.entry(tag.clone()).or_default()
        } else {
            &mut self.unkeyed
        };
        bucket.push(index);
    }

    pub(super) fn matching<'a>(
        &self,
        node: Node<'_, '_>,
        rules: &'a [StyleRule],
    ) -> Vec<&'a StyleRule> {
        let mut candidates = self.unkeyed.clone();
        if let Some(indices) = self.tags.get(&node.tag_name().name().to_ascii_lowercase()) {
            candidates.extend_from_slice(indices);
        }
        if let Some(id) = attribute_local(node, "id")
            && let Some(indices) = self.ids.get(id)
        {
            candidates.extend_from_slice(indices);
        }
        if let Some(classes) = attribute_local(node, "class") {
            for class in classes.split_ascii_whitespace() {
                if let Some(indices) = self.classes.get(class) {
                    candidates.extend_from_slice(indices);
                }
            }
        }
        // Duplicate class tokens must not apply the same rule twice. Restore
        // source order before the stable specificity/order sort so tied rules
        // behave exactly as the former full rules scan.
        candidates.sort_unstable();
        candidates.dedup();
        let mut matching: Vec<_> = candidates
            .into_iter()
            .map(|index| &rules[index])
            .filter(|rule| rule.selector.matches(node))
            .collect();
        matching.sort_by_key(|rule| (rule.specificity, rule.order));
        matching
    }
}

type Properties = HashMap<String, String>;

const MAX_CACHED_STYLE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(super) struct NodeStyleCache {
    nodes: HashMap<NodeId, Rc<Properties>>,
    estimated_bytes: usize,
}

impl NodeStyleCache {
    pub(super) fn get(&self, node: NodeId) -> Option<Rc<Properties>> {
        self.nodes.get(&node).cloned()
    }

    pub(super) fn insert(&mut self, node: NodeId, properties: Rc<Properties>) {
        // Include spare hash buckets, string allocations, Rc bookkeeping and
        // a conservative node-index allowance. Above budget, simply recompute;
        // eviction or a global cache must never affect CSS semantics.
        let bytes = size_of::<Properties>()
            + 2 * size_of::<usize>()
            + 2 * size_of::<(NodeId, Rc<Properties>)>()
            + properties.capacity() * (size_of::<(String, String)>() + 1)
            + properties
                .iter()
                .map(|(key, value)| key.capacity() + value.capacity())
                .sum::<usize>();
        if self.estimated_bytes.saturating_add(bytes) <= MAX_CACHED_STYLE_BYTES {
            self.nodes.insert(node, properties);
            self.estimated_bytes += bytes;
        }
    }

    pub(super) fn clear(&mut self) {
        self.nodes.clear();
        self.estimated_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::{StyleSheet, insert_declarations};
    use rebook_publication::{PublicationUrl, TextAlignment};
    use roxmltree::Document;

    // Independent reference for the former full scan, including its stable
    // ordering and the declaration validator/shorthand expansion.
    fn linear_properties(sheet: &StyleSheet, node: Node<'_, '_>) -> Properties {
        let mut matching: Vec<_> = sheet
            .rules
            .iter()
            .filter(|rule| rule.selector.matches(node))
            .collect();
        matching.sort_by_key(|rule| (rule.specificity, rule.order));
        let mut properties = Properties::new();
        for rule in matching {
            insert_declarations(&mut properties, rule.declarations.iter().cloned());
        }
        if let Some(inline) = attribute_local(node, "style") {
            insert_declarations(&mut properties, crate::declarations(inline));
        }
        properties
    }

    #[test]
    fn indexed_cascade_matches_linear_scan_for_compound_and_tied_rules() {
        let mut xml = String::from("<html><body>");
        let mut css = String::from(
            "body { color: #101010; text-align: end; } \
             p { font-size: 1.25em; margin: 1em 2em; } \
             .shared, p.shared { padding: 2em; } \
             .shared { font-size: invalid; margin-left: 3em; } \
             .first.second { text-align: center; } \
             P.first.second { color: #123456; } \
             .first.second#target { font-weight: bold; } \
             #target { font-style: italic; }",
        );
        for index in 0..200 {
            write!(
                xml,
                "<P id='item{index}' class='shared group{index} shared' \
                 style='margin-right: 4em; font-size: invalid'>Text</P>"
            )
            .unwrap();
            write!(
                css,
                ".group{index} {{ color: #{index:06x}; }} \
                 P.group{index} {{ padding-inline: 1em 3em; }}"
            )
            .unwrap();
        }
        xml.push_str(
            "<p id='target' class='shared first second' style='color: #abcdef'>Compound</p>\
             <p id='Target' class='Shared first'>Case sensitive class and ID</p>\
             <p>Tag only</p></body></html>",
        );
        let document = Document::parse(&xml).unwrap();
        let mut sheet = StyleSheet::default();
        sheet.add_css(&css);
        for node in document.descendants().filter(Node::is_element) {
            let expected = linear_properties(&sheet, node);
            assert_eq!(*sheet.cascaded_properties(node), expected);
            assert_eq!(*sheet.cascaded_properties(node), expected);
        }
        let target = document
            .descendants()
            .find(|node| node.attribute("id") == Some("target"))
            .unwrap();
        let properties = sheet.cascaded_properties(target);
        assert_eq!(properties.get("color").map(String::as_str), Some("#abcdef"));
        assert_eq!(
            properties.get("font-weight").map(String::as_str),
            Some("bold")
        );
        assert_eq!(
            properties.get("text-align").map(String::as_str),
            Some("center")
        );
    }

    #[test]
    fn adding_rules_invalidates_cached_nodes_without_mutating_prior_results() {
        let document = Document::parse("<p class='caption' style='font-weight: bold'/>").unwrap();
        let node = document.root_element();
        let mut sheet = StyleSheet::default();
        sheet.add_css(".caption { text-align: center; }");
        let prior = sheet.cascaded_properties(node);
        sheet.add_css(".caption { text-align: end; }");
        let updated = sheet.cascaded_properties(node);
        assert_eq!(prior.get("text-align").map(String::as_str), Some("center"));
        assert_eq!(updated.get("text-align").map(String::as_str), Some("end"));
        assert_eq!(*updated, linear_properties(&sheet, node));
    }

    #[test]
    fn cache_budget_fallback_keeps_the_complete_cascade() {
        let document =
            Document::parse("<p id='target' class='one two' style='margin-left: 8px'/>").unwrap();
        let node = document.root_element();
        let mut sheet = StyleSheet::default();
        sheet.add_css("p.one.two { margin: 1px; } #target { text-align: end; }");
        sheet.cascaded_cache.borrow_mut().estimated_bytes = MAX_CACHED_STYLE_BYTES;
        let expected = linear_properties(&sheet, node);
        assert_eq!(*sheet.cascaded_properties(node), expected);
        assert_eq!(*sheet.cascaded_properties(node), expected);
        assert!(sheet.cascaded_cache.borrow().nodes.is_empty());
        assert_eq!(
            sheet.cascaded_cache.borrow().estimated_bytes,
            MAX_CACHED_STYLE_BYTES
        );
    }

    #[test]
    fn separate_documents_do_not_share_node_styles_or_inherited_alignment() {
        let href = PublicationUrl::parse("chapter.xhtml").unwrap();
        let first = Document::parse(
            "<html><style>body { text-align: center; }</style><body><p>Caption</p></body></html>",
        )
        .unwrap();
        let second = Document::parse(
            "<html><style>body { text-align: end; }</style><body><p>Caption</p></body></html>",
        )
        .unwrap();
        let first_sheet = StyleSheet::from_document(&first, &href, &mut |_| None);
        let second_sheet = StyleSheet::from_document(&second, &href, &mut |_| None);
        let first_node = first
            .descendants()
            .find(|node| node.has_tag_name("p"))
            .unwrap();
        let second_node = second
            .descendants()
            .find(|node| node.has_tag_name("p"))
            .unwrap();
        assert_eq!(first_node.id(), second_node.id());
        assert_eq!(
            first_sheet.inherited_text_alignment(first_node),
            Some(TextAlignment::Center)
        );
        assert_eq!(
            second_sheet.inherited_text_alignment(second_node),
            Some(TextAlignment::End)
        );
    }
}
