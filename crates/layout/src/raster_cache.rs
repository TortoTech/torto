use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};

use super::*;

const BUDGET: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterOrigin {
    pub href: PublicationUrl,
    pub width: u32,
    pub height: u32,
}

#[derive(Default, Clone, Copy)]
pub struct RasterCacheStats {
    pub bytes: usize,
    pub images: usize,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<u64, (RasterImage, u64)>,
    owners: HashMap<u64, (String, bool)>,
    mode_generations: HashMap<(String, bool), u64>,
    clock: u64,
    generation: u64,
    stats: RasterCacheStats,
}
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::default()));

pub fn raster_cache_stats() -> RasterCacheStats {
    CACHE.lock().map(|c| c.stats).unwrap_or_default()
}

pub(super) fn generation() -> u64 {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).generation
}

pub fn clear_raster_cache() {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let generation = cache.generation.wrapping_add(1);
    *cache = Cache {
        generation,
        ..Cache::default()
    };
}

/// Removes pixels belonging to one inactive publication view. Runs on a
/// retirement worker; in-flight decodes cannot insert into the retired view.
pub fn retire_publication_rasters(publication_id: &str, fixed_page: bool) {
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    cache.retire_view(publication_id, fixed_page);
}

fn raster(width: u32, height: u32, pixels: Arc<[u8]>, origin: Option<RasterOrigin>) -> RasterImage {
    RasterImage {
        origin,
        blob: Some(peniko::Blob::new(Arc::new(pixels.clone()))),
        width,
        height,
        pixels,
    }
}

pub fn load_original_raster(
    source: &dyn BookSource,
    href: &PublicationUrl,
) -> Result<RasterImage, LayoutError> {
    if let Some(image) = source.raster_resource(href)? {
        return Ok(raster(image.width, image.height, image.pixels, None));
    }
    let resource = source.resource(href)?;
    let decoded = image::load_from_memory(&resource.bytes)?.to_rgba8();
    Ok(raster(
        decoded.width(),
        decoded.height(),
        decoded.into_raw().into(),
        None,
    ))
}

fn display_raster(
    image: image::DynamicImage,
    href: &PublicationUrl,
    target: [u32; 2],
) -> RasterImage {
    let (width, height) = (image.width(), image.height());
    let reduced = width > target[0] || height > target[1];
    let decoded = if reduced {
        image.thumbnail(target[0], target[1])
    } else {
        image
    }
    .to_rgba8();
    raster(
        decoded.width(),
        decoded.height(),
        decoded.into_raw().into(),
        reduced.then(|| RasterOrigin {
            href: href.clone(),
            width,
            height,
        }),
    )
}

impl Cache {
    fn retire_view(&mut self, publication_id: &str, fixed_page: bool) {
        let owner = (publication_id.to_owned(), fixed_page);
        let epoch = self.mode_generations.entry(owner.clone()).or_default();
        *epoch = epoch.wrapping_add(1);
        let retired = self
            .owners
            .iter()
            .filter(|(_, value)| **value == owner)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        for key in retired {
            self.owners.remove(&key);
            if let Some((image, _)) = self.entries.remove(&key) {
                self.stats.bytes -= image.pixels.len();
            }
        }
        self.stats.images = self.entries.len();
    }

    fn insert(&mut self, key: u64, raster: RasterImage, budget: usize) -> RasterImage {
        // Foreground layout and prefetch share the same Blob ID even if both
        // finished decoding at once.
        if let Some((existing, _)) = self.entries.get(&key) {
            return existing.clone();
        }
        if raster.pixels.len() <= budget {
            while self.stats.bytes + raster.pixels.len() > budget {
                let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, (_, tick))| *tick)
                    .map(|(key, _)| *key)
                else {
                    break;
                };
                if let Some((image, _)) = self.entries.remove(&oldest) {
                    self.owners.remove(&oldest);
                    self.stats.bytes -= image.pixels.len();
                }
            }
            self.stats.bytes += raster.pixels.len();
            self.entries.insert(key, (raster.clone(), self.clock));
            self.stats.images = self.entries.len();
        }
        raster
    }
}

pub(super) fn load(
    source: &dyn BookSource,
    block: &ImageBlock,
    target: [u32; 2],
    generation: u64,
) -> Result<RasterImage, LayoutError> {
    let owner = (
        source.book().id.to_string(),
        source.book().metadata.layout == RenditionLayout::PrePaginated,
    );
    let mode_generation = {
        let cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
        cache
            .mode_generations
            .get(&owner)
            .copied()
            .unwrap_or_default()
    };
    // PDF text coordinates and formulas require exact pixels; they still reuse
    // decoded data and its upload identity.
    let exact = block.text_layer.is_some() || block.formula_image || block.formula.is_some();
    let decoded = source.raster_resource(&block.href)?;
    let encoded = if decoded.is_none() {
        Some(source.resource(&block.href)?)
    } else {
        None
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.book().id.hash(&mut hash);
    block.href.hash(&mut hash);
    exact.hash(&mut hash);
    if !exact {
        target.hash(&mut hash);
    }
    if let Some(image) = &decoded {
        image.width.hash(&mut hash);
        image.height.hash(&mut hash);
        image.pixels.hash(&mut hash);
    } else if let Some(resource) = &encoded {
        resource.bytes.hash(&mut hash);
    }
    let key = hash.finish();
    {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.clock = cache.clock.wrapping_add(1);
        let clock = cache.clock;
        if let Some((image, touched)) = cache.entries.get_mut(&key) {
            *touched = clock;
            let image = image.clone();
            cache.stats.hits += 1;
            return Ok(image);
        }
        cache.stats.misses += 1;
    }
    let image = if let Some(image) = decoded {
        // Source-owned rasters (notably PDF) have their own resolution policy.
        raster(image.width, image.height, image.pixels, None)
    } else {
        let image = image::load_from_memory(&encoded.expect("encoded image resource").bytes)?;
        if exact {
            let rgba = image.to_rgba8();
            raster(rgba.width(), rgba.height(), rgba.into_raw().into(), None)
        } else {
            display_raster(image, &block.href, target)
        }
    };
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    // Closing/replacing a book invalidates an in-progress decode's insertion.
    if cache.generation != generation
        || cache
            .mode_generations
            .get(&owner)
            .copied()
            .unwrap_or_default()
            != mode_generation
    {
        return Ok(image);
    }
    let image = cache.insert(key, image, BUDGET);
    if cache.entries.contains_key(&key) {
        cache.owners.insert(key, owner);
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebook_publication::{Book, Metadata, PublicationId};

    #[test]
    fn retiring_one_view_keeps_other_views_and_books() {
        let mut cache = Cache::default();
        for (key, book, fixed) in [(1, "book", true), (2, "book", false), (3, "other", true)] {
            cache.insert(key, raster(2, 2, vec![255; 16].into(), None), 128);
            cache.owners.insert(key, (book.into(), fixed));
        }
        cache.retire_view("book", true);
        assert!(!cache.entries.contains_key(&1));
        assert!(cache.entries.contains_key(&2));
        assert!(cache.entries.contains_key(&3));
        assert_eq!(cache.stats.bytes, 32);
        assert_eq!(cache.stats.images, 2);
        assert_eq!(cache.mode_generations.get(&("book".into(), true)), Some(&1));
    }

    #[test]
    fn dpi_variants_reuse_pixels_keep_layout_and_reload_full_original() {
        struct Source {
            book: Book,
            bytes: Arc<[u8]>,
        }
        impl BookSource for Source {
            fn book(&self) -> &Book {
                &self.book
            }
            fn parse_section(&self, _: usize) -> Result<Section, PublicationError> {
                unreachable!()
            }
            fn resource(
                &self,
                href: &PublicationUrl,
            ) -> Result<rebook_publication::Resource, PublicationError> {
                Ok(rebook_publication::Resource {
                    href: href.clone(),
                    media_type: "image/png".into(),
                    bytes: self.bytes.clone(),
                })
            }
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1200,
            1800,
            image::Rgba([42, 43, 44, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let source = Source {
            book: Book {
                id: PublicationId::new("raster-dpi-original-test").unwrap(),
                metadata: Metadata::default(),
                cover: None,
                sections: Vec::new(),
                table_of_contents: Vec::new(),
            },
            bytes: png.into_inner().into(),
        };
        let block = ImageBlock {
            href: PublicationUrl::parse("large.png").unwrap(),
            alt: String::new(),
            style: ImageStyle::default(),
            source: None,
            formula_image: false,
            formula: None,
            text_layer: None,
        };
        let first = load(&source, &block, [512, 1024], generation()).unwrap();
        let repeated = load(&source, &block, [512, 1024], generation()).unwrap();
        assert!(Arc::ptr_eq(&first.pixels, &repeated.pixels));
        assert_eq!(
            first.blob.as_ref().unwrap().id(),
            repeated.blob.unwrap().id()
        );
        let mut geometry = Vec::new();
        for scale in [1.0, 2.0] {
            let layout = LayoutEngine::new()
                .layout_blocks(
                    &source,
                    &[Block::Image(block.clone())],
                    LayoutViewport::new(500, 900)
                        .unwrap()
                        .with_raster_scale(scale),
                    &ReaderStyle::default(),
                )
                .unwrap();
            let placed = layout
                .pages
                .iter()
                .flat_map(|page| &page.items)
                .find_map(|item| {
                    if let PageItem::Image(image) = item {
                        Some(image)
                    } else {
                        None
                    }
                })
                .unwrap();
            geometry.push((placed.width, placed.height));
            assert_eq!(placed.image.width, if scale == 1.0 { 512 } else { 1024 });
        }
        assert_eq!(geometry[0], geometry[1]);
        let original = load_original_raster(&source, &block.href).unwrap();
        assert_eq!((original.width, original.height), (1200, 1800));
        assert_eq!(original.pixels.len(), 1200 * 1800 * 4);
        assert_eq!(&original.pixels[..4], &[42, 43, 44, 255]);
    }

    #[test]
    fn display_variant_preserves_intrinsic_geometry() {
        let href = PublicationUrl::parse("image.png").unwrap();
        let image = display_raster(
            image::DynamicImage::new_rgba8(2000, 3000),
            &href,
            [512, 768],
        );
        assert_eq!((image.width, image.height), (512, 768));
        let origin = image.origin.unwrap();
        assert_eq!((origin.width, origin.height), (2000, 3000));
        assert_eq!(origin.href, href);
    }

    #[test]
    fn cache_budget_and_concurrent_insert_preserve_shared_identity() {
        let mut cache = Cache::default();
        let first = cache.insert(1, raster(2, 2, vec![255; 16].into(), None), 32);
        let same = cache.insert(1, raster(2, 2, vec![255; 16].into(), None), 32);
        assert!(Arc::ptr_eq(&first.pixels, &same.pixels));
        assert_eq!(first.blob.as_ref().unwrap().id(), same.blob.unwrap().id());
        cache.clock += 1;
        cache.insert(2, raster(2, 2, vec![255; 16].into(), None), 32);
        cache.clock += 1;
        cache.insert(3, raster(2, 2, vec![255; 16].into(), None), 32);
        assert_eq!(cache.stats.bytes, 32);
        assert!(!cache.entries.contains_key(&1));
        assert_eq!(first.pixels.len(), 16);
    }
}
