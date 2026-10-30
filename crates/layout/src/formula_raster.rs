//! Formula geometry is independent of pixel preparation. The interactive
//! reader schedules these rasters through the same bounded queue as images.
use super::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex};

const GEOMETRY_BUDGET: usize = 8 * 1024 * 1024;
const GEOMETRY_ENTRIES: usize = 512;
const PADDING: f32 = 1.5;
const SCALE: f32 = 2.0;

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    latex: String,
    size: u32,
    color: [u8; 4],
    display: bool,
}

struct Geometry {
    svg: Arc<str>,
    width: f32,
    height: f32,
    descent: f32,
    size: f32,
}
#[derive(Default)]
struct GeometryCache {
    entries: HashMap<Key, (Result<Arc<Geometry>, String>, u64, usize)>,
    bytes: usize,
    clock: u64,
}
static GEOMETRY: LazyLock<Mutex<GeometryCache>> =
    LazyLock::new(|| Mutex::new(GeometryCache::default()));

fn geometry(
    run: &MathRun,
    typography: &ReaderTypography,
    color: Rgba,
) -> Result<Arc<Geometry>, String> {
    let _timing = timing::stage(timing::TimingStage::FormulaLayout);
    let size = (typography.font_size
        * run.size_scale.clamp(0.5, 3.0)
        * if run.display { 1.12 } else { 1.0 })
    .max(typography.minimum_font_size);
    let key = Key {
        latex: run.latex.clone(),
        size: size.to_bits(),
        color: [color.red, color.green, color.blue, color.alpha],
        display: run.display,
    };
    {
        let mut cache = GEOMETRY.lock().unwrap_or_else(|e| e.into_inner());
        cache.clock = cache.clock.wrapping_add(1);
        let clock = cache.clock;
        if let Some((result, tick, _)) = cache.entries.get_mut(&key) {
            *tick = clock;
            return result.clone();
        }
    }
    let result = rebook_math::math::render_math(&run.latex, size,
        &format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue), run.display)
        .map(|rendered| {
            let width = (rendered.width + PADDING * 2.0).max(1.0);
            let height = (rendered.ascent + rendered.descent + PADDING * 2.0).max(1.0);
            let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{} {} {} {}" width="{}" height="{}">{}</svg>"#,
                -PADDING, -rendered.ascent - PADDING, width, height, width, height, rendered.svg_fragment);
            Arc::new(Geometry { svg: svg.into(), width, height, descent: rendered.descent, size })
        });
    let bytes = key.latex.len() + result.as_ref().map_or_else(|e| e.len(), |g| g.svg.len()) + 128;
    let mut cache = GEOMETRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((existing, _, _)) = cache.entries.get(&key) {
        return existing.clone();
    }
    if bytes <= GEOMETRY_BUDGET {
        while cache.bytes + bytes > GEOMETRY_BUDGET || cache.entries.len() >= GEOMETRY_ENTRIES {
            let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, (_, tick, _))| *tick)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some((_, _, size)) = cache.entries.remove(&oldest) {
                cache.bytes -= size;
            }
        }
        let tick = cache.clock;
        cache.bytes += bytes;
        cache.entries.insert(key, (result.clone(), tick, bytes));
    }
    result
}

pub(super) struct PreparedFormula {
    pub image: RasterImage,
    pub width: f32,
    pub height: f32,
    geometry: Arc<Geometry>,
}
impl PreparedFormula {
    pub fn vertical_metrics(&self, height: f32) -> (f32, f32) {
        let ratio = height / self.geometry.height;
        let below = (self.geometry.descent + PADDING) * ratio;
        let above = (height - below).max(self.geometry.size * 0.8);
        let box_height = (above + below.max(self.geometry.size * 0.2)).max(height);
        (box_height, box_height - height + below)
    }
    pub fn into_tuple(self) -> (RasterImage, f32, f32) {
        (self.image, self.width, self.height)
    }
}

struct FormulaTask {
    geometry: Arc<Geometry>,
    dimensions: [u32; 2],
    fonts: Arc<resvg::usvg::fontdb::Database>,
    family: String,
}
impl std::fmt::Debug for FormulaTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormulaTask")
            .field("dimensions", &self.dimensions)
            .finish()
    }
}
impl raster_cache::RasterTask for FormulaTask {
    fn render(&self) -> Result<RasterImage, LayoutError> {
        let _timing = timing::stage(timing::TimingStage::FormulaRaster);
        let options = resvg::usvg::Options {
            fontdb: self.fonts.clone(),
            font_family: self.family.clone(),
            ..Default::default()
        };
        let tree = resvg::usvg::Tree::from_data(self.geometry.svg.as_bytes(), &options)
            .map_err(|e| PublicationError::InvalidPublication(e.to_string()))?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(self.dimensions[0], self.dimensions[1])
            .ok_or_else(|| {
                PublicationError::InvalidPublication("Formula raster is too large".into())
            })?;
        resvg::render(
            &tree,
            resvg::usvg::Transform::from_scale(
                self.dimensions[0] as f32 / self.geometry.width,
                self.dimensions[1] as f32 / self.geometry.height,
            ),
            &mut pixmap.as_mut(),
        );
        Ok(RasterImage {
            deferred: None,
            origin: None,
            blob: None,
            width: self.dimensions[0],
            height: self.dimensions[1],
            pixels: pixmap.take().into(),
        })
    }
}

#[derive(Clone)]
pub(super) struct Placement {
    pub image: RasterImage,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl std::fmt::Debug for Placement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Placement")
            .field("bounds", &[self.x, self.y, self.width, self.height])
            .field("pixels", &[self.image.width, self.image.height])
            .finish()
    }
}

#[derive(Debug)]
struct CompositeTask {
    dimensions: [u32; 2],
    layers: Vec<Placement>,
}
impl raster_cache::RasterTask for CompositeTask {
    fn render(&self) -> Result<RasterImage, LayoutError> {
        use resvg::tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};
        let error =
            || PublicationError::InvalidPublication("Invalid formula composite dimensions".into());
        let mut canvas = Pixmap::new(self.dimensions[0], self.dimensions[1]).ok_or_else(error)?;
        for layer in &self.layers {
            let image = if let Some(request) = &layer.image.deferred {
                request.render_generated()?
            } else {
                layer.image.clone()
            };
            let pixels = Pixmap::from_vec(
                image.pixels.to_vec(),
                IntSize::from_wh(image.width, image.height).ok_or_else(error)?,
            )
            .ok_or_else(error)?;
            canvas.draw_pixmap(
                0,
                0,
                pixels.as_ref(),
                &PixmapPaint::default(),
                Transform::from_scale(
                    layer.width * SCALE / image.width as f32,
                    layer.height * SCALE / image.height as f32,
                )
                .post_translate(layer.x * SCALE, layer.y * SCALE),
                None,
            );
        }
        Ok(RasterImage {
            deferred: None,
            origin: None,
            blob: None,
            width: canvas.width(),
            height: canvas.height(),
            pixels: canvas.take().into(),
        })
    }
}

pub(super) fn compose(width: f32, height: f32, layers: Vec<Placement>) -> Option<RasterImage> {
    let dimensions = [
        (width * SCALE).ceil().max(1.0) as u32,
        (height * SCALE).ceil().max(1.0) as u32,
    ];
    let deferred = layers.iter().find_map(|l| l.image.deferred.clone());
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for layer in &layers {
        if let Some(request) = &layer.image.deferred {
            request.key().hash(&mut hash);
        } else {
            layer.image.pixels.hash(&mut hash);
        }
        for value in [layer.x, layer.y, layer.width, layer.height] {
            value.to_bits().hash(&mut hash);
        }
    }
    let task = Arc::new(CompositeTask { dimensions, layers });
    if let Some(request) = deferred {
        Some(request.with_generated(hash.finish(), dimensions, task))
    } else {
        use raster_cache::RasterTask as _;
        task.render().ok()
    }
}

pub(super) fn prepare(
    run: &MathRun,
    typography: &ReaderTypography,
    color: Rgba,
    available_width: f32,
    options: &resvg::usvg::Options<'_>,
    deferred: Option<(&dyn BookSource, u64)>,
) -> Result<PreparedFormula, String> {
    let geometry = geometry(run, typography, color)?;
    let scale = (available_width / geometry.width).min(1.0);
    let width = (geometry.width * scale).max(1.0);
    let height = (geometry.height * scale).max(1.0);
    let dimensions = [
        (width * SCALE).ceil().max(1.0) as u32,
        (height * SCALE).ceil().max(1.0) as u32,
    ];
    let task = Arc::new(FormulaTask {
        geometry: geometry.clone(),
        dimensions,
        fonts: options.fontdb.clone(),
        family: options.font_family.clone(),
    });
    let image = if let Some((source, generation)) = deferred {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        geometry.svg.hash(&mut hash);
        options.font_family.hash(&mut hash);
        // Font discovery is immutable and shared across layout workers.
        (Arc::as_ptr(&options.fontdb) as usize).hash(&mut hash);
        raster_cache::prepare_generated(source, hash.finish(), dimensions, task, generation)
    } else {
        // The task does not access the source for embedded formula SVGs.
        use raster_cache::RasterTask as _;
        task.render().map_err(|e| e.to_string())?
    };
    Ok(PreparedFormula {
        image,
        width,
        height,
        geometry,
    })
}

pub(super) fn prepare_svg(
    svg: String,
    width: f32,
    height: f32,
    options: &resvg::usvg::Options<'_>,
    deferred: Option<(&dyn BookSource, u64)>,
) -> Result<RasterImage, String> {
    let dimensions = [
        (width * SCALE).ceil().max(1.0) as u32,
        (height * SCALE).ceil().max(1.0) as u32,
    ];
    if dimensions[0] > 8192 || dimensions[1] > 16384 {
        return Err("Formula notes are too large".into());
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    svg.hash(&mut hash);
    options.font_family.hash(&mut hash);
    (Arc::as_ptr(&options.fontdb) as usize).hash(&mut hash);
    let task = Arc::new(FormulaTask {
        geometry: Arc::new(Geometry {
            svg: svg.into(),
            width,
            height,
            descent: 0.0,
            size: 0.0,
        }),
        dimensions,
        fonts: options.fontdb.clone(),
        family: options.font_family.clone(),
    });
    if let Some((source, generation)) = deferred {
        Ok(raster_cache::prepare_generated(
            source,
            hash.finish(),
            dimensions,
            task,
            generation,
        ))
    } else {
        use raster_cache::RasterTask as _;
        task.render().map_err(|e| e.to_string())
    }
}
