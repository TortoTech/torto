//! Recognize unlisted pull-quote spreads that repeat their preceding TOC unit.
use crate::html_context::attribute_local;
use rebook_publication::{PublicationUrl, SpineItem, TocEntry};
use roxmltree::{Document, Node};
use std::collections::HashMap;

pub(crate) fn mark_quote_continuations(
    sections: &mut [SpineItem],
    toc: &[TocEntry],
    mut read_document: impl FnMut(&PublicationUrl) -> Option<String>,
) {
    fn targets(entries: &[TocEntry], result: &mut HashMap<String, Vec<PublicationUrl>>) {
        for entry in entries {
            if let Some(href) = &entry.href {
                result
                    .entry(href.path().to_owned())
                    .or_default()
                    .push(href.clone());
            }
            targets(&entry.children, result);
        }
    }
    let mut mapped = HashMap::new();
    targets(toc, &mut mapped);
    for index in 1..sections.len() {
        let previous = &sections[index - 1];
        let current = &sections[index];
        if !current.linear
            || !previous.linear
            || current.is_note_section()
            || previous.is_note_section()
            || mapped.contains_key(current.href.path())
            || !matches!(
                current.media_type.as_str(),
                "application/xhtml+xml" | "text/html"
            )
        {
            continue;
        }
        let Some(previous_targets) = mapped.get(previous.href.path()) else {
            continue;
        };
        // Read only unmapped candidates adjacent to a mapped unit, and only
        // read the previous document after the candidate proves quote-only.
        let Some(xml) = read_document(&current.href) else {
            continue;
        };
        let Ok(document) = Document::parse(&xml) else {
            continue;
        };
        let heading = heading_only(&document);
        let quote = quote_spread_text(&document);
        if heading.is_none() && quote.is_none() {
            continue;
        }
        let Some(previous_xml) = read_document(&previous.href) else {
            continue;
        };
        let Ok(previous_document) = Document::parse(&previous_xml) else {
            continue;
        };
        let split_heading = heading.as_ref().is_some_and(|(level, title)| {
            rebook_publication::heading_ordinal_key(title).is_none()
                && heading_only(&previous_document).is_some_and(|(previous_level, ordinal)| {
                    previous_level == *level
                        && rebook_publication::heading_ordinal_key(&ordinal).is_some()
                })
                && document
                    .descendants()
                    .find(|node| node.has_tag_name("body"))
                    .and_then(|node| attribute_local(node, "id"))
                    .is_some_and(|id| {
                        previous_document
                            .descendants()
                            .find(|node| node.has_tag_name("body"))
                            .and_then(|node| attribute_local(node, "id"))
                            == Some(id)
                    })
        });
        if (split_heading
            || quote.as_ref().is_some_and(|quote| {
                repeats_last_unit(quote, &previous_document, previous_targets)
            }))
            && !current
                .properties
                .iter()
                .any(|property| property == rebook_publication::CONTINUATION_SECTION_PROPERTY)
        {
            sections[index]
                .properties
                .push(rebook_publication::CONTINUATION_SECTION_PROPERTY.to_owned());
        }
    }
}

// Conversion tools sometimes cut one title page across two spine documents.
// Require heading-only pages and a shared authored body ID, so separate short
// chapters or independently numbered subsections cannot be joined by accident.
fn heading_only(document: &Document<'_>) -> Option<(String, String)> {
    let body = document
        .descendants()
        .find(|node| node.has_tag_name("body"))?;
    let headings: Vec<_> = body
        .descendants()
        .filter(|node| {
            node.is_element()
                && matches!(
                    node.tag_name().name(),
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                )
        })
        .collect();
    let [heading] = headings.as_slice() else {
        return None;
    };
    if body.descendants().any(|node| {
        node.is_element()
            && matches!(
                node.tag_name().name(),
                "img" | "image" | "svg" | "table" | "nav"
            )
    }) || body.descendants().filter(Node::is_text).any(|node| {
        !node.text().unwrap_or_default().trim().is_empty()
            && !node.ancestors().any(|ancestor| ancestor == *heading)
    }) {
        return None;
    }
    let text: String = heading
        .descendants()
        .filter_map(|node| node.text().filter(|_| node.is_text()))
        .collect();
    (!text.trim().is_empty() && text.chars().count() <= 120)
        .then(|| (heading.tag_name().name().to_owned(), text.trim().to_owned()))
}

fn normalized_text(nodes: impl Iterator<Item = char>) -> String {
    nodes
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn quote_marker(node: Node<'_, '_>) -> bool {
    node.has_tag_name("blockquote")
        || [attribute_local(node, "type"), attribute_local(node, "role")]
            .into_iter()
            .flatten()
            .flat_map(str::split_whitespace)
            .any(|kind| matches!(kind, "pullquote" | "doc-pullquote"))
}

fn quote_spread_text(document: &Document<'_>) -> Option<String> {
    let body = document
        .descendants()
        .find(|node| node.has_tag_name("body"))?;
    if body.descendants().filter(Node::is_element).any(|node| {
        matches!(
            node.tag_name().name(),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "nav" | "table" | "ul" | "ol" | "dl" | "pre"
        ) || [attribute_local(node, "type"), attribute_local(node, "role")]
            .into_iter()
            .flatten()
            .flat_map(str::split_whitespace)
            .any(|kind| {
                matches!(
                    kind,
                    "epigraph"
                        | "doc-epigraph"
                        | "part"
                        | "dedication"
                        | "titlepage"
                        | "footnote"
                        | "endnote"
                        | "endnotes"
                        | "doc-footnote"
                        | "doc-endnotes"
                )
            })
    }) {
        return None;
    }
    let mut text = String::new();
    let mut explicit = true;
    for node in body.descendants().filter(Node::is_text) {
        if node
            .ancestors()
            .any(|parent| matches!(parent.tag_name().name(), "script" | "style"))
        {
            continue;
        }
        let value = node.text().unwrap_or_default();
        if value.trim().is_empty() {
            continue;
        }
        let marked = node
            .ancestors()
            .take_while(|parent| *parent != body)
            .any(quote_marker);
        if !marked
            && !node
                .ancestors()
                .take_while(|parent| *parent != body)
                .any(|parent| parent.has_tag_name("p"))
        {
            return None;
        }
        explicit &= marked;
        text.push_str(value);
    }
    let text = normalized_text(text.chars());
    // Untagged editorial spreads need enough matching content to establish a
    // relationship. Whole-spread matching below rejects independent prose;
    // selector names and filenames never supply evidence.
    let media_spread = body.descendants().filter(|n| n.has_tag_name("p")).count() == 1
        && body
            .descendants()
            .filter(|n| n.has_tag_name("img") || n.has_tag_name("image"))
            .count()
            >= 2;
    (!text.is_empty()
        && (explicit || text.chars().count() >= 96 || (media_spread && text.chars().count() >= 48)))
        .then_some(text)
}

fn repeats_last_unit(quote: &str, document: &Document<'_>, targets: &[PublicationUrl]) -> bool {
    let Some(body) = document
        .descendants()
        .find(|node| node.has_tag_name("body"))
    else {
        return false;
    };
    let mut start = body.range().start;
    for target in targets {
        if let Some(fragment) = target.fragment() {
            let Some(anchor) = body.descendants().find(|node| {
                node.is_element()
                    && [attribute_local(*node, "id"), attribute_local(*node, "name")]
                        .contains(&Some(fragment))
            }) else {
                return false;
            };
            start = start.max(anchor.range().start);
        }
    }
    let text = normalized_text(
        body.descendants()
            .filter(Node::is_text)
            .filter(|node| node.range().start >= start)
            .filter_map(|node| node.text())
            .flat_map(str::chars),
    );
    text.contains(quote) || contains_edited_quote(&text, quote)
}

fn contains_edited_quote(text: &str, quote: &str) -> bool {
    // Pull quotes sometimes replace a short connective ("because" / ", as").
    // Require long identical anchors and permit just one small edited span.
    // Search a bounded window around each prefix, never a document-wide edit
    // distance matrix, and never search outside the preceding logical unit.
    let characters = quote.chars().collect::<Vec<_>>();
    if characters.len() < 96 {
        return false;
    }
    let budget = (characters.len() / 16).min(8);
    let prefix = characters[..32].iter().collect::<String>();
    let suffix = characters[characters.len() - 32..]
        .iter()
        .collect::<String>();
    text.match_indices(&prefix).any(|(start, _)| {
        let window = text[start..]
            .chars()
            .take(characters.len() + budget)
            .collect::<String>();
        window.match_indices(&suffix).any(|(end, _)| {
            let candidate = window[..end + suffix.len()].chars().collect::<Vec<_>>();
            let before = characters
                .iter()
                .zip(&candidate)
                .take_while(|(a, b)| a == b)
                .count();
            let after = characters[before..]
                .iter()
                .rev()
                .zip(candidate[before..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            characters.len() - before - after <= budget
                && candidate.len() - before - after <= budget
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_title_page_is_continuous_only_with_matching_body_and_heading_level() {
        use rebook_publication::SpineItemId;
        for (body_id, heading, expected) in [
            ("same", "h1", true),
            ("other", "h1", false),
            ("same", "h2", false),
        ] {
            let mut sections: Vec<_> = ["ordinal.xhtml", "title.xhtml"]
                .into_iter()
                .map(|path| SpineItem {
                    id: SpineItemId::new(path).unwrap(),
                    href: PublicationUrl::parse(path).unwrap(),
                    media_type: "application/xhtml+xml".into(),
                    linear: true,
                    properties: vec![],
                })
                .collect();
            let toc = vec![TocEntry {
                label: "Chapter 1: A title".into(),
                href: Some(sections[0].href.clone()),
                children: vec![],
            }];
            mark_quote_continuations(&mut sections, &toc, |href| {
                Some(if href.path() == "ordinal.xhtml" {
                    "<html><body id='same'><h1>1.</h1></body></html>".to_owned()
                } else {
                    format!(
                        "<html><body id='{body_id}'><{heading}>A title</{heading}></body></html>"
                    )
                })
            });
            assert_eq!(
                sections[1]
                    .properties
                    .iter()
                    .any(|property| property == rebook_publication::CONTINUATION_SECTION_PROPERTY),
                expected
            );
        }
    }

    #[test]
    fn quote_spreads_use_explicit_semantics_and_exclude_epigraphs_and_independent_content() {
        for xml in [
            "<html><body><img src='photo.jpg'/><p role='doc-pullquote'>An extracted statement.</p></body></html>",
            "<html xmlns:epub='http://www.idpf.org/2007/ops'><body><aside epub:type='pullquote'><p>An extracted statement.</p><img src='closing.png'/></aside></body></html>",
        ] {
            assert_eq!(
                quote_spread_text(&Document::parse(xml).unwrap()).as_deref(),
                Some("anextractedstatement.")
            );
        }
        for body in [
            "<p>An extracted statement.</p>",
            "<h2>New chapter</h2><p class='Big_Quote'>An extracted statement.</p>",
            "<blockquote role='doc-epigraph'>An extracted statement.</blockquote>",
            "<p class='Big_Quote'>An extracted statement.</p><p>New independent prose.</p>",
        ] {
            let xml = format!("<html><body>{body}</body></html>");
            assert!(quote_spread_text(&Document::parse(&xml).unwrap()).is_none());
        }
    }

    #[test]
    fn untagged_editorial_spreads_require_whole_text_matching_and_ignore_class_names() {
        let text = "A luxury brand would never sell a cheap product even if production costs decreased drastically, as it would communicate a lower brand value.";
        for class in ["Big_Quote", "x932", ""] {
            let xml = format!(
                "<html><body><img src='photo.jpg'/><div><p class='{class}'>{text}</p></div></body></html>"
            );
            let quote = quote_spread_text(&Document::parse(&xml).unwrap()).unwrap();
            let previous = format!(
                "<html><body><h2 id='current'>Current section</h2><p>{text}</p></body></html>"
            );
            let previous = Document::parse(&previous).unwrap();
            let targets = [PublicationUrl::parse("chapter.xhtml#current").unwrap()];
            assert!(repeats_last_unit(&quote, &previous, &targets));
            let independent = format!(
                "<html><body><p>{text}</p><p>Additional independent content.</p></body></html>"
            );
            let quote = quote_spread_text(&Document::parse(&independent).unwrap()).unwrap();
            assert!(!repeats_last_unit(&quote, &previous, &targets));
        }
    }

    #[test]
    fn repeated_quote_must_belong_to_last_logical_unit_not_an_earlier_subsection() {
        let xml = "<html><body><h2>First</h2><p>An earlier statement.</p><h2 id='second'>Second</h2><p>The later <i>statement</i>.</p></body></html>";
        let document = Document::parse(xml).unwrap();
        let targets = [
            PublicationUrl::parse("chapter.xhtml").unwrap(),
            PublicationUrl::parse("chapter.xhtml#second").unwrap(),
        ];
        assert!(!repeats_last_unit(
            "anearlierstatement.",
            &document,
            &targets
        ));
        assert!(repeats_last_unit("thelaterstatement.", &document, &targets));
        assert!(!repeats_last_unit("anewstatement.", &document, &targets));
    }

    #[test]
    fn edited_pull_quote_accepts_a_small_connective_change_but_not_a_rewrite() {
        let source = normalized_text("The biggest risks with DIY skincare are irritation, rashes, and infection from microbial spoilage because recipes rarely include effective preservatives.".chars());
        let quote = normalized_text("The biggest risks with DIY skincare are irritation, rashes, and infection from microbial spoilage, as recipes rarely include effective preservatives.".chars());
        assert!(contains_edited_quote(&source, &quote));
        let rewritten = quote.replace(
            "irritation,rashes,andinfection",
            "othercompletelydifferentrisks",
        );
        assert!(!contains_edited_quote(&source, &rewritten));
        assert!(!contains_edited_quote(
            "An unrelated short quote.",
            "Another short quote."
        ));
    }
}
