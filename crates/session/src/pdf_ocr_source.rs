use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rebook_publication::{
    Block, Book, BookSource, PublicationError, PublicationUrl, RasterResource, RenditionLayout,
    Resource, Section, SpineItem, SpineItemId, TableOfContentsOrigin, TocEntry,
};

use crate::{PDF_PAGE_ANCHOR_PREFIX, PdfOcrPageRole, PdfOcrPageRoleAssignment};

/// One completed OCR page in provider-owned markup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdfOcrReflowPage {
    /// Provider-owned markup consumed by the configured markup engine.
    pub markup: String,
}

/// One lazily loaded resource referenced by OCR markup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfOcrReflowResource {
    /// Canonical publication URL used by the generated Reading IR.
    pub href: PublicationUrl,
    /// Local compatibility-store path containing the resource bytes.
    pub path: PathBuf,
    /// Persisted media type. Image extensions may refine generic values.
    pub media_type: String,
}

/// Completed OCR data needed to construct a reflowable publication view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdfOcrReflowDocument {
    /// OCR pages in physical PDF order.
    pub pages: Vec<PdfOcrReflowPage>,
    /// Resources referenced by the page markup.
    pub resources: Vec<PdfOcrReflowResource>,
    /// Physical pages that must retain their original page image.
    pub page_roles: Vec<PdfOcrPageRoleAssignment>,
}

/// Stable heading anchor requested while rendering one OCR page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfOcrTocAnchor {
    /// Fragment installed on the matching rendered heading.
    pub fragment: String,
    /// Navigation label used to select the matching heading.
    pub label: String,
}

/// Provider-markup boundary used by the shared OCR Reading IR source.
///
/// Implementations may use Markdown, HTML, or another cached representation.
/// The session crate owns publication structure and navigation but deliberately
/// does not depend on a provider parser.
pub trait PdfOcrMarkupEngine: Send + Sync {
    /// Returns normalized heading keys in document order.
    fn heading_keys(&self, markup: &str) -> Vec<String>;

    /// Renders safe XHTML body content and installs requested heading anchors.
    fn render_html(&self, markup: &str, anchors: &[PdfOcrTocAnchor]) -> String;
}

/// Reflowable Reading IR source constructed from completed OCR pages.
pub struct PdfOcrReflowBookSource {
    inner: Arc<dyn BookSource>,
    book: Book,
    pages: Vec<PdfOcrReflowPage>,
    page_ranges: Vec<Range<usize>>,
    page_targets: Vec<PublicationUrl>,
    toc_anchors: Vec<Vec<PdfOcrTocAnchor>>,
    page_roles: HashMap<usize, PdfOcrPageRole>,
    resources: HashMap<String, PdfOcrReflowResource>,
    markup_engine: Arc<dyn PdfOcrMarkupEngine>,
}

struct ContinuousReflowSections {
    page_ranges: Vec<Range<usize>>,
    page_targets: Vec<PublicationUrl>,
    toc_anchors: Vec<Vec<PdfOcrTocAnchor>>,
}

impl PdfOcrReflowBookSource {
    /// Builds a reflowable publication without changing the canonical PDF.
    pub fn new(
        inner: Arc<dyn BookSource>,
        document: PdfOcrReflowDocument,
        continuous_reflow: bool,
        markup_engine: Arc<dyn PdfOcrMarkupEngine>,
    ) -> Result<Self, PublicationError> {
        let mut book = inner.book().clone();
        book.metadata.layout = RenditionLayout::Reflowable;
        let resources = document
            .resources
            .into_iter()
            .map(|resource| (resource.href.resource_url().path().to_owned(), resource))
            .collect();
        let page_roles = document
            .page_roles
            .into_iter()
            .filter_map(|assignment| {
                assignment
                    .physical_page
                    .checked_sub(1)
                    .map(|index| (index, assignment.role))
            })
            .collect::<HashMap<_, _>>();
        let pages = document.pages;
        let reflow_sections = if continuous_reflow {
            build_continuous_reflow_sections(
                &mut book,
                &pages,
                inner.table_of_contents_origin(),
                markup_engine.as_ref(),
            )?
        } else {
            ContinuousReflowSections {
                page_ranges: (0..pages.len()).map(|index| index..index + 1).collect(),
                page_targets: book
                    .sections
                    .iter()
                    .map(|section| section.href.clone())
                    .collect(),
                toc_anchors: vec![Vec::new(); pages.len()],
            }
        };
        Ok(Self {
            inner,
            book,
            pages,
            page_ranges: reflow_sections.page_ranges,
            page_targets: reflow_sections.page_targets,
            toc_anchors: reflow_sections.toc_anchors,
            page_roles,
            resources,
            markup_engine,
        })
    }

    /// Physical-page targets used when entering OCR reflow mode.
    #[must_use]
    pub fn page_targets(&self) -> &[PublicationUrl] {
        &self.page_targets
    }

    /// Physical-page ranges grouped into generated reflow sections.
    #[must_use]
    pub fn page_ranges(&self) -> &[Range<usize>] {
        &self.page_ranges
    }

    /// Returns the cached provider markup for one physical page.
    #[must_use]
    pub fn page_markup(&self, index: usize) -> Option<&str> {
        self.pages.get(index).map(|page| page.markup.as_str())
    }
}

fn build_continuous_reflow_sections(
    book: &mut Book,
    pages: &[PdfOcrReflowPage],
    toc_origin: TableOfContentsOrigin,
    markup_engine: &dyn PdfOcrMarkupEngine,
) -> Result<ContinuousReflowSections, PublicationError> {
    let page_count = pages.len();
    let original_sections = book.sections.clone();
    let page_indices = original_sections
        .iter()
        .enumerate()
        .map(|(index, section)| (section.href.path().to_owned(), index))
        .collect::<HashMap<_, _>>();
    let mut starts = vec![0];
    if toc_origin != TableOfContentsOrigin::Fallback {
        starts.extend(
            book.table_of_contents
                .iter()
                .filter_map(|entry| first_toc_page(entry, &page_indices)),
        );
    }
    starts.retain(|start| *start < page_count);
    starts.sort_unstable();
    starts.dedup();
    if starts.is_empty() {
        starts.push(0);
    }

    let mut page_ranges = Vec::with_capacity(starts.len());
    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(page_count);
        if start < end || (page_count == 0 && page_ranges.is_empty()) {
            page_ranges.push(start..end);
        }
    }
    if page_ranges.is_empty() {
        page_ranges.push(0..page_count);
    }

    let mut page_targets = Vec::with_capacity(page_count);
    let mut sections = Vec::with_capacity(page_ranges.len());
    for (index, range) in page_ranges.iter().enumerate() {
        let href = PublicationUrl::parse(&format!("Text/ocr-reflow-{}.xhtml", index + 1))?;
        sections.push(SpineItem {
            id: SpineItemId::new(format!("pdf-ocr-reflow-{}", index + 1))?,
            href: href.clone(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        });
        for page_index in range.clone() {
            page_targets
                .push(href.resolve(&format!("#{PDF_PAGE_ANCHOR_PREFIX}{}", page_index + 1))?);
        }
    }
    let mut toc_anchors = vec![Vec::new(); page_count];
    let heading_keys = pages
        .iter()
        .map(|page| markup_engine.heading_keys(&page.markup))
        .collect::<Vec<_>>();
    let mut next_anchor = 0;
    remap_reflow_toc(
        &mut book.table_of_contents,
        &page_indices,
        &page_targets,
        &heading_keys,
        &mut toc_anchors,
        &mut next_anchor,
    )?;
    book.sections = sections;
    Ok(ContinuousReflowSections {
        page_ranges,
        page_targets,
        toc_anchors,
    })
}

fn first_toc_page(entry: &TocEntry, page_indices: &HashMap<String, usize>) -> Option<usize> {
    entry
        .href
        .as_ref()
        .and_then(|href| page_indices.get(href.path()))
        .copied()
        .or_else(|| {
            entry
                .children
                .iter()
                .filter_map(|child| first_toc_page(child, page_indices))
                .min()
        })
}

fn remap_reflow_toc(
    entries: &mut [TocEntry],
    page_indices: &HashMap<String, usize>,
    page_targets: &[PublicationUrl],
    heading_keys: &[Vec<String>],
    toc_anchors: &mut [Vec<PdfOcrTocAnchor>],
    next_anchor: &mut usize,
) -> Result<(), PublicationError> {
    for entry in entries {
        if let Some(page_index) = entry
            .href
            .as_ref()
            .and_then(|href| page_indices.get(href.path()))
            .copied()
            && let Some(page_target) = page_targets.get(page_index)
        {
            let descendant_labels = toc_descendant_heading_keys(&entry.children);
            let first_child_page = entry
                .children
                .first()
                .and_then(|child| child.href.as_ref())
                .and_then(|href| page_indices.get(href.path()))
                .copied();
            let heading_page = find_matching_heading_page(
                heading_keys,
                page_index,
                &entry.label,
                &descendant_labels,
                &[],
                first_child_page,
            );
            let target = if let Some(heading_page) = heading_page {
                let fragment = format!("ocr-toc-{next_anchor}");
                *next_anchor += 1;
                toc_anchors[heading_page].push(PdfOcrTocAnchor {
                    fragment: fragment.clone(),
                    label: entry.label.clone(),
                });
                page_targets[heading_page]
                    .resource_url()
                    .resolve(&format!("#{fragment}"))?
            } else {
                page_target.clone()
            };
            entry.href = Some(target);
        }
        remap_reflow_toc(
            &mut entry.children,
            page_indices,
            page_targets,
            heading_keys,
            toc_anchors,
            next_anchor,
        )?;
    }
    Ok(())
}

fn toc_descendant_heading_keys(entries: &[TocEntry]) -> Vec<String> {
    let mut labels = Vec::new();
    for entry in entries {
        let label = normalize_toc_heading(&entry.label);
        if !label.is_empty() {
            labels.push(label);
        }
        labels.extend(toc_descendant_heading_keys(&entry.children));
    }
    labels
}

fn find_matching_heading_page(
    heading_keys: &[Vec<String>],
    preferred_page: usize,
    label: &str,
    descendant_labels: &[String],
    excluded_pages: &[bool],
    maximum_page: Option<usize>,
) -> Option<usize> {
    const SEARCH_RADIUS: usize = 12;

    if heading_keys.is_empty() || preferred_page >= heading_keys.len() {
        return None;
    }
    let label = normalize_toc_heading(label);
    if label.is_empty() {
        return None;
    }
    let start = preferred_page.saturating_sub(SEARCH_RADIUS);
    let end = preferred_page
        .saturating_add(SEARCH_RADIUS)
        .min(heading_keys.len() - 1);
    (start..=end)
        .filter(|page| !excluded_pages.get(*page).copied().unwrap_or(false))
        .filter(|page| maximum_page.is_none_or(|maximum| *page <= maximum))
        .filter(|page| {
            heading_keys[*page]
                .iter()
                .any(|heading| normalized_toc_headings_match(heading, &label))
        })
        .min_by_key(|page| {
            let descendant_matches = descendant_labels
                .iter()
                .filter(|descendant| {
                    heading_keys[*page]
                        .iter()
                        .any(|heading| normalized_toc_headings_match(heading, descendant))
                })
                .count();
            (
                descendant_matches,
                page.abs_diff(preferred_page),
                usize::from(*page < preferred_page),
            )
        })
}

fn normalized_toc_headings_match(heading: &str, label: &str) -> bool {
    heading == label
        || (heading.chars().count().min(label.chars().count()) >= 4
            && (heading.contains(label) || label.contains(heading)))
}

fn normalize_toc_heading(value: &str) -> String {
    let normalized = value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    let prefix_bytes = normalized
        .char_indices()
        .take_while(|(_, character)| {
            character.is_ascii_digit() || ('０'..='９').contains(character)
        })
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    let without_numeric_prefix = &normalized[prefix_bytes..];
    if prefix_bytes > 0 && !without_numeric_prefix.is_empty() {
        without_numeric_prefix.to_owned()
    } else {
        normalized
    }
}

impl BookSource for PdfOcrReflowBookSource {
    fn book(&self) -> &Book {
        &self.book
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.inner.table_of_contents_origin()
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let descriptor = self
            .book
            .sections
            .get(index)
            .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))?;
        let range = self
            .page_ranges
            .get(index)
            .ok_or_else(|| PublicationError::ResourceNotFound(format!("section {index}")))?;
        let mut body = String::new();
        for page_index in range.clone() {
            let _ = write!(
                body,
                r#"<div id="{PDF_PAGE_ANCHOR_PREFIX}{}"></div>"#,
                page_index + 1
            );
            if self.page_roles.contains_key(&page_index) {
                if let Ok(page) = self.inner.parse_section(page_index)
                    && let Some(image) = page.blocks.iter().find_map(original_page_image)
                {
                    let _ = write!(
                        body,
                        r#"<img src="../{}" alt="PDF page {}" style="display:block;max-width:100%;max-height:100%;margin:0 auto" />"#,
                        escape_xml_attribute(image.href.path()),
                        page_index + 1
                    );
                }
                body.push_str("<br />");
                continue;
            }
            let markup = self
                .pages
                .get(page_index)
                .map_or("", |page| page.markup.as_str());
            if !markup.trim().is_empty() {
                body.push_str(&self.markup_engine.render_html(
                    markup,
                    self.toc_anchors.get(page_index).map_or(&[], Vec::as_slice),
                ));
            }
        }
        let document = format!(
            "<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title></title><style>h1 {{ font-size: 1.75em; margin-top: 32px; margin-bottom: 12px; }} h2 {{ font-size: 1.5em; margin-top: 28px; margin-bottom: 10px; }} h3 {{ font-size: 1.28em; margin-top: 22px; margin-bottom: 8px; }} h4, h5, h6 {{ font-size: 1.12em; margin-top: 18px; margin-bottom: 6px; }}</style></head><body>{body}</body></html>"
        );
        rebook_html::parse_section(&document, descriptor, |_| None)
            .map_err(|error| PublicationError::InvalidPublication(error.to_string()))
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        let resource_url = href.resource_url();
        let path = resource_url.path();
        if let Some(resource) = self.resources.get(path) {
            let bytes = fs::read(&resource.path)
                .map_err(|_| PublicationError::ResourceNotFound(href.to_string()))?;
            let media_type = if resource.media_type.starts_with("image/") {
                resource.media_type.clone()
            } else {
                image_media_type(path).map_or_else(|| resource.media_type.clone(), str::to_owned)
            };
            return Ok(Resource {
                href: resource_url,
                media_type,
                bytes: bytes.into(),
            });
        }
        self.inner.resource(href)
    }

    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        if self.resources.contains_key(href.resource_url().path()) {
            return Ok(None);
        }
        self.inner.raster_resource(href)
    }
}

fn original_page_image(block: &Block) -> Option<&rebook_publication::ImageBlock> {
    match block {
        Block::Image(image) => Some(image),
        Block::Figure(figure) => figure.images.first(),
        Block::Text(_)
        | Block::Quote(_)
        | Block::Table(_)
        | Block::Separator
        | Block::LineBreak
        | Block::PageBreak => None,
    }
}

fn image_media_type(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("webp") => Some("image/webp"),
        Some("gif") => Some("image/gif"),
        Some("bmp") => Some("image/bmp"),
        _ => None,
    }
}

fn escape_xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use rebook_publication::{Metadata, PublicationId, SectionAnchor, TextBlockKind};

    use super::*;

    struct StubSource {
        book: Book,
    }

    impl BookSource for StubSource {
        fn book(&self) -> &Book {
            &self.book
        }

        fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
            let descriptor = self
                .book
                .sections
                .get(index)
                .ok_or_else(|| PublicationError::ResourceNotFound(index.to_string()))?;
            Ok(Section {
                id: descriptor.id.clone(),
                href: descriptor.href.clone(),
                blocks: Vec::new(),
                anchors: Vec::new(),
            })
        }

        fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
            Err(PublicationError::ResourceNotFound(href.to_string()))
        }
    }

    struct TestMarkupEngine;

    impl PdfOcrMarkupEngine for TestMarkupEngine {
        fn heading_keys(&self, markup: &str) -> Vec<String> {
            markup
                .lines()
                .filter_map(|line| line.strip_prefix("## "))
                .map(normalize_toc_heading)
                .collect()
        }

        fn render_html(&self, markup: &str, anchors: &[PdfOcrTocAnchor]) -> String {
            let mut output = String::new();
            for line in markup.lines().filter(|line| !line.is_empty()) {
                if let Some(heading) = line.strip_prefix("## ") {
                    let anchor = anchors.iter().find(|anchor| {
                        normalize_toc_heading(&anchor.label) == normalize_toc_heading(heading)
                    });
                    if let Some(anchor) = anchor {
                        let _ = write!(output, "<h2 id=\"{}\">{heading}</h2>", anchor.fragment);
                    } else {
                        let _ = write!(output, "<h2>{heading}</h2>");
                    }
                } else {
                    let _ = write!(output, "<p>{line}</p>");
                }
            }
            output
        }
    }

    fn page(number: usize) -> SpineItem {
        SpineItem {
            id: SpineItemId::new(format!("page-{number}")).unwrap(),
            href: PublicationUrl::parse(&format!("Text/page-{number}.xhtml")).unwrap(),
            media_type: "application/xhtml+xml".into(),
            linear: true,
            properties: Vec::new(),
        }
    }

    fn source() -> Arc<dyn BookSource> {
        Arc::new(StubSource {
            book: Book {
                id: PublicationId::new("ocr-source-test").unwrap(),
                metadata: Metadata {
                    layout: RenditionLayout::PrePaginated,
                    ..Metadata::default()
                },
                cover: None,
                sections: vec![page(1), page(2), page(3)],
                table_of_contents: vec![
                    TocEntry {
                        label: "Chapter 1".into(),
                        href: Some(PublicationUrl::parse("Text/page-1.xhtml").unwrap()),
                        children: vec![TocEntry {
                            label: "Section 1.1".into(),
                            href: Some(PublicationUrl::parse("Text/page-2.xhtml").unwrap()),
                            children: Vec::new(),
                        }],
                    },
                    TocEntry {
                        label: "Chapter 2".into(),
                        href: Some(PublicationUrl::parse("Text/page-3.xhtml").unwrap()),
                        children: Vec::new(),
                    },
                ],
            },
        })
    }

    #[test]
    fn builds_reflow_sections_and_stable_physical_page_targets() {
        let reflow = PdfOcrReflowBookSource::new(
            source(),
            PdfOcrReflowDocument {
                pages: ["First page.", "## Section 1.1\nSecond page.", "Third page."]
                    .into_iter()
                    .map(|markup| PdfOcrReflowPage {
                        markup: markup.into(),
                    })
                    .collect(),
                ..PdfOcrReflowDocument::default()
            },
            true,
            Arc::new(TestMarkupEngine),
        )
        .unwrap();

        assert_eq!(reflow.book().metadata.layout, RenditionLayout::Reflowable);
        assert_eq!(reflow.page_ranges(), &[0..2, 2..3]);
        assert_eq!(
            reflow
                .page_targets()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "Text/ocr-reflow-1.xhtml#pdf-page-1",
                "Text/ocr-reflow-1.xhtml#pdf-page-2",
                "Text/ocr-reflow-2.xhtml#pdf-page-3",
            ]
        );
    }

    #[test]
    fn generated_section_contains_reading_ir_and_exact_navigation_anchors() {
        let reflow = PdfOcrReflowBookSource::new(
            source(),
            PdfOcrReflowDocument {
                pages: ["First page.", "## Section 1.1\nSecond page.", "Third page."]
                    .into_iter()
                    .map(|markup| PdfOcrReflowPage {
                        markup: markup.into(),
                    })
                    .collect(),
                ..PdfOcrReflowDocument::default()
            },
            true,
            Arc::new(TestMarkupEngine),
        )
        .unwrap();

        let section = reflow.parse_section(0).unwrap();
        assert!(section.blocks.iter().any(|block| {
            matches!(block, Block::Text(text) if text.kind == TextBlockKind::Paragraph)
        }));
        assert_eq!(
            section
                .anchors
                .iter()
                .map(|SectionAnchor { fragment, .. }| fragment.as_str())
                .collect::<Vec<_>>(),
            vec!["pdf-page-1", "pdf-page-2", "ocr-toc-0"]
        );
        assert_eq!(
            reflow.book().table_of_contents[0].children[0]
                .href
                .as_ref()
                .and_then(PublicationUrl::fragment),
            Some("ocr-toc-0")
        );
    }
}
