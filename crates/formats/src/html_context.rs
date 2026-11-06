//! Publication relationships shared by every HTML-producing reflow source.
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Mutex;

use image::ImageReader;
use rebook_html::SectionParseHints;
use rebook_publication::{
    Block, Inline, LinkRole, NOTE_SECTION_PROPERTY, PublicationUrl, Section, SpineItem,
    TextBaseline, TextBlock, TocEntry,
};
use roxmltree::{Document, Node};

/// Only inspect the raster header; callers run while parsing a section.
pub(crate) fn image_dimensions(bytes: &[u8]) -> Option<[u32; 2]> {
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    Some([width, height])
}

/// Container-independent navigation and note hints, computed once per book.
#[derive(Debug, Default)]
pub struct HtmlContext {
    pub(crate) navigation_documents: Vec<PublicationUrl>,
    ancestors: HashMap<String, Vec<PublicationUrl>>,
    headings: HashMap<String, Vec<PublicationUrl>>,
    note_sections: HashSet<String>,
    separator_images: Mutex<HashMap<String, bool>>,
    font_metadata: crate::font_metadata::FontMetadataCache,
}

impl HtmlContext {
    pub(crate) fn font_traits<B: AsRef<[u8]>>(&self, href: &PublicationUrl, load: impl FnOnce() -> Option<B>) -> Option<rebook_html::FontTraits> {
        self.font_metadata.get(href, load)
    }
    pub fn new(toc: &[TocEntry], navigation_documents: Vec<PublicationUrl>) -> Self {
        let mut context = Self {
            navigation_documents,
            note_sections: collect_note_section_paths(toc),
            ..Self::default()
        };
        fn visit(
            entries: &[TocEntry],
            ancestors: &mut Vec<PublicationUrl>,
            context: &mut HtmlContext,
        ) {
            for entry in entries {
                if let Some(href) = &entry.href {
                    context
                        .headings
                        .entry(href.path().to_owned())
                        .or_default()
                        .push(href.clone());
                    let targets = context.ancestors.entry(href.path().to_owned()).or_default();
                    for ancestor in ancestors.iter() {
                        if !targets.contains(ancestor) {
                            targets.push(ancestor.clone());
                        }
                    }
                    ancestors.push(href.clone());
                }
                visit(&entry.children, ancestors, context);
                if entry.href.is_some() {
                    ancestors.pop();
                }
            }
        }
        visit(toc, &mut Vec::new(), &mut context);
        context
    }

    pub fn hints<'a>(&'a self, descriptor: &SpineItem) -> SectionParseHints<'a> {
        SectionParseHints {
            ignore_body_margins: false,
            note_section: descriptor.is_note_section()
                || self.note_sections.contains(descriptor.href.path()),
            navigation_documents: &self.navigation_documents,
            ancestor_targets: self
                .ancestors
                .get(descriptor.href.path())
                .map_or(&[], Vec::as_slice),
            heading_targets: self
                .headings
                .get(descriptor.href.path())
                .map_or(&[], Vec::as_slice),
        }
    }

    pub fn mark_note_sections(&self, sections: &mut [SpineItem]) {
        for section in sections {
            if self.note_sections.contains(section.href.path()) && !section.is_note_section() {
                section.properties.push(NOTE_SECTION_PROPERTY.to_owned());
            }
        }
    }

    /// Legacy EPUB markers can use small, top-aligned links rather than <sup>.
    /// The publication's Notes destination supplies the missing semantic evidence.
    pub(crate) fn mark_note_references(&self, section: &mut Section) {
        fn text(block: &mut TextBlock, notes: &HashSet<String>) {
            for inline in &mut block.content {
                let Inline::Text(run) = inline else { continue };
                let Some(target) = &run.link else { continue };
                let marker = run
                    .text
                    .trim()
                    .trim_matches(['[', ']', '(', ')', '（', '）', '【', '】', '.']);
                if run.style.link_role == LinkRole::Normal
                    && target.fragment().is_some()
                    && notes.contains(target.path())
                    && (run.style.size_scale <= 0.85
                        || run.style.baseline == TextBaseline::Superscript)
                    && !marker.is_empty()
                    && marker.chars().count() <= 8
                    && marker
                        .chars()
                        .all(|ch| ch.is_ascii_digit() || matches!(ch, '*' | '†' | '‡' | '§'))
                {
                    run.style.link_role = LinkRole::FootnoteReference;
                }
            }
        }
        fn visit(block: &mut Block, notes: &HashSet<String>) {
            match block {
                Block::Text(t) => text(t, notes),
                Block::Quote(q) => q
                    .body
                    .iter_mut()
                    .chain(q.attribution.iter_mut())
                    .for_each(|t| text(t, notes)),
                Block::Table(t) => t.text_blocks_mut().for_each(|t| text(t, notes)),
                Block::Figure(f) => f.text_blocks_mut().for_each(|t| text(t, notes)),
                Block::Note(n) => n.blocks.iter_mut().for_each(|b| visit(b, notes)),
                _ => {}
            }
        }
        if !self.note_sections.is_empty() {
            section
                .blocks
                .iter_mut()
                .for_each(|b| visit(b, &self.note_sections));
        }
    }

    /// Reads intrinsic metadata once; neither full pixel decoding nor UI work is needed.
    pub fn is_separator_image<B: AsRef<[u8]>>(
        &self,
        href: &PublicationUrl,
        load: impl FnOnce() -> Option<B>,
    ) -> bool {
        let key = href.resource_url().to_string();
        if let Some(classified) = self
            .separator_images
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).copied())
        {
            return classified;
        }
        let classified = load()
            .and_then(|bytes| {
                ImageReader::new(Cursor::new(bytes))
                    .with_guessed_format()
                    .ok()?
                    .into_dimensions()
                    .ok()
            })
            .is_some_and(|(width, height)| {
                (1..=8).contains(&height)
                    && (32..=512).contains(&width)
                    && width >= height.saturating_mul(8)
            });
        if let Ok(mut cache) = self.separator_images.lock() {
            cache.insert(key, classified);
        }
        classified
    }
}

pub(crate) fn attribute_local<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value())
}

pub(crate) fn collect_note_section_paths(entries: &[TocEntry]) -> HashSet<String> {
    fn visit(entries: &[TocEntry], classifications: &mut HashMap<String, (bool, bool)>) {
        for entry in entries {
            if let Some(href) = &entry.href {
                let classification = classifications.entry(href.path().to_owned()).or_default();
                if is_note_navigation_label(&entry.label) {
                    classification.0 = true;
                } else {
                    classification.1 = true;
                }
            }
            visit(&entry.children, classifications);
        }
    }
    let mut classifications = HashMap::new();
    visit(entries, &mut classifications);
    classifications
        .into_iter()
        .filter_map(|(path, (notes, other))| (notes && !other).then_some(path))
        .collect()
}

pub(crate) fn is_note_navigation_label(label: &str) -> bool {
    let label = label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches([':', '：'])
        .to_ascii_lowercase();
    matches!(
        label.as_str(),
        "note"
            | "notes"
            | "endnote"
            | "endnotes"
            | "footnote"
            | "footnotes"
            | "注释"
            | "注解"
            | "尾注"
            | "本章注"
            | "章节注释"
            | "作者附注"
    )
}

/// Only explicit authored cover relationships qualify; ordinary opening chapters do not.
pub(crate) fn cover_page(xml: &str, base: &PublicationUrl) -> Option<PublicationUrl> {
    let document = Document::parse(xml).ok()?;
    for node in document.descendants().filter(Node::is_element) {
        if node.has_tag_name("reference")
            && attribute_local(node, "type") == Some("cover")
            && node.ancestors().any(|node| node.has_tag_name("guide"))
        {
            if let Some(href) =
                attribute_local(node, "href").and_then(|href| base.resolve(href).ok())
            {
                return Some(href.resource_url());
            }
        }
        if [attribute_local(node, "type"), attribute_local(node, "role")]
            .into_iter()
            .flatten()
            .flat_map(str::split_whitespace)
            .any(|kind| matches!(kind, "cover" | "doc-cover"))
        {
            return Some(base.resource_url());
        }
    }
    None
}

pub(crate) fn navigation_documents(xml: &str, base: &PublicationUrl) -> Vec<PublicationUrl> {
    let Ok(document) = Document::parse(xml) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for node in document.descendants().filter(Node::is_element) {
        if node.has_tag_name("reference")
            && attribute_local(node, "type") == Some("toc")
            && node.ancestors().any(|node| node.has_tag_name("guide"))
        {
            if let Some(target) =
                attribute_local(node, "href").and_then(|href| base.resolve(href).ok())
            {
                targets.push(target.resource_url());
            }
        }
        if node.has_tag_name("nav")
            && [attribute_local(node, "type"), attribute_local(node, "role")]
                .into_iter()
                .flatten()
                .flat_map(str::split_whitespace)
                .any(|kind| matches!(kind, "toc" | "doc-toc"))
        {
            targets.push(base.resource_url());
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_linked_markers_require_a_known_notes_destination() {
        let context = HtmlContext::new(
            &[TocEntry {
                label: "Notes".into(),
                href: Some(PublicationUrl::parse("notes.xhtml").unwrap()),
                children: Vec::new(),
            }],
            Vec::new(),
        );
        let descriptor = SpineItem {
            id: rebook_publication::SpineItemId::new("chapter").unwrap(),
            href: PublicationUrl::parse("chapter.xhtml").unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        };
        let mut section = rebook_html::parse_section(
            r##"<html><head><style>.marker {font-size:0.65em;vertical-align:top}</style></head><body><p>Claim<a class="marker" href="notes.xhtml#n3">3</a>. An index<a class="marker" href="index.xhtml#n4">4</a>. A chapter<a href="notes.xhtml#n5">5</a>. A navigation link<a class="marker" href="notes.xhtml">6</a>. A name<a class="marker" href="notes.xhtml#n7">Notes</a>.</p></body></html>"##,
            &descriptor,
            |_| None,
        )
        .unwrap();
        let Block::Text(original) = &section.blocks[0] else {
            panic!()
        };
        assert!(original.content.iter().all(|inline| !matches!(inline,
            Inline::Text(run) if run.style.link_role == LinkRole::FootnoteReference)));
        let sources = section.anchors.clone();
        context.mark_note_references(&mut section);
        let Block::Text(text) = &section.blocks[0] else {
            panic!()
        };
        let marked: Vec<_> = text
            .content
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(run) if run.style.link_role == LinkRole::FootnoteReference => {
                    Some(run.text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(marked, ["3"]);
        assert_eq!(section.anchors, sources);
    }

    #[test]
    fn ambiguous_note_targets_and_subsection_labels_do_not_classify_whole_chapters() {
        let entries = [TocEntry {
            label: "Chapter".into(),
            href: Some(PublicationUrl::parse("chapter.xhtml").unwrap()),
            children: vec![TocEntry {
                label: "Notes".into(),
                href: Some(PublicationUrl::parse("chapter.xhtml#notes").unwrap()),
                children: Vec::new(),
            }],
        }];
        assert!(collect_note_section_paths(&entries).is_empty());
    }

    #[test]
    fn separator_metadata_and_failure_results_are_cached_without_pixel_decoding() {
        let context = HtmlContext::default();
        let rule = PublicationUrl::parse("rule.png").unwrap();
        assert!(context.is_separator_image(&rule, || Some(crate::reflow_format_tests::png(96, 3))));
        assert!(context.is_separator_image(&rule, || -> Option<Vec<u8>> {
            panic!("metadata was already cached")
        }));
        let missing = PublicationUrl::parse("missing.png").unwrap();
        assert!(!context.is_separator_image(&missing, || None::<Vec<u8>>));
        assert!(
            !context.is_separator_image(&missing, || -> Option<Vec<u8>> {
                panic!("failure was already cached")
            })
        );
        for (width, height) in [(96, 20), (1024, 3), (20, 3)] {
            let href = PublicationUrl::parse(&format!("{width}-{height}.png")).unwrap();
            assert!(
                !context.is_separator_image(&href, || Some(crate::reflow_format_tests::png(
                    width, height
                )))
            );
        }
    }
}
