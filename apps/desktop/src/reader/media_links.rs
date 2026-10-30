//! A bounded, metadata-only catalog. Parsing runs on a worker; shaping and popup
//! code only read an immutable snapshot. Pending/missing/ambiguous links are prose.
use super::media_preview::{self, Target};
use rebook_publication::{
    media_references::{self, MediaKey, MediaKind, MediaReference},
    *,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const BUDGET: usize = 2 * 1024 * 1024;

#[cfg(test)]
#[path = "media_links_tests.rs"]
mod tests;

struct Entry {
    target: Target,
    node: Option<String>,
    anchors: Vec<String>,
}

#[derive(Default)]
struct Catalog {
    has_references: bool,
    revision: Option<u64>,
    entries: Vec<Entry>,
    by_key: HashMap<MediaKey, Vec<usize>>,
    // Authored links survive translation and popup flattening through source identity.
    links: HashMap<(usize, String), Vec<(MediaKey, PublicationUrl)>>,
    // Existing non-media anchors used by references. Keep only exceptional links,
    // rather than retaining every paragraph/page anchor in the book.
    document_links: HashSet<PublicationUrl>,
}

fn href_for(content: &[Inline], reference: &MediaReference) -> Result<Option<PublicationUrl>, ()> {
    let mut offset = 0;
    let mut found = None;
    for inline in content {
        if let Inline::Text(run) = inline {
            let end = offset + run.text.len();
            if offset < reference.range.end
                && end > reference.range.start
                && let Some(href) = run
                    .link
                    .as_ref()
                    .filter(|href| href.website_url().is_none())
            {
                if found.as_ref().is_some_and(|previous| previous != href) {
                    return Err(());
                }
                found = Some(href.clone());
            }
            offset = end;
        } else {
            offset += 1;
        }
    }
    Ok(found)
}

impl Catalog {
    fn build(source: &dyn BookSource, needed: impl Fn() -> bool) -> Result<Self, String> {
        let mut catalog = Self {
            revision: source.content_revision(),
            ..Self::default()
        };
        let mut bytes = 0;
        for index in 0..source.book().sections.len() {
            if !needed() {
                return Err("cancelled".into());
            }
            let section = source.parse_section(index).map_err(|e| e.to_string())?;
            for target in media_preview::targets(&section, index) {
                let anchors: Vec<_> = section
                    .anchors
                    .iter()
                    .filter(|anchor| {
                        let mut href = source.book().sections[index].href.clone();
                        // PublicationUrl owns canonical escaping and resource identity.
                        if let Ok(with_fragment) =
                            PublicationUrl::parse(&format!("{}#{}", href.path(), anchor.fragment))
                        {
                            href = with_fragment;
                        }
                        media_preview::target_has_anchor(&section, &target, &href)
                    })
                    .map(|anchor| anchor.fragment.clone())
                    .collect();
                let block = &section.blocks[target.block];
                let source = match block {
                    Block::Image(image) => image.source.as_ref(),
                    Block::Figure(figure) => figure.source.as_ref(),
                    Block::Table(table) => table.source.as_ref(),
                    _ => None,
                }
                .or_else(|| block.text_blocks().find_map(|text| text.source.as_ref()));
                let node = source.map(|source| source.start.node.clone());
                bytes += std::mem::size_of::<Entry>()
                    + target.retained_bytes()
                    + node.as_ref().map_or(0, String::len)
                    + anchors.iter().map(|a| a.len() + 24).sum::<usize>()
                    + 128;
                if bytes > BUDGET {
                    return Err("media catalog budget exceeded".into());
                }
                bytes += target.key.number.len() + 64;
                if bytes > BUDGET {
                    return Err("media catalog budget exceeded".into());
                }
                catalog
                    .by_key
                    .entry(target.key.clone())
                    .or_default()
                    .push(catalog.entries.len());
                catalog.entries.push(Entry {
                    target,
                    anchors,
                    node,
                });
            }
            for text in section.blocks.iter().flat_map(Block::text_blocks) {
                let Some(origin) = &text.source else {
                    continue;
                };
                for reference in media_references::inline_references(&text.content) {
                    catalog.has_references |= text.kind != TextBlockKind::Caption;
                    if let Ok(Some(href)) = href_for(&text.content, &reference) {
                        bytes += origin.start.node.len()
                            + reference.key.number.len()
                            + href.path().len()
                            + href.fragment().map_or(0, str::len)
                            + 160;
                        if bytes > BUDGET {
                            return Err("media catalog budget exceeded".into());
                        }
                        catalog
                            .links
                            .entry((index, origin.start.node.clone()))
                            .or_default()
                            .push((reference.key, href));
                    }
                }
            }
            // No section, image bytes, or layouts are retained by the catalog.
        }
        let mut pending = HashSet::new();
        for (key, href) in catalog.links.values().flatten() {
            let Some(fragment) = href.fragment() else { continue };
            if pending.contains(href)
                || catalog.entries.iter().any(|entry| {
                    source.book().sections[entry.target.section].href.path() == href.path()
                        && entry.anchors.iter().any(|anchor| anchor == fragment)
                })
            { continue; }
            let candidates = catalog.key_entries(key).into_iter().filter(|entry| {
                source.book().sections[entry.target.section].href.path() == href.path()
            }).map(|entry| entry.target.clone());
            if !matches!(media_preview::select(candidates, key), Ok(Some(_))) { continue; }
            bytes += href.path().len() + fragment.len() + 128;
            if bytes > BUDGET { return Err("media catalog budget exceeded".into()); }
            pending.insert(href.clone());
        }
        // Validate only the documents whose otherwise unique media references
        // missed a media anchor. This exceptional pass stays on the catalog worker.
        for (index, descriptor) in source.book().sections.iter().enumerate() {
            if !needed() { return Err("cancelled".into()); }
            let links: Vec<_> = pending.extract_if(|href| href.path() == descriptor.href.path()).collect();
            if links.is_empty() { continue; }
            let section = source.parse_section(index).map_err(|e| e.to_string())?;
            for href in links {
                if section.anchors.iter().any(|anchor| Some(anchor.fragment.as_str()) == href.fragment()) {
                    catalog.document_links.insert(href);
                }
            }
        }
        Ok(catalog)
    }

    fn key_entries(&self, key: &MediaKey) -> Vec<&Entry> {
        let fallback = key.number.strip_suffix(|c: char| c.is_ascii_alphabetic())
            .and_then(|number| self.by_key.get(&MediaKey { kind: key.kind, number: number.into() }));
        self.by_key.get(key).into_iter().chain(fallback).flatten()
            .map(|index| &self.entries[*index]).collect()
    }

    fn authored(
        &self,
        origin: usize,
        node: &str,
        key: &MediaKey,
    ) -> Result<Option<PublicationUrl>, ()> {
        let mut found = None;
        if let Some(links) = self.links.get(&(origin, node.to_owned())) {
            for (_, href) in links.iter().filter(|(k, _)| k == key) {
                if found.as_ref().is_some_and(|previous| previous != href) {
                    return Err(());
                }
                found = Some(href.clone());
            }
        }
        Ok(found)
    }

    fn resolve(
        &self,
        source: &dyn BookSource,
        origin: usize,
        key: &MediaKey,
        href: Option<&PublicationUrl>,
    ) -> Option<Target> {
        // Exact/subfigure preference is applied inside each document/chapter
        // scope, rather than letting a remote exact label defeat a local group.
        let entries = self.key_entries(key);
        let select = |entries: Vec<&Entry>| {
            media_preview::select(entries.into_iter().map(|entry| entry.target.clone()), key)
        };
        if let Some(href) = href {
            let document: Vec<_> = entries.iter().copied().filter(|entry| {
                source.book().sections[entry.target.section].href.path() == href.path()
            }).collect();
            let exact = select(document.iter().copied().filter(|entry| {
                href.fragment().is_some_and(|fragment| entry.anchors.iter().any(|anchor| anchor == fragment))
            }).collect());
            match exact {
                Ok(Some(target)) => return Some(target),
                Err(_) => return None,
                _ => {}
            }
            return (href.fragment().is_none() || self.document_links.contains(href))
                .then(|| select(document).ok().flatten()).flatten();
        }
        let path = source.book().sections.get(origin)?.href.path();
        let local = select(
            entries
                .iter()
                .copied()
                .filter(|entry| source.book().sections[entry.target.section].href.path() == path)
                .collect(),
        );
        match local {
            Ok(Some(target)) => return Some(target),
            Err(_) => return None,
            _ => {}
        }
        let scope = media_preview::chapter_scope(source, origin);
        let chapter = select(
            entries
                .iter()
                .copied()
                .filter(|entry| scope.contains(&entry.target.section))
                .collect(),
        );
        match chapter {
            Ok(Some(target)) => return Some(target),
            Err(_) => return None,
            _ => {}
        }
        select(entries).ok().flatten()
    }
}

pub(super) struct Source {
    inner: Arc<dyn BookSource>,
    original: Arc<dyn BookSource>,
    catalog: arc_swap::ArcSwapOption<Catalog>,
    generation: AtomicU64,
    ready: AtomicU64,
}

impl Source {
    /// Read the same validated links used by displayed prose; never parse a book here.
    pub(super) fn preview_references(&self, origin: &SourceRange, content: &[Inline])
        -> Vec<(MediaKey, Target, Option<String>)>
    {
        let snapshot = self.catalog.load();
        let Some(catalog) = snapshot.as_ref().filter(|c| c.revision == self.original.content_revision()) else { return vec![] };
        let Some(index) = self.book().sections.iter().position(|s| s.id == origin.start.spine) else { return vec![] };
        media_references::inline_references(content).into_iter()
            .filter(|r| matches!(r.key.kind, MediaKind::Figure | MediaKind::Table))
            .filter_map(|reference| {
                let href = href_for(content, &reference).and_then(|href| {
                    if href.is_some() { Ok(href) } else { catalog.authored(index, &origin.start.node, &reference.key) }
                }).ok()?;
                let target = catalog.resolve(self.original.as_ref(), index, &reference.key, href.as_ref())?;
                let node = catalog.entries.iter().find(|entry| entry.target == target).and_then(|entry| entry.node.clone());
                Some((reference.key, target, node))
            }).collect()
    }

    pub(super) fn changes_display(&self) -> bool {
        self.catalog
            .load()
            .as_ref()
            .is_some_and(|catalog| catalog.has_references && !catalog.entries.is_empty())
    }
    #[cfg(test)]
    pub(super) fn prepare_for_test(&self) {
        self.catalog.store(Some(Arc::new(
            Catalog::build(self.original.as_ref(), || true).unwrap(),
        )));
        self.ready.fetch_add(1, Ordering::Release);
    }
    pub(super) fn target_node(
        &self,
        origin: &SourceRange,
        key: &MediaKey,
    ) -> Option<(usize, String)> {
        let snapshot = self.catalog.load();
        let catalog = snapshot
            .as_ref()
            .filter(|catalog| catalog.revision == self.original.content_revision())?;
        let index = self
            .book()
            .sections
            .iter()
            .position(|section| section.id == origin.start.spine)?;
        let href = catalog.authored(index, &origin.start.node, key).ok()?;
        let target = catalog.resolve(self.original.as_ref(), index, key, href.as_ref())?;
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.target == target)?;
        Some((target.section, entry.node.clone()?))
    }
    pub(super) fn new(inner: Arc<dyn BookSource>, original: Arc<dyn BookSource>) -> Self {
        Self {
            inner,
            original,
            catalog: Default::default(),
            generation: AtomicU64::new(0),
            ready: AtomicU64::new(0),
        }
    }

    pub(super) fn start(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Runtime,
        proxy: &winit::event_loop::EventLoopProxy<crate::platform::UserEvent>,
    ) {
        let revision = self.original.content_revision();
        let generation = revision.map_or(1, |revision| revision.saturating_add(2));
        let previous = self.generation.load(Ordering::Acquire);
        if previous == generation
            || self
                .generation
                .compare_exchange(previous, generation, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        self.catalog.store(None);
        if previous != 0 {
            self.ready.fetch_add(1, Ordering::Release);
        }
        let weak = Arc::downgrade(self);
        let source = self.original.clone();
        let proxy = proxy.clone();
        runtime.spawn_blocking(move || {
            let needed = || weak.upgrade().is_some_and(|owner| owner.generation.load(Ordering::Acquire) == generation)
                && source.content_revision() == revision;
            let result = Catalog::build(source.as_ref(), needed);
            let Some(owner) = weak.upgrade().filter(|_| needed()) else { return; };
            match result {
                Ok(catalog) => owner.catalog.store(Some(Arc::new(catalog))),
                Err(error) => tracing::warn!(%error, "media reference catalog unavailable; keeping references as prose"),
            }
            owner.ready.fetch_add(1, Ordering::Release);
            let _ = proxy.send_event(crate::platform::UserEvent::RepaintAfter(std::time::Duration::ZERO));
        });
    }

    fn apply(&self, content: &mut Vec<Inline>, origin: Option<&SourceRange>) {
        let snapshot = self.catalog.load();
        let index = origin.and_then(|s| {
            self.book()
                .sections
                .iter()
                .position(|section| section.id == s.start.spine)
        });
        let refs = media_references::inline_references(content);
        if refs.is_empty()
            && !content
                .iter()
                .flat_map(Inline::text_runs)
                .any(|run| run.style.inline_citation != 0)
        {
            return;
        }
        let valid: Vec<_> = refs
            .iter()
            .filter_map(|reference| {
                let (Some(catalog), Some(index), Some(origin)) = (
                    snapshot
                        .as_ref()
                        .filter(|catalog| catalog.revision == self.original.content_revision()),
                    index,
                    origin,
                ) else {
                    return None;
                };
                let href = href_for(content, reference).and_then(|href| {
                    if href.is_some() {
                        Ok(href)
                    } else {
                        catalog.authored(index, &origin.start.node, &reference.key)
                    }
                });
                let href = href.ok()?;
                catalog.resolve(self.original.as_ref(), index, &reference.key, href.as_ref())?;
                Some((reference, href))
            })
            .collect();
        let mut groups: HashMap<u32, (String, std::ops::Range<usize>)> = HashMap::new();
        let mut offset = 0;
        for inline in content.iter() {
            if let Inline::Text(run) = inline {
                if run.style.inline_citation != 0 {
                    let group = groups
                        .entry(run.style.inline_citation)
                        .or_insert_with(|| (String::new(), offset..offset));
                    group.0.push_str(&run.text);
                    group.1.end = offset + run.text.len();
                }
                offset += run.text.len();
            } else {
                offset += 1;
            }
        }
        let restore: Vec<_> = groups
            .into_iter()
            .filter(|(_, (text, range))| {
                media_references::is_citation(text)
                    && !valid
                        .iter()
                        .any(|(r, _)| r.range.start < range.end && r.range.end > range.start)
            })
            .map(|(id, _)| id)
            .collect();
        offset = 0;
        let mut result = Vec::with_capacity(content.len());
        for inline in std::mem::take(content) {
            let Inline::Text(run) = inline else {
                offset += 1;
                result.push(inline);
                continue;
            };
            let len = run.text.len();
            let mut cuts = vec![0, len];
            for reference in &refs {
                for at in [reference.range.start, reference.range.end] {
                    if at > offset && at < offset + len {
                        cuts.push(at - offset);
                    }
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            for pair in cuts.windows(2) {
                let mut part = run.clone();
                part.text = run.text[pair[0]..pair[1]].into();
                if restore.contains(&part.style.inline_citation) {
                    part.style.inline_citation = 0;
                }
                if refs
                    .iter()
                    .any(|r| r.range.start <= offset + pair[0] && r.range.end >= offset + pair[1])
                {
                    let target = valid.iter().find(|(r, _)| {
                        r.range.start <= offset + pair[0] && r.range.end >= offset + pair[1]
                    });
                    part.style.suppress_media_references = target.is_none();
                    // Unified layout applies the final clickable brush. Original
                    // book layout must not acquire media links from this overlay.
                    part.style.media_reference = None;
                    if let Some((_, href)) = target {
                        if part.link.is_none() {
                            part.link = href.clone();
                        }
                    }
                }
                result.push(Inline::Text(part));
            }
            offset += len;
        }
        *content = result;
    }
}

impl BookSource for Source {
    fn inner_source(&self) -> Option<&dyn BookSource> {
        Some(self.inner.as_ref())
    }
    fn book(&self) -> &Book {
        self.inner.book()
    }
    fn content_revision(&self) -> Option<u64> {
        self.inner.content_revision()
    }
    fn media_reference_revision(&self) -> u64 {
        self.ready.load(Ordering::Acquire)
    }
    fn prepare_media_references(&self, content: &mut Vec<Inline>, origin: Option<&SourceRange>) {
        self.apply(content, origin);
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        let mut section = self.inner.parse_section(index)?;
        for text in section.blocks.iter_mut().flat_map(Block::text_blocks_mut) {
            self.apply(&mut text.content, text.source.as_ref());
        }
        Ok(section)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.inner.resource(href)
    }
}
