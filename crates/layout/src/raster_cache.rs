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
    variants: HashMap<u64, u64>,
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
    let decoded = image::load_from_memory(&resource.bytes)?.into_rgba8();
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
    let _timing = timing::stage(timing::TimingStage::ImagePixels);
    let (width, height) = (image.width(), image.height());
    let reduced = width > target[0] || height > target[1];
    let decoded = if reduced {
        resize_display_image(image, target)
    } else {
        image
    }
    .into_rgba8();
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

fn resize_display_image(image: image::DynamicImage, target: [u32; 2]) -> image::DynamicImage {
    image_processing::resize(image, target, image::imageops::FilterType::Lanczos3)
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
            self.variants.remove(&key);
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
                    self.variants.remove(&oldest);
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
    let _timing = timing::stage(timing::TimingStage::ImageCache);
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
    let (decoded, encoded) = {
        let _timing = timing::stage(timing::TimingStage::ImageSource);
        let decoded = source.raster_resource(&block.href)?;
        let encoded = if decoded.is_none() {
            Some(source.resource(&block.href)?)
        } else {
            None
        };
        (decoded, encoded)
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.book().id.hash(&mut hash);
    owner.1.hash(&mut hash);
    block.href.hash(&mut hash);
    exact.hash(&mut hash);
    if let Some(image) = &decoded {
        image.width.hash(&mut hash);
        image.height.hash(&mut hash);
        image.pixels.hash(&mut hash);
    } else if let Some(resource) = &encoded {
        resource.bytes.hash(&mut hash);
    }
    let resource_key = hash.finish();
    if !exact {
        target.hash(&mut hash);
    }
    let key = hash.finish();
    {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.clock = cache.clock.wrapping_add(1);
        let clock = cache.clock;
        // Reuse the smallest sufficiently detailed variant, preserving its Blob
        // identity. A viewport/DPI bucket change must not decode the resource
        // again when retained pixels already meet the requested resolution.
        let reusable = (!exact)
            .then(|| {
                cache
                    .variants
                    .iter()
                    .filter_map(|(candidate, resource)| {
                        let (image, _) = cache.entries.get(candidate)?;
                        (*resource == resource_key && sufficient_detail(image, target))
                            .then_some((*candidate, image.pixels.len()))
                    })
                    .min_by_key(|(_, bytes)| *bytes)
                    .map(|(candidate, _)| candidate)
            })
            .flatten();
        let hit = cache.entries.contains_key(&key).then_some(key).or(reusable);
        if let Some((image, touched)) = hit.and_then(|key| cache.entries.get_mut(&key)) {
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
        let image = {
            let _timing = timing::stage(timing::TimingStage::ImageDecode);
            image::load_from_memory(&encoded.expect("encoded image resource").bytes)?
        };
        if exact {
            let _timing = timing::stage(timing::TimingStage::ImagePixels);
            let rgba = image.into_rgba8();
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
        if !exact {
            cache.variants.insert(key, resource_key);
        }
    }
    Ok(image)
}

fn sufficient_detail(image: &RasterImage, target: [u32; 2]) -> bool {
    let Some(origin) = &image.origin else {
        // The full original is already retained; no larger decode is possible.
        return true;
    };
    let scale = (f64::from(target[0]) / f64::from(origin.width))
        .min(f64::from(target[1]) / f64::from(origin.height))
        .min(1.0);
    // Match the display resizer's rounded dimensions exactly;
    // rounding up would falsely reject an equally detailed retained thumbnail.
    f64::from(image.width) >= (f64::from(origin.width) * scale).round().max(1.0)
        && f64::from(image.height) >= (f64::from(origin.height) * scale).round().max(1.0)
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
            cache.variants.insert(key, key + 10);
        }
        cache.retire_view("book", true);
        assert!(!cache.entries.contains_key(&1));
        assert!(cache.entries.contains_key(&2));
        assert!(cache.entries.contains_key(&3));
        assert_eq!(cache.stats.bytes, 32);
        assert_eq!(cache.stats.images, 2);
        assert_eq!(cache.mode_generations.get(&("book".into(), true)), Some(&1));
        assert!(!cache.variants.contains_key(&1));
        assert_eq!(cache.variants.len(), 2);
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
        let capture = timing::TimingScope::start().unwrap();
        let first = load(&source, &block, [512, 1024], generation()).unwrap();
        let cold = capture.finish();
        assert_eq!(cold.calls(timing::TimingStage::ImageDecode), 1);
        assert_eq!(cold.calls(timing::TimingStage::ImagePixels), 1);
        let capture = timing::TimingScope::start().unwrap();
        let repeated = load(&source, &block, [512, 1024], generation()).unwrap();
        let warm = capture.finish();
        assert_eq!(warm.calls(timing::TimingStage::ImageDecode), 0);
        assert_eq!(warm.calls(timing::TimingStage::ImagePixels), 0);
        assert!(Arc::ptr_eq(&first.pixels, &repeated.pixels));
        assert_eq!(
            first.blob.as_ref().unwrap().id(),
            repeated.blob.unwrap().id()
        );
        for target in [[384, 768], [1024, 512]] {
            let smaller = load(&source, &block, target, generation()).unwrap();
            assert!(Arc::ptr_eq(&first.pixels, &smaller.pixels));
            assert_eq!(
                first.blob.as_ref().unwrap().id(),
                smaller.blob.unwrap().id()
            );
        }
        let larger = load(&source, &block, [1024, 2048], generation()).unwrap();
        assert!(!Arc::ptr_eq(&first.pixels, &larger.pixels));
        assert!(larger.width > first.width && larger.height > first.height);
        // Exact formula pixels never use a display-size variant.
        let exact = load(
            &source,
            &ImageBlock {
                formula_image: true,
                ..block.clone()
            },
            [384, 768],
            generation(),
        )
        .unwrap();
        assert_eq!((exact.width, exact.height), (1200, 1800));
        assert!(exact.origin.is_none());
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
    fn resizing_transparent_pixels_does_not_bleed_hidden_color() {
        let source = image::RgbaImage::from_fn(2, 2, |x, _| {
            if x == 0 {
                image::Rgba([255, 0, 0, 0])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let resized = display_raster(
            source.into(),
            &PublicationUrl::parse("transparent.png").unwrap(),
            [1, 1],
        );
        assert_eq!((resized.width, resized.height), (1, 1));
        assert!(resized.pixels[0] <= 1);
        assert!(resized.pixels[2] >= 254);
        assert!((127..=128).contains(&resized.pixels[3]));
    }

    #[test]
    fn display_resize_handles_grayscale_rgb_alpha_and_high_bit_depth() {
        for color in [
            image::ColorType::L8,
            image::ColorType::La8,
            image::ColorType::Rgb8,
            image::ColorType::Rgba8,
            image::ColorType::L16,
            image::ColorType::La16,
            image::ColorType::Rgb16,
            image::ColorType::Rgba16,
            image::ColorType::Rgb32F,
            image::ColorType::Rgba32F,
        ] {
            let source = image::DynamicImage::new(33, 17, color);
            let color_space = source.color_space();
            let resized = resize_display_image(source, [16, 16]);
            assert_eq!(resized.color(), color);
            assert_eq!(resized.color_space(), color_space);
            assert_eq!((resized.width(), resized.height()), (16, 8));
        }
        let source = image::RgbaImage::from_pixel(3, 2, image::Rgba([42, 43, 44, 128]));
        let displayed = display_raster(
            source.into(),
            &PublicationUrl::parse("small.png").unwrap(),
            [256, 256],
        );
        assert_eq!((displayed.width, displayed.height), (3, 2));
        assert!(displayed.origin.is_none());
        assert_eq!(&displayed.pixels[..4], &[42, 43, 44, 128]);
    }

    #[test]
    fn cache_budget_and_concurrent_insert_preserve_shared_identity() {
        let mut cache = Cache::default();
        let first = cache.insert(1, raster(2, 2, vec![255; 16].into(), None), 32);
        cache.variants.insert(1, 10);
        let same = cache.insert(1, raster(2, 2, vec![255; 16].into(), None), 32);
        assert!(Arc::ptr_eq(&first.pixels, &same.pixels));
        assert_eq!(first.blob.as_ref().unwrap().id(), same.blob.unwrap().id());
        cache.clock += 1;
        cache.insert(2, raster(2, 2, vec![255; 16].into(), None), 32);
        cache.clock += 1;
        cache.insert(3, raster(2, 2, vec![255; 16].into(), None), 32);
        assert_eq!(cache.stats.bytes, 32);
        assert!(!cache.entries.contains_key(&1));
        assert!(!cache.variants.contains_key(&1));
        assert_eq!(first.pixels.len(), 16);
    }

    #[test]
    fn detail_check_respects_both_dimensions_and_native_resolution() {
        let origin = Some(RasterOrigin {
            href: PublicationUrl::parse("portrait.png").unwrap(),
            width: 1200,
            height: 1800,
        });
        let small = raster(512, 768, Arc::from([]), origin);
        assert!(sufficient_detail(&small, [384, 768]));
        assert!(sufficient_detail(&small, [1024, 512]));
        assert!(!sufficient_detail(&small, [1024, 2048]));
        assert!(sufficient_detail(
            &raster(1200, 1800, Arc::from([]), None),
            [2400, 3600]
        ));
        let rounded = display_raster(
            image::DynamicImage::new_rgba8(2001, 3001),
            &PublicationUrl::parse("rounded.png").unwrap(),
            [512, 768],
        );
        assert!(sufficient_detail(&rounded, [1024, 768]));
    }
}
