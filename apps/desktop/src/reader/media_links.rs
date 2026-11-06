//! A bounded, metadata-only catalog. Parsing runs on a worker; shaping and popup
//! code only read an immutable snapshot. Pending/missing/ambiguous links are prose.
use super::media_preview::Target;
use rebook_publication::{
    media_references::{self, MediaKey, MediaKind},
    *,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(test)]
#[path = "media_links_tests.rs"]
mod tests;

pub(super) use rebook_app_core::media::{Catalog, href_for};

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
        let snapshot=self.catalog.load();
        rebook_app_core::media::apply_links(self.original.as_ref(), snapshot.as_ref().map(Arc::as_ref), content, origin);
    }
}

impl BookSource for Source {
    fn navigation_section(&self, index: usize) -> Result<Option<Section>, PublicationError> {
        self.inner.navigation_section(index)
    }
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
