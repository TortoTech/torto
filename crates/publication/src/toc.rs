//! One navigation lookup policy for loading headers and search result labels.
use crate::{PublicationUrl, TocEntry};

#[derive(Clone, Copy)]
pub enum TocMatch {
    /// A restored reading position must not borrow another subsection's title.
    ExactOrChapter,
    /// A section label may fall back to its first authored anchor-only entry.
    Section,
}

pub fn toc_entry_for_href<'a>(
    entries: &'a [TocEntry],
    href: &PublicationUrl,
    policy: TocMatch,
) -> Option<&'a TocEntry> {
    fn find<'a>(
        entries: &'a [TocEntry],
        matches: &impl Fn(&PublicationUrl) -> bool,
        children_first: bool,
    ) -> Option<&'a TocEntry> {
        entries.iter().find_map(|entry| {
            let own = || {
                entry
                    .href
                    .as_ref()
                    .filter(|target| matches(target))
                    .map(|_| entry)
            };
            let children = || find(&entry.children, matches, children_first);
            if children_first {
                children().or_else(own)
            } else {
                own().or_else(children)
            }
        })
    }
    find(entries, &|target| target == href, true)
        .or_else(|| {
            find(
                entries,
                &|target| {
                    target.resource_url() == href.resource_url() && target.fragment().is_none()
                },
                true,
            )
        })
        .or_else(|| match policy {
            TocMatch::ExactOrChapter => None,
            TocMatch::Section => find(
                entries,
                &|target| target.resource_url() == href.resource_url(),
                false,
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, href: &str, children: Vec<TocEntry>) -> TocEntry {
        TocEntry {
            label: label.into(),
            href: Some(PublicationUrl::parse(href).unwrap()),
            children,
        }
    }

    #[test]
    fn exact_anchors_win_over_parent_documents_for_both_consumers() {
        let entries = vec![entry(
            "Chapter",
            "book.xhtml",
            vec![
                entry("First", "book.xhtml#a", vec![]),
                entry("Second", "book.xhtml#b", vec![]),
            ],
        )];
        for policy in [TocMatch::ExactOrChapter, TocMatch::Section] {
            for (href, label) in [
                ("book.xhtml#b", "Second"),
                ("book.xhtml#unknown", "Chapter"),
                ("book.xhtml", "Chapter"),
            ] {
                assert_eq!(
                    toc_entry_for_href(&entries, &PublicationUrl::parse(href).unwrap(), policy)
                        .unwrap()
                        .label,
                    label
                );
            }
            assert!(
                toc_entry_for_href(
                    &entries,
                    &PublicationUrl::parse("other.xhtml").unwrap(),
                    policy
                )
                .is_none()
            );
        }
    }

    #[test]
    fn only_section_labels_can_fall_back_to_an_unspecified_anchor() {
        let entries = vec![entry("Anchored chapter", "book.xhtml#chapter", vec![])];
        let href = PublicationUrl::parse("book.xhtml").unwrap();
        assert!(toc_entry_for_href(&entries, &href, TocMatch::ExactOrChapter).is_none());
        assert_eq!(
            toc_entry_for_href(&entries, &href, TocMatch::Section)
                .unwrap()
                .label,
            "Anchored chapter"
        );
    }
}
