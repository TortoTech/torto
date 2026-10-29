//! Nonblocking adapter for egui's synchronous system-font provider.
//! Discovery, mapping files and querying missing grapheme clusters run on workers.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use egui::{FallbackRequest, FontFamily, FontInsert, FontProvider};

const LOOKUP_LIMIT: usize = 1024;
const BATCH_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key(FontFamily, char);

struct Request {
    key: Key,
    cluster: String,
}

#[derive(Default)]
struct State {
    cache: HashMap<Key, Option<FontInsert>>,
    pending: HashSet<Key>,
    queue: VecDeque<Request>,
    running: bool,
    changed: bool,
    generation: u64,
}

pub(super) struct SystemFonts {
    state: Mutex<State>,
    backend: OnceLock<Arc<dyn FontProvider>>,
    locale: &'static str,
    #[cfg(test)]
    completed: std::sync::Condvar,
}

impl SystemFonts {
    pub(super) fn new(language: crate::preferences::AppLanguage) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            backend: OnceLock::new(),
            locale: if language.resolved() == crate::preferences::AppLanguage::SimplifiedChinese {
                "zh-CN"
            } else {
                "en-US"
            },
            #[cfg(test)]
            completed: std::sync::Condvar::new(),
        })
    }

    /// Called after the UI pass, never while egui holds its font locks.
    pub(super) fn advance(self: &Arc<Self>, ctx: &egui::Context) -> bool {
        let (changed, requests, generation) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = std::mem::take(&mut state.changed);
            let requests = if state.running || state.queue.is_empty() {
                Vec::new()
            } else {
                state.running = true;
                state.queue.drain(..).collect::<Vec<_>>()
            };
            (changed, requests, state.generation)
        };
        if changed {
            // A previous None is cached by egui. Reload providers once per
            // completed batch so the now-ready font can satisfy that request.
            ctx.set_font_providers(vec![self.clone()]);
        }
        if !requests.is_empty() {
            let provider = self.clone();
            let wake = ctx.clone();
            std::thread::spawn(move || {
                let backend = provider.backend.get_or_init(|| {
                    Arc::new(
                        egui_system_font_provider::SystemFontProvider::new()
                            .with_locale(Some(provider.locale)),
                    )
                });
                let results = requests
                    .into_iter()
                    .map(|request| {
                        let query = FallbackRequest {
                            cluster: &request.cluster,
                            family: &request.key.0,
                        };
                        let font = backend
                            .font_for(&query)
                            .or_else(|| downloaded_fallback(&query));
                        (request.key, font)
                    })
                    .collect::<Vec<_>>();
                {
                    let mut state = provider
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    for (key, font) in results {
                        state.pending.remove(&key);
                        if font.is_some() || generation == state.generation {
                            state.cache.insert(key, font);
                        }
                    }
                    state.running = false;
                    state.changed = true;
                }
                #[cfg(test)]
                provider.completed.notify_all();
                // The Context is retained only for this finite task, not by
                // the registered provider, avoiding a Context/provider cycle.
                wake.request_repaint();
            });
        }
        changed
    }

    pub(super) fn resources_changed(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.generation = state.generation.wrapping_add(1);
        state.cache.retain(|_, value| value.is_some());
        state.changed = true;
    }

    #[cfg(test)]
    pub(super) fn wait(&self) {
        let (state, _) = self
            .completed
            .wait_timeout_while(
                self.state.lock().unwrap(),
                std::time::Duration::from_secs(10),
                |state| state.running,
            )
            .unwrap();
        assert!(!state.running, "system font lookup worker completed");
    }
}

impl FontProvider for SystemFonts {
    fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
        let base = request.cluster.chars().next()?;
        if request.cluster.len() > 256 || base.is_control() {
            return None;
        }
        let key = Key(request.family.clone(), base);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(font) = state.cache.get(&key) {
            return font.clone();
        }
        // Keep failures too: evicting them while reloading providers can cause
        // endless rediscovery/redraws for a long string of unsupported glyphs.
        if state.cache.len() + state.pending.len() < LOOKUP_LIMIT
            && state.pending.len() < BATCH_LIMIT
            && state.pending.insert(key.clone())
        {
            state.queue.push_back(Request {
                key,
                cluster: request.cluster.into(),
            });
        }
        None
    }
}

fn downloaded_fallback(request: &FallbackRequest<'_>) -> Option<FontInsert> {
    use skrifa::MetadataProvider as _;
    let face = crate::fonts::cjk_face()?;
    let font = skrifa::FontRef::from_index(face.data.as_ref(), face.index).ok()?;
    font.charmap().map(request.cluster.chars().next()?)?;
    Some(FontInsert {
        name: "reader-cjk".into(),
        data: egui::FontData::from_blob(Arc::new(face.data), face.index),
        families: vec![egui::epaint::text::InsertFontFamily {
            family: request.family.clone(),
            priority: egui::epaint::text::FontPriority::Lowest,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    struct SlowFont {
        started: mpsc::Sender<std::thread::ThreadId>,
        release: Mutex<mpsc::Receiver<()>>,
        calls: AtomicUsize,
        data: egui::FontData,
    }

    impl FontProvider for SlowFont {
        fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.started.send(std::thread::current().id()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            Some(FontInsert {
                name: "test-system-font".into(),
                data: self.data.clone(),
                families: vec![egui::epaint::text::InsertFontFamily {
                    family: request.family.clone(),
                    priority: egui::epaint::text::FontPriority::Lowest,
                }],
            })
        }
    }

    #[test]
    fn slow_system_lookup_runs_off_ui_thread_and_coalesces_requests() {
        let ctx = egui::Context::default();
        let provider = SystemFonts::new(crate::preferences::AppLanguage::English);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let backend = Arc::new(SlowFont {
            started: started_tx,
            release: Mutex::new(release_rx),
            calls: AtomicUsize::new(0),
            data: (*egui::FontDefinitions::default().font_data["Ubuntu-Light"]).clone(),
        });
        assert!(provider.backend.set(backend.clone()).is_ok());
        let request = FallbackRequest {
            cluster: "X",
            family: &FontFamily::Proportional,
        };
        for _ in 0..3 {
            assert!(provider.font_for(&request).is_none());
        }
        assert_eq!(provider.state.lock().unwrap().pending.len(), 1);
        assert!(!provider.advance(&ctx));
        let worker = started_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_ne!(worker, std::thread::current().id());
        // This still returns while the backend is deliberately blocked.
        assert!(provider.font_for(&request).is_none());
        release_tx.send(()).unwrap();
        provider.wait();
        assert!(provider.advance(&ctx));
        assert_eq!(
            provider.font_for(&request).unwrap().name,
            "test-system-font"
        );
        assert_eq!(backend.calls.load(Ordering::Relaxed), 1);
        assert!(!provider.advance(&ctx));
    }

    #[test]
    fn resource_change_discards_an_in_flight_missing_font_result() {
        let ctx = egui::Context::default();
        let provider = SystemFonts::new(crate::preferences::AppLanguage::English);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Mutex::new(release_rx);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        assert!(
            provider
                .backend
                .set(Arc::new(move |_: &FallbackRequest<'_>| {
                    if count.fetch_add(1, Ordering::Relaxed) == 0 {
                        started_tx.send(()).unwrap();
                        release
                            .lock()
                            .unwrap()
                            .recv_timeout(std::time::Duration::from_secs(10))
                            .unwrap();
                    }
                    None
                }))
                .is_ok()
        );
        let request = FallbackRequest {
            cluster: "\u{10ffff}",
            family: &FontFamily::Proportional,
        };
        provider.font_for(&request);
        provider.advance(&ctx);
        started_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        provider.resources_changed();
        release_tx.send(()).unwrap();
        provider.wait();
        provider.advance(&ctx);
        assert!(provider.state.lock().unwrap().cache.is_empty());
        // The old failure must not hide newly downloaded font resources.
        provider.font_for(&request);
        provider.advance(&ctx);
        provider.wait();
        provider.advance(&ctx);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(provider.state.lock().unwrap().cache.len(), 1);
        assert!(!provider.advance(&ctx));
    }

    #[test]
    fn unsupported_text_has_bounded_lookups_and_stops_requesting_frames() {
        let ctx = egui::Context::default();
        let provider = SystemFonts::new(crate::preferences::AppLanguage::English);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        assert!(
            provider
                .backend
                .set(Arc::new(move |_: &FallbackRequest<'_>| {
                    count.fetch_add(1, Ordering::Relaxed);
                    None
                }))
                .is_ok()
        );
        for _ in 0..12 {
            for index in 0..1200 {
                let cluster = char::from_u32(0xf0000 + index).unwrap().to_string();
                provider.font_for(&FallbackRequest {
                    cluster: &cluster,
                    family: &FontFamily::Proportional,
                });
            }
            provider.advance(&ctx);
            provider.wait();
            provider.advance(&ctx);
        }
        assert_eq!(calls.load(Ordering::Relaxed), LOOKUP_LIMIT);
        let state = provider.state.lock().unwrap();
        assert_eq!(state.cache.len(), LOOKUP_LIMIT);
        assert!(
            state.queue.is_empty() && state.pending.is_empty() && !state.running && !state.changed
        );
        drop(state);
        assert!(!provider.advance(&ctx));
    }
}
