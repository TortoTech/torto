//! Archive-local SVG images. Pixels are owned by the existing renderer cache;
//! only a bounded dimension summary is kept on the publication.
use rebook_publication::{BookSource, PublicationError, PublicationUrl, RasterResource};
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_PIXELS: f64 = 8_000_000.0;

#[derive(Debug, Default)]
pub(crate) struct Dimensions(Mutex<HashMap<PublicationUrl, Option<[u32; 2]>>>);
impl Dimensions {
    pub(crate) fn get(
        &self,
        source: &dyn BookSource,
        href: &PublicationUrl,
    ) -> Result<Option<[u32; 2]>, PublicationError> {
        if let Some(value) = self.0.lock().ok().and_then(|m| m.get(href).copied()) {
            return Ok(value);
        }
        let resource = source.resource(href)?;
        let value = parse_tree(source, href, &resource.bytes)
            .ok()
            .map(|t| dimensions(t.size()));
        if let Ok(mut cache) = self.0.lock() {
            if cache.len() >= 128
                && let Some(old) = cache.keys().next().cloned()
            {
                cache.remove(&old);
            }
            cache.insert(href.clone(), value);
        }
        Ok(value)
    }
}

fn dimensions(size: resvg::usvg::Size) -> [u32; 2] {
    let (w, h) = (size.width() as f64, size.height() as f64);
    let scale = (4096.0 / w)
        .min(4096.0 / h)
        .min((MAX_PIXELS / (w * h)).sqrt())
        .min(1.0);
    [
        (w * scale).floor().max(1.0) as u32,
        (h * scale).floor().max(1.0) as u32,
    ]
}

fn raster_kind(data: Arc<Vec<u8>>) -> Option<resvg::usvg::ImageKind> {
    if data.len() > MAX_BYTES {
        return None;
    }
    let reader = image::ImageReader::new(Cursor::new(data.as_slice()))
        .with_guessed_format()
        .ok()?;
    let format = reader.format()?;
    let (w, h) = reader.into_dimensions().ok()?;
    if f64::from(w) * f64::from(h) > MAX_PIXELS {
        return None;
    }
    match format {
        image::ImageFormat::Png => Some(resvg::usvg::ImageKind::PNG(data)),
        image::ImageFormat::Jpeg => Some(resvg::usvg::ImageKind::JPEG(data)),
        image::ImageFormat::Gif => Some(resvg::usvg::ImageKind::GIF(data)),
        image::ImageFormat::WebP => Some(resvg::usvg::ImageKind::WEBP(data)),
        _ => None,
    }
}

fn parse_tree(
    source: &dyn BookSource,
    href: &PublicationUrl,
    bytes: &[u8],
) -> Result<resvg::usvg::Tree, PublicationError> {
    if bytes.len() > MAX_BYTES {
        return Err(PublicationError::ResourceLimit(
            "SVG exceeds byte limit".into(),
        ));
    }
    let bytes_loaded = AtomicUsize::new(0);
    let files_loaded = AtomicUsize::new(0);
    let options = resvg::usvg::Options {
        fontdb: rebook_layout::svg_font_database(),
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, data, _| {
                if bytes_loaded
                    .fetch_add(data.len(), Ordering::Relaxed)
                    .saturating_add(data.len())
                    > MAX_BYTES
                {
                    return None;
                }
                raster_kind(data)
            }),
            resolve_string: Box::new(|value, _| {
                if files_loaded.fetch_add(1, Ordering::Relaxed) >= 16 {
                    return None;
                }
                let target = href.resolve(value).ok()?;
                let resource = source.resource(&target).ok()?;
                if bytes_loaded
                    .fetch_add(resource.bytes.len(), Ordering::Relaxed)
                    .saturating_add(resource.bytes.len())
                    > MAX_BYTES
                {
                    return None;
                }
                raster_kind(Arc::new(resource.bytes.to_vec()))
            }),
        },
        ..Default::default()
    };
    resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|e| PublicationError::InvalidPublication(format!("Invalid SVG: {e}")))
}

pub(crate) fn raster(
    source: &dyn BookSource,
    href: &PublicationUrl,
) -> Result<Option<RasterResource>, PublicationError> {
    let resource = source.resource(href)?;
    let tree = parse_tree(source, href, &resource.bytes)?;
    let [width, height] = dimensions(tree.size());
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| PublicationError::ResourceLimit("SVG raster dimensions".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(
            width as f32 / tree.size().width(),
            height as f32 / tree.size().height(),
        ),
        &mut pixmap.as_mut(),
    );
    let mut pixels = pixmap.take();
    // resvg supplies premultiplied RGBA; publication pixels use straight RGBA.
    for p in pixels.chunks_exact_mut(4) {
        if p[3] != 0 && p[3] != 255 {
            let alpha = u16::from(p[3]);
            for c in &mut p[..3] {
                *c = ((u16::from(*c) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    Ok(Some(RasterResource {
        width,
        height,
        pixels: pixels.into(),
    }))
}
