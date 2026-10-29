//! Media preview rasterization is dispatched only after a click.
//! The worker retains bounded identifiers, never sections, layouts or pixels.
use anyrender::ImageRenderer;
use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle, SpreadMode};
use rebook_publication::{
    Block, BookSource, Inline, PublicationUrl, Section, SourceRange, TableBlock, TextBlock,
    media_references::{self, MediaKey, MediaKind},
};
use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

const INDEX_BUDGET: usize = 2 * 1024 * 1024;
const PIXEL_BUDGET: usize = 32 * 1024 * 1024;

pub(super) use crate::ui::image_preview::PreviewMotion;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub(super) section: usize,
    pub(super) block: usize,
    images: Vec<usize>,
    captions: Vec<usize>,
    group: Option<usize>,
    pub(super) key: MediaKey,
}

impl Target {
    pub(super) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.key.number.len()
            + (self.images.len() + self.captions.len()) * std::mem::size_of::<usize>()
    }
}

fn plain(text: &TextBlock) -> String {
    text.content
        .iter()
        .flat_map(Inline::text_runs)
        .map(|run| run.text.as_str())
        .collect()
}

fn caption_text(text: &TextBlock) -> String {
    text.content
        .iter()
        .map(|inline| match inline {
            Inline::Math(math) => math
                .original_text()
                .unwrap_or_else(|| format!("\\({}\\)", math.latex)),
            Inline::Image(image) => image
                .image
                .formula
                .as_ref()
                .map(|formula| format!("\\({}\\)", formula.latex))
                .unwrap_or_else(|| {
                    let alt = &image.image.alt;
                    if [".png", ".jpg", ".jpeg", ".svg", ".gif", ".webp"]
                        .iter()
                        .any(|suffix| alt.to_ascii_lowercase().ends_with(suffix))
                    {
                        "□".into()
                    } else {
                        alt.clone()
                    }
                }),
            _ => inline
                .text_runs()
                .iter()
                .map(|run| run.text.as_str())
                .collect(),
        })
        .collect()
}

#[cfg(test)]
#[path = "media_preview_tests.rs"]
mod tests;

pub(super) fn targets(section: &Section, index: usize) -> Vec<Target> {
    let mut result = Vec::new();
    for (at, block) in section.blocks.iter().enumerate() {
        if let Some(key) = media_references::equation_target(block) {
            result.push(Target { section: index, block: at, images: vec![], captions: vec![], group: None, key });
            continue;
        }
        let mut captions = Vec::new();
        let mut images = Vec::new();
        let (kind, texts): (Option<MediaKind>, Vec<&TextBlock>) = match block {
            // Some books publish their numbered tables as images.
            Block::Figure(figure) => {
                captions.extend(adjacent_captions(section, at));
                (None, figure.text_blocks().filter(|text| text.kind == rebook_publication::TextBlockKind::Caption).collect())
            },
            Block::Table(table) => {
                let mut texts: Vec<_> = table.before.iter().chain(&table.after).collect();
                if let Some(Block::Text(text)) =
                    at.checked_sub(1).and_then(|i| section.blocks.get(i))
                    && media_references::caption_label(&plain(text))
                        .is_some_and(|label| label.key.kind == MediaKind::Table)
                {
                    captions.push(at - 1);
                    texts.push(text);
                }
                for (next, adjacent) in section.blocks.iter().enumerate().skip(at + 1) {
                    let Block::Text(text) = adjacent else { break };
                    let value = plain(text);
                    let lower = value.trim_start().to_ascii_lowercase();
                    if [
                        "note:",
                        "notes:",
                        "source:",
                        "sources:",
                        "注：",
                        "注:",
                        "来源：",
                        "来源:",
                    ]
                    .iter()
                    .any(|prefix| lower.starts_with(prefix))
                    {
                        captions.push(next);
                    } else {
                        break;
                    }
                }
                (Some(MediaKind::Table), texts)
            }
            Block::Text(text) if media_references::caption_label(&plain(text)).is_some() => {
                // A label alone (e.g. a list of illustrations) is never a target.
                captions.push(at);
                let mut start = at;
                while start > 0 && matches!(section.blocks[start - 1], Block::Image(_)) {
                    start -= 1;
                }
                let mut end = at + 1;
                while end < section.blocks.len() && matches!(section.blocks[end], Block::Image(_)) {
                    end += 1;
                }
                images.extend(start..at);
                if images.is_empty() {
                    images.extend(at + 1..end);
                }
                if images.is_empty() {
                    continue;
                }
                for image in &images {
                    captions.extend(adjacent_captions(section, *image));
                }
                captions.extend(adjacent_captions(section, at));
                captions.sort_unstable();
                captions.dedup();
                (None, vec![text])
            }
            _ => continue,
        };
        for text in texts {
            let Some(label) = media_references::caption_label(&plain(text)) else {
                continue;
            };
            if kind.is_some_and(|kind| label.key.kind != kind) {
                continue;
            }
            let target = Target {
                section: index,
                block: at,
                images: images.clone(),
                captions: captions.clone(),
                group: None,
                key: label.key,
            };
            if !result.contains(&target) {
                result.push(target);
            }
        }
    }
    // Publishers sometimes split (a), (b), ... into adjacent figure elements,
    // with the number and page anchor present only in the first element.
    // Require an explicit, consecutive caption sequence; never infer from class names.
    let mut group: Option<(usize, MediaKey, u8)> = None;
    for (at, block) in section.blocks.iter().enumerate() {
        let Block::Figure(figure) = block else { group = None; continue };
        let numbered = figure.caption_blocks().find_map(|text| media_references::caption_label(&plain(text)));
        let mut letters = figure.caption_blocks().filter_map(|text| subfigure_letter(&plain(text)));
        let letter = letters.next().filter(|first| letters.all(|letter| letter == *first));
        if let Some(label) = numbered {
            group = None;
            if label.key.kind == MediaKind::Figure && letter == Some(b'a')
                && !label.key.number.ends_with(|c: char| c.is_ascii_alphabetic())
            {
                let mut key = label.key.clone();
                key.number.push('a');
                result.push(Target { section: index, block: at, images: vec![], captions: vec![], group: None, key });
                group = Some((at, label.key, b'b'));
            }
        } else if let Some((root, key, next)) = group.as_mut() {
            if letter == Some(*next) && *next <= b'z' {
                let mut key = key.clone();
                key.number.push(char::from(*next));
                result.push(Target { section: index, block: at, images: vec![], captions: vec![], group: Some(*root), key });
                *next += 1;
            } else { group = None; }
        }
    }
    result
}

// Collect only explicit adjacent caption/source semantics; prose is a boundary.
fn adjacent_captions(section: &Section, at: usize) -> Vec<usize> {
    let eligible = |block: &Block| {
        matches!(block, Block::Text(text)
        if text.kind == rebook_publication::TextBlockKind::Caption || rebook_layout::is_media_source(text))
    };
    let mut media_start = at;
    let mut media_end = at + 1;
    if matches!(section.blocks[at], Block::Image(_)) {
        while media_start > 0 && matches!(section.blocks[media_start - 1], Block::Image(_)) {
            media_start -= 1;
        }
        while media_end < section.blocks.len()
            && matches!(section.blocks[media_end], Block::Image(_))
        {
            media_end += 1;
        }
    }
    let mut start = media_start;
    while start > 0 && eligible(&section.blocks[start - 1]) {
        start -= 1;
    }
    let mut end = media_end;
    while end < section.blocks.len() && eligible(&section.blocks[end]) {
        end += 1;
    }
    (start..media_start).chain(media_end..end).collect()
}

fn image_target(section: &Section, index: usize, href: &PublicationUrl, source: Option<&SourceRange>) -> Option<Target> {
    fn matches(block: &Block, href: &PublicationUrl, source: Option<&SourceRange>) -> bool {
        let image_matches = |image: &rebook_publication::ImageBlock| &image.href == href
            && source.is_none_or(|source| image.source.as_ref().is_some_and(|range| range.start.node == source.start.node));
        match block {
            Block::Image(image) => image_matches(image),
            Block::Figure(figure) => figure.images.iter().any(image_matches)
                || figure.body.iter().any(|block| matches(block, href, source)),
            _ => false,
        }
    }
    let mut candidates = section.blocks.iter().enumerate().filter(|(_, block)| matches(block, href, source));
    let (at, _) = candidates.next()?;
    // Without an authored identity, repeated logos/photos must not borrow another caption.
    if candidates.next().is_some() { return None; }
    if let Some(target) = targets(section, index)
        .into_iter()
        .find(|target| target.block == at || target.images.contains(&at))
    {
        return Some(target);
    }
    let captions = adjacent_captions(section, at);
    let images = if matches!(section.blocks[at], Block::Image(_)) {
        vec![at]
    } else {
        vec![]
    };
    Some(Target {
        section: index,
        block: at,
        images,
        captions,
        group: None,
        key: MediaKey {
            kind: MediaKind::Figure,
            number: String::new(),
        },
    })
}

fn subfigure_letter(value: &str) -> Option<u8> {
    let value = value.trim_start();
    let value = value.strip_prefix('(').or_else(|| value.strip_prefix('（')).unwrap_or(value);
    let mut chars = value.chars();
    let letter = chars.next()?.to_ascii_lowercase();
    if !letter.is_ascii_lowercase() || !matches!(chars.next()?, ')' | '）' | '.') { return None; }
    chars.next().is_none_or(char::is_whitespace).then_some(letter as u8)
}

fn text_blocks(block: &Block) -> Vec<&TextBlock> {
    match block {
        Block::Text(t) => vec![t],
        Block::Figure(f) => f.text_blocks().collect(),
        Block::Table(t) => t.text_blocks().collect(),
        Block::Quote(q) => q.body.iter().chain(&q.attribution).collect(),
        Block::Note(n) => n.blocks.iter().flat_map(text_blocks).collect(),
        _ => vec![],
    }
}

fn authored_link(
    section: &Section,
    origin: &SourceRange,
    key: &MediaKey,
) -> Result<Option<PublicationUrl>, String> {
    let mut found = None;
    for text in section.blocks.iter().flat_map(text_blocks) {
        if !text
            .source
            .as_ref()
            .is_some_and(|s| s.start.node == origin.start.node)
        {
            continue;
        }
        let value = plain(text);
        // Authored source offsets count text scalars, without image placeholders.
        let content: Vec<_> = text.content.iter().filter(|inline| matches!(inline, Inline::Text(_))).cloned().collect();
        for reference in media_references::inline_references(&content)
            .into_iter()
            .filter(|r| &r.key == key)
        {
            let base = text.source.as_ref().unwrap().start.text_offset;
            let start = base + value[..reference.range.start].chars().count() as u64;
            let end = base + value[..reference.range.end].chars().count() as u64;
            if end <= origin.start.text_offset || start >= origin.end.text_offset {
                continue;
            }
            let mut offset = 0;
            for run in text.content.iter().flat_map(Inline::text_runs) {
                let end = offset + run.text.len();
                if offset < reference.range.end
                    && end > reference.range.start
                    && let Some(href) = run
                        .link
                        .as_ref()
                        .filter(|href| href.website_url().is_none())
                {
                    if found.as_ref().is_some_and(|previous| previous != href) {
                        return Err("This reference has several different authored targets.".into());
                    }
                    found = Some(href.clone());
                }
                offset = end;
            }
        }
    }
    Ok(found)
}

pub(super) fn target_has_anchor(section: &Section, target: &Target, href: &PublicationUrl) -> bool {
    let Some(fragment) = href.fragment() else {
        return false;
    };
    let Some(anchor) = section.anchors.iter().find(|a| a.fragment == fragment) else {
        return false;
    };
    target_has_node(section, target, &anchor.source.node)
}

fn target_has_node(section: &Section, target: &Target, node: &str) -> bool {
    let mut indices = vec![target.block];
    indices.extend(target.group);
    indices.extend(&target.images);
    indices.extend(&target.captions);
    indices.into_iter().any(|index| {
        let block = &section.blocks[index];
        let mut sources: Vec<_> = text_blocks(block)
            .into_iter()
            .filter_map(|t| t.source.as_ref())
            .collect();
        match block {
            Block::Image(i) => sources.extend(i.source.as_ref()),
            Block::Figure(f) => {
                sources.extend(f.source.as_ref());
                sources.extend(f.images.iter().filter_map(|i| i.source.as_ref()));
            }
            Block::Table(t) => sources.extend(t.source.as_ref()),
            _ => {}
        }
        sources.iter().any(|s| s.start.node == node)
    })
}

fn matching(target: &Target, key: &MediaKey) -> bool {
    target.key == *key
        || (target.key.kind == key.kind
            && key.number.strip_suffix(|c: char| c.is_ascii_alphabetic())
                == Some(target.key.number.as_str()))
}

fn unique(candidates: impl Iterator<Item = Target>) -> Result<Option<Target>, String> {
    let mut candidates = candidates.collect::<Vec<_>>();
    // Multiple subfigure labels can describe the same complete figure.
    candidates.dedup_by(|a, b| a.section == b.section && a.block == b.block);
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.pop()),
        _ => Err("This figure/table number has several possible targets in this book.".into()),
    }
}

pub(super) fn select(
    candidates: impl Iterator<Item = Target>,
    key: &MediaKey,
) -> Result<Option<Target>, String> {
    let matches = candidates
        .filter(|target| matching(target, key))
        .collect::<Vec<_>>();
    if matches.iter().any(|t| t.key == *key) {
        unique(matches.into_iter().filter(|t| t.key == *key))
    } else {
        unique(matches.into_iter())
    }
}

pub(super) fn chapter_scope(source: &dyn BookSource, origin: usize) -> Range<usize> {
    fn collect(
        entries: &[rebook_publication::TocEntry],
        source: &dyn BookSource,
        starts: &mut Vec<usize>,
    ) {
        for entry in entries {
            let label = entry.label.to_ascii_lowercase();
            if (label.contains("chapter") || (label.contains('第') && label.contains('章')))
                && let Some(href) = &entry.href
                && let Some(index) = source
                    .book()
                    .sections
                    .iter()
                    .position(|s| s.href.path() == href.path())
            {
                starts.push(index);
            }
            collect(&entry.children, source, starts);
        }
    }
    let mut starts = vec![];
    collect(&source.book().table_of_contents, source, &mut starts);
    starts.sort_unstable();
    starts.dedup();
    let Some(start) = starts.iter().copied().take_while(|s| *s <= origin).last() else {
        return origin..origin + 1;
    };
    start
        ..starts
            .iter()
            .copied()
            .find(|s| *s > origin)
            .unwrap_or(source.book().sections.len())
}

#[derive(Default)]
struct Index {
    entries: HashMap<usize, Vec<Target>>,
    bytes: usize,
}
impl Index {
    fn add(&mut self, section: &Section, at: usize) -> Result<(), String> {
        if self.entries.contains_key(&at) {
            return Ok(());
        }
        let entries = targets(section, at);
        let bytes = entries
            .iter()
            .map(|t| {
                std::mem::size_of::<Target>()
                    + t.key.number.len()
                    + (t.images.len() + t.captions.len()) * 8
            })
            .sum::<usize>()
            + 64;
        if self.bytes + bytes > INDEX_BUDGET {
            return Err("The figure/table index exceeds the preview limit.".into());
        }
        self.bytes += bytes;
        self.entries.insert(at, entries);
        Ok(())
    }
    fn resolve(&mut self, job: &Job, needed: &impl Fn() -> bool) -> Result<Target, String> {
        let source = job.source.as_ref();
        // Reuse the validated catalog and stable source identity, even when AI
        // layout or translation has shifted the displayed block ordinals.
        if let Some((index, node)) = job.destination.clone().or_else(|| job.catalog.as_ref().and_then(|catalog| catalog.target_node(&job.origin, &job.key))) {
            if !needed() { return Err("Cancelled".into()); }
            let section = source.parse_section(index).map_err(|e| e.to_string())?;
            if job.key.number.is_empty() {
                if let Some((at, block)) = section.blocks.iter().enumerate().find(|(_, block)| matches!(block, Block::Figure(_) | Block::Image(_) | Block::Table(_)) && super::block_source_range(block).is_some_and(|source| source.start.node == node)) {
                    return Ok(Target { section: index, block: at,
                        images: if matches!(block, Block::Image(_)) { vec![at] } else { vec![] },
                        captions: adjacent_captions(&section, at), group: None, key: job.key.clone() });
                }
                return Err("The image changed. Please reopen the preview.".into());
            }
            self.add(&section, index)?;
            if let Some(target) = select(self.entries[&index].iter().filter(|t| target_has_node(&section, t, &node)).cloned(), &job.key)? {
                return Ok(target);
            }
            if job.destination.is_some() { return Err("The figure/table changed. Please reopen the preview.".into()); }
        }
        let origin_index = source
            .book()
            .sections
            .iter()
            .position(|s| s.id == job.origin.start.spine)
            .ok_or("The reference section is unavailable.")?;
        let origin = source
            .parse_section(origin_index)
            .map_err(|e| e.to_string())?;
        self.add(&origin, origin_index)?;
        if let Some(href) = authored_link(&origin, &job.origin, &job.key)? {
            let mut candidates = Vec::new();
            let mut document = Vec::new();
            let mut anchor_exists = href.fragment().is_none();
            let mut media_anchor = false;
            for (index, _) in source.book().sections.iter().enumerate().filter(|(_, s)| s.href.path() == href.path()) {
                if !needed() { return Err("Cancelled".into()); }
                let section = if index == origin_index { origin.clone() }
                    else { source.parse_section(index).map_err(|e| e.to_string())? };
                self.add(&section, index)?;
                anchor_exists |= section.anchors.iter().any(|anchor| Some(anchor.fragment.as_str()) == href.fragment());
                for target in &self.entries[&index] {
                    if target_has_anchor(&section, target, &href) {
                        media_anchor = true;
                        candidates.push(target.clone());
                    }
                    document.push(target.clone());
                }
            }
            if let Some(target) = select(candidates.into_iter(), &job.key)? {
                return Ok(target);
            }
            if anchor_exists && !media_anchor
                && let Some(target) = select(document.into_iter(), &job.key)?
            {
                return Ok(target);
            }
            return Err("The authored media target is unavailable.".into());
        }
        let local: Vec<_> = source.book().sections.iter().enumerate()
            .filter(|(_, s)| s.href.path() == source.book().sections[origin_index].href.path())
            .map(|(index, _)| index).collect();
        for index in &local {
            if !needed() { return Err("Cancelled".into()); }
            if !self.entries.contains_key(index) { self.add(&source.parse_section(*index).map_err(|e| e.to_string())?, *index)?; }
        }
        if let Some(target) = select(local.iter().flat_map(|index| &self.entries[index]).cloned(), &job.key)? {
            return Ok(target);
        }
        let scope = chapter_scope(source, origin_index);
        for index in scope.clone() {
            if !needed() {
                return Err("Cancelled".into());
            }
            if !self.entries.contains_key(&index) {
                self.add(
                    &source.parse_section(index).map_err(|e| e.to_string())?,
                    index,
                )?;
            }
        }
        if let Some(target) = select(
            scope.flat_map(|index| &self.entries[&index]).cloned(),
            &job.key,
        )? {
            return Ok(target);
        }
        // Build remaining metadata incrementally on demand. Every iteration is cancellable;
        // parsed sections are dropped immediately and no image is decoded here.
        for index in 0..source.book().sections.len() {
            if !needed() {
                return Err("Cancelled".into());
            }
            if self.entries.contains_key(&index) {
                continue;
            }
            let section = source.parse_section(index).map_err(|e| e.to_string())?;
            self.add(&section, index)?;
        }
        select(self.entries.values().flatten().cloned(), &job.key)?
            .ok_or_else(|| "No matching figure/table was found in this book.".into())
    }
}

/// Bound each table shaping job before layout sees the rows. Never cut a rowspan.
#[cfg(test)]
fn table_chunks(table: &TableBlock) -> Result<Vec<Range<usize>>, String> {
    table_chunks_rows(&table.rows)
}

fn table_chunks_rows(rows: &[rebook_publication::TableRow]) -> Result<Vec<Range<usize>>, String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < rows.len() {
        let mut end = (start + 24).min(rows.len());
        let mut row = start;
        while row < end {
            for cell in &rows[row].cells {
                end = end.max(
                    row.saturating_add(usize::from(cell.row_span.max(1)))
                        .min(rows.len()),
                );
            }
            if end - start > 128 {
                return Err("A table row group is too large to preview safely.".into());
            }
            row += 1;
        }
        chunks.push(start..end);
        start = end;
    }
    Ok(chunks)
}

#[derive(Clone, Copy, Default)]
struct Cursor {
    chunk: usize,
    page: usize,
}
struct Job {
    generation: u64,
    source: Arc<dyn BookSource>,
    catalog: Option<Arc<super::media_links::Source>>,
    destination: Option<(usize, String)>,
    origin: SourceRange,
    key: MediaKey,
    cursor: Cursor,
    style: ReaderStyle,
    ctx: egui::Context,
}
struct Ready {
    image: egui::ColorImage,
    size: [usize; 2],
    captions: Vec<String>,
    caption_images: Vec<egui::ColorImage>,
    previous: Option<Cursor>,
    next: Option<Cursor>,
    position: String,
}
struct Completion {
    generation: u64,
    revision: Option<u64>,
    result: Result<Ready, String>,
}
struct Worker {
    tx: mpsc::SyncSender<()>,
    pending: Arc<arc_swap::ArcSwapOption<Job>>,
    result: Arc<arc_swap::ArcSwapOption<Completion>>,
    generation: Arc<AtomicU64>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::Release);
        self.pending.store(None);
    }
}

fn render(job: &Job, target: Target, needed: &impl Fn() -> bool) -> Result<Ready, String> {
    let section = job
        .source
        .parse_section(target.section)
        .map_err(|e| e.to_string())?;
    if !needed() {
        return Err("Cancelled".into());
    }
    // Sources without a revision can still receive semantic/translation updates
    // between lookup and rendering. Never reuse a shifted block ordinal.
    let direct = job.key.number.is_empty() && job.destination.as_ref().is_some_and(|(index, node)| {
        *index == target.section && section.blocks.get(target.block).is_some_and(|block|
            matches!(block, Block::Figure(_) | Block::Image(_) | Block::Table(_))
            && super::block_source_range(block).is_some_and(|source| &source.start.node == node))
    });
    if !direct && !targets(&section, target.section).contains(&target) {
        return Err("The figure/table changed. Please reopen the preview.".into());
    }
    let block = section
        .blocks
        .get(target.block)
        .ok_or("The figure/table changed. Please reopen the preview.")?;
    let mut caption_blocks: Vec<TextBlock> = Vec::new();
    if matches!(block, Block::Figure(_) | Block::Table(_)) {
        caption_blocks.extend(target.captions.iter().filter(|i| **i < target.block).filter_map(|i| match &section.blocks[*i] {
            Block::Text(text) => Some(text.clone()), _ => None,
        }));
    }
    let (chunk, chunk_count, blocks) = match block {
        Block::Figure(figure) => {
            if let Some(Block::Figure(parent)) = target.group.and_then(|at| section.blocks.get(at)) {
                caption_blocks.extend(parent.caption_blocks().filter(|text| media_references::caption_label(&plain(text)).is_some()).cloned());
            }
            caption_blocks.extend(figure.caption_blocks().cloned());
            if !figure.body.is_empty() {
                if figure.body.len() > 4096
                    || figure.body.iter().flat_map(Block::text_blocks)
                        .map(|text| plain(text).len()).sum::<usize>() > 512 * 1024
                {
                    return Err("This figure is too large to preview safely.".into());
                }
                let body: Vec<_> = figure.body.iter().filter(|block| {
                    !matches!(block, Block::Text(text) if matches!(text.kind,
                        rebook_publication::TextBlockKind::Caption
                            | rebook_publication::TextBlockKind::QuoteAttribution))
                }).cloned().collect();
                (0, 1, vec![Block::Figure(rebook_publication::FigureBlock {
                    body,
                    images: Vec::new(),
                    captions: Vec::new(),
                    caption_position: figure.caption_position,
                    style: figure.style,
                    source: figure.source.clone(),
                })])
            } else {
                let at = job.cursor.chunk.min(figure.images.len().saturating_sub(1));
                (
                    at,
                    figure.images.len(),
                    vec![Block::Image(
                        figure.images.get(at).ok_or("The figure has no image.")?.clone(),
                    )],
                )
            }
        }
        Block::Table(table) => {
            let presentation = rebook_layout::table_presentation(table, true);
            caption_blocks.extend(presentation.before.iter().chain(presentation.after.iter()).cloned());
            let rows = &table.rows[usize::from(presentation.skip_title)..table.rows.len() - presentation.skip_sources];
            let ranges = table_chunks_rows(rows)?;
            let at = job.cursor.chunk.min(ranges.len().saturating_sub(1));
            let range = ranges.get(at).ok_or("The table is empty.")?.clone();
            let mut part = TableBlock {
                before: vec![],
                after: vec![],
                rows: rows[range].to_vec(),
                source: table.source.clone(),
            };
            if at > 0 {
                let headers: Vec<_> = rows
                    .iter()
                    .take(8)
                    .take_while(|row| {
                        !row.cells.is_empty()
                            && row
                                .cells
                                .iter()
                                .all(|cell| cell.header && cell.row_span == 1)
                    })
                    .cloned()
                    .collect();
                part.rows.splice(0..0, headers);
            }
            if part
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .map(|c| plain(&c.text).len())
                .sum::<usize>()
                > 512 * 1024
            {
                return Err("This table segment is too large to preview safely.".into());
            }
            (at, ranges.len(), vec![Block::Table(part)])
        }
        Block::Text(_) | Block::Image(_) if media_references::equation_target(block).is_some() => {
            (0, 1, vec![block.clone()])
        }
        Block::Text(_) | Block::Image(_) => {
            caption_blocks.extend(target.captions.iter().filter_map(|i| {
                match &section.blocks[*i] {
                    Block::Text(t) => Some(t.clone()),
                    _ => None,
                }
            }));
            let at = job.cursor.chunk.min(target.images.len().saturating_sub(1));
            (
                at,
                target.images.len(),
                vec![
                    section.blocks[*target.images.get(at).ok_or("The figure has no image.")?]
                        .clone(),
                ],
            )
        }
        _ => return Err("The figure/table changed. Please reopen the preview.".into()),
    };
    if matches!(block, Block::Figure(_) | Block::Table(_)) {
        caption_blocks.extend(target.captions.iter().filter(|i| **i > target.block).filter_map(|i| match &section.blocks[*i] {
            Block::Text(text) => Some(text.clone()), _ => None,
        }));
    }
    let mut seen = Vec::new();
    caption_blocks.retain(|text| {
        let identity = (text.source.clone(), caption_text(text));
        if seen.contains(&identity) { false } else { seen.push(identity); true }
    });
    let mut style = job.style.clone();
    style.typesetting.mode = rebook_layout::TypesettingMode::Unified;
    style.spread = SpreadMode::Single;
    style.focus_footnote_icons = false;
    style.website_icons = false;
    style.horizontal_margin = 24.0;
    style.minimum_horizontal_margin = 0.0;
    style.minimum_content_width = 0.0;
    style.top_margin = 24.0;
    style.bottom_margin = 24.0;
    style.foreground = rebook_publication::Rgba::BLACK;
    style.background = rebook_publication::Rgba {
        red: 255,
        green: 255,
        blue: 255,
        alpha: 255,
    };
    style.typography.font_size = 22.0;
    let height = if target.key.kind == MediaKind::Table {
        2048
    } else {
        1400
    };
    let mut engine =
        LayoutEngine::with_fonts(crate::fonts::reader_fonts().iter().cloned());
    engine.set_deferred_images(true);
    let layout = engine
        .layout_blocks(
            job.source.as_ref(),
            &blocks,
            LayoutViewport::new(1200, height).map_err(|e| e.to_string())?,
            &style,
        )
        .map_err(|e| e.to_string())?;
    if !needed() {
        return Err("Cancelled".into());
    }
    let at = job.cursor.page.min(layout.pages.len().saturating_sub(1));
    let page = layout.pages.get(at).ok_or("The preview is empty.")?;
    let display = rebook_renderer::DisplayListCompiler.compile(page);
    for (request, _) in display.deferred_rasters() {
        request
            .load_guarded(job.source.as_ref(), needed)
            .map_err(|e| e.to_string())?;
    }
    if !needed() {
        return Err("Cancelled".into());
    }
    let surface = matches!(block, Block::Figure(figure) if !figure.body.is_empty())
        .then(|| page.items.iter().find_map(|item| match item {
            rebook_layout::PageItem::FigureBody(body) => Some(body), _ => None,
        })).flatten();
    // Fit the preview to the actual figure surface, including its padding,
    // rather than retaining an entire reading column around a short diagram.
    let top = surface.map_or(0.0, |body| body.y);
    let height = surface.map_or_else(|| display.content_bottom().unwrap_or(100.0) + 8.0, |body| body.height)
        .ceil().clamp(1.0, height as f32) as usize;
    let width = surface.map_or_else(|| rebook_layout::reading_content_width(1200.0, &style).ceil() as usize + 32, |body| body.width.ceil() as usize);
    let left = surface.map_or_else(|| rebook_layout::reading_content_left(1200.0, &style) - 16.0, |body| body.x);
    if width * height * 4 > PIXEL_BUDGET {
        return Err("The preview exceeds the image memory limit.".into());
    }
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width as u32, height as u32);
    let mut pixels = Vec::new();
    renderer.render_to_vec(
        |scene| display.paint_scaled_at(scene, 1.0, -left, -top),
        &mut pixels,
    );
    let image = egui::ColorImage::from_rgba_premultiplied([width, height], &pixels);
    drop(pixels);
    let media_width = page.items.iter().filter_map(|item| match item {
        rebook_layout::PageItem::Image(image) => Some(image.width),
        rebook_layout::PageItem::FigureBody(body) => Some(body.width),
        rebook_layout::PageItem::Table(table) => {
            let left = table.cells.iter().map(|cell| cell.x).fold(f32::INFINITY, f32::min);
            let right = table.cells.iter().map(|cell| cell.x + cell.width).fold(0.0, f32::max);
            Some((right - left).max(1.0))
        }
        _ => None,
    }).fold(1.0, f32::max);
    let captions = caption_blocks.iter().map(caption_text).collect();
    let mut caption_style = style.clone();
    caption_style.typography.font_size = 20.0;
    caption_style.horizontal_margin = 12.0;
    caption_style.top_margin = 8.0;
    caption_style.bottom_margin = 8.0;
    for caption in &mut caption_blocks {
        let text = caption_text(caption).trim_start().to_lowercase();
        if ["source:", "sources:", "source：", "credit:", "credits:", "credit：", "来源:", "来源：", "图片来源:", "图片来源："]
            .iter().any(|prefix| text.starts_with(prefix))
        {
            caption.kind = rebook_publication::TextBlockKind::QuoteAttribution;
        } else if caption.kind != rebook_publication::TextBlockKind::QuoteAttribution {
            caption.kind = rebook_publication::TextBlockKind::Caption;
        }
    }
    let caption_layout = engine
        .layout_media_captions(
            job.source.as_ref(),
            &caption_blocks,
            LayoutViewport::new(width as u32, 2048).map_err(|e| e.to_string())?,
            &caption_style,
            media_width,
        )
        .map_err(|e| e.to_string())?;
    let mut caption_images = Vec::new();
    let mut retained_bytes = image.pixels.len() * 4;
    for page in &caption_layout.pages {
        if !needed() {
            return Err("Cancelled".into());
        }
        let display = rebook_renderer::DisplayListCompiler.compile(page);
        let Some(bottom) = display.content_bottom() else {
            continue;
        };
        let height = (bottom + 8.0).ceil().clamp(1.0, 2048.0) as usize;
        retained_bytes += width * height * 4;
        if retained_bytes > PIXEL_BUDGET {
            return Err("The caption exceeds the preview memory limit.".into());
        }
        for (request, _) in display.deferred_rasters() {
            request
                .load_guarded(job.source.as_ref(), needed)
                .map_err(|e| e.to_string())?;
        }
        let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width as u32, height as u32);
        let mut pixels = Vec::new();
        renderer.render_to_vec(|scene| display.paint(scene), &mut pixels);
        caption_images.push(egui::ColorImage::from_rgba_premultiplied(
            [width, height],
            &pixels,
        ));
    }
    let previous = if at > 0 {
        Some(Cursor {
            chunk,
            page: at - 1,
        })
    } else if chunk > 0 {
        Some(Cursor {
            chunk: chunk - 1,
            page: usize::MAX,
        })
    } else {
        None
    };
    let next = if at + 1 < layout.pages.len() {
        Some(Cursor {
            chunk,
            page: at + 1,
        })
    } else if chunk + 1 < chunk_count {
        Some(Cursor {
            chunk: chunk + 1,
            page: 0,
        })
    } else {
        None
    };
    Ok(Ready {
        image,
        size: [width, height],
        captions,
        caption_images,
        previous,
        next,
        position: format!(
            "{} / {} · {} / {}",
            chunk + 1,
            chunk_count,
            at + 1,
            layout.pages.len()
        ),
    })
}

impl Worker {
    fn new() -> Self {
        let (tx, wake) = mpsc::sync_channel::<()>(1);
        let pending = Arc::new(arc_swap::ArcSwapOption::empty());
        let jobs: Arc<arc_swap::ArcSwapOption<Job>> = pending.clone();
        let complete = Arc::new(arc_swap::ArcSwapOption::empty());
        let result = complete.clone();
        let generation = Arc::new(AtomicU64::new(0));
        let epoch = generation.clone();
        std::thread::Builder::new()
            .name("media-preview".into())
            .spawn(move || {
                let mut index = Index::default();
                let mut identity = None;
                while wake.recv().is_ok() {
                    let Some(job) = jobs.swap(None) else { continue };
                    let Ok(job) = Arc::try_unwrap(job) else {
                        continue;
                    };
                    let key = (
                        job.source.book().id.clone(),
                        job.source.content_revision(),
                        Arc::as_ptr(&job.source) as *const () as usize,
                    );
                    if identity.as_ref() != Some(&key) || key.1.is_none() {
                        index = Index::default();
                        identity = Some(key);
                    }
                    let needed = || epoch.load(Ordering::Acquire) == job.generation;
                    if !needed() {
                        continue;
                    }
                    let revision = job.source.content_revision();
                    let result = index
                        .resolve(&job, &needed)
                        .and_then(|target| render(&job, target, &needed));
                    if needed() {
                        complete.store(Some(Arc::new(Completion {
                            generation: job.generation,
                            revision,
                            result,
                        })));
                        job.ctx.request_repaint();
                    }
                }
            })
            .expect("media preview worker");
        Self {
            tx,
            pending,
            result,
            generation,
        }
    }
}

#[derive(Clone)]
struct PreviewItem {
    origin: SourceRange,
    key: MediaKey,
    destination: Option<(usize, String)>,
}

struct Preview {
    motion: PreviewMotion,
    content_motion: Option<PreviewMotion>,
    origin: SourceRange,
    key: MediaKey,
    texture: Option<egui::TextureHandle>,
    captions: Vec<String>,
    caption_textures: Vec<egui::TextureHandle>,
    size: egui::Vec2,
    zoom: f32,
    pan: egui::Vec2,
    previous: Option<Cursor>,
    next: Option<Cursor>,
    error: Option<String>,
    loading: bool,
    items: Vec<PreviewItem>,
    item_index: usize,
    displayed_index: usize,
    position: String,
    multipart: bool,
}
#[derive(Default)]
pub(super) struct MediaPreviewController {
    pub(super) links: Option<Arc<super::media_links::Source>>,
    pub(super) links_revision: u64,
    worker: Option<Worker>,
    active: Option<Preview>,
    restore_parent: bool,
}

impl MediaPreviewController {
    pub(super) fn with_links(links: Option<Arc<super::media_links::Source>>) -> Self {
        Self { links, ..Default::default() }
    }
}

impl super::DesktopReader {
    pub(super) fn preview_is_open(&self) -> bool {
        self.image_preview.is_some() || self.media_preview.active.is_some()
    }
    pub(super) fn media_preview_is_open(&self) -> bool {
        self.media_preview.active.is_some()
    }
    pub(super) fn preserve_media_parent_popup(&mut self) -> bool {
        if !self.ui.focus_footnotes_visible {
            self.media_preview.restore_parent = false;
        }
        self.media_preview.active.is_some()
            || (self.media_preview.restore_parent && self.ui.focus_footnotes_visible)
    }
    pub(super) fn media_preview_copy(&self, ctx: &egui::Context) -> bool {
        let Some(active) = &self.media_preview.active else {
            return false;
        };
        if active.motion.is_closing() { return true; }
        if ctx.input_mut(|i| i.consume_shortcut(&self.shortcuts.copy)) {
            ctx.copy_text(active.captions.join("\n"));
        }
        true
    }
    pub(super) fn open_media_preview(
        &mut self,
        ctx: &egui::Context,
        origin: SourceRange,
        key: MediaKey,
    ) {
        let mut items = self.focus_reference_previews(&origin);
        let destination = self.media_preview.links.as_ref().and_then(|links| links.target_node(&origin, &key));
        let index = items.iter().position(|item| item.key == key && item.origin.start.node == origin.start.node)
            .or_else(|| destination.as_ref().and_then(|destination| items.iter().position(|item| item.key == key && item.destination.as_ref() == Some(destination))))
            .or_else(|| destination.as_ref().and_then(|destination| {
                let mut matches = items.iter().enumerate().filter(|(_,item)| item.destination.as_ref() == Some(destination));
                let first = matches.next()?;
                matches.next().is_none().then_some(first.0)
            }));
        let index = match index {
            Some(index) => index,
            None => {
                items = vec![PreviewItem { origin, key, destination: None }];
                0
            }
        };
        self.open_media_items(ctx, items, index);
    }

    /// Use already loaded IR and stable source identity. Parsing and raster work
    /// stay on the preview worker, including unnumbered tables and figures.
    pub(super) fn open_current_media_preview(&mut self, ctx: &egui::Context) -> bool {
        let Some(unit) = self.focus_units.get(self.focus_unit_index) else { return false; };
        let Some(sections) = self.reader.cached_reading_unit_content() else { return false; };
        let found = sections.iter().find_map(|(index, section)| section.blocks.iter().find_map(|block| {
            if !matches!(block, Block::Figure(_) | Block::Table(_)) { return None; }
            let origin = super::block_source_range(block)?;
            if origin.start.spine != unit.range.start.spine || origin.start.node != unit.range.start.node { return None; }
            let kind = if matches!(block, Block::Table(_)) { MediaKind::Table } else { MediaKind::Figure };
            Some((*index, origin.clone(), kind))
        }));
        let Some((index, origin, kind)) = found else { return false; };
        let destination = Some((index, origin.start.node.clone()));
        let key = unit.media_key.clone().unwrap_or(MediaKey { kind, number: String::new() });
        self.open_media_items(ctx, vec![PreviewItem { origin, key, destination }], 0);
        true
    }

    pub(super) fn open_associated_image_preview(
        &mut self,
        ctx: &egui::Context,
        image: &rebook_reader::ReaderImage,
    ) -> bool {
        let Some(raster) = &image.origin else {
            return false;
        };
        let Some(sections) = self.reader.cached_reading_unit_content() else {
            return false;
        };
        let Some((section, target)) = sections.iter().find_map(|(index, section)| {
            (*index == image.position.section_index)
                .then(|| image_target(section, *index, &raster.href, image.source.as_ref()))
                .flatten()
                .map(|target| (section, target))
        }) else {
            return false;
        };
        let Some(origin) = super::block_source_range(&section.blocks[target.block]).cloned() else {
            return false;
        };
        let destination = Some((target.section, origin.start.node.clone()));
        self.open_media_items(
            ctx,
            vec![PreviewItem {
                origin,
                key: target.key,
                destination,
            }],
            0,
        );
        true
    }

    fn focus_reference_previews(&self, origin: &SourceRange) -> Vec<PreviewItem> {
        if !self.is_focus_mode() { return vec![]; }
        let Some(unit) = self.focus_units.iter().find(|unit| unit.paint_ranges.iter()
            .any(|range| super::source_range_contains_anchor(range, &origin.start))) else { return vec![] };
        if unit.is_image || unit.is_table || unit.media_key.is_some() { return vec![]; }
        let ranges = unit.list_group.as_ref().map_or(unit.paint_ranges.as_slice(), |group| group.paint_ranges.as_slice());
        let Some(links) = &self.media_preview.links else { return vec![] };
        let Ok(sections) = self.reader.current_reading_unit_content() else { return vec![] };
        let mut seen = Vec::new();
        let mut items = Vec::new();
        let mut bytes = 0;
        for text in sections.iter().flat_map(|(_, s)| &s.blocks).flat_map(Block::text_blocks) {
            let Some(source) = &text.source else { continue };
            if !ranges.iter().any(|range| super::source_range_contains_anchor(range, &source.start)
                || super::source_range_contains_anchor(source, &range.start)) { continue; }
            for (key, target, node) in links.preview_references(source, &text.content) {
                if seen.iter().any(|old: &Target| old.section == target.section && old.block == target.block
                    && old.images == target.images && old.captions == target.captions) { continue; }
                bytes += target.retained_bytes() + key.number.len() + node.as_ref().map_or(0, String::len) + 256;
                if bytes > INDEX_BUDGET { return items; }
                let destination = node.map(|node| (target.section, node));
                seen.push(target);
                items.push(PreviewItem { origin: source.clone(), key, destination });
            }
        }
        items
    }

    pub(super) fn open_focus_reference_preview(&mut self, ctx: &egui::Context) -> bool {
        let Some(unit) = self.focus_units.get(self.focus_unit_index) else { return false };
        let items = self.focus_reference_previews(&unit.range);
        if items.is_empty() { return false; }
        self.open_media_items(ctx, items, 0);
        true
    }

    fn open_media_items(&mut self, ctx: &egui::Context, items: Vec<PreviewItem>, index: usize) {
        let Some(item) = items.get(index) else { return };
        self.ui.focus_navigation_repeat = Default::default();
        self.ui.focus_wheel.reset();
        self.ui.wheel_accumulator = 0.0;
        self.media_preview.restore_parent = self.ui.focus_footnotes_visible;
        self.media_preview.active = Some(Preview {
            motion: PreviewMotion::new(ctx),
            content_motion: None,
            origin: item.origin.clone(),
            key: item.key.clone(),
            texture: None,
            captions: vec![],
            caption_textures: vec![],
            size: egui::Vec2::ZERO,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            previous: None,
            next: None,
            error: None,
            loading: true,
            items,
            item_index: index,
            displayed_index: index,
            position: String::new(),
            multipart: false,
        });
        self.request_media_page(ctx, Cursor::default());
    }
    fn request_media_page(&mut self, ctx: &egui::Context, cursor: Cursor) {
        let worker = self.media_preview.worker.get_or_insert_with(Worker::new);
        // One pending request and one result slot bound rapid close/reopen cycles.
        worker.result.store(None);
        let Some(active) = &mut self.media_preview.active else {
            return;
        };
        active.loading = true;
        active.error = None;
        active.previous = None;
        active.next = None;
        let generation = worker.generation.fetch_add(1, Ordering::AcqRel) + 1;
        worker.pending.store(Some(Arc::new(Job {
            generation,
            source: self.source.clone(),
            catalog: self.media_preview.links.clone(),
            destination: active.items.get(active.item_index).and_then(|item| item.destination.clone()),
            origin: active.origin.clone(),
            key: active.key.clone(),
            cursor,
            style: self.reader.style(),
            ctx: ctx.clone(),
        })));
        let _ = worker.tx.try_send(());
        ctx.request_repaint();
    }

    fn step_media_item(&mut self, ctx: &egui::Context, direction: isize) {
        let Some(active) = &mut self.media_preview.active else { return };
        let Some(index) = active.item_index.checked_add_signed(direction).filter(|i| *i < active.items.len()) else { return };
        active.item_index = index;
        active.origin = active.items[index].origin.clone();
        active.key = active.items[index].key.clone();
        self.request_media_page(ctx, Cursor::default());
    }
    pub(super) fn close_media_preview(&mut self, ctx: &egui::Context) {
        let Some(active) = &mut self.media_preview.active else { return };
        if active.motion.is_closing() { return; }
        active.motion.close(ctx);
        if let Some(motion) = &mut active.content_motion { motion.close(ctx); }
        if let Some(worker) = &self.media_preview.worker {
            worker.generation.fetch_add(1, Ordering::Release);
            worker.pending.store(None);
            worker.result.store(None);
        }
        ctx.request_repaint();
    }
    pub(super) fn media_preview_keyboard(&mut self, ctx: &egui::Context) -> bool {
        if self.media_preview.active.is_none() {
            return false;
        }
        if self.media_preview.active.as_ref().is_some_and(|active| active.motion.is_closing()) {
            return true;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
            || i.consume_shortcut(&self.shortcuts.focus_image_preview)) {
            self.close_media_preview(ctx);
            return true;
        }
        let direction = ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                -1
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                1
            } else {
                0
            }
        });
        if direction != 0 {
            self.step_media_item(ctx, direction);
        }
        let part = ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) { -1 }
            else if i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) { 1 }
            else { 0 }
        });
        if part != 0
            && let Some(active) = &self.media_preview.active
            && !active.loading
        {
            let cursor = if part < 0 {
                active.previous
            } else {
                active.next
            };
            if let Some(cursor) = cursor {
                self.request_media_page(ctx, cursor);
            }
        }
        let zoom = ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Minus) {
                0.8
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Equals) {
                1.25
            } else {
                1.0
            }
        });
        if zoom != 1.0
            && let Some(active) = &mut self.media_preview.active
        {
            active.zoom = (active.zoom * zoom).clamp(1.0, 16.0);
            ctx.request_repaint();
        }
        true
    }
    fn accept_media_preview_completion(&mut self, ctx: &egui::Context) {
        if let Some(worker) = &self.media_preview.worker {
            if let Some(completion) = worker.result.swap(None) {
                if completion.generation != worker.generation.load(Ordering::Acquire) {
                    return;
                }
                let Ok(completion) = Arc::try_unwrap(completion) else {
                    return;
                };
                let Some(active) = &mut self.media_preview.active else {
                    return;
                };
                if active.motion.is_closing() { return; }
                active.loading = false;
                if completion.revision != self.source.content_revision() {
                    active.texture = None;
                    active.caption_textures.clear();
                    active.error =
                        Some("The book content changed. Please reopen the preview.".into());
                    return;
                }
                match completion.result {
                    Ok(ready) => {
                        active.content_motion = active.texture.is_none().then(|| PreviewMotion::new(ctx));
                        active.displayed_index = active.item_index;
                        active.position = ready.position;
                        active.multipart = ready.previous.is_some() || ready.next.is_some();
                        active.zoom = 1.0;
                        active.pan = egui::Vec2::ZERO;
                        active.size = egui::vec2(ready.size[0] as f32, ready.size[1] as f32);
                        active.texture = Some(ctx.load_texture(
                            "media-preview",
                            ready.image,
                            egui::TextureOptions::LINEAR,
                        ));
                        active.captions = ready.captions;
                        active.caption_textures = ready
                            .caption_images
                            .into_iter()
                            .map(|image| {
                                ctx.load_texture(
                                    "media-caption",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                )
                            })
                            .collect();
                        active.previous = ready.previous;
                        active.next = ready.next;
                    }
                    Err(error) => {
                        active.texture = None;
                        active.caption_textures.clear();
                        active.captions.clear();
                        active.multipart = false;
                        active.displayed_index = active.item_index;
                        active.error = Some(error);
                    }
                }
            }
        }
    }

    pub(super) fn media_preview_overlay(&mut self, ctx: &egui::Context) {
        self.accept_media_preview_completion(ctx);
        let language = self.language;
        let Some(active) = &mut self.media_preview.active else {
            return;
        };
        let opacity = active.motion.opacity(ctx);
        if active.motion.is_closing() && opacity <= f32::EPSILON {
            self.media_preview.active = None;
            // The parent footnote popup is earlier in the frame's paint order.
            ctx.request_repaint();
            return;
        }
        let content_opacity = active.content_motion.as_ref().map_or(1.0, |motion| motion.opacity(ctx));
        let screen = crate::ui::overlay_rect(ctx);
        let available = if active.items.len() > 1 {
            screen.shrink2(egui::vec2(64.0, 48.0))
        } else { screen.shrink(32.0) };
        let mut close = false;
        let mut navigate = None;
        let mut item_direction = 0;
        #[cfg(target_os = "windows")]
        crate::app::window_chrome::dim_controls(ctx, 190.0 / 255.0 * opacity);
        egui::Area::new(egui::Id::unique("reader-media-preview"))
            .order(egui::Order::Tooltip)
            .fade_in(false)
            .fixed_pos(ctx.content_rect().min)
            .constrain(false)
            .show(ctx, |ui| {
                if active.motion.is_closing() { ui.disable(); }
                ui.set_opacity(opacity);
                ui.set_clip_rect(ctx.content_rect());
                let (bounds, backdrop) =
                    ui.allocate_exact_size(ctx.content_rect().size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(bounds, 0.0, egui::Color32::from_black_alpha(190));
                if let Some(texture) = &active.texture {
                    let captions: Vec<_> = active
                        .caption_textures
                        .iter()
                        .map(egui::TextureHandle::size_vec2)
                        .collect();
                    let natural = media_paper_size(active.size, &captions);
                    let fit = crate::ui::image_preview::preview_fit_scale(
                        natural,
                        available.size().max(egui::Vec2::splat(1.0)),
                    );
                    let wheel = ctx.input(|i| i.smooth_scroll_delta.y);
                    if available
                        .contains(ctx.input(|i| i.pointer.hover_pos()).unwrap_or(screen.min))
                        && wheel != 0.0
                        && !active.motion.is_closing()
                    {
                        active.zoom = (active.zoom * (wheel * 0.002).exp()).clamp(1.0, 16.0);
                    }
                    let scale = fit * active.zoom;
                    let size = natural * scale;
                    let limit = (size - available.size()).max(egui::Vec2::ZERO) * 0.5;
                    active.pan = active.pan.clamp(-limit, limit);
                    let paper = egui::Rect::from_center_size(available.center() + active.pan, size);
                    let mut painter = ui.painter().with_clip_rect(available);
                    painter.multiply_opacity(content_opacity);
                    painter.rect_filled(paper, 0.0, egui::Color32::WHITE);
                    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
                    let mut top = paper.top();
                    for (texture, natural_size) in std::iter::once((texture, active.size))
                        .chain(active.caption_textures.iter().zip(captions.iter().copied()))
                    {
                        let size = natural_size * scale;
                        let rect = egui::Rect::from_min_size(
                            egui::pos2(paper.center().x - size.x * 0.5, top),
                            size,
                        );
                        painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
                        top = rect.bottom();
                    }
                    painter.rect_stroke(
                        paper,
                        2.0,
                        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(48)),
                        egui::StrokeKind::Inside,
                    );
                    let visible = paper.intersect(available);
                    let response = ui
                        .interact(
                            visible,
                            ui.scope_id().with("paper"),
                            egui::Sense::click_and_drag(),
                        )
                        .on_hover_cursor(egui::CursorIcon::Grab);
                    if response.dragged() {
                        active.pan += ctx.input(|i| i.pointer.delta());
                    }
                    if response.double_clicked() {
                        active.zoom = 1.0;
                        active.pan = egui::Vec2::ZERO;
                    }
                    close |= backdrop.clicked()
                        && backdrop
                            .interact_pointer_pos()
                            .is_some_and(|p| !visible.contains(p));
                } else {
                    let message = if active
                        .error
                        .as_ref()
                        .is_some_and(|error| error.contains("several"))
                    {
                        language.text(
                            "存在多个同号图表或公式，无法确定对应内容",
                            "Several figures, tables or equations share this number",
                        )
                    } else if active.error.is_some() {
                        language.text(
                            "未能预览对应图表或公式，请关闭后重试",
                            "Unable to preview this figure, table or equation",
                        )
                    } else {
                        language.text("正在加载…", "Loading…")
                    };
                    ui.painter().text(
                        available.center(),
                        egui::Align2::CENTER_CENTER,
                        message,
                        egui::FontId::proportional(16.0),
                        egui::Color32::WHITE,
                    );
                    close |= backdrop.clicked();
                }
                let mut over_navigation = false;
                if active.items.len() > 1 {
                    for (direction, x, glyph, hint) in [
                        (-1_isize, screen.left() + 28.0, crate::ui::Icon::ChevronLeft,
                            language.text("上一个引用（←）", "Previous reference (←)")),
                        (1, screen.right() - 28.0, crate::ui::Icon::ChevronRight,
                            language.text("下一个引用（→）", "Next reference (→)")),
                    ] {
                        if active.item_index.checked_add_signed(direction).is_none_or(|i| i >= active.items.len()) { continue; }
                        let rect = egui::Rect::from_center_size(egui::pos2(x, screen.center().y), egui::vec2(40.0, 56.0));
                        let response = ui.interact(rect, ui.scope_id().with(("reference", direction)), egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(hint);
                        ui.painter().rect_filled(rect, 20.0, egui::Color32::from_black_alpha(if response.hovered() { 110 } else { 55 }));
                        crate::ui::paint_icon(ui, egui::Rect::from_center_size(rect.center(), egui::vec2(26.0, 26.0)), glyph,
                            egui::Color32::from_white_alpha(if response.hovered() { 255 } else { 180 }));
                        over_navigation |= response.hovered() || response.clicked();
                        if response.clicked() { item_direction = direction; }
                    }
                    let shown = active.items.get(active.displayed_index);
                    if let Some(shown) = shown {
                        let kind = match shown.key.kind {
                            MediaKind::Figure => language.text("图", "Figure"),
                            MediaKind::Table => language.text("表", "Table"),
                            MediaKind::Equation => language.text("公式", "Equation"),
                        };
                        ui.painter().text(egui::pos2(screen.center().x, screen.bottom() - 42.0), egui::Align2::CENTER_CENTER,
                            format!("{kind} {} · {} / {}", shown.key.number, active.displayed_index + 1, active.items.len()),
                            egui::FontId::proportional(14.0), egui::Color32::from_white_alpha(210));
                    }
                }
                if active.multipart {
                    let y = screen.bottom() - 18.0;
                    ui.painter().text(egui::pos2(screen.center().x, y), egui::Align2::CENTER_CENTER,
                        format!("{} {}", language.text("部分／页", "Part / page"), active.position),
                        egui::FontId::proportional(12.0), egui::Color32::from_white_alpha(190));
                    for (cursor, x, glyph, hint) in [
                        (active.previous, screen.center().x - 145.0, crate::ui::Icon::ChevronLeft,
                            language.text("上一部分（PageUp）", "Previous part (PageUp)")),
                        (active.next, screen.center().x + 145.0, crate::ui::Icon::ChevronRight,
                            language.text("下一部分（PageDown）", "Next part (PageDown)")),
                    ] {
                        let Some(cursor) = cursor.filter(|_| !active.loading) else { continue };
                        let rect = egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(32.0, 24.0));
                        let response = ui.interact(rect, ui.scope_id().with(("part", x.to_bits())), egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(hint);
                        crate::ui::paint_icon(ui, rect.shrink2(egui::vec2(6.0, 2.0)), glyph, egui::Color32::WHITE);
                        over_navigation |= response.hovered() || response.clicked();
                        if response.clicked() { navigate = Some(cursor); }
                    }
                }
                if active.loading && active.texture.is_some() {
                    ui.painter().text(egui::pos2(screen.center().x, screen.top() + 18.0), egui::Align2::CENTER_CENTER,
                        language.text("正在加载…", "Loading…"), egui::FontId::proportional(13.0), egui::Color32::WHITE);
                }
                if over_navigation { close = false; }
            });
        if close {
            self.close_media_preview(ctx);
        } else if item_direction != 0 {
            self.step_media_item(ctx, item_direction);
        } else if let Some(cursor) = navigate {
            self.request_media_page(ctx, cursor);
        }
    }
}

fn media_paper_size(image: egui::Vec2, captions: &[egui::Vec2]) -> egui::Vec2 {
    egui::vec2(
        captions
            .iter()
            .fold(image.x, |width, caption| width.max(caption.x))
            .max(1.0),
        (image.y + captions.iter().map(|caption| caption.y).sum::<f32>()).max(1.0),
    )
}
