use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use egui::{ColorImage, Context, TextureHandle};
use sha2::{Digest, Sha256};

use super::background::BackgroundJob;

const BUDGET: usize = 32 * 1024 * 1024;
const READER_BUDGET: usize = 4 * 1024 * 1024;
type Key = (String, u32, [u32; 2]);
const SHELF_SIZE: [u32; 2] = [160, 228];

struct Entry {
    texture: TextureHandle,
    bytes: usize,
    touched: u64,
}

#[derive(Default)]
pub(crate) struct CoverCache {
    entries: HashMap<Key, Entry>,
    wanted: Vec<(Key, Arc<[u8]>)>,
    failed: HashSet<Key>,
    in_flight: HashSet<Key>,
    job: BackgroundJob<(u64, Vec<(Key, Option<ColorImage>)>)>,
    generation: u64,
    frame: u64,
}

impl CoverCache {
    pub fn begin_frame(&mut self, ctx: &Context) {
        self.frame = self.frame.wrapping_add(1);
        if let Some((generation, results)) = self.job.poll()
            && generation == self.generation
        {
            for (key, image) in results {
                self.in_flight.remove(&key);
                if let Some(image) = image {
                    let bytes = image.pixels.len() * 4;
                    let texture = ctx.load_texture(
                        format!(
                            "cover-thumbnail:{}:{}:{}x{}",
                            key.0, key.1, key.2[0], key.2[1]
                        ),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.entries.insert(
                        key,
                        Entry {
                            texture,
                            bytes,
                            touched: self.frame,
                        },
                    );
                } else {
                    self.failed.insert(key);
                }
            }
        }
        self.wanted.clear();
    }

    pub fn texture(&mut self, ctx: &Context, id: &str, bytes: &[u8]) -> Option<TextureHandle> {
        self.texture_sized(ctx, id, bytes, SHELF_SIZE)
    }

    pub fn texture_sized(
        &mut self,
        ctx: &Context,
        id: &str,
        bytes: &[u8],
        size: [u32; 2],
    ) -> Option<TextureHandle> {
        // Quantized DPI avoids generating a new texture for each small scale change.
        let scale = ctx.pixels_per_point().ceil().clamp(1.0, 4.0) as u32;
        let key = (id.to_owned(), scale, size);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.touched = self.frame;
            return Some(entry.texture.clone());
        }
        // A larger cached thumbnail can also serve a smaller view of this book.
        let cached = self
            .entries
            .keys()
            .filter(|(book, dpi, bounds)| {
                book == id && *dpi == scale && bounds[0] >= size[0] && bounds[1] >= size[1]
            })
            .min_by_key(|(_, _, bounds)| u64::from(bounds[0]) * u64::from(bounds[1]))
            .cloned();
        if let Some(entry) = cached.and_then(|key| self.entries.get_mut(&key)) {
            entry.touched = self.frame;
            return Some(entry.texture.clone());
        }
        if !self.failed.contains(&key)
            && !self.in_flight.contains(&key)
            && !self.wanted.iter().any(|(pending, _)| pending == &key)
        {
            self.wanted.push((key, Arc::from(bytes)));
        }
        None
    }

    pub fn spawn(
        &mut self,
        runtime: &tokio::runtime::Runtime,
        wake: impl FnOnce() + Send + 'static,
    ) {
        if self.job.is_running() || self.wanted.is_empty() {
            return;
        }
        let requests = self.wanted.iter().take(2).cloned().collect::<Vec<_>>();
        self.in_flight
            .extend(requests.iter().map(|(key, _)| key.clone()));
        let generation = self.generation;
        self.job.start(
            runtime,
            move || {
                (
                    generation,
                    requests
                        .into_iter()
                        .map(|(key, bytes)| {
                            let image = thumbnail(&bytes, key.1, key.2).ok();
                            (key, image)
                        })
                        .collect(),
                )
            },
            wake,
        );
    }

    pub fn trim(&mut self) {
        self.trim_to(BUDGET);
    }

    pub fn trim_for_reader(&mut self) {
        self.trim_to(READER_BUDGET);
    }

    fn trim_to(&mut self, budget: usize) {
        let mut bytes = self.bytes();
        let mut candidates = self
            .entries
            .iter()
            .filter(|(_, e)| e.touched != self.frame)
            .map(|(k, e)| (k.clone(), e.touched))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, touched)| *touched);
        for (key, _) in candidates {
            if bytes <= budget {
                break;
            }
            if let Some(entry) = self.entries.remove(&key) {
                bytes -= entry.bytes;
            }
        }
    }

    pub fn suspend(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.frame = self.frame.wrapping_add(1);
        self.wanted.clear();
        self.in_flight.clear();
        self.trim_to(READER_BUDGET);
    }

    pub fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
        self.wanted.clear();
        self.failed.clear();
        self.in_flight.clear();
    }

    pub fn remove(&mut self, id: &str) {
        // A completed worker must not resurrect a removed/replaced cover.
        self.generation = self.generation.wrapping_add(1);
        self.entries.retain(|(book, _, _), _| book != id);
        self.wanted.retain(|((book, _, _), _)| book != id);
        self.failed.retain(|(book, _, _)| book != id);
        self.in_flight.clear();
    }

    pub fn bytes(&self) -> usize {
        self.entries.values().map(|e| e.bytes).sum()
    }
}

fn thumbnail(bytes: &[u8], scale: u32, size: [u32; 2]) -> Result<ColorImage, image::ImageError> {
    let width = size[0] * scale;
    let height = size[1] * scale;
    let digest = format!("{:x}", Sha256::digest(bytes));
    let path = crate::smoke::project_dirs().map(|p| {
        p.cache_dir()
            .join("cover-thumbnails-v2")
            .join(if size == SHELF_SIZE {
                format!("{digest}-{scale}.png")
            } else {
                format!("{digest}-{scale}-{}x{}.png", size[0], size[1])
            })
    });
    let cached = path
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|bytes| image::load_from_memory(&bytes).ok())
        .filter(|im| im.width() <= width && im.height() <= height);
    let image = if let Some(image) = cached {
        image
    } else {
        let image = rebook_layout::image_processing::resize(
            image::load_from_memory(bytes)?,
            [width, height],
            image::imageops::FilterType::Lanczos3,
        );
        if let Some(path) = path {
            let mut encoded = Cursor::new(Vec::new());
            if image
                .write_to(&mut encoded, image::ImageFormat::Png)
                .is_ok()
            {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                crate::persistence::write_bytes_atomic(&path, encoded.get_ref()).ok();
            }
        }
        image
    };
    let rgba = image.into_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspended_cover_worker_cannot_restore_stale_textures() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ctx = Context::default();
        let mut cache = CoverCache::default();
        let generation = cache.generation;
        let (sender, receiver) = std::sync::mpsc::channel();
        cache.job.start(
            &runtime,
            move || {
                (
                    generation,
                    vec![(
                        ("old-book".into(), 1, SHELF_SIZE),
                        Some(ColorImage::new([1, 1], vec![egui::Color32::WHITE])),
                    )],
                )
            },
            move || {
                sender.send(()).unwrap();
            },
        );
        cache.suspend();
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        cache.begin_frame(&ctx);
        assert!(cache.entries.is_empty());
        assert!(!cache.job.is_running());
    }

    #[test]
    fn cover_budget_evicts_oldest_and_pins_visible_cards() {
        let ctx = Context::default();
        let mut cache = CoverCache::default();
        for (id, tick) in [("old", 0), ("recent", 1), ("visible", 2)] {
            let texture = ctx.load_texture(
                id,
                ColorImage::new([1, 1], vec![egui::Color32::WHITE]),
                egui::TextureOptions::LINEAR,
            );
            cache.entries.insert(
                (id.into(), 1, SHELF_SIZE),
                Entry {
                    texture,
                    bytes: 4,
                    touched: tick,
                },
            );
        }
        cache.frame = 2;
        cache.trim_to(8);
        assert!(!cache.entries.contains_key(&("old".into(), 1, SHELF_SIZE)));
        assert!(
            cache
                .entries
                .contains_key(&("visible".into(), 1, SHELF_SIZE))
        );
        assert_eq!(cache.bytes(), 8);
    }
    #[test]
    fn large_cover_is_reduced_to_the_physical_card_size() {
        let image = image::DynamicImage::new_rgba8(2007, 2719);
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let result = thumbnail(png.get_ref(), 2, SHELF_SIZE).unwrap();
        assert!(result.size[0] <= 320 && result.size[1] <= 456);
        assert!(result.pixels.len() * 4 < 600_000);
        for size in [[48, 72], [100, 150], [52, 74]] {
            let result = thumbnail(png.get_ref(), 2, size).unwrap();
            assert!(result.size[0] <= size[0] as usize * 2);
            assert!(result.size[1] <= size[1] as usize * 2);
        }
    }

    #[test]
    fn statistics_and_reader_reuse_a_larger_shelf_texture_without_copying_cover_bytes() {
        let ctx = Context::default();
        let mut cache = CoverCache::default();
        let texture = ctx.load_texture(
            "shared-cover",
            ColorImage::new([160, 228], vec![egui::Color32::WHITE; 160 * 228]),
            egui::TextureOptions::LINEAR,
        );
        let expected = texture.id();
        cache.entries.insert(
            ("book".into(), 1, SHELF_SIZE),
            Entry {
                texture,
                bytes: 160 * 228 * 4,
                touched: 0,
            },
        );
        cache.begin_frame(&ctx);
        for size in [[48, 72], [100, 150], [52, 74]] {
            assert_eq!(
                cache
                    .texture_sized(&ctx, "book", &[1, 2, 3], size)
                    .unwrap()
                    .id(),
                expected
            );
        }
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.wanted.is_empty());
        assert_eq!(cache.entries.values().next().unwrap().touched, cache.frame);
    }

    #[test]
    fn reader_cover_is_prepared_by_worker_and_removed_results_cannot_return() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ctx = Context::default();
        let mut cache = CoverCache::default();
        let image = image::DynamicImage::new_rgb8(400, 600);
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        cache.begin_frame(&ctx);
        assert!(
            cache
                .texture_sized(&ctx, "reader", png.get_ref(), [52, 74])
                .is_none()
        );
        assert_eq!(cache.wanted.len(), 1);
        assert!(cache.entries.is_empty());
        cache.spawn(&runtime, move || {
            sender.send(()).unwrap();
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        cache.begin_frame(&ctx);
        let texture = cache
            .texture_sized(&ctx, "reader", png.get_ref(), [52, 74])
            .unwrap();
        assert!(texture.size()[0] <= 52 && texture.size()[1] <= 74);
        assert!(cache.wanted.is_empty());
        cache.remove("reader");
        let (sender, receiver) = std::sync::mpsc::channel();
        cache.texture_sized(&ctx, "reader", png.get_ref(), [52, 74]);
        cache.spawn(&runtime, move || {
            sender.send(()).unwrap();
        });
        cache.remove("reader");
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        cache.begin_frame(&ctx);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn repeated_statistics_frames_do_not_duplicate_pending_decodes() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ctx = Context::default();
        let mut cache = CoverCache::default();
        let image = image::DynamicImage::new_rgba8(100, 150);
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        assert!(
            cache
                .texture_sized(&ctx, "book", png.get_ref(), [48, 72])
                .is_none()
        );
        cache.spawn(&runtime, move || {
            sender.send(()).unwrap();
        });
        for _ in 0..5 {
            cache.begin_frame(&ctx);
            cache.texture_sized(&ctx, "book", png.get_ref(), [48, 72]);
            assert!(cache.wanted.is_empty());
        }
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        cache.begin_frame(&ctx);
        assert!(
            cache
                .texture_sized(&ctx, "book", png.get_ref(), [48, 72])
                .is_some()
        );
        assert!(cache.in_flight.is_empty());
    }
}
