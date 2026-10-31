use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rebook_publication::{
    Book, BookSource, PublicationError, PublicationUrl, Resource, Section, TableOfContentsOrigin,
    TocEntry,
};
use serde::{Deserialize, Serialize};

use crate::persistence::{self, BookRecord, write_json_atomic};

const GENERATED_TOC_VERSION: u8 = 1;
const PAGE_MAPPING_REVISION: u8 = 2;
const GENERATED_TOC_DIRECTORY: &str = "generated-toc";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedTocEntry {
    pub depth: usize,
    pub title: String,
    pub printed_page: String,
    /// One-based physical PDF page.
    pub physical_page: usize,
    pub confidence: f32,
}

#[derive(Clone, Debug)]
pub struct GeneratedTocDraft {
    pub provider_name: String,
    pub model: String,
    pub source_pages: Vec<usize>,
    pub entries: Vec<GeneratedTocEntry>,
}

#[derive(Serialize, Deserialize)]
struct StoredGeneratedToc {
    version: u8,
    book_id: String,
    provider_name: String,
    model: String,
    source_pages: Vec<usize>,
    entries: Vec<GeneratedTocEntry>,
    #[serde(default)]
    verified_pages: bool,
    #[serde(default)]
    page_mapping_revision: u8,
}

impl BookRecord for StoredGeneratedToc {
    const VERSION: u8 = GENERATED_TOC_VERSION;
    const KIND: &'static str = "generated TOC";
    fn identity(&self) -> (u8, &str) {
        (self.version, &self.book_id)
    }
}

impl StoredGeneratedToc {
    fn from_draft(book_id: &str, draft: &GeneratedTocDraft) -> Self {
        Self {
            version: GENERATED_TOC_VERSION,
            book_id: book_id.to_owned(),
            provider_name: draft.provider_name.clone(),
            model: draft.model.clone(),
            source_pages: draft.source_pages.clone(),
            entries: normalize_entries(draft.entries.clone()),
            verified_pages: true,
            page_mapping_revision: PAGE_MAPPING_REVISION,
        }
    }
}

pub fn save(book_id: &str, draft: &GeneratedTocDraft) -> io::Result<()> {
    let path = generated_toc_path(book_id)?;
    write_json_atomic(&path, &StoredGeneratedToc::from_draft(book_id, draft))?;
    crate::sync::mark_derived_dirty(book_id, crate::sync::DerivedDataKind::Metadata)
}

pub fn export_sync_bytes(book_id: &str) -> io::Result<Option<Vec<u8>>> {
    persistence::export_book_record::<StoredGeneratedToc>(&generated_toc_path(book_id)?, book_id)
}

pub fn import_sync_bytes(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    persistence::import_book_record::<StoredGeneratedToc>(
        &generated_toc_path(book_id)?,
        book_id,
        bytes,
    )
}

pub fn validate_sync_bytes(book_id: &str, bytes: &[u8]) -> io::Result<()> {
    persistence::validate_book_record::<StoredGeneratedToc>(book_id, bytes)
}

pub fn load(book_id: &str) -> io::Result<Option<GeneratedTocDraft>> {
    let Some(stored) = persistence::load_book_record::<StoredGeneratedToc>(
        &generated_toc_path(book_id)?,
        book_id,
    )?
    else {
        return Ok(None);
    };
    if !stored.verified_pages || stored.page_mapping_revision != PAGE_MAPPING_REVISION {
        return Ok(None);
    }
    Ok(Some(GeneratedTocDraft {
        provider_name: stored.provider_name,
        model: stored.model,
        source_pages: stored.source_pages,
        entries: normalize_entries(stored.entries),
    }))
}

pub fn load_source(source: Arc<dyn BookSource>) -> io::Result<Arc<dyn BookSource>> {
    let book_id = source.book().id.to_string();
    let Some(mut draft) = load(&book_id)? else {
        return Ok(source);
    };
    if crate::plugins::correct_generated_toc_pages_from_ocr(&book_id, &mut draft.entries)? {
        save(&book_id, &draft)?;
    }
    let entries = draft.entries;
    if entries.is_empty()
        || entries.iter().any(|entry| {
            entry.physical_page == 0 || entry.physical_page > source.book().sections.len()
        })
    {
        return Ok(source);
    }
    Ok(Arc::new(GeneratedTocBookSource::new(source, &entries)))
}

fn generated_toc_path(book_id: &str) -> io::Result<PathBuf> {
    persistence::book_data_path(GENERATED_TOC_DIRECTORY, book_id, "json")
}

fn normalize_entries(mut entries: Vec<GeneratedTocEntry>) -> Vec<GeneratedTocEntry> {
    entries.retain(|entry| !entry.title.trim().is_empty() && entry.physical_page > 0);
    for entry in &mut entries {
        entry.title = entry.title.trim().to_owned();
        entry.printed_page = entry.printed_page.trim().to_owned();
        entry.confidence = entry.confidence.clamp(0.0, 1.0);
    }
    entries.sort_by(|left, right| {
        left.physical_page
            .cmp(&right.physical_page)
            .then_with(|| left.depth.cmp(&right.depth))
            .then_with(|| {
                toc_title_starts_with_number(&left.title)
                    .cmp(&toc_title_starts_with_number(&right.title))
            })
    });
    let mut previous_depth = 0;
    let mut previous_page = 0;
    let mut previous_title_was_numbered = false;
    for (index, entry) in entries.iter_mut().enumerate() {
        let title_is_numbered = toc_title_starts_with_number(&entry.title);
        entry.depth = if index == 0 {
            0
        } else if entry.depth == 0
            && previous_depth == 0
            && entry.physical_page == previous_page
            && !previous_title_was_numbered
            && title_is_numbered
        {
            1
        } else {
            entry.depth.min(previous_depth + 1)
        };
        previous_depth = entry.depth;
        previous_page = entry.physical_page;
        previous_title_was_numbered = title_is_numbered;
    }
    entries
}

fn toc_title_starts_with_number(title: &str) -> bool {
    title
        .trim_start()
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit() || ('０'..='９').contains(&character))
}

fn nested_toc(
    entries: &[GeneratedTocEntry],
    sections: &[rebook_publication::SpineItem],
) -> Vec<TocEntry> {
    let mut cursor = 0;
    build_level(entries, sections, &mut cursor, 0)
}

fn build_level(
    entries: &[GeneratedTocEntry],
    sections: &[rebook_publication::SpineItem],
    cursor: &mut usize,
    depth: usize,
) -> Vec<TocEntry> {
    let mut output: Vec<TocEntry> = Vec::new();
    while let Some(entry) = entries.get(*cursor) {
        if entry.depth < depth {
            break;
        }
        if entry.depth > depth
            && let Some(parent) = output.last_mut()
        {
            parent.children = build_level(entries, sections, cursor, depth + 1);
            continue;
        }
        let href = entry
            .physical_page
            .checked_sub(1)
            .and_then(|index| sections.get(index))
            .map(|section| section.href.clone());
        output.push(TocEntry {
            label: entry.title.clone(),
            href,
            children: Vec::new(),
        });
        *cursor += 1;
        if entries.get(*cursor).is_some_and(|next| next.depth > depth) {
            let children = build_level(entries, sections, cursor, depth + 1);
            if let Some(parent) = output.last_mut() {
                parent.children = children;
            }
        }
    }
    output
}

struct GeneratedTocBookSource {
    inner: Arc<dyn BookSource>,
    book: Book,
}

impl GeneratedTocBookSource {
    fn new(inner: Arc<dyn BookSource>, entries: &[GeneratedTocEntry]) -> Self {
        let mut book = inner.book().clone();
        book.table_of_contents = nested_toc(entries, &book.sections);
        Self { inner, book }
    }
}

impl BookSource for GeneratedTocBookSource {
    fn inner_source(&self) -> Option<&dyn BookSource> {
        Some(self.inner.as_ref())
    }
    fn book(&self) -> &Book {
        &self.book
    }

    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        TableOfContentsOrigin::Generated
    }

    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        self.inner.parse_section(index)
    }

    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.inner.resource(href)
    }
}

#[cfg(test)]
mod tests {
    use super::{GeneratedTocEntry, normalize_entries};

    #[test]
    fn generated_depth_never_skips_a_level() {
        let entries = normalize_entries(vec![
            GeneratedTocEntry {
                depth: 4,
                title: " Chapter ".into(),
                printed_page: " 1 ".into(),
                physical_page: 8,
                confidence: 2.0,
            },
            GeneratedTocEntry {
                depth: 5,
                title: "Section".into(),
                printed_page: "2".into(),
                physical_page: 9,
                confidence: 0.8,
            },
        ]);
        assert_eq!(entries[0].depth, 0);
        assert_eq!(entries[1].depth, 1);
        assert_eq!(entries[0].title, "Chapter");
        assert!((entries[0].confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn generated_entries_are_sorted_by_page_with_parent_before_same_page_chapter() {
        let entry = |depth, title: &str, printed_page: &str, physical_page| GeneratedTocEntry {
            depth,
            title: title.into(),
            printed_page: printed_page.into(),
            physical_page,
            confidence: 0.9,
        };
        let entries = normalize_entries(vec![
            entry(0, "1 书籍的过去、现在和未来", "6", 10),
            entry(1, "2 一本书的诞生", "13", 17),
            entry(1, "致谢", "253", 257),
            entry(0, "书是什么？", "6", 10),
            entry(0, "书籍设计师的画板", "30", 34),
        ]);

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.depth, entry.title.as_str(), entry.physical_page))
                .collect::<Vec<_>>(),
            vec![
                (0, "书是什么？", 10),
                (1, "1 书籍的过去、现在和未来", 10),
                (1, "2 一本书的诞生", 17),
                (0, "书籍设计师的画板", 34),
                (1, "致谢", 257),
            ]
        );
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].physical_page <= pair[1].physical_page)
        );
    }
}
