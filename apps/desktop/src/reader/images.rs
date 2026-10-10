//! Viewport-driven image work. The UI owns a replaceable interest window;
//! two sleeping workers own all resource I/O and pixel processing.
#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Instant;

use kurbo::Rect;
use rebook_layout::DeferredRaster;
use rebook_publication::BookSource;

use super::DesktopReader;

const PREFETCH_IMAGES: usize = 4;
const PREFETCH_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
struct Interest {
    request: DeferredRaster,
    visible: bool,
    distance: f64,
    queued: Instant,
}

struct Job {
    request: DeferredRaster,
    source: Arc<dyn BookSource>,
    context: egui::Context,
    needed: Arc<AtomicBool>,
    queued: Instant,
    visible: bool,
}

struct Finished {
    slot: usize,
    key: u64,
    error: Option<String>,
}

struct Running {
    key: u64,
    visible: bool,
    needed: Arc<AtomicBool>,
}

pub(super) struct ImageLoader {
    workers: Vec<mpsc::Sender<Job>>,
    results: mpsc::Receiver<Finished>,
    running: HashMap<usize, Running>,
    interests: Vec<Interest>,
    failed: HashMap<u64, Instant>,
    previous_offset: Option<f32>,
    direction: i8,
    visible_keys: HashSet<u64>,
    visible_since: Instant,
    visible_reported: bool,
}

impl Default for ImageLoader {
    fn default() -> Self {
        let (complete, results) = mpsc::channel();
        let mut workers = Vec::new();
        for slot in 0..2 {
            let (send, receive) = mpsc::channel::<Job>();
            let complete = complete.clone();
            if std::thread::Builder::new()
                .name(format!("reader-image-{slot}"))
                .spawn(move || {
                    while let Ok(job) = receive.recv() {
                        let started = Instant::now();
                        let capture = cfg!(debug_assertions)
                            .then(rebook_layout::timing::TimingScope::start)
                            .flatten();
                        let result = job.request.load_guarded(job.source.as_ref(), || {
                            job.needed.load(Ordering::Acquire)
                        });
                        if let Some(capture) = capture {
                            use crate::diagnostics::{Field, log};
                            use rebook_layout::timing::TimingStage;
                            let timings = capture.finish();
                            log(
                                "reader.image_load",
                                &[
                                    Field::U64("key", job.request.key()),
                                    Field::Bool("visible", job.visible),
                                    Field::F32(
                                        "queue_ms",
                                        started.duration_since(job.queued).as_secs_f32() * 1000.0,
                                    ),
                                    Field::F32(
                                        "source_ms",
                                        timings.duration(TimingStage::ImageSource).as_secs_f32()
                                            * 1000.0,
                                    ),
                                    Field::F32(
                                        "decode_ms",
                                        timings.duration(TimingStage::ImageDecode).as_secs_f32()
                                            * 1000.0,
                                    ),
                                    Field::F32(
                                        "pixels_ms",
                                        timings.duration(TimingStage::ImagePixels).as_secs_f32()
                                            * 1000.0,
                                    ),
                                    Field::Bool("cancelled", !job.needed.load(Ordering::Acquire)),
                                ],
                            );
                        }
                        // Always publish before the reliable wakeup. No timer polls
                        // are needed to start the next bounded batch.
                        let error = if job.needed.load(Ordering::Acquire) {
                            result.err().map(|e| e.to_string())
                        } else {
                            None
                        };
                        if complete
                            .send(Finished {
                                slot,
                                key: job.request.key(),
                                error,
                            })
                            .is_err()
                        {
                            break;
                        }
                        job.context.request_repaint();
                    }
                })
                .is_ok()
            {
                workers.push(send);
            } else {
                tracing::error!(slot, "cannot start reader image worker");
                break;
            }
        }
        Self {
            workers,
            results,
            running: HashMap::new(),
            interests: Vec::new(),
            failed: HashMap::new(),
            previous_offset: None,
            direction: 0,
            visible_keys: HashSet::new(),
            visible_since: Instant::now(),
            visible_reported: false,
        }
    }
}

impl Drop for ImageLoader {
    fn drop(&mut self) {
        for running in self.running.values() {
            running.needed.store(false, Ordering::Release);
        }
        // Closing senders wakes idle workers; never join a decoder on the UI.
    }
}

impl ImageLoader {
    fn drain(&mut self) -> bool {
        let mut changed = false;
        for result in self.results.try_iter() {
            self.running.remove(&result.slot);
            changed |= self
                .interests
                .iter()
                .any(|i| i.visible && i.request.key() == result.key);
            if let Some(error) = result.error {
                tracing::warn!(image = result.key, %error, "reader image loading failed");
                self.failed.insert(result.key, Instant::now());
            }
        }
        changed
    }

    fn replace(&mut self, mut interests: Vec<Interest>) {
        interests.sort_by(|a, b| {
            b.visible
                .cmp(&a.visible)
                .then(a.distance.total_cmp(&b.distance))
        });
        let mut seen = HashSet::new();
        let mut count = 0;
        let mut bytes: usize = 0;
        interests.retain(|interest| {
            if !seen.insert(interest.request.key()) {
                return false;
            }
            if interest.visible {
                return true;
            }
            let size = interest.request.estimated_bytes();
            if count >= PREFETCH_IMAGES || bytes.saturating_add(size) > PREFETCH_BYTES {
                return false;
            }
            count += 1;
            bytes += size;
            true
        });
        for interest in &mut interests {
            if let Some(old) = self
                .interests
                .iter()
                .find(|old| old.request.key() == interest.request.key())
            {
                interest.queued = old.queued;
            }
        }
        for running in self.running.values_mut() {
            let current = interests.iter().find(|i| i.request.key() == running.key);
            if let Some(current) = current {
                running.visible = current.visible;
            } else {
                running.needed.store(false, Ordering::Release);
            }
        }
        let visible_keys = interests
            .iter()
            .filter(|i| i.visible)
            .map(|i| i.request.key())
            .collect();
        if self.visible_keys != visible_keys {
            self.visible_keys = visible_keys;
            self.visible_since = Instant::now();
            self.visible_reported = false;
        }
        self.interests = interests;
        // Never evict failures still in the interest window: a page full of
        // broken images must settle rather than retry forever.
        while self.failed.len() > 64 {
            let oldest = self
                .failed
                .iter()
                .filter(|(key, _)| !self.interests.iter().any(|i| i.request.key() == **key))
                .min_by_key(|(_, time)| **time)
                .map(|(key, _)| *key);
            let Some(oldest) = oldest else {
                break;
            };
            self.failed.remove(&oldest);
        }
    }

    fn dispatch(
        &mut self,
        context: &egui::Context,
        source: &Arc<dyn BookSource>,
        visible_allowed: bool,
        prefetch_allowed: bool,
    ) {
        if !visible_allowed {
            return;
        }
        for slot in 0..self.workers.len() {
            if self.running.contains_key(&slot) {
                continue;
            }
            let background_running = self.running.values().any(|r| !r.visible);
            let foreground_pending = self.interests.iter().any(|i| {
                i.visible
                    && !self.failed.contains_key(&i.request.key())
                    && i.request.ready().is_none()
            });
            let next = self.interests.iter().find(|i| {
                !self.failed.contains_key(&i.request.key())
                    && !self.running.values().any(|r| r.key == i.request.key())
                    && (i.visible
                        || prefetch_allowed
                            && !background_running
                            && !foreground_pending
                            && i.request.can_prefetch())
                    && i.request.ready().is_none()
            });
            let Some(next) = next else {
                continue;
            };
            let needed = Arc::new(AtomicBool::new(true));
            let job = Job {
                request: next.request.clone(),
                source: source.clone(),
                context: context.clone(),
                needed: needed.clone(),
                queued: next.queued,
                visible: next.visible,
            };
            if self.workers[slot].send(job).is_ok() {
                self.running.insert(
                    slot,
                    Running {
                        key: next.request.key(),
                        visible: next.visible,
                        needed,
                    },
                );
            }
        }
    }

    fn report_visible_ready(&mut self) {
        if !self.visible_reported
            && !self.visible_keys.is_empty()
            && self
                .interests
                .iter()
                .filter(|i| i.visible)
                .all(|i| i.request.ready().is_some())
        {
            self.visible_reported = true;
            crate::diagnostics::log(
                "reader.visible_images_ready",
                &[
                    crate::diagnostics::Field::Usize("images", self.visible_keys.len()),
                    crate::diagnostics::Field::F32(
                        "viewport_wait_ms",
                        self.visible_since.elapsed().as_secs_f32() * 1000.0,
                    ),
                ],
            );
        }
    }
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.intersect(b).area() > 0.0
}

impl DesktopReader {
    /// Called after UI navigation has established the actual saved/scroll
    /// viewport, before choosing the GPU scene revision for this frame.
    pub(super) fn prepare_visible_images(&mut self, context: &egui::Context) {
        let changed = self.images.drain();
        let mut interests = Vec::new();
        let now = Instant::now();
        if self.is_scroll_mode() {
            if let (Some(layout), Some(viewport)) = (&self.scroll_section, self.scroll_viewport) {
                if let Some(previous) = self.images.previous_offset {
                    let delta = viewport.offset_y - previous;
                    if delta.abs() > 1.0 {
                        self.images.direction = if delta > 0.0 { 1 } else { -1 };
                    }
                }
                self.images.previous_offset = Some(viewport.offset_y);
                let height = f64::from(viewport.size.y.max(1.0));
                let visible = Rect::new(
                    0.0,
                    f64::from(viewport.offset_y),
                    f64::from(viewport.size.x),
                    f64::from(viewport.offset_y) + height,
                );
                let (before, after) = match self.images.direction {
                    1 => (1.0, 2.0),
                    -1 => (2.0, 1.0),
                    _ => (1.0, 1.0),
                };
                let nearby = Rect::new(
                    visible.x0,
                    visible.y0 - height * before,
                    visible.x1,
                    visible.y1 + height * after,
                );
                let padding = self.scroll_content_padding(viewport.size.y);
                for (index, entry) in layout.pages.iter().enumerate() {
                    for (request, bounds) in entry.page.deferred_rasters() {
                        let dy = f64::from(
                            layout.page_tops[index] + padding - layout.page_origins[index],
                        );
                        // Crop images against the semantic unit, not the EPUB file.
                        let crop = Rect::new(
                            0.0,
                            f64::from(layout.page_origins[index]),
                            f64::from(entry.page.width()),
                            f64::from(layout.page_origins[index] + layout.page_heights[index]),
                        );
                        let bounds = bounds.intersect(crop) + kurbo::Vec2::new(0.0, dy);
                        if intersects(bounds, nearby) {
                            let is_visible = intersects(bounds, visible);
                            let distance = (bounds.y0 - visible.y1)
                                .max(visible.y0 - bounds.y1)
                                .max(0.0);
                            interests.push(Interest {
                                request: request.clone(),
                                visible: is_visible,
                                distance,
                                queued: now,
                            });
                        }
                    }
                }
            }
        } else {
            if let Ok(spread) = self.reader.current_spread() {
                for page in std::iter::once(&spread.primary).chain(spread.secondary.as_ref()) {
                    interests.extend(page.deferred_rasters().map(|(request, _)| Interest {
                        request: request.clone(),
                        visible: true,
                        distance: 0.0,
                        queued: now,
                    }));
                }
            }
            let pages = self.reader.cached_reading_unit_pages();
            if let Some(index) = pages.iter().position(|p| {
                p.position.section_index == self.snapshot.location.section_index
                    && p.position.segment_index == self.snapshot.location.segment_index
                    && p.position.page_index == self.snapshot.location.page_index
            }) {
                let visible_pages = self
                    .reader
                    .current_spread()
                    .map_or(1, |spread| 1 + usize::from(spread.secondary.is_some()));
                for (other, entry) in pages.iter().enumerate() {
                    if other + 1 < index || other > index + visible_pages + 1 {
                        continue;
                    }
                    for (request, bounds) in entry.page.deferred_rasters() {
                        if entry
                            .visible_top
                            .is_some_and(|top| bounds.y1 <= f64::from(top))
                            || entry
                                .visible_bottom
                                .is_some_and(|bottom| bounds.y0 >= f64::from(bottom))
                        {
                            continue;
                        }
                        interests.push(Interest {
                            request: request.clone(),
                            visible: false,
                            distance: other.abs_diff(index) as f64,
                            queued: now,
                        });
                    }
                }
            }
        }
        self.images.replace(interests);
        let protected = DeferredRaster::protect_visible(
            self.source.as_ref(),
            self.images
                .interests
                .iter()
                .filter(|i| i.visible)
                .map(|i| i.request.clone()),
        );
        self.images.dispatch(
            context,
            &self.source,
            !context.input(|i| i.viewport().minimized.unwrap_or(false))
                && self.completion.is_none(),
            context.input(|i| i.focused) && protected,
        );
        self.images.report_visible_ready();
        if changed {
            self.bump_scene_revision();
        }
    }
}
