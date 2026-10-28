//! Bounded descendant/child matching, indexed by the rightmost simple selector.
use super::SimpleSelector;
use roxmltree::Node;

pub(super) struct Selector {
    parts: Vec<SimpleSelector>,
    // true = descendant; false = immediate child
    links: Vec<bool>,
}

impl Selector {
    pub(super) fn parse(raw: &str) -> Option<Self> {
        if raw.len() > 1024 { return None; }
        let tokens = raw.replace('>', " > ");
        let mut parts = Vec::new();
        let mut links = Vec::new();
        let mut child = false;
        for token in tokens.split_ascii_whitespace() {
            if token == ">" {
                if parts.is_empty() || child { return None; }
                child = true;
            } else {
                if parts.len() == 16 { return None; }
                let part = SimpleSelector::parse(token)?;
                if !parts.is_empty() { links.push(!child); }
                parts.push(part);
                child = false;
            }
        }
        if parts.is_empty() || child { return None; }
        Some(Self { parts, links })
    }

    pub(super) fn target(&self) -> &SimpleSelector { self.parts.last().unwrap() }

    pub(super) fn specificity(&self) -> u16 {
        self.parts.iter().fold(0u16, |sum, part| sum.saturating_add(part.specificity()))
    }

    pub(super) fn matches(&self, node: Node<'_, '_>) -> bool {
        if !node.is_element() || !self.target().matches(node) { return false; }
        if self.parts.len() == 1 { return true; }
        // Carry all viable ancestor matches, including alternatives where a
        // nearer matching descendant fails a subsequent child constraint.
        let mut pending = 1u16 << (self.parts.len() - 2);
        for parent in node.ancestors().skip(1).filter(Node::is_element).take(512) {
            let mut next = 0u16;
            for index in 0..self.links.len() {
                let bit = 1 << index;
                if pending & bit == 0 { continue; }
                if self.parts[index].matches(parent) {
                    if index == 0 { return true; }
                    next |= 1 << (index - 1);
                }
                if self.links[index] { next |= bit; }
            }
            if next == 0 { return false; }
            pending = next;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StyleSheet;
    use roxmltree::Document;

    #[test]
    fn descendant_overrides_inheritance_and_child_constraints_backtrack() {
        let doc = Document::parse("<figure class='a'><div class='b'><aside><div class='b'><p id='cell'>Text</p></div></aside></div><p id='note'>Note</p></figure>").unwrap();
        let mut sheet = StyleSheet::default();
        sheet.add_css("figure {font-weight:bold} .a > .b p {font-weight:normal} figure > p {font-style:italic} .missing p {font-weight:bold}");
        let cell = doc.descendants().find(|n| n.attribute("id") == Some("cell")).unwrap();
        let note = doc.descendants().find(|n| n.attribute("id") == Some("note")).unwrap();
        assert_eq!(sheet.cascaded_properties(cell).get("font-weight").unwrap(), "normal");
        assert!(!sheet.cascaded_properties(cell).contains_key("font-style"));
        assert_eq!(sheet.cascaded_properties(note).get("font-style").unwrap(), "italic");
        assert!(Selector::parse("figure > > p").is_none());
        assert!(Selector::parse("figure >").is_none());
        for unsupported in ["p + p", "p:hover", "p[x]", "* p"] {
            assert!(Selector::parse(unsupported).is_none());
        }
    }
}
