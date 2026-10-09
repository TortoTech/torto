//! Bounded, conversion-only page workers. Semantic recovery stays ordered.
use super::{
    ConversionTimings, MAX_RENDER_SCALE, NativeGlyph, NativePage, PAGE_MAX_DIMENSION,
    PdfPublication, RenderCache, check_cancelled, layout, save_regions,
};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{Scope, ScopedJoinHandle};
use std::time::Duration;

const MEMORY_BUDGET: usize = 192 * 1024 * 1024;
const OUTLINE_BUDGET: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct State {
    used: usize,
    peak: usize,
    stopped: bool,
    error: Option<String>,
}

struct Budget {
    limit: usize,
    state: Mutex<State>,
    changed: Condvar,
}

impl Budget {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: limit.max(1),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        })
    }

    fn acquire(
        self: &Arc<Self>,
        estimate: usize,
        cancelled: &AtomicBool,
    ) -> Result<Permit, String> {
        // An oversized page runs alone. This schedules estimated live memory;
        // it cannot impose a hard allocator limit on arbitrary PDF decoders.
        let bytes = estimate.max(1).min(self.limit);
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            check_cancelled(cancelled)?;
            if state.stopped {
                return Err("PDF image workers stopped".into());
            }
            if state.used <= self.limit - bytes {
                state.used += bytes;
                state.peak = state.peak.max(state.used);
                return Ok(Permit {
                    budget: self.clone(),
                    bytes,
                });
            }
            // Only the conversion worker waits; recognize external cancellation
            // even when all rasterizers are still inside a blocking PDF call.
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }

    fn fail(&self, error: String) {
        let mut state = self.state.lock().unwrap();
        state.error.get_or_insert(error);
        state.stopped = true;
        self.changed.notify_all();
    }

    fn stopped(&self) -> bool {
        self.state.lock().unwrap().stopped
    }
}

struct Permit {
    budget: Arc<Budget>,
    bytes: usize,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().unwrap();
        state.used -= self.bytes;
        self.budget.changed.notify_all();
    }
}

struct Job {
    index: usize,
    page: NativePage,
    regions: Vec<layout::Crop>,
    _permit: Permit,
}

pub(in crate::pdf::reflow) struct Pipeline<'scope, 'env> {
    sender: Option<mpsc::SyncSender<Job>>,
    workers: Vec<ScopedJoinHandle<'scope, ConversionTimings>>,
    budget: Arc<Budget>,
    cancelled: &'env AtomicBool,
}

impl<'scope, 'env: 'scope> Pipeline<'scope, 'env> {
    pub(in crate::pdf::reflow) fn new(
        scope: &'scope Scope<'scope, 'env>,
        publication: &'env PdfPublication,
        directory: &'env Path,
        cancelled: &'env AtomicBool,
        worker_count: usize,
    ) -> Self {
        let worker_count = worker_count.clamp(1, 4);
        let (sender, receiver) = mpsc::sync_channel::<Job>(worker_count);
        let receiver = Arc::new(Mutex::new(receiver));
        let budget = Budget::new(MEMORY_BUDGET);
        let workers = (0..worker_count)
            .map(|_| {
                let receiver = receiver.clone();
                let budget = budget.clone();
                scope.spawn(move || {
                    // Rc-based renderer/interpreter caches never cross workers.
                    let mut cache = RenderCache::with_outline_budget(OUTLINE_BUDGET);
                    let mut timings = ConversionTimings::default();
                    loop {
                        let Ok(job) = receiver.lock().unwrap().recv() else {
                            break;
                        };
                        if budget.stopped() {
                            continue;
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            check_cancelled(cancelled)?;
                            save_regions(
                                publication,
                                job.index,
                                &job.page,
                                &job.regions,
                                directory,
                                cancelled,
                                &mut cache,
                                &mut timings,
                            )
                        }));
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => budget.fail(error),
                            Err(_) => budget.fail("PDF image worker panicked".into()),
                        }
                        // Release page objects/decoded images before admitting more
                        // work. Idle workers retain only their bounded outlines.
                        cache.begin_page();
                    }
                    timings
                })
            })
            .collect();
        Self {
            sender: Some(sender),
            workers,
            budget,
            cancelled,
        }
    }

    pub(in crate::pdf::reflow) fn worker_count() -> usize {
        std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).clamp(1, 4))
    }

    pub(in crate::pdf::reflow) fn submit(
        &self,
        index: usize,
        page: NativePage,
        regions: Vec<layout::Crop>,
    ) -> Result<(), String> {
        if regions.is_empty() {
            if let Some(error) = &self.budget.state.lock().unwrap().error {
                return Err(error.clone());
            }
            return check_cancelled(self.cancelled);
        }
        let permit = self
            .budget
            .acquire(estimated_bytes(&page, &regions), self.cancelled)?;
        let job = Job {
            index,
            page,
            regions,
            _permit: permit,
        };
        self.sender
            .as_ref()
            .unwrap()
            .send(job)
            .map_err(|_| "PDF image workers disconnected".to_owned())
    }

    pub(in crate::pdf::reflow) fn finish(
        mut self,
        timings: &mut ConversionTimings,
    ) -> Result<(), String> {
        self.sender.take();
        timings.raster_workers = self.workers.len();
        for worker in self.workers.drain(..) {
            match worker.join() {
                Ok(work) => {
                    // These are accumulated worker durations, not wall time.
                    timings.raster_ms += work.raster_ms;
                    timings.image_write_ms += work.image_write_ms;
                    timings.direct_images += work.direct_images;
                    timings.region_pages += work.region_pages;
                    timings.full_pages += work.full_pages;
                    timings.raster_pixels += work.raster_pixels;
                }
                Err(_) => self.budget.fail("PDF image worker panicked".into()),
            }
        }
        let state = self.budget.state.lock().unwrap();
        timings.raster_peak_estimated_bytes = state.peak;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        check_cancelled(self.cancelled)
    }
}

impl Drop for Pipeline<'_, '_> {
    fn drop(&mut self) {
        // On semantic/I/O failure, discard queued jobs. Join every worker before
        // the caller removes staging files; none can write into a removed stage.
        self.budget.state.lock().unwrap().stopped = true;
        self.budget.changed.notify_all();
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

// Validated nonnegative geometry is rounded up for scheduling only; saturating
// arithmetic keeps estimates conservative without changing render dimensions.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn estimated_bytes(page: &NativePage, regions: &[layout::Crop]) -> usize {
    let scale = (f64::from(PAGE_MAX_DIMENSION) / page.width.max(page.height).max(1.0))
        .min(f64::from(MAX_RENDER_SCALE));
    let page_pixels = ((page.width * scale).ceil() as usize)
        .saturating_mul((page.height * scale).ceil() as usize);
    let crop_pixels = regions
        .iter()
        .map(|r| {
            ((r.bounds.width() * scale).ceil() as usize)
                .saturating_mul((r.bounds.height() * scale).ceil() as usize)
        })
        .max()
        .unwrap_or(0);
    let glyphs = page.glyphs.iter().fold(
        page.glyphs
            .capacity()
            .saturating_mul(size_of::<NativeGlyph>()),
        |sum, glyph| {
            sum.saturating_add(glyph.text.capacity())
                .saturating_add(glyph.tag.as_ref().map_or(0, String::capacity))
                .saturating_add(glyph.link.as_ref().map_or(0, String::capacity))
        },
    );
    let geometry = page
        .images
        .capacity()
        .saturating_add(page.image_obstacles.capacity())
        .saturating_add(page.rules.capacity())
        .saturating_add(page.graphics.capacity())
        .saturating_mul(size_of::<[f64; 4]>())
        .saturating_add(
            page.encoded_images
                .capacity()
                .saturating_mul(size_of::<Option<super::EncodedImage>>()),
        );
    let crops = regions.iter().fold(
        regions.len().saturating_mul(size_of::<layout::Crop>()),
        |sum, crop| {
            sum.saturating_add(crop.path.capacity()).saturating_add(
                crop.image_sources
                    .capacity()
                    .saturating_mul(size_of::<usize>()),
            )
        },
    );
    page_pixels
        .saturating_mul(12)
        .saturating_add(crop_pixels.saturating_mul(4))
        .saturating_add(page.raster_decode_bytes)
        .saturating_add(glyphs)
        .saturating_add(geometry)
        .saturating_add(crops)
        .saturating_add(OUTLINE_BUDGET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::reflow::extract;
    use kurbo::Rect;
    use std::{collections::HashMap, fs, path::PathBuf, sync::atomic::Ordering};

    #[test]
    fn oversized_pages_run_alone_and_released_permits_wake_producers() {
        let budget = Budget::new(100);
        let cancelled = AtomicBool::new(false);
        let first = budget.acquire(30, &cancelled).unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| budget.acquire(500, &cancelled).unwrap());
            drop(first);
            let oversized = worker.join().unwrap();
            assert_eq!(budget.state.lock().unwrap().used, 100);
            drop(oversized);
        });
        assert_eq!(budget.state.lock().unwrap().used, 0);
        assert_eq!(budget.state.lock().unwrap().peak, 100);
    }

    #[test]
    fn cancelled_and_failed_producers_do_not_admit_more_pages() {
        let budget = Budget::new(100);
        let cancelled = AtomicBool::new(true);
        assert!(budget.acquire(20, &cancelled).is_err());
        cancelled.store(false, Ordering::Release);
        let permit = budget.acquire(100, &cancelled).unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| budget.acquire(20, &cancelled));
            budget.fail("image write failed".into());
            assert_eq!(worker.join().unwrap().err().unwrap(), "image write failed");
        });
        drop(permit);
        assert_eq!(budget.state.lock().unwrap().used, 0);
    }

    #[test]
    fn external_cancellation_releases_a_producer_waiting_on_a_full_budget() {
        let budget = Budget::new(100);
        let cancelled = AtomicBool::new(false);
        let held = budget.acquire(100, &cancelled).unwrap();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| budget.acquire(10, &cancelled));
            cancelled.store(true, Ordering::Release);
            assert_eq!(
                worker.join().unwrap().err().unwrap(),
                "PDF reflow cancelled"
            );
        });
        drop(held);
    }

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "torto-raster-pipeline-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn page_workers_preserve_serial_pixels_for_rotated_masked_and_direct_images() {
        let root = root("pixels");
        let serial = root.join("serial");
        let parallel = root.join("parallel");
        fs::create_dir_all(&serial).unwrap();
        fs::create_dir_all(&parallel).unwrap();
        let cancelled = AtomicBool::new(false);
        let mut expected = ConversionTimings::default();
        let mut actual = ConversionTimings::default();
        for (rotation, masked) in [(0, false), (90, true), (180, true), (270, true)] {
            let content = if masked {
                "q 30 30 250 170 re W n 0.2 0.4 0.7 rg 40 50 180 100 re f /GS1 gs 160 0 0 120 70 50 cm /Im1 Do Q BT /F1 20 Tf 65 95 Td (Viewport) Tj ET"
            } else {
                "q 160 0 0 120 70 50 cm /Im1 Do Q"
            };
            let publication = crate::pdf::open(
                super::super::tests::fixture(rotation, content, masked),
                "test.pdf",
            )
            .unwrap();
            let probe = extract::page(&publication.pdf, 0, &HashMap::new());
            assert!(probe.raster_decode_bytes >= 32 * 24 * 8);
            if masked {
                assert!(probe.encoded_images[0].is_none());
            }
            let mut cache = RenderCache::with_outline_budget(OUTLINE_BUDGET);
            std::thread::scope(|scope| {
                let pipeline = Pipeline::new(scope, &publication, &parallel, &cancelled, 4);
                for index in 0..12 {
                    let page = extract::page(&publication.pdf, 0, &HashMap::new());
                    let rect = page.images[0];
                    let regions = vec![layout::Crop {
                        path: format!("{rotation}-{index}.png"),
                        bounds: Rect::new(rect[0], rect[1], rect[2], rect[3]).inflate(2.0, 2.0),
                        image_sources: vec![0],
                    }];
                    save_regions(
                        &publication,
                        0,
                        &page,
                        &regions,
                        &serial,
                        &cancelled,
                        &mut cache,
                        &mut expected,
                    )
                    .unwrap();
                    pipeline.submit(0, page, regions).unwrap();
                }
                pipeline.finish(&mut actual).unwrap();
            });
        }
        for item in fs::read_dir(&serial).unwrap() {
            let path = item.unwrap().path();
            let counterpart = parallel.join(path.file_name().unwrap());
            assert_eq!(
                fs::read(&path).unwrap(),
                fs::read(counterpart).unwrap(),
                "{}",
                path.display()
            );
        }
        assert_eq!(expected.direct_images, actual.direct_images);
        assert_eq!(expected.region_pages, actual.region_pages);
        assert_eq!(expected.raster_pixels, actual.raster_pixels);
        assert!(actual.raster_peak_estimated_bytes <= MEMORY_BUDGET);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_image_writes_and_cancellation_fail_the_final_barrier() {
        let publication = crate::pdf::open(
            super::super::tests::fixture(0, "q 160 0 0 120 70 50 cm /Im1 Do Q", false),
            "test.pdf",
        )
        .unwrap();
        let directory = root("missing-directory");
        for cancel in [false, true] {
            let cancelled = AtomicBool::new(false);
            std::thread::scope(|scope| {
                let pipeline = Pipeline::new(scope, &publication, &directory, &cancelled, 2);
                let page = extract::page(&publication.pdf, 0, &HashMap::new());
                pipeline
                    .submit(
                        0,
                        page,
                        vec![layout::Crop {
                            path: "image.png".into(),
                            bounds: Rect::new(0.0, 0.0, 300.0, 200.0),
                            image_sources: Vec::new(),
                        }],
                    )
                    .unwrap();
                cancelled.store(cancel, Ordering::Release);
                assert!(pipeline.finish(&mut ConversionTimings::default()).is_err());
            });
            assert!(!directory.exists());
        }
    }
}
