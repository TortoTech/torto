//! Local PDF reflow. Intermediate pages live on disk; the PDF and interpreter
//! caches belong only to the cancellable conversion job, never the reader.
mod extract;
mod layout;
mod paragraphs;
mod provenance;
mod quality;
mod raster;
mod spool;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::*;
use rebook_publication::*;
use serde::{Deserialize, Serialize};

// The revision also isolates semantic/translation caches from older grouping.
pub const VERSION: u32 = 13;
pub const PAGE_ANCHOR_PREFIX: &str = "pdf-page-";
pub use quality::{Assessment, TextRoute, assess};
const MAX_SECTION_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub original: Book,
    pub origin: TableOfContentsOrigin,
    pub book: Book,
    pub page_targets: Vec<PublicationUrl>,
    pub resources: Vec<String>,
    pub stats: Statistics,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Statistics {
    pub pages: usize,
    pub text_pages: usize,
    pub fallback_pages: usize,
    pub fallback_regions: usize,
    pub tables: usize,
    pub notes: usize,
    pub glyphs: usize,
    pub unmapped_glyphs: usize,
    pub removed_chrome_glyphs: usize,
    /// Runtime diagnostics only; timings do not enter synced derived content.
    #[serde(skip)]
    pub timings: ConversionTimings,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ConversionTimings {
    pub total_ms: f64,
    pub open_ms: f64,
    pub text_extract_ms: f64,
    pub text_analysis_ms: f64,
    pub raw_write_ms: f64,
    pub raw_read_ms: f64,
    pub layout_ms: f64,
    pub raster_ms: f64,
    pub image_write_ms: f64,
    pub section_write_ms: f64,
    pub finalize_ms: f64,
    pub direct_images: usize,
    pub region_pages: usize,
    pub full_pages: usize,
    pub raster_pixels: u64,
    /// Producer backpressure and final image barrier; wall time on this worker.
    pub raster_wait_ms: f64,
    pub raster_workers: usize,
    /// Scheduling estimate including queued jobs, not measured process memory.
    pub raster_peak_estimated_bytes: usize,
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Character offsets are Unicode scalar offsets in the reconstructed block.
/// A joined paragraph keeps one entry per source glyph, including page breaks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceSlice {
    pub source: SourceRange,
    pub page: usize,
    #[serde(serialize_with = "serialize_source_rect")]
    pub rect: [f64; 4],
    pub original_glyph: usize,
}

#[derive(Serialize, Deserialize)]
pub struct StoredSection {
    pub section: Section,
    pub provenance: Vec<SourceSlice>,
}

// Keep reading IR in JSON and transport source mappings as optional resources.
// Legacy combined JSON remains readable; older readers can still display the
// new section and preserve its sidecar when exporting all manifest resources.
#[derive(Deserialize)]
struct ReadingSection {
    section: Section,
    #[serde(default)]
    provenance_resource: Option<String>,
}

#[derive(Deserialize)]
struct FullSection {
    #[serde(flatten)]
    stored: StoredSection,
    #[serde(default)]
    provenance_resource: Option<String>,
}

#[derive(Serialize)]
struct SectionFile<'a> {
    section: &'a Section,
    provenance: &'a [SourceSlice],
    provenance_resource: &'a str,
}

fn serialize_source_rect<S: serde::Serializer>(
    rect: &[f64; 4],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Provenance is persisted only after geometry/layout decisions. Retain the
    // original values in memory and in any future binary transport.
    if !serializer.is_human_readable() {
        return rect.serialize(serializer);
    }
    rect.map(|value| {
        let scaled = value * 10_000.0;
        if scaled.is_finite() {
            let rounded = scaled.round() / 10_000.0;
            if rounded == 0.0 { 0.0 } else { rounded }
        } else {
            value
        }
    })
    .serialize(serializer)
}

#[derive(Clone, Serialize, Deserialize)]
struct NativeGlyph {
    text: String,
    rect: [f64; 4],
    baseline: [f64; 2],
    advance: [f64; 2],
    size: f64,
    bold: bool,
    italic: bool,
    rotated: bool,
    tag: Option<String>,
    mcid: Option<i32>,
    link: Option<String>,
    index: usize,
    unmapped: bool,
}

impl NativeGlyph {
    fn bounds(&self) -> Rect {
        Rect::new(self.rect[0], self.rect[1], self.rect[2], self.rect[3])
    }
}

#[derive(Default, Serialize, Deserialize)]
struct NativePage {
    width: f64,
    height: f64,
    glyphs: Vec<NativeGlyph>,
    images: Vec<[f64; 4]>,
    #[serde(default)]
    encoded_images: Vec<Option<EncodedImage>>,
    /// Scheduling estimate for all visible rasters, including masked/clipped
    /// images that are deliberately ineligible for direct export.
    #[serde(default)]
    raster_decode_bytes: usize,
    #[serde(default)]
    image_obstacles: Vec<[f64; 4]>,
    rules: Vec<[f64; 4]>,
    graphics: Vec<[f64; 4]>,
    unmapped: usize,
    invisible: usize,
    #[serde(default)]
    headings: Vec<NativeHeading>,
}

#[derive(Clone, Serialize, Deserialize)]
struct EncodedImage {
    object: [i32; 2],
    width: u32,
    height: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct NativeHeading {
    ordinal: Vec<usize>,
    title: Vec<usize>,
    level: u8,
}

/// The caller owns a unique staging directory and publishes it atomically only
/// after this function succeeds. Existing generations are never modified.
pub fn convert(
    pdf_path: &Path,
    original: &Book,
    origin: TableOfContentsOrigin,
    directory: &Path,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(usize, usize, &'static str),
) -> Result<Manifest, String> {
    let started = Instant::now();
    let bytes = fs::read(pdf_path).map_err(|e| e.to_string())?;
    check_cancelled(cancelled)?;
    let publication =
        open_with_id(bytes, "document.pdf", original.id.as_str()).map_err(|e| e.to_string())?;
    // Regeneration can receive a descriptor saved before PDF /Lang was read.
    // Refresh missing language from the opened PDF without changing an explicit
    // caller language. Persist it so native reading still never opens the PDF.
    let mut refreshed_original = original.clone();
    if refreshed_original.metadata.languages.is_empty() {
        refreshed_original.metadata.languages = publication.book().metadata.languages.clone();
    }
    let original = &refreshed_original;
    fs::create_dir_all(directory.join("raw")).map_err(|e| e.to_string())?;
    fs::create_dir_all(directory.join("sections")).map_err(|e| e.to_string())?;
    fs::create_dir_all(directory.join("resources")).map_err(|e| e.to_string())?;
    let count = publication.page_count;
    let mut sizes = BTreeMap::<u32, usize>::new();
    let mut edges = layout::ChromeDetector::default();
    let mut stats = Statistics {
        pages: count,
        ..Statistics::default()
    };
    stats.timings.open_ms = elapsed_ms(started);
    let mut usable_pages = 0;
    let structure_tags = extract::structure_tags(&publication.pdf);
    let mut outline_labels = HashMap::<usize, Vec<(String, u8)>>::new();
    let mut outline_spans = Vec::<(usize, usize, String)>::new();
    fn collect_outline(
        entries: &[TocEntry],
        original: &Book,
        depth: u8,
        parent_end: usize,
        out: &mut HashMap<usize, Vec<(String, u8)>>,
        spans: &mut Vec<(usize, usize, String)>,
    ) {
        let pages = entries
            .iter()
            .map(|entry| {
                entry.href.as_ref().and_then(|href| {
                    original
                        .sections
                        .iter()
                        .position(|s| s.href.path() == href.path())
                })
            })
            .collect::<Vec<_>>();
        for (index, entry) in entries.iter().enumerate() {
            let end = pages[index + 1..]
                .iter()
                .flatten()
                .copied()
                .find(|p| pages[index].is_none_or(|start| *p >= start))
                .unwrap_or(parent_end)
                .min(parent_end);
            if let Some(page) = pages[index] {
                out.entry(page)
                    .or_default()
                    .push((entry.label.clone(), depth));
                spans.push((page, end.max(page + 1), entry.label.clone()));
            }
            collect_outline(
                &entry.children,
                original,
                depth.saturating_add(1).min(6),
                end,
                out,
                spans,
            );
        }
    }
    collect_outline(
        &original.table_of_contents,
        original,
        1,
        count,
        &mut outline_labels,
        &mut outline_spans,
    );
    let extracting = Instant::now();
    for index in 0..count {
        check_cancelled(cancelled)?;
        let extracting_page = Instant::now();
        let mut page = extract::page(&publication.pdf, index, &structure_tags);
        stats.timings.text_extract_ms += elapsed_ms(extracting_page);
        if let Some(labels) = outline_labels.get(&index) {
            layout::mark_outline_headings(&mut page, labels);
        }
        if !layout::requires_page_fallback(&page) {
            usable_pages += 1;
        }
        stats.glyphs += page.glyphs.len();
        stats.unmapped_glyphs += page.unmapped;
        for glyph in &page.glyphs {
            if !glyph.rotated
                && glyph.baseline[1] > page.height * 0.1
                && glyph.baseline[1] < page.height * 0.9
            {
                *sizes.entry((glyph.size * 2.0).round() as u32).or_default() +=
                    glyph.text.chars().count();
            }
        }
        if !layout::requires_page_fallback(&page) {
            edges.observe(&page, index);
        }
        let writing = Instant::now();
        spool::write(&directory.join(format!("raw/{index}.bin")), &page)?;
        stats.timings.raw_write_ms += elapsed_ms(writing);
        progress(index + 1, count, "extract");
    }
    stats.timings.text_analysis_ms =
        (elapsed_ms(extracting) - stats.timings.text_extract_ms - stats.timings.raw_write_ms)
            .max(0.0);
    if usable_pages == 0 {
        return Err("PDF has no usable native text. Use PDF OCR for this book.".into());
    }
    edges.finish();
    let body_size = sizes
        .into_iter()
        .max_by_key(|(_, weight)| *weight)
        .map_or(12.0, |(size, _)| f64::from(size) / 2.0)
        .max(1.0);
    let mut book = original.clone();
    book.metadata.layout = RenditionLayout::Reflowable;
    book.cover = None;
    book.sections.clear();
    let mut resources = Vec::new();
    let mut page_targets = Vec::new();
    let destinations = super::catalog::locations(&publication.pdf);
    let mut outline_targets = HashMap::<(usize, String), PublicationUrl>::new();
    let chapter_pages: std::collections::HashSet<_> = original
        .table_of_contents
        .iter()
        .filter(|_| origin != TableOfContentsOrigin::Fallback)
        .filter_map(|entry| entry.href.as_ref())
        .filter_map(|href| {
            original
                .sections
                .iter()
                .position(|s| s.href.path() == href.path())
        })
        .collect();
    let mut pending: Option<StoredSection> = None;
    let mut previous_frame = None;
    let mut chunk_pages = 0;
    let mut floats = paragraphs::Floats::new();
    let mut anchor_targets = HashMap::new();
    let laying_out = Instant::now();
    std::thread::scope(|scope| -> Result<(), String> {
        let pipeline = raster::Pipeline::new(
            scope,
            &publication,
            directory,
            cancelled,
            raster::Pipeline::worker_count(),
        );
        for index in 0..count {
            check_cancelled(cancelled)?;
            let raw = directory.join(format!("raw/{index}.bin"));
            let reading = Instant::now();
            let page = spool::read(&raw)?;
            stats.timings.raw_read_ms += elapsed_ms(reading);
            let frame = layout::body_frame(&page, body_size);
            let labels = outline_spans
                .iter()
                .filter(|(start, end, _)| *start <= index && index < *end)
                .map(|(_, _, label)| label.as_str())
                .collect::<Vec<_>>();
            let repeated = edges.removed_keys(index, &labels);
            let (mut stored, regions) =
                layout::build(&page, index, body_size, &repeated, &mut stats)?;
            paragraphs::register_floats(&stored, &regions, index + 1, &mut floats);
            paragraphs::same_page(&mut stored, body_size, &floats);
            resources.extend(regions.iter().map(|region| region.path.clone()));
            let chapter = chapter_pages.contains(&index);
            // One physical page of semantic boundary spill is permitted. Do not
            // retain an arbitrarily long paragraph spanning the whole document.
            if pending.is_some() && (chapter || chunk_pages >= 9) {
                save_section(
                    directory,
                    &mut book,
                    &mut resources,
                    pending.take().unwrap(),
                    &mut stats.timings,
                )?;
                chunk_pages = 0;
            }
            let cut = pending.is_some() && chunk_pages >= 8;
            let section_index = book.sections.len() + usize::from(cut);
            relocate(
                &mut stored,
                spine_id(section_index)?,
                section_href(section_index)?,
            );
            for (destination_index, destination) in destinations
                .iter()
                .enumerate()
                .filter(|(_, d)| d.page == index && d.top.is_some())
            {
                let point = publication.pdf.pages()[index]
                    .initial_transform(true)
                    .to_kurbo()
                    * Point::new(
                        destination.left.unwrap_or(0.0),
                        destination.top.unwrap_or(0.0),
                    );
                if let Some(slice) = stored.provenance.iter().min_by(|a, b| {
                    let distance = |s: &SourceSlice| {
                        (s.rect[1] - point.y).abs() * 4.0
                            + if destination.left.is_some() {
                                (s.rect[0] - point.x).abs()
                            } else {
                                0.0
                            }
                    };
                    distance(a).total_cmp(&distance(b))
                }) {
                    let fragment = format!("pdf-native-outline-{destination_index}");
                    stored.section.anchors.push(SectionAnchor {
                        fragment: fragment.clone(),
                        source: slice.source.start.clone(),
                    });
                    outline_targets.insert(
                        (index, destination.label.clone()),
                        stored
                            .section
                            .href
                            .resolve(&format!("#{fragment}"))
                            .map_err(|e| e.to_string())?,
                    );
                }
            }
            let href = stored.section.href.clone();
            page_targets.push(
                PublicationUrl::parse(&format!("{href}#{PAGE_ANCHOR_PREFIX}{}", index + 1))
                    .map_err(|e| e.to_string())?,
            );
            if let Some(previous) = pending.as_mut() {
                paragraphs::across_pages(
                    previous,
                    &mut stored,
                    body_size,
                    previous_frame,
                    frame,
                    &floats,
                );
                // Retain the existing conservative join for sparse pages without a
                // reliable body frame. Never use it across a storage cut.
                if !cut {
                    join_page_paragraph(
                        previous,
                        &mut stored,
                        &page,
                        body_size,
                        previous_frame,
                        frame,
                    );
                }
                for anchor in &previous.section.anchors {
                    anchor_targets.insert(anchor.fragment.clone(), previous.section.href.clone());
                }
                if cut && !stored.section.blocks.is_empty() {
                    save_section(
                        directory,
                        &mut book,
                        &mut resources,
                        pending.take().unwrap(),
                        &mut stats.timings,
                    )?;
                    chunk_pages = 0;
                    pending = Some(stored);
                } else {
                    // A page consumed entirely by a continuation does not create an
                    // empty spine item; keep its notes and anchors with that owner.
                    relocate(
                        &mut stored,
                        previous.section.id.clone(),
                        previous.section.href.clone(),
                    );
                    previous.section.blocks.append(&mut stored.section.blocks);
                    previous.section.anchors.append(&mut stored.section.anchors);
                    previous.provenance.append(&mut stored.provenance);
                }
            } else {
                pending = Some(stored);
            }
            chunk_pages += 1;
            previous_frame = frame;
            for anchor in &pending.as_ref().unwrap().section.anchors {
                anchor_targets.insert(
                    anchor.fragment.clone(),
                    pending.as_ref().unwrap().section.href.clone(),
                );
            }
            floats.retain(|_, (p, _)| *p + 9 > index);
            fs::remove_file(raw).map_err(|e| e.to_string())?;
            let submitting = Instant::now();
            pipeline.submit(index, page, regions)?;
            stats.timings.raster_wait_ms += elapsed_ms(submitting);
            progress(index + 1, count, "layout");
        }
        let waiting = Instant::now();
        pipeline.finish(&mut stats.timings)?;
        stats.timings.raster_wait_ms += elapsed_ms(waiting);
        Ok(())
    })?;
    if let Some(stored) = pending {
        save_section(
            directory,
            &mut book,
            &mut resources,
            stored,
            &mut stats.timings,
        )?;
    }
    stats.timings.layout_ms = (elapsed_ms(laying_out)
        - stats.timings.raw_read_ms
        - stats.timings.raster_wait_ms
        - stats.timings.section_write_ms)
        .max(0.0);
    let finalizing = Instant::now();
    // Paragraph/figure moves may cross the storage cut. Physical page and
    // outline destinations follow their anchors, rather than the initial chunk.
    for target in page_targets.iter_mut().chain(outline_targets.values_mut()) {
        if let Some(fragment) = target.fragment()
            && let Some(href) = anchor_targets.get(fragment)
        {
            *target = href
                .resolve(&format!("#{fragment}"))
                .map_err(|e| e.to_string())?;
        }
    }
    // Embedded/generated outlines retain hierarchy; every target has a physical
    // page anchor even when the page must fall back to an image.
    fn map_toc(
        entries: &mut [TocEntry],
        original: &Book,
        targets: &[PublicationUrl],
        outline_targets: &HashMap<(usize, String), PublicationUrl>,
    ) {
        for entry in entries {
            if let Some(href) = &entry.href {
                entry.href = original
                    .sections
                    .iter()
                    .position(|s| s.href.path() == href.path())
                    .and_then(|p| {
                        outline_targets
                            .get(&(p, entry.label.clone()))
                            .cloned()
                            .or_else(|| targets.get(p).cloned())
                    });
            }
            map_toc(&mut entry.children, original, targets, outline_targets);
        }
    }
    map_toc(
        &mut book.table_of_contents,
        original,
        &page_targets,
        &outline_targets,
    );
    if stats.text_pages == 0 {
        return Err("PDF has no usable native text. Use PDF OCR for this book.".into());
    }
    check_cancelled(cancelled)?;
    let mut manifest = Manifest {
        version: VERSION,
        original: original.clone(),
        origin,
        book,
        page_targets,
        resources,
        stats,
    };
    mark_storage_continuations(&mut manifest);
    write_json(&directory.join("manifest.json"), &manifest)?;
    let _ = fs::remove_dir(directory.join("raw"));
    manifest.stats.timings.finalize_ms = elapsed_ms(finalizing);
    manifest.stats.timings.total_ms = elapsed_ms(started);
    Ok(manifest)
}

fn mark_storage_continuations(manifest: &mut Manifest) {
    // Conversion cuts at top-level chapter pages and an eight-page storage
    // budget. Only the latter continues the preceding semantic subsection.
    // Derive this lightweight hint from the page map so existing generations
    // also receive it on open without loading sections, provenance or the PDF.
    let chapter_sections = manifest
        .original
        .table_of_contents
        .iter()
        .filter_map(|entry| entry.href.as_ref())
        .filter_map(|href| {
            manifest
                .original
                .sections
                .iter()
                .position(|section| section.href.path() == href.path())
        })
        .filter_map(|page| manifest.page_targets.get(page))
        .map(|target| target.path().to_owned())
        .collect::<HashSet<_>>();
    for (index, section) in manifest.book.sections.iter_mut().enumerate() {
        section
            .properties
            .retain(|property| property != CONTINUATION_SECTION_PROPERTY);
        if index > 0 && !chapter_sections.contains(section.href.path()) {
            section
                .properties
                .push(CONTINUATION_SECTION_PROPERTY.to_owned());
        }
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        Err("PDF reflow cancelled".into())
    } else {
        Ok(())
    }
}

fn save_section(
    directory: &Path,
    book: &mut Book,
    resources: &mut Vec<String>,
    stored: StoredSection,
    timings: &mut ConversionTimings,
) -> Result<(), String> {
    let writing = Instant::now();
    let index = book.sections.len();
    book.sections.push(SpineItem {
        id: stored.section.id.clone(),
        href: stored.section.href.clone(),
        media_type: "application/vnd.torto.ir+json".into(),
        linear: true,
        properties: Vec::new(),
    });
    let mapping = provenance::name(index);
    provenance::write(&directory.join(&mapping), &stored.provenance)?;
    write_json(
        &directory.join(format!("sections/{index}.json")),
        &SectionFile {
            section: &stored.section,
            provenance: &[],
            provenance_resource: &mapping,
        },
    )?;
    resources.push(mapping);
    timings.section_write_ms += elapsed_ms(writing);
    Ok(())
}

fn relocate(stored: &mut StoredSection, id: SpineItemId, href: PublicationUrl) {
    let old = stored.section.href.clone();
    fn range(range: &mut Option<SourceRange>, id: &SpineItemId) {
        if let Some(range) = range {
            range.start.spine = id.clone();
            range.end.spine = id.clone();
        }
    }
    fn text(text: &mut TextBlock, id: &SpineItemId, old: &PublicationUrl, href: &PublicationUrl) {
        range(&mut text.source, id);
        for inline in &mut text.content {
            if let Inline::Text(run) = inline
                && let Some(link) = &run.link
                && link.path() == old.path()
            {
                run.link = link
                    .fragment()
                    .and_then(|f| href.resolve(&format!("#{f}")).ok());
            }
        }
    }
    fn block(value: &mut Block, id: &SpineItemId, old: &PublicationUrl, href: &PublicationUrl) {
        match value {
            Block::Text(t) => text(t, id, old, href),
            Block::Image(i) => range(&mut i.source, id),
            Block::Table(t) => {
                range(&mut t.source, id);
                for text_block in t.text_blocks_mut() {
                    text(text_block, id, old, href);
                }
            }
            Block::Figure(f) => {
                range(&mut f.source, id);
                for i in &mut f.images {
                    range(&mut i.source, id);
                }
                for t in &mut f.captions {
                    text(t, id, old, href);
                }
            }
            Block::Note(n) => {
                range(&mut n.source, id);
                for b in &mut n.blocks {
                    block(b, id, old, href);
                }
            }
            _ => {}
        }
    }
    for b in &mut stored.section.blocks {
        block(b, &id, &old, &href);
    }
    for p in &mut stored.provenance {
        p.source.start.spine = id.clone();
        p.source.end.spine = id.clone();
    }
    for anchor in &mut stored.section.anchors {
        anchor.source.spine = id.clone();
    }
    stored.section.id = id;
    stored.section.href = href;
}

fn join_page_paragraph(
    previous: &mut StoredSection,
    next: &mut StoredSection,
    page: &NativePage,
    body: f64,
    previous_frame: Option<layout::BodyFrame>,
    next_frame: Option<layout::BodyFrame>,
) {
    // Notes belong to the paragraph and do not constitute a body-flow boundary.
    // Figures, tables and headings still stop cross-page joins.
    let last_index = previous
        .section
        .blocks
        .iter()
        .rposition(|b| !matches!(b, Block::Note(_)));
    let (Some(Block::Text(last)), Some(Block::Text(first))) = (
        last_index.and_then(|index| previous.section.blocks.get_mut(index)),
        next.section.blocks.first(),
    ) else {
        return;
    };
    if last.kind != TextBlockKind::Paragraph || first.kind != TextBlockKind::Paragraph {
        return;
    }
    let (Some(old), Some(new)) = (&last.source, &first.source) else {
        return;
    };
    let old_tail = previous
        .provenance
        .iter()
        .rev()
        .find(|p| p.source.start.node == old.start.node);
    let new_head = next
        .provenance
        .iter()
        .find(|p| p.source.start.node == new.start.node);
    let (Some(tail), Some(head)) = (old_tail, new_head) else {
        return;
    };
    let body_right = page
        .glyphs
        .iter()
        .filter(|g| g.baseline[1] > page.height * 0.15 && g.baseline[1] < page.height * 0.85)
        .map(|g| g.rect[2])
        .fold(0.0, f64::max);
    let old_text = last
        .content
        .iter()
        .filter_map(|i| {
            if let Inline::Text(t) = i {
                Some(t.text.as_str())
            } else {
                None
            }
        })
        .collect::<String>();
    let old_left = previous
        .provenance
        .iter()
        .filter(|p| {
            p.source.start.node == old.start.node
                && p.page == tail.page
                && (p.rect[1] - tail.rect[1]).abs() < body * 0.2
        })
        .map(|p| p.rect[0])
        .fold(f64::INFINITY, f64::min);
    let frames = previous_frame.zip(next_frame).filter(|(old, new)| {
        ((old.right - old.left) - (new.right - new.left)).abs() <= body * 1.5
            && (old.left - new.left).abs() <= body * 4.0
    });
    let (old_height, old_right, indent) = frames.map_or(
        (page.height, body_right, head.rect[0] - old_left),
        |(old, new)| {
            (
                old.height,
                old.right,
                (head.rect[0] - new.left) - (old_left - old.left),
            )
        },
    );
    // Cross-page joins require a bottom-to-top continuation, a full final line,
    // matching column/indent and no heading/table/chapter boundary. This avoids
    // joining unrelated paragraphs merely because punctuation is absent.
    if tail.rect[3] < old_height * 0.75
        || head.rect[1] > page.height * 0.25
        || (tail.rect[2] - old_right).abs() > body * 2.0
        || indent.abs() > body * 0.8
        || !crate::reflow::continues(
            &old_text,
            &crate::reflow::text(first),
            crate::reflow::ContinuationEvidence::Geometry,
        )
    {
        return;
    }
    let Some(movement) = crate::reflow::append_text(last, first.clone()) else {
        return;
    };
    for slice in &mut next.provenance {
        movement.apply(&mut slice.source.start);
        movement.apply(&mut slice.source.end);
    }
    for anchor in &mut next.section.anchors {
        movement.apply(&mut anchor.source);
    }
    next.section.blocks.remove(0);
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    use std::io::Write;
    let file = fs::File::create(path).map_err(|e| e.to_string())?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut writer, value).map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_SECTION_BYTES {
        return Err("PDF reflow entry exceeds size limit".into());
    }
    serde_json::from_reader(std::io::BufReader::new(file)).map_err(|e| e.to_string())
}

/// The source keeps its manifest and one prepared section. Other sections and
/// resources use the reader's bounded caches; opening it never opens the PDF.
pub struct ReflowSource {
    directory: PathBuf,
    pub manifest: Manifest,
    prepared: arc_swap::ArcSwapOption<PreparedSection>,
}

struct PreparedSection {
    index: usize,
    section: Section,
}

impl ReflowSource {
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn open(directory: &Path, book_id: &str) -> Result<Self, String> {
        let mut manifest: Manifest = read_json(&directory.join("manifest.json"))?;
        if manifest.version != VERSION
            || manifest.book.id.as_str() != book_id
            || manifest.original.id != manifest.book.id
            || manifest.book.sections.is_empty()
            || manifest.book.sections.len() > manifest.stats.pages
            || manifest.page_targets.len() != manifest.stats.pages
        {
            return Err("Invalid PDF reflow manifest".into());
        }
        for path in &manifest.resources {
            let url = PublicationUrl::parse(path).map_err(|e| e.to_string())?;
            if url.path() != path
                || !path.starts_with("resources/")
                || !directory.join(path).is_file()
            {
                return Err("Invalid PDF reflow resource".into());
            }
        }
        for (index, item) in manifest.book.sections.iter().enumerate() {
            if item.id != spine_id(index)?
                || item.href != section_href(index)?
                || !directory.join(format!("sections/{index}.json")).is_file()
            {
                return Err("Invalid PDF reflow section".into());
            }
        }
        crate::HtmlContext::new(&manifest.book.table_of_contents, Vec::new())
            .mark_note_sections(&mut manifest.book.sections);
        mark_storage_continuations(&mut manifest);
        Ok(Self {
            directory: directory.to_owned(),
            manifest,
            prepared: arc_swap::ArcSwapOption::empty(),
        })
    }

    pub fn provenance(&self, index: usize) -> Result<Vec<SourceSlice>, String> {
        Ok(self.stored(index)?.provenance)
    }

    fn stored(&self, index: usize) -> Result<StoredSection, String> {
        if index >= self.manifest.book.sections.len() {
            return Err("PDF reflow section is out of range".into());
        }
        let mut file: FullSection =
            read_json(&self.directory.join(format!("sections/{index}.json")))?;
        self.validate_section(
            index,
            &file.stored.section,
            file.provenance_resource.as_deref(),
        )?;
        if let Some(path) = file.provenance_resource {
            if !file.stored.provenance.is_empty() {
                return Err("Ambiguous PDF provenance storage".into());
            }
            file.stored.provenance = provenance::read(&self.directory.join(path))?;
        }
        Ok(file.stored)
    }

    fn validate_section(
        &self,
        index: usize,
        section: &Section,
        mapping: Option<&str>,
    ) -> Result<(), String> {
        let item = self
            .manifest
            .book
            .sections
            .get(index)
            .ok_or("PDF reflow section is out of range")?;
        if section.id != item.id || section.href != item.href {
            return Err("PDF reflow section identity mismatch".into());
        }
        if let Some(mapping) = mapping
            && (mapping != provenance::name(index)
                || !self.manifest.resources.iter().any(|p| p == mapping))
        {
            return Err("Invalid PDF provenance resource".into());
        }
        Ok(())
    }

    fn reading_section(&self, index: usize) -> Result<Section, String> {
        if index >= self.manifest.book.sections.len() {
            return Err("PDF reflow section is out of range".into());
        }
        let file: ReadingSection =
            read_json(&self.directory.join(format!("sections/{index}.json")))?;
        self.validate_section(index, &file.section, file.provenance_resource.as_deref())?;
        Ok(file.section)
    }
}

impl BookSource for ReflowSource {
    fn book(&self) -> &Book {
        &self.manifest.book
    }
    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.manifest.origin
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        // The mode-loading worker warms this entry before publishing the
        // switch. UI adoption clones only IR, never rereads per-glyph mapping.
        if let Some(prepared) = self.prepared.load_full()
            && prepared.index == index
        {
            return Ok(prepared.section.clone());
        }
        let section = self
            .reading_section(index)
            .map_err(PublicationError::InvalidPublication)?;
        self.prepared.store(Some(Arc::new(PreparedSection {
            index,
            section: section.clone(),
        })));
        Ok(section)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        if !self.manifest.resources.iter().any(|p| p == href.path()) {
            return Err(PublicationError::ResourceNotFound(href.to_string()));
        }
        let path = self.directory.join(href.path());
        let metadata =
            fs::metadata(&path).map_err(|e| PublicationError::InvalidPublication(e.to_string()))?;
        if metadata.len() > MAX_SECTION_BYTES {
            return Err(PublicationError::InvalidPublication(
                "PDF resource exceeds size limit".into(),
            ));
        }
        let bytes =
            fs::read(path).map_err(|e| PublicationError::InvalidPublication(e.to_string()))?;
        Ok(Resource {
            href: href.clone(),
            media_type: if href.path().ends_with(".bin.z") {
                "application/octet-stream"
            } else {
                "image/png"
            }
            .into(),
            bytes: Arc::from(bytes),
        })
    }
}

fn spine_id(index: usize) -> Result<SpineItemId, String> {
    SpineItemId::new(format!("pdf-native-v{VERSION}-{}", index + 1)).map_err(|e| e.to_string())
}
fn section_href(index: usize) -> Result<PublicationUrl, String> {
    PublicationUrl::parse(&format!("Text/pdf-native-v{VERSION}-{}.json", index + 1))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_json_is_compact_without_rounding_memory_or_binary_transport() {
        let anchor = SourceAnchor {
            spine: SpineItemId::new("test").unwrap(),
            node: "p1-g0".into(),
            text_offset: 0,
        };
        let original = SourceSlice {
            source: SourceRange {
                start: anchor.clone(),
                end: anchor,
            },
            page: 0,
            rect: [1.23456789, -0.000000001, -204.64221820355954, 500.0],
            original_glyph: 0,
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("\"rect\":[1.2346,0.0,-204.6422,500.0]"));
        let restored: SourceSlice = serde_json::from_str(&json).unwrap();
        for (saved, raw) in restored.rect.into_iter().zip(original.rect) {
            assert!((saved - raw).abs() <= 0.00005);
        }
        assert_eq!(original.rect[0].to_bits(), 1.23456789_f64.to_bits());
        let restored: SourceSlice =
            bincode::deserialize(&bincode::serialize(&original).unwrap()).unwrap();
        assert_eq!(
            restored.rect.map(f64::to_bits),
            original.rect.map(f64::to_bits)
        );
    }

    #[test]
    fn storage_continuation_hints_preserve_chapter_boundaries_and_other_properties() {
        let opened = super::super::open(pdf(), "input.pdf").unwrap();
        let mut original = opened.book().clone();
        let template = original.sections[0].clone();
        original.sections = (0..6)
            .map(|index| SpineItem {
                id: SpineItemId::new(format!("page-{index}")).unwrap(),
                href: PublicationUrl::parse(&format!("Pages/{index}.svg")).unwrap(),
                ..template.clone()
            })
            .collect();
        original.table_of_contents = vec![TocEntry {
            label: "New chapter".into(),
            href: Some(original.sections[4].href.clone()),
            children: Vec::new(),
        }];
        let mut book = original.clone();
        book.sections = (0..3)
            .map(|index| SpineItem {
                id: spine_id(index).unwrap(),
                href: section_href(index).unwrap(),
                properties: vec!["unrelated-property".into()],
                ..template.clone()
            })
            .collect();
        let page_targets = (0..6)
            .map(|page| {
                book.sections[page / 2]
                    .href
                    .resolve(&format!("#pdf-page-{}", page + 1))
                    .unwrap()
            })
            .collect();
        let mut manifest = Manifest {
            version: VERSION,
            original,
            origin: TableOfContentsOrigin::Embedded,
            book,
            page_targets,
            resources: Vec::new(),
            stats: Statistics {
                pages: 6,
                ..Default::default()
            },
        };
        // The same derivation handles manifests written before this hint existed.
        for _ in 0..2 {
            mark_storage_continuations(&mut manifest);
            assert_eq!(manifest.book.sections[0].properties, ["unrelated-property"]);
            assert_eq!(
                manifest.book.sections[1].properties,
                ["unrelated-property", CONTINUATION_SECTION_PROPERTY]
            );
            assert_eq!(manifest.book.sections[2].properties, ["unrelated-property"]);
        }
    }

    fn pdf() -> Vec<u8> {
        pdf_with_catalog_entry("")
    }

    fn pdf_with_catalog_entry(entry: &str) -> Vec<u8> {
        let stream =
            "BT /F1 12 Tf 50 700 Td (Hello native PDF.) Tj 0 -18 Td (A second line.) Tj ET";
        let objects=vec![format!("<< /Type /Catalog /Pages 2 0 R {entry} >>"),"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".into(),"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>".into(),format!("<< /Length {} >>\nstream\n{stream}\nendstream",stream.len()),"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>".into(),format!("<< /Length {} >>\nstream\n{stream}\nendstream",stream.len()),"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into()];
        let mut output = "%PDF-1.4\n".to_owned();
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(output.len());
            output.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
        }
        let xref = output.len();
        output.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()));
        for offset in offsets.iter().skip(1) {
            output.push_str(&format!("{offset:010} 00000 n \n"));
        }
        output.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        ));
        output.into_bytes()
    }

    #[test]
    fn native_regeneration_refreshes_legacy_language_and_preserves_explicit_language() {
        let root = std::env::temp_dir().join(format!(
            "torto-native-language-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let bytes = pdf_with_catalog_entry("/Lang (en-GB)");
        let path = root.join("input.pdf");
        fs::write(&path, &bytes).unwrap();
        let opened = super::super::open(bytes, "input.pdf").unwrap();
        for (index, languages, expected) in [
            (0, Vec::<String>::new(), vec!["en-GB".to_owned()]),
            (1, vec!["fr".to_owned()], vec!["fr".to_owned()]),
        ] {
            let mut original = opened.book().clone();
            original.metadata.languages = languages;
            let output = root.join(format!("cache-{index}"));
            let manifest = convert(
                &path,
                &original,
                TableOfContentsOrigin::Fallback,
                &output,
                &AtomicBool::new(false),
                |_, _, _| {},
            )
            .unwrap();
            assert_eq!(manifest.original.metadata.languages, expected);
            assert_eq!(manifest.book.metadata.languages, expected);
            let source = ReflowSource::open(&output, original.id.as_str()).unwrap();
            assert_eq!(source.book().metadata.languages, expected);
            assert!(!source.parse_section(0).unwrap().blocks.is_empty());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_conversion_is_lazy_durable_and_keeps_physical_anchors() {
        let root = std::env::temp_dir().join(format!(
            "torto-native-format-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("input.pdf");
        fs::write(&path, pdf()).unwrap();
        let original = super::super::open(pdf(), "input.pdf").unwrap();
        let book = original.book().clone();
        drop(original);
        let result = convert(
            &path,
            &book,
            TableOfContentsOrigin::Fallback,
            &root.join("cache"),
            &AtomicBool::new(false),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(result.stats.text_pages, 2);
        assert_eq!(result.book.sections.len(), 1);
        assert_eq!(result.page_targets.len(), 2);
        // Once converted, opening/reading the result needs no original PDF.
        fs::remove_file(path).unwrap();
        let source = ReflowSource::open(&root.join("cache"), book.id.as_str()).unwrap();
        let section = source.parse_section(0).unwrap();
        assert!(section.anchors.iter().any(|a| a.fragment == "pdf-page-2"));
        assert!(source.provenance(0).unwrap().iter().any(|p| p.page == 2));
        let plain = section
            .blocks
            .iter()
            .filter_map(|b| {
                if let Block::Text(t) = b {
                    Some(
                        t.content
                            .iter()
                            .filter_map(|i| {
                                if let Inline::Text(t) = i {
                                    Some(t.text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<String>(),
                    )
                } else {
                    None
                }
            })
            .collect::<String>();
        assert_eq!(plain.matches("Hello native PDF.").count(), 2);
        let stored = source.stored(0).unwrap();
        let mapping = root.join("cache").join(provenance::name(0));
        let original_mapping = fs::read(&mapping).unwrap();
        assert!(source.manifest.resources.contains(&provenance::name(0)));
        let section_path = root.join("cache/sections/0.json");
        let json = fs::read(&section_path).unwrap();
        // Older ordinary readers can display IR; source mappings are explicit,
        // separately synchronized resources for the current implementation.
        let legacy_reader: StoredSection = serde_json::from_slice(&json).unwrap();
        assert_eq!(legacy_reader.section, section);
        assert!(legacy_reader.provenance.is_empty());
        fs::write(&mapping, b"corrupt").unwrap();
        source.prepared.store(None);
        assert_eq!(source.parse_section(0).unwrap(), section);
        assert!(source.provenance(0).is_err());
        fs::write(&mapping, original_mapping).unwrap();
        let mut unsafe_path: serde_json::Value = serde_json::from_slice(&json).unwrap();
        unsafe_path["provenance_resource"] = "resources/other.bin.z".into();
        write_json(&section_path, &unsafe_path).unwrap();
        source.prepared.store(None);
        assert!(source.parse_section(0).is_err());
        assert!(source.provenance(0).is_err());
        // Existing combined JSON retains both its reading and mapping support.
        write_json(&section_path, &stored).unwrap();
        source.prepared.store(None);
        assert_eq!(source.parse_section(0).unwrap(), section);
        assert_eq!(source.provenance(0).unwrap().len(), stored.provenance.len());
        let original_provenance = fs::read(&mapping).unwrap();
        let manifest_path = root.join("cache/manifest.json");
        let mut manifest: Manifest = read_json(&manifest_path).unwrap();
        for entry in &mut manifest.book.table_of_contents {
            entry.label = "Notes".into();
        }
        write_json(&manifest_path, &manifest).unwrap();
        let notes = ReflowSource::open(&root.join("cache"), book.id.as_str()).unwrap();
        assert!(notes.book().sections[0].is_note_section());
        assert_eq!(notes.parse_section(0).unwrap(), section);
        assert_eq!(notes.provenance(0).unwrap().len(), stored.provenance.len());
        assert_eq!(fs::read(&mapping).unwrap(), original_provenance);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires TORTO_TEST_NATIVE_BASE and TORTO_TEST_NATIVE_GENERATION"]
    fn local_native_storage_comparison() {
        let base = PathBuf::from(std::env::var("TORTO_TEST_NATIVE_BASE").unwrap());
        let next = PathBuf::from(std::env::var("TORTO_TEST_NATIVE_GENERATION").unwrap());
        let manifest: Manifest = read_json(&next.join("manifest.json")).unwrap();
        let old = ReflowSource::open(&base, manifest.book.id.as_str()).unwrap();
        let new = ReflowSource::open(&next, manifest.book.id.as_str()).unwrap();
        assert_eq!(old.book(), new.book());
        assert_eq!(old.manifest.page_targets, new.manifest.page_targets);
        let mut slices = 0;
        let mut max_error = 0.0f64;
        let mut section_bytes = 0;
        let mut map_bytes = 0;
        let mut image_bytes = 0;
        for index in 0..new.book().sections.len() {
            assert_eq!(
                old.parse_section(index).unwrap(),
                new.parse_section(index).unwrap(),
                "IR section {index}"
            );
            let a = old.provenance(index).unwrap();
            let b = new.provenance(index).unwrap();
            assert_eq!(a.len(), b.len());
            slices += a.len();
            for (a, b) in a.iter().zip(&b) {
                assert_eq!(a.source, b.source);
                assert_eq!(a.page, b.page);
                assert_eq!(a.original_glyph, b.original_glyph);
                for (a, b) in a.rect.into_iter().zip(b.rect) {
                    max_error = max_error.max((a - b).abs());
                    assert!((a - b).abs() <= 0.0000500001);
                }
            }
            section_bytes += fs::metadata(next.join(format!("sections/{index}.json")))
                .unwrap()
                .len();
            map_bytes += fs::metadata(next.join(provenance::name(index)))
                .unwrap()
                .len();
        }
        for name in &old.manifest.resources {
            assert!(new.manifest.resources.contains(name));
            let a = image::open(base.join(name)).unwrap().into_rgba8();
            let b = image::open(next.join(name)).unwrap().into_rgba8();
            assert_eq!(a, b, "decoded image {name}");
            image_bytes += fs::metadata(next.join(name)).unwrap().len();
        }
        println!(
            "{}",
            serde_json::json!({
                "sections": new.book().sections.len(), "source_slices": slices,
                "pixel_identical_images": old.manifest.resources.len(),
                "section_bytes": section_bytes, "provenance_bytes": map_bytes,
                "image_bytes": image_bytes, "max_coordinate_difference_from_4dp_json": max_error,
            })
        );
    }

    #[test]
    fn cancelled_conversion_does_not_publish_manifest() {
        let root = std::env::temp_dir().join(format!(
            "torto-native-cancel-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("input.pdf");
        fs::write(&path, pdf()).unwrap();
        let original = super::super::open(pdf(), "input.pdf").unwrap();
        assert!(
            convert(
                &path,
                original.book(),
                TableOfContentsOrigin::Fallback,
                &root.join("cache"),
                &AtomicBool::new(true),
                |_, _, _| {}
            )
            .is_err()
        );
        assert!(!root.join("cache/manifest.json").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
