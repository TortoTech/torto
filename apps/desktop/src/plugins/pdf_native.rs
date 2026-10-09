//! Native text and OCR are independent derived sources. A completed generation
//! is immutable; atomic pointer replacement cannot expose half-written sections.
use super::pdf_ocr::{PdfModeSource, PdfOcrLoadedSource, PdfOcrViewMode};
use rebook_formats::pdf_reflow::{self, ReflowSource, Statistics};
use rebook_publication::BookSource;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TextSource {
    #[default]
    Ocr,
    Native,
}

#[derive(Serialize, Deserialize)]
struct Current {
    version: u32,
    generation: String,
}

pub(crate) fn directory(book_id: &str) -> io::Result<PathBuf> {
    if book_id.is_empty()
        || !book_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(io::Error::other("Invalid PDF native book identity"));
    }
    let project = crate::smoke::project_dirs()
        .ok_or_else(|| io::Error::other("Data directory is unavailable"))?;
    Ok(project.data_local_dir().join("pdf-native").join(book_id))
}

pub(crate) fn selected(book_id: &str) -> TextSource {
    directory(book_id)
        .ok()
        .and_then(|d| fs::read(d.join("text-source.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(crate) fn select(book_id: &str, source: TextSource) -> io::Result<()> {
    crate::persistence::write_json_atomic(&directory(book_id)?.join("text-source.json"), &source)
}

pub(crate) fn open(book_id: &str) -> io::Result<Option<ReflowSource>> {
    let root = directory(book_id)?;
    let bytes = match fs::read(root.join("current.json")) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let pointer: Current = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if uuid::Uuid::parse_str(&pointer.generation).is_err() {
        return Err(io::Error::other("Invalid PDF reflow generation"));
    }
    // Retain old generations on disk, but do not reuse their reconstructed
    // text or semantic caches. The toolbar can regenerate the local result.
    if pointer.version != pdf_reflow::VERSION {
        return Ok(None);
    }
    ReflowSource::open(&root.join(&pointer.generation), book_id)
        .map(Some)
        .map_err(io::Error::other)
}

pub(crate) fn cached_original(
    path: &Path,
    book_id: &str,
) -> io::Result<Option<Arc<PdfModeSource>>> {
    if selected(book_id) != TextSource::Native {
        return Ok(None);
    }
    let Some(native) = open(book_id)? else {
        return Ok(None);
    };
    let path = path.to_owned();
    let id = book_id.to_owned();
    Ok(Some(Arc::new(PdfModeSource::new(
        native.manifest.original,
        native.manifest.origin,
        None,
        move || {
            rebook_formats::open_file_for_reading(&path, Some(&id))
                .map(|s| s.source())
                .map_err(|e| e.to_string())
        },
    ))))
}

pub(crate) fn load(
    original: Arc<dyn BookSource>,
    original_cache: Option<Arc<PdfModeSource>>,
) -> io::Result<Option<PdfOcrLoadedSource>> {
    let id = original.book().id.to_string();
    if selected(&id) != TextSource::Native {
        return Ok(None);
    }
    let Some(native) = open(&id)? else {
        return Ok(None);
    };
    let targets = native.manifest.page_targets.clone();
    let generation = native.directory().to_owned();
    let mode = load_mode(&id)?;
    let cache = Arc::new(PdfModeSource::new(
        native.book().clone(),
        native.table_of_contents_origin(),
        Some(Arc::new(native)),
        move || ReflowSource::open(&generation, &id).map(|s| Arc::new(s) as Arc<dyn BookSource>),
    ));
    Ok(Some(super::pdf_ocr::loaded_reflow(
        original,
        cache,
        original_cache,
        targets,
        mode,
    )))
}

pub(crate) fn set_mode(book_id: &str, mode: PdfOcrViewMode) -> io::Result<()> {
    crate::persistence::write_json_atomic(&directory(book_id)?.join("view-mode.json"), &mode)
}
fn load_mode(book_id: &str) -> io::Result<PdfOcrViewMode> {
    let bytes = match fs::read(directory(book_id)?.join("view-mode.json")) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(PdfOcrViewMode::Reflow),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub(crate) fn generate(
    path: &Path,
    original: &rebook_publication::Book,
    origin: rebook_publication::TableOfContentsOrigin,
    cancelled: &AtomicBool,
    progress: impl FnMut(usize, usize, &'static str),
) -> Result<Statistics, String> {
    let id = original.id.to_string();
    let root = directory(&id).map_err(|e| e.to_string())?;
    let generation = uuid::Uuid::new_v4().to_string();
    let stage = root.join(&generation);
    // The worker has only a descriptor. Conversion opens and releases its own
    // parser, so an inactive original-mode cache is never resurrected.
    let result = pdf_reflow::convert(path, original, origin, &stage, cancelled, progress);
    if let Ok(manifest) = &result {
        let timings = &manifest.stats.timings;
        use crate::diagnostics::Field;
        crate::diagnostics::log(
            "pdf.native.converted",
            &[
                Field::Detail("book_id", &id),
                Field::Usize("pages", manifest.stats.pages),
                Field::F32("total_ms", timings.total_ms as f32),
                Field::F32("open_ms", timings.open_ms as f32),
                Field::F32("text_extract_ms", timings.text_extract_ms as f32),
                Field::F32("text_analysis_ms", timings.text_analysis_ms as f32),
                Field::F32("raw_write_ms", timings.raw_write_ms as f32),
                Field::F32("raw_read_ms", timings.raw_read_ms as f32),
                Field::F32("layout_ms", timings.layout_ms as f32),
                Field::F32("raster_ms", timings.raster_ms as f32),
                Field::F32("image_write_ms", timings.image_write_ms as f32),
                Field::F32("section_write_ms", timings.section_write_ms as f32),
                Field::F32("finalize_ms", timings.finalize_ms as f32),
                Field::Usize("direct_images", timings.direct_images),
                Field::Usize("region_pages", timings.region_pages),
                Field::Usize("full_pages", timings.full_pages),
            ],
        );
    }
    let mut published = false;
    let result = result.and_then(|manifest| {
        if cancelled.load(Ordering::Acquire) {
            return Err("PDF reflow cancelled".into());
        }
        publish(&root, &generation)?;
        published = true;
        select(&id, TextSource::Native).map_err(|e| e.to_string())?;
        set_mode(&id, PdfOcrViewMode::Reflow).map_err(|e| e.to_string())?;
        crate::sync::mark_derived_dirty(&id, crate::sync::DerivedDataKind::NativePdf)
            .map_err(|e| e.to_string())?;
        Ok(manifest.stats)
    });
    if result.is_err() && !published {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

fn publish(root: &Path, generation: &str) -> Result<(), String> {
    crate::persistence::write_json_atomic(
        &root.join("current.json"),
        &Current {
            version: pdf_reflow::VERSION,
            generation: generation.to_owned(),
        },
    )
    .map_err(|e| e.to_string())
}

/// Sync uses a separate archive and cloud key from OCR. No derived payload is
/// copied into the OCR provider's document or translated block cache.
pub(crate) fn export(book_id: &str) -> io::Result<Option<Vec<(String, Vec<u8>)>>> {
    let Some(source) = open(book_id)? else {
        return Ok(None);
    };
    let generation = source.directory().to_owned();
    let mut files = vec![(
        "manifest.json".into(),
        fs::read(generation.join("manifest.json"))?,
    )];
    for index in 0..source.book().sections.len() {
        let name = format!("sections/{index}.json");
        files.push((name.clone(), fs::read(generation.join(name))?));
    }
    for name in source.manifest.resources {
        files.push((name.clone(), fs::read(generation.join(name))?));
    }
    Ok(Some(files))
}

pub(crate) fn import(book_id: &str, files: Vec<(String, Vec<u8>)>) -> io::Result<()> {
    let root = directory(book_id)?;
    let generation = uuid::Uuid::new_v4().to_string();
    let stage = root.join(&generation);
    let result = (|| {
        for (name, bytes) in files {
            let url = rebook_publication::PublicationUrl::parse(&name).map_err(io::Error::other)?;
            if url.path() != name
                || !(name == "manifest.json"
                    || name.starts_with("sections/")
                    || name.starts_with("resources/"))
            {
                return Err(io::Error::other("Invalid native PDF archive path"));
            }
            let path = stage.join(name);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, bytes)?;
        }
        let source = ReflowSource::open(&stage, book_id).map_err(io::Error::other)?;
        for index in 0..source.book().sections.len() {
            source.parse_section(index).map_err(io::Error::other)?;
        }
        publish(&root, &generation).map_err(io::Error::other)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(stage);
    }
    result
}

pub(crate) fn cache_identity(book_id: &str, source: &dyn BookSource) -> String {
    if source
        .book()
        .sections
        .first()
        .is_some_and(|s| s.id.as_str().starts_with("pdf-native-v"))
    {
        format!("{book_id}-native-v{}", pdf_reflow::VERSION)
    } else {
        book_id.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::*;

    #[test]
    #[ignore = "requires TORTO_TEST_NATIVE_GENERATION pointing to the local Pick, Click, Flick! cache"]
    #[allow(
        clippy::too_many_lines,
        reason = "checks real cached figures, TOC navigation and locator restoration"
    )]
    fn local_native_chunk_continuation_context() {
        use rebook_layout::{LayoutViewport, ReaderStyle, TypesettingMode};
        use rebook_reader::{PageDirection, ReaderSession};
        fn target(entries: &[TocEntry], prefix: &str) -> PublicationUrl {
            fn find(entries: &[TocEntry], prefix: &str) -> Option<PublicationUrl> {
                for entry in entries {
                    if entry.label.starts_with(prefix) {
                        return entry.href.clone();
                    }
                    if let Some(found) = find(&entry.children, prefix) {
                        return Some(found);
                    }
                }
                None
            }
            find(entries, prefix).expect("fixture subsection")
        }
        fn plain(content: &[(usize, Section)]) -> String {
            content
                .iter()
                .flat_map(|(_, section)| &section.blocks)
                .flat_map(|block| match block {
                    Block::Text(text) => vec![text],
                    Block::Figure(figure) => figure.captions.iter().collect(),
                    _ => Vec::new(),
                })
                .flat_map(|text| &text.content)
                .filter_map(|inline| match inline {
                    Inline::Text(run) => Some(run.text.as_str()),
                    _ => None,
                })
                .collect()
        }
        let directory = PathBuf::from(std::env::var("TORTO_TEST_NATIVE_GENERATION").unwrap());
        let manifest: pdf_reflow::Manifest =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        let source = Arc::new(
            pdf_reflow::ReflowSource::open(&directory, manifest.book.id.as_str()).unwrap(),
        );
        let direct = target(&source.book().table_of_contents, "2.5.1.3 ");
        let windows = target(&source.book().table_of_contents, "2.5.1.4 ");
        let mut style = ReaderStyle::default();
        style.typesetting.mode = TypesettingMode::Unified;
        let viewport = LayoutViewport::new(1000, 700).unwrap();
        let mut reader = ReaderSession::open(source.clone(), viewport, style.clone()).unwrap();
        reader.go_to_href(&direct).unwrap();
        let owner_unit = reader.reading_unit_location().index;
        let content = reader.current_reading_unit_content().unwrap();
        let text = plain(&content);
        assert!(text.contains("Direct  Manipulation"));
        assert!(text.contains("Figure  2.24"));
        assert!(text.contains("by  Ivan  Sutherland"));
        assert!(!text.contains("Window  Managers"));
        assert_eq!(reader.current_reading_unit_sections(), 19..21);
        let pages = reader.current_reading_unit_pages().unwrap();
        assert!(pages.iter().any(|page| page.position.section_index == 19));
        let prefix = pages
            .iter()
            .find(|page| page.position.section_index == 20)
            .unwrap();
        assert!(prefix.visible_top.is_some());
        let position = prefix.position;
        drop(pages);
        reader.set_visible_position(position).unwrap();
        assert_eq!(reader.reading_unit_location().index, owner_unit);
        let active = reader.snapshot().active_toc_id.unwrap();
        assert_eq!(
            reader
                .toc_items()
                .iter()
                .find(|item| item.id == active)
                .unwrap()
                .target
                .as_ref(),
            Some(&direct)
        );
        let locator = reader.current_locator();
        reader
            .go_to_adjacent_reading_unit(PageDirection::Next)
            .unwrap();
        assert!(!plain(&reader.current_reading_unit_content().unwrap()).contains("Figure  2.24"));
        assert_eq!(reader.reading_unit_location().index, 1);
        let active = reader.snapshot().active_toc_id.unwrap();
        assert_eq!(
            reader
                .toc_items()
                .iter()
                .find(|item| item.id == active)
                .unwrap()
                .target
                .as_ref(),
            Some(&windows)
        );
        reader
            .go_to_adjacent_reading_unit(PageDirection::Previous)
            .unwrap();
        assert_eq!(reader.location().section_index, 19);
        assert!(plain(&reader.current_reading_unit_content().unwrap()).contains("Figure  2.24"));
        let mut restored = ReaderSession::open_with_fonts_at_locator(
            source,
            viewport,
            style,
            Arc::default(),
            &locator,
        )
        .unwrap();
        assert_eq!(restored.reading_unit_location().index, owner_unit);
        assert_eq!(restored.current_reading_unit_sections(), 19..21);
        assert!(plain(&restored.current_reading_unit_content().unwrap()).contains("Figure  2.24"));
        let active = restored.snapshot().active_toc_id.unwrap();
        assert_eq!(
            restored
                .toc_items()
                .iter()
                .find(|item| item.id == active)
                .unwrap()
                .target
                .as_ref(),
            Some(&direct)
        );
        eprintln!(
            "native continuation: Figure 2.24 and Sketchpad prose join the same 2.5.1.3 view; next goes directly to 2.5.1.4; locator restores the complete unit without opening the PDF"
        );
    }

    fn fixture(id: &str) -> Vec<(String, Vec<u8>)> {
        let spine = SpineItemId::new(format!("pdf-native-v{}-1", pdf_reflow::VERSION)).unwrap();
        let href =
            PublicationUrl::parse(&format!("Text/pdf-native-v{}-1.json", pdf_reflow::VERSION))
                .unwrap();
        let book = Book {
            id: PublicationId::new(id).unwrap(),
            metadata: Metadata {
                title: "Native fixture".into(),
                layout: RenditionLayout::Reflowable,
                ..Metadata::default()
            },
            cover: None,
            sections: vec![SpineItem {
                id: spine.clone(),
                href: href.clone(),
                media_type: "application/vnd.torto.ir+json".into(),
                linear: true,
                properties: vec![],
            }],
            table_of_contents: vec![],
        };
        let range = SourceRange {
            start: SourceAnchor {
                spine: spine.clone(),
                node: "p1-g0".into(),
                text_offset: 0,
            },
            end: SourceAnchor {
                spine: spine.clone(),
                node: "p1-g0".into(),
                text_offset: 6,
            },
        };
        let stored = pdf_reflow::StoredSection {
            section: Section {
                id: spine,
                href: href.clone(),
                blocks: vec![Block::Text(TextBlock {
                    kind: TextBlockKind::Paragraph,
                    content: vec![Inline::Text(TextRun {
                        text: "Native".into(),
                        style: TextStyle::default(),
                        link: None,
                    })],
                    style: BlockStyle::default(),
                    source: Some(range.clone()),
                })],
                anchors: vec![SectionAnchor {
                    fragment: "pdf-page-1".into(),
                    source: range.start,
                }],
            },
            provenance: vec![],
        };
        let mut original = book.clone();
        original.metadata.layout = RenditionLayout::PrePaginated;
        original.sections[0].id = SpineItemId::new("pdf-page-1").unwrap();
        original.sections[0].href = PublicationUrl::parse("Pages/page-1.svg").unwrap();
        let manifest = pdf_reflow::Manifest {
            version: pdf_reflow::VERSION,
            original,
            origin: TableOfContentsOrigin::Fallback,
            book,
            page_targets: vec![href.resolve("#pdf-page-1").unwrap()],
            resources: vec![],
            stats: Statistics {
                pages: 1,
                text_pages: 1,
                ..Statistics::default()
            },
        };
        vec![
            (
                "manifest.json".into(),
                serde_json::to_vec(&manifest).unwrap(),
            ),
            (
                "sections/0.json".into(),
                serde_json::to_vec(&stored).unwrap(),
            ),
        ]
    }
    #[test]
    fn native_cache_reads_without_pdf_and_failed_import_preserves_generation() {
        let id = uuid::Uuid::new_v4().to_string();
        let files = fixture(&id);
        import(&id, files.clone()).unwrap();
        select(&id, TextSource::Native).unwrap();
        let root = directory(&id).unwrap();
        let pointer = fs::read(root.join("current.json")).unwrap();
        let original = cached_original(&root.join("missing.pdf"), &id)
            .unwrap()
            .unwrap();
        let loaded = load(original.clone(), Some(original.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.source.parse_section(0).unwrap().blocks.len(), 1);
        assert!(original.lease().is_err());
        let controller = loaded.controller.as_ref().unwrap();
        let mut locator = LocatorV1::at_start(
            original.book().id.clone(),
            original.book().sections[0].href.clone(),
        );
        controller.remap_locator(&mut locator);
        assert_eq!(
            locator.href.path(),
            format!("Text/pdf-native-v{}-1.json", pdf_reflow::VERSION)
        );
        assert_eq!(locator.href.fragment(), Some("pdf-page-1"));
        assert_eq!(
            cache_identity(&id, loaded.source.as_ref()),
            format!("{id}-native-v{}", pdf_reflow::VERSION)
        );
        assert_eq!(export(&id).unwrap().unwrap(), files);
        let old = Current {
            version: pdf_reflow::VERSION - 1,
            generation: serde_json::from_slice::<Current>(&pointer)
                .unwrap()
                .generation,
        };
        fs::write(root.join("current.json"), serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(open(&id).unwrap().is_none());
        assert!(
            cached_original(&root.join("missing.pdf"), &id)
                .unwrap()
                .is_none()
        );
        fs::write(root.join("current.json"), &pointer).unwrap();
        assert!(import(&id, vec![("manifest.json".into(), b"invalid".to_vec())]).is_err());
        assert_eq!(fs::read(root.join("current.json")).unwrap(), pointer);
        assert!(open(&id).unwrap().is_some());

        // A reader's lazy reload must retain its original immutable generation,
        // even when sync publishes a replacement while that mode is inactive.
        let previous = loaded.source.parse_section(0).unwrap();
        let mut replacement = files;
        let payload = &mut replacement[1].1;
        let mut stored: pdf_reflow::StoredSection = serde_json::from_slice(payload).unwrap();
        if let Block::Text(block) = &mut stored.section.blocks[0]
            && let Inline::Text(run) = &mut block.content[0]
        {
            run.text = "New text".into();
        }
        *payload = serde_json::to_vec(&stored).unwrap();
        import(&id, replacement).unwrap();
        assert_ne!(
            open(&id).unwrap().unwrap().parse_section(0).unwrap(),
            previous
        );
        controller.set_mode(PdfOcrViewMode::Original);
        drop(controller.retire_inactive());
        let reloaded = controller.prepare_mode(PdfOcrViewMode::Reflow).unwrap();
        assert_eq!(reloaded.parse_section(0).unwrap(), previous);
        fs::remove_dir_all(root).unwrap();
    }
}
