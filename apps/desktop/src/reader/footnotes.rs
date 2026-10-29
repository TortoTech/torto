//! Linked note text is prepared after the first body frame, near the current
//! focus unit. Numbers, owners and stable identities remain available immediately.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use rebook_publication::{BookSource, PublicationUrl};

use super::{DesktopReader, FocusFootnote, focus_footnote_text_in_section};

const BATCH_SIZE: usize = 16;
const CACHE_ENTRIES: usize = 256;
const CACHE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
struct Reference {
    id: egui::Id,
    target: PublicationUrl,
    marker: String,
}

struct Job {
    generation: u64,
    references: Vec<Reference>,
    source: Arc<dyn BookSource>,
    needed: Arc<AtomicBool>,
    context: egui::Context,
}

struct Finished {
    generation: u64,
    notes: Vec<(egui::Id, Option<String>)>,
}

#[derive(Default)]
pub(super) struct FootnoteLoader {
    generation: u64,
    references: HashMap<egui::Id, Reference>,
    cache: HashMap<egui::Id, Option<String>>,
    order: VecDeque<egui::Id>,
    interests: HashSet<egui::Id>,
    bytes: usize,
    worker: Option<mpsc::SyncSender<Job>>,
    results: Option<mpsc::Receiver<Finished>>,
    running: Option<Arc<AtomicBool>>,
}

impl FootnoteLoader {
    pub(super) fn reset(&mut self) {
        if let Some(needed) = &self.running {
            needed.store(false, Ordering::Release);
        }
        self.generation = self.generation.wrapping_add(1);
        self.references.clear();
        self.cache.clear();
        self.order.clear();
        self.interests.clear();
        self.bytes = 0;
        // Retain running until its completion: cancellation cannot interrupt I/O,
        // and a second batch must not accumulate behind the obsolete one.
    }

    pub(super) fn register(&mut self, id: egui::Id, target: PublicationUrl, marker: String) {
        self.references
            .entry(id)
            .or_insert(Reference { id, target, marker });
    }

    fn start_worker(&mut self) -> bool {
        if self.worker.is_some() {
            return true;
        }
        let (send, receive) = mpsc::sync_channel::<Job>(1);
        let (complete, results) = mpsc::sync_channel(1);
        if std::thread::Builder::new()
            .name("reader-footnotes".into())
            .spawn(move || {
                while let Ok(job) = receive.recv() {
                    let mut sections = HashMap::new();
                    let mut notes = Vec::new();
                    for reference in job.references {
                        if !job.needed.load(Ordering::Acquire) {
                            break;
                        }
                        let index = job
                            .source
                            .book()
                            .sections
                            .iter()
                            .position(|s| s.href.path() == reference.target.path());
                        let text = index.and_then(|index| {
                            let section = sections
                                .entry(index)
                                .or_insert_with(|| job.source.parse_section(index).ok());
                            section.as_ref().and_then(|s| {
                                focus_footnote_text_in_section(
                                    s,
                                    &reference.target,
                                    &reference.marker,
                                )
                            })
                        });
                        notes.push((reference.id, text));
                    }
                    // Publish before waking the UI; there is no unfinished-task poll.
                    if complete
                        .send(Finished {
                            generation: job.generation,
                            notes,
                        })
                        .is_err()
                    {
                        break;
                    }
                    job.context.request_repaint();
                }
            })
            .is_err()
        {
            return false;
        }
        self.worker = Some(send);
        self.results = Some(results);
        true
    }

    fn accept(&mut self, finished: Finished) {
        self.running = None;
        if finished.generation != self.generation {
            return;
        }
        for (id, text) in finished.notes {
            let size = text.as_ref().map_or(0, String::len);
            // Keep a single oversized value intact; the byte budget bounds
            // retained neighbours, never the readable content of one note.
            while self.cache.len() >= CACHE_ENTRIES || self.bytes + size > CACHE_BYTES {
                let Some(index) = self
                    .order
                    .iter()
                    .position(|id| !self.interests.contains(id))
                else {
                    break;
                };
                let old = self.order.remove(index).unwrap();
                if let Some(text) = self.cache.remove(&old) {
                    self.bytes -= text.as_ref().map_or(0, String::len);
                }
            }
            self.bytes += size;
            self.order.push_back(id);
            self.cache.insert(id, text);
        }
    }

    fn prepare(
        &mut self,
        ids: impl IntoIterator<Item = egui::Id>,
        source: Arc<dyn BookSource>,
        ctx: &egui::Context,
    ) {
        let finished = self.results.as_ref().and_then(|r| r.try_recv().ok());
        if let Some(finished) = finished {
            self.accept(finished);
        }
        if self.running.is_some() {
            return;
        }
        let mut seen = HashSet::new();
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| seen.insert(*id))
            .filter(|id| self.references.contains_key(id))
            .take(CACHE_ENTRIES)
            .collect();
        self.interests = ids.iter().copied().collect();
        let references: Vec<_> = ids
            .into_iter()
            .filter(|id| !self.cache.contains_key(id))
            .filter_map(|id| self.references.get(&id).cloned())
            .take(BATCH_SIZE)
            .collect();
        if references.is_empty() {
            return;
        }
        if !self.start_worker() {
            self.accept(Finished {
                generation: self.generation,
                notes: references.iter().map(|r| (r.id, None)).collect(),
            });
            return;
        }
        let needed = Arc::new(AtomicBool::new(true));
        let job = Job {
            generation: self.generation,
            references,
            source,
            needed: Arc::clone(&needed),
            context: ctx.clone(),
        };
        if self.worker.as_ref().unwrap().try_send(job).is_ok() {
            self.running = Some(needed);
        }
    }

    pub(super) fn text<'a>(&'a self, note: &'a FocusFootnote, unavailable: &'a str) -> &'a str {
        match note.stable_id.and_then(|id| self.cache.get(&id)) {
            Some(Some(text)) => text,
            Some(None) => unavailable,
            None => note.popup_text(),
        }
    }
}

impl Drop for FootnoteLoader {
    fn drop(&mut self) {
        if let Some(needed) = &self.running {
            needed.store(false, Ordering::Release);
        }
    }
}

impl DesktopReader {
    pub(super) fn prepare_nearby_footnotes(&mut self, ctx: &egui::Context) {
        if self.opening_prefetch_pending {
            return;
        }
        let mut ids: Vec<_> = self
            .classic_footnotes
            .iter()
            .filter_map(|n| n.stable_id)
            .collect();
        if let Some(unit) = self.focus_units.get(self.focus_unit_index) {
            ids.extend(unit.footnotes.iter().filter_map(|n| n.stable_id));
        }
        let start = self.focus_unit_index.saturating_sub(2);
        let end = (self.focus_unit_index + 3).min(self.focus_units.len());
        if start < end {
            ids.extend(
                self.focus_units[start..end]
                    .iter()
                    .flat_map(|u| &u.footnotes)
                    .filter_map(|n| n.stable_id),
            );
        }
        self.footnote_loader
            .prepare(ids, Arc::clone(&self.source), ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepares_only_interested_references_off_thread_and_then_stops() {
        use rebook_publication::*;
        use std::sync::atomic::AtomicUsize;
        struct Source {
            book: Book,
            section: Section,
            parses: Arc<AtomicUsize>,
            ui_thread: std::thread::ThreadId,
        }
        impl BookSource for Source {
            fn book(&self) -> &Book {
                &self.book
            }
            fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
                assert_ne!(std::thread::current().id(), self.ui_thread);
                self.parses.fetch_add(1, Ordering::Relaxed);
                Ok(self.section.clone())
            }
            fn resource(&self, href: &PublicationUrl) -> Result<Resource, PublicationError> {
                Err(PublicationError::ResourceNotFound(href.to_string()))
            }
        }
        let (reader, _, _) = super::super::semantic_layout::tests::fixture();
        let book = reader.source.book().clone();
        let descriptor = &book.sections[0];
        let section = rebook_html::parse_section(
            "<html><body><aside role='doc-footnote' id='one'><p>1 First note.</p></aside><aside role='doc-footnote' id='two'><p>2 Second note.</p></aside></body></html>",
            descriptor, |_| None).unwrap();
        let target1 = descriptor.href.resolve("#one").unwrap();
        let target2 = descriptor.href.resolve("#two").unwrap();
        let parses = Arc::new(AtomicUsize::new(0));
        let source: Arc<dyn BookSource> = Arc::new(Source {
            book,
            section,
            parses: parses.clone(),
            ui_thread: std::thread::current().id(),
        });
        let mut loader = FootnoteLoader::default();
        let first = egui::Id::unique("one");
        let second = egui::Id::unique("two");
        loader.register(first, target1, "1".into());
        loader.register(second, target2, "2".into());
        assert_eq!(parses.load(Ordering::Relaxed), 0);
        let ctx = egui::Context::default();
        // Direct citation/inline text can precede a linked note in the popup.
        // These rows must not consume the linked-reference loading budget.
        let ids = (0..CACHE_ENTRIES)
            .map(|index| egui::Id::unique(("citation", index)))
            .chain([first, first]);
        loader.prepare(ids, source.clone(), &ctx);
        let finished = loader
            .results
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        loader.accept(finished);
        assert_eq!(parses.load(Ordering::Relaxed), 1);
        assert!(
            loader.cache[&first]
                .as_ref()
                .unwrap()
                .contains("First note.")
        );
        assert!(!loader.cache.contains_key(&second));
        loader.prepare([first], source.clone(), &ctx);
        assert!(
            loader.running.is_none(),
            "completed notes must not cause repaint polling"
        );
        loader.prepare([second], source, &ctx);
        let finished = loader
            .results
            .as_ref()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        loader.reset();
        loader.accept(finished);
        assert!(
            loader.cache.is_empty(),
            "old source completion must be rejected"
        );
    }

    #[test]
    fn stale_completion_does_not_populate_new_source_and_cache_is_bounded() {
        let mut loader = FootnoteLoader::default();
        loader.reset();
        loader.accept(Finished {
            generation: 0,
            notes: vec![(egui::Id::unique(0), Some("old".into()))],
        });
        assert!(loader.cache.is_empty());
        for i in 0..CACHE_ENTRIES + 10 {
            loader.accept(Finished {
                generation: loader.generation,
                notes: vec![(egui::Id::unique(i), Some("note".into()))],
            });
        }
        assert_eq!(loader.cache.len(), CACHE_ENTRIES);
        assert_eq!(loader.bytes, CACHE_ENTRIES * 4);
        loader.accept(Finished {
            generation: loader.generation,
            notes: vec![(egui::Id::unique("large"), Some("x".repeat(CACHE_BYTES + 1)))],
        });
        assert_eq!(
            loader.cache[&egui::Id::unique("large")]
                .as_ref()
                .unwrap()
                .len(),
            CACHE_BYTES + 1
        );
        assert_eq!(loader.cache.len(), 1);
        loader.reset();
        assert_eq!(loader.bytes, 0);
    }

    #[test]
    fn interested_notes_are_not_evicted_into_a_reload_repaint_loop() {
        let mut loader = FootnoteLoader::default();
        let ids: Vec<_> = (0..BATCH_SIZE).map(egui::Id::unique).collect();
        loader.interests = ids.iter().copied().collect();
        loader.accept(Finished {
            generation: 0,
            notes: ids
                .iter()
                .map(|id| (*id, Some("x".repeat(CACHE_BYTES / 8))))
                .collect(),
        });
        assert_eq!(loader.cache.len(), BATCH_SIZE);
        assert!(ids.iter().all(|id| loader.cache.contains_key(id)));
        // The bounded active window forms the cache floor. Once it changes,
        // obsolete text returns to the normal cold-cache byte budget.
        loader.interests.clear();
        loader.accept(Finished {
            generation: 0,
            notes: vec![(egui::Id::unique("new"), Some("new".into()))],
        });
        assert!(loader.bytes <= CACHE_BYTES);
    }
}
