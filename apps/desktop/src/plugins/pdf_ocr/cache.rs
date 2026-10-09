//! Swappable source generations keep mode changes off worker locks. A worker
//! finishing an old generation can never repopulate the active cache.
use super::*;
use arc_swap::ArcSwap;

type LoadSource = dyn Fn() -> Result<Arc<dyn BookSource>, String> + Send + Sync;

struct SourceGeneration {
    enabled: bool,
    source: OnceLock<Result<Arc<dyn BookSource>, String>>,
}

pub(crate) struct RetiredPdfSource {
    _generation: Arc<SourceGeneration>,
}

pub(crate) struct PdfModeSource {
    book: Book,
    origin: TableOfContentsOrigin,
    generation: ArcSwap<SourceGeneration>,
    load: Box<LoadSource>,
}

impl PdfModeSource {
    pub(crate) fn new(
        book: Book,
        origin: TableOfContentsOrigin,
        loaded: Option<Arc<dyn BookSource>>,
        load: impl Fn() -> Result<Arc<dyn BookSource>, String> + Send + Sync + 'static,
    ) -> Self {
        let source = OnceLock::new();
        if let Some(loaded) = loaded {
            let _ = source.set(Ok(loaded));
        }
        Self {
            book,
            origin,
            generation: ArcSwap::from_pointee(SourceGeneration {
                enabled: true,
                source,
            }),
            load: Box::new(load),
        }
    }

    /// Only called by a loading worker before it publishes a mode switch.
    pub(crate) fn prepare(&self) -> Result<Arc<dyn BookSource>, String> {
        if !self.generation.load().enabled {
            self.generation.store(Arc::new(SourceGeneration {
                enabled: true,
                source: OnceLock::new(),
            }));
        }
        self.source().map_err(|error| error.to_string())
    }

    /// Explicit background tools get a lease without making an inactive mode
    /// resident. The lease keeps one load alive for the whole metadata task.
    pub(crate) fn lease(&self) -> Result<Arc<dyn BookSource>, String> {
        let generation = self.generation.load_full();
        if generation.enabled {
            self.source().map_err(|error| error.to_string())
        } else {
            (self.load)()
        }
    }

    pub(crate) fn retire(&self) -> RetiredPdfSource {
        RetiredPdfSource {
            _generation: self.generation.swap(Arc::new(SourceGeneration {
                enabled: false,
                source: OnceLock::new(),
            })),
        }
    }

    fn source(&self) -> Result<Arc<dyn BookSource>, PublicationError> {
        let generation = self.generation.load_full();
        if !generation.enabled {
            return Err(PublicationError::ResourceNotFound(
                "inactive PDF view".into(),
            ));
        }
        generation
            .source
            .get_or_init(|| (self.load)())
            .as_ref()
            .map(Arc::clone)
            .map_err(|error| PublicationError::InvalidPublication(error.clone()))
    }
}

impl BookSource for PdfModeSource {
    fn book(&self) -> &Book {
        &self.book
    }
    fn table_of_contents_origin(&self) -> TableOfContentsOrigin {
        self.origin
    }
    fn parse_section(&self, index: usize) -> Result<Section, PublicationError> {
        self.source()?.parse_section(index)
    }
    fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
        self.source()?.resource(href)
    }
    fn raster_resource(
        &self,
        href: &PublicationUrl,
    ) -> Result<Option<RasterResource>, PublicationError> {
        self.source()?.raster_resource(href)
    }
    fn fixed_page_dimensions(
        &self,
        index: usize,
    ) -> Result<Option<rebook_publication::FixedPageDimensions>, PublicationError> {
        self.source()?.fixed_page_dimensions(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Stub {
        book: Book,
    }
    impl BookSource for Stub {
        fn book(&self) -> &Book {
            &self.book
        }
        fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
            unreachable!()
        }
        fn resource(&self, _: &PublicationUrl) -> Result<Resource, PublicationError> {
            unreachable!()
        }
    }
    fn book() -> Book {
        Book {
            id: rebook_publication::PublicationId::new("reloadable-mode-test").unwrap(),
            metadata: Default::default(),
            cover: None,
            sections: Vec::new(),
            table_of_contents: Vec::new(),
        }
    }

    #[test]
    fn retirement_drops_payload_and_next_prepare_reloads_it() {
        let loads = Arc::new(AtomicUsize::new(0));
        let counter = loads.clone();
        let cache = PdfModeSource::new(book(), TableOfContentsOrigin::Embedded, None, move || {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(Stub { book: book() }))
        });
        for expected in 1..=3 {
            let source = cache.prepare().unwrap();
            let weak = Arc::downgrade(&source);
            drop(source);
            assert!(weak.upgrade().is_some());
            assert_eq!(loads.load(Ordering::Relaxed), expected);
            drop(cache.retire());
            assert!(weak.upgrade().is_none());
            assert!(cache.source().is_err());
        }
        let source = cache.lease().unwrap();
        let weak = Arc::downgrade(&source);
        drop(source);
        assert!(weak.upgrade().is_none());
        assert!(cache.source().is_err());
    }

    #[test]
    fn finishing_old_load_cannot_resurrect_a_retired_generation() {
        let started = Arc::new(std::sync::Barrier::new(2));
        let finish = Arc::new(std::sync::Barrier::new(2));
        let a = started.clone();
        let b = finish.clone();
        let cache = Arc::new(PdfModeSource::new(
            book(),
            TableOfContentsOrigin::Embedded,
            None,
            move || {
                a.wait();
                b.wait();
                Ok(Arc::new(Stub { book: book() }))
            },
        ));
        let worker_cache = cache.clone();
        let worker = std::thread::spawn(move || worker_cache.source().unwrap());
        started.wait();
        drop(cache.retire());
        assert!(cache.source().is_err());
        finish.wait();
        let old_source = worker.join().unwrap();
        let weak = Arc::downgrade(&old_source);
        drop(old_source);
        assert!(weak.upgrade().is_none());
        assert!(cache.source().is_err());
    }
}
