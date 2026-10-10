//! Conversion-only image work. No render cache is shared with the reader.
mod encoding;
mod pipeline;
use super::*;
use hayro::hayro_interpret::font::GlyphRun;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Device, DrawMode, DrawProps, Image, ImageDrawProps, SoftMask,
};
pub(super) use pipeline::Pipeline;

// Vello CPU 0.3.0 uses 256 x 4 wide tiles. Keep the local viewport on
// the original tile grid to preserve edge coverage and image sampling.
const RASTER_TILE_WIDTH: u32 = 256;
const RASTER_TILE_HEIGHT: u32 = 4;

pub(super) fn save_regions<'a>(
    publication: &'a PdfPublication,
    index: usize,
    native: &NativePage,
    regions: &[layout::Crop],
    directory: &Path,
    cancelled: &AtomicBool,
    cache: &mut RenderCache<'a>,
    timings: &mut ConversionTimings,
) -> Result<(), String> {
    cache.begin_page();
    if regions.is_empty() {
        return Ok(());
    }
    let page = &publication.pdf.pages()[index];
    let (width, height) = page.render_dimensions();
    let scale = (PAGE_MAX_DIMENSION / width.max(height).max(1.0)).min(MAX_RENDER_SCALE);
    let full_width = (width * scale).floor() as u32;
    let full_height = (height * scale).floor() as u32;
    let sx = f64::from(full_width) / native.width;
    let sy = f64::from(full_height) / native.height;
    let writing = Instant::now();
    let exported = export_simple_images(
        publication,
        index,
        native,
        regions,
        directory,
        cancelled,
        scale,
        (sx, sy),
    )?;
    timings.image_write_ms += elapsed_ms(writing);
    timings.direct_images += exported.len();
    let mut crops = Vec::new();
    for region in regions {
        check_cancelled(cancelled)?;
        if exported.contains(&region.path) {
            continue;
        }
        let rect = region.bounds;
        let x0 = (rect.x0 * sx).floor().max(0.0) as u32;
        let y0 = (rect.y0 * sy).floor().max(0.0) as u32;
        let x1 = (rect.x1 * sx).ceil().clamp(0.0, f64::from(full_width)) as u32;
        let y1 = (rect.y1 * sy).ceil().clamp(0.0, f64::from(full_height)) as u32;
        if x1 <= x0 || y1 <= y0 {
            return Err("Invalid PDF crop bounds".into());
        }
        crops.push((region, [x0, y0, x1, y1]));
    }
    if crops.is_empty() {
        return Ok(());
    }
    // One viewport per physical page avoids decoding the same image repeatedly
    // when formulas/subfigures produce multiple crops. Pixel-aligned padding
    // preserves the edge sampling of a full-page render.
    let x0 = crops
        .iter()
        .map(|(_, r)| r[0])
        .min()
        .unwrap()
        .saturating_sub(4)
        / RASTER_TILE_WIDTH
        * RASTER_TILE_WIDTH;
    let y0 = crops
        .iter()
        .map(|(_, r)| r[1])
        .min()
        .unwrap()
        .saturating_sub(4)
        / RASTER_TILE_HEIGHT
        * RASTER_TILE_HEIGHT;
    let x1 = (crops.iter().map(|(_, r)| r[2]).max().unwrap() + 4).min(full_width);
    let y1 = (crops.iter().map(|(_, r)| r[3]).max().unwrap() + 4).min(full_height);
    let full = x0 == 0 && y0 == 0 && x1 == full_width && y1 == full_height;
    timings.full_pages += usize::from(full);
    timings.region_pages += usize::from(!full);
    timings.raster_pixels += u64::from(x1 - x0) * u64::from(y1 - y0);
    check_cancelled(cancelled)?;
    let rasterizing = Instant::now();
    let pixmap = render_page_region(
        page,
        cache,
        &interpreter_settings(),
        &PageRasterSettings {
            x_scale: scale,
            y_scale: scale,
            width: Some((x1 - x0) as u16),
            height: Some((y1 - y0) as u16),
            bg_color: WHITE,
        },
        (x0 as u16, y0 as u16),
    );
    timings.raster_ms += elapsed_ms(rasterizing);
    let writing = Instant::now();
    for (region, rect) in crops {
        check_cancelled(cancelled)?;
        let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
        // Copy only the saved crop, not a second complete viewport bitmap.
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        let stride = usize::from(pixmap.width()) * 4;
        for row in (rect[1] - y0)..(rect[3] - y0) {
            let start = row as usize * stride + (rect[0] - x0) as usize * 4;
            pixels.extend_from_slice(&pixmap.data_as_u8_slice()[start..start + w as usize * 4]);
        }
        encoding::save(&directory.join(&region.path), pixels, w, h)?;
    }
    timings.image_write_ms += elapsed_ms(writing);
    Ok(())
}

struct Exporter<'a> {
    page: &'a NativePage,
    crops: &'a [layout::Crop],
    directory: &'a Path,
    cancelled: &'a AtomicBool,
    scale: f32,
    pixel_scale: (f64, f64),
    exported: HashSet<String>,
    error: Option<String>,
}

fn export_simple_images(
    publication: &PdfPublication,
    index: usize,
    native: &NativePage,
    crops: &[layout::Crop],
    directory: &Path,
    cancelled: &AtomicBool,
    scale: f32,
    pixel_scale: (f64, f64),
) -> Result<HashSet<String>, String> {
    if !crops.iter().any(|c| direct_candidate(native, c).is_some()) {
        return Ok(HashSet::new());
    }
    let page = &publication.pdf.pages()[index];
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, native.width, native.height),
        &cache,
        page.xref(),
        interpreter_settings(),
    );
    let mut exporter = Exporter {
        page: native,
        crops,
        directory,
        cancelled,
        scale,
        pixel_scale,
        exported: HashSet::new(),
        error: None,
    };
    interpret_page(page, &mut context, &mut exporter);
    check_cancelled(cancelled)?;
    match exporter.error {
        Some(error) => Err(error),
        None => Ok(exporter.exported),
    }
}

// Replay drawing instructions with a no-op device to access the authoritative
// PDF image decoder. It resolves calibrated/CMYK/indexed colors exactly as the
// renderer does. Only proven isolated targets are decoded and sampled; no text
// outlines, page bitmap or second reading representation is built here.
impl Device<'_> for Exporter<'_> {
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn pop_clip(&mut self) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'_>>, _: BlendMode) {}
    fn pop_transparency_group(&mut self) {}
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'_>, _: &DrawMode) {}
    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, '_>, _: DrawProps<'_>, _: &DrawMode) {}
    fn draw_image(&mut self, image: Image<'_, '_>, props: ImageDrawProps<'_>) {
        let transform = props.transform;
        if self.error.is_some() || self.cancelled.load(Ordering::Acquire) {
            return;
        }
        let Image::Raster(raster) = image else { return };
        let id = raster.stream().obj_id();
        let bounds = transform.transform_rect_bbox(Rect::new(
            0.0,
            0.0,
            f64::from(raster.width()),
            f64::from(raster.height()),
        ));
        for crop in self.crops {
            if self.exported.contains(&crop.path) {
                continue;
            }
            let Some((i, encoded)) = direct_candidate(self.page, crop) else {
                continue;
            };
            if encoded.object != [id.obj_number, id.gen_number]
                || self.page.images[i]
                    .iter()
                    .zip([bounds.x0, bounds.y0, bounds.x1, bounds.y1])
                    .any(|(a, b)| (*a - b).abs() > 1e-6)
            {
                continue;
            }
            let (sx, sy) = self.pixel_scale;
            let x0 = (crop.bounds.x0 * sx).floor().max(0.0);
            let y0 = (crop.bounds.y0 * sy).floor().max(0.0);
            let x1 = (crop.bounds.x1 * sx)
                .ceil()
                .min((self.page.width * sx).round());
            let y1 = (crop.bounds.y1 * sy)
                .ceil()
                .min((self.page.height * sy).round());
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let target = Affine::scale(f64::from(self.scale)) * transform;
            let dimensions = (
                (target * Point::new(f64::from(raster.width()), 0.0) - target * Point::ORIGIN)
                    .length()
                    .ceil() as u32,
                (target * Point::new(0.0, f64::from(raster.height())) - target * Point::ORIGIN)
                    .length()
                    .ceil() as u32,
            );
            raster.with_rgba(
                |image, alpha| {
                    if alpha.is_some() || image.width().max(image.height()) > 2048 {
                        return;
                    }
                    let factors = image.scale_factors();
                    let rx0 = (x0 as u32).saturating_sub(4) / RASTER_TILE_WIDTH * RASTER_TILE_WIDTH;
                    let ry0 =
                        (y0 as u32).saturating_sub(4) / RASTER_TILE_HEIGHT * RASTER_TILE_HEIGHT;
                    let rx1 = (x1 as u32 + 4).min((self.page.width * sx).round() as u32);
                    let ry1 = (y1 as u32 + 4).min((self.page.height * sy).round() as u32);
                    let transform = Affine::translate((-f64::from(rx0), -f64::from(ry0)))
                        * target
                        * Affine::scale_non_uniform(f64::from(factors.0), f64::from(factors.1));
                    let pixmap = hayro::render_embedded_image(
                        image,
                        transform,
                        (rx1 - rx0) as u16,
                        (ry1 - ry0) as u16,
                        WHITE,
                    );
                    let width = (x1 - x0) as u32;
                    let height = (y1 - y0) as u32;
                    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
                    for y in (y0 as u32 - ry0)..(y1 as u32 - ry0) {
                        let start = (y as usize * usize::from(pixmap.width()) + x0 as usize
                            - rx0 as usize)
                            * 4;
                        pixels.extend_from_slice(
                            &pixmap.data_as_u8_slice()[start..start + width as usize * 4],
                        );
                    }
                    match encoding::save(&self.directory.join(&crop.path), pixels, width, height) {
                        Ok(()) => {
                            self.exported.insert(crop.path.clone());
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                },
                Some(dimensions),
            );
        }
    }
}

fn direct_candidate<'a>(
    page: &'a NativePage,
    crop: &layout::Crop,
) -> Option<(usize, &'a EncodedImage)> {
    let [index] = crop.image_sources.as_slice() else {
        return None;
    };
    let encoded = page.encoded_images.get(*index)?.as_ref()?;
    let rect = page.images[*index];
    let bounds = Rect::new(rect[0], rect[1], rect[2], rect[3]);
    // Only the isolated image plus the normal small white crop margin. Preserve
    // subfigure labels, rules, backgrounds, formulas and overlays by rasterizing.
    if !crop.bounds.contains_rect(bounds)
        || (crop.bounds.width() - bounds.width()).abs() > 4.01
        || (crop.bounds.height() - bounds.height()).abs() > 4.01
        || ((bounds.width() / bounds.height())
            / (f64::from(encoded.width) / f64::from(encoded.height))
            - 1.0)
            .abs()
            > 0.001
        || page
            .glyphs
            .iter()
            .any(|g| g.bounds().intersect(crop.bounds).area() > 0.0)
        || page.image_obstacles.iter().any(|r| {
            Rect::new(r[0], r[1], r[2], r[3])
                .intersect(crop.bounds)
                .area()
                > 0.0
        })
        || page.images.iter().enumerate().any(|(i, r)| {
            i != *index
                && Rect::new(r[0], r[1], r[2], r[3])
                    .intersect(crop.bounds)
                    .area()
                    > 0.0
        })
    {
        return None;
    }
    Some((*index, encoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    pub(super) fn fixture(rotate: u16, content: &str, masked: bool) -> Vec<u8> {
        let mut jpeg = Vec::new();
        let rgb = image::RgbImage::from_fn(32, 24, |x, y| {
            image::Rgb([(x * 7) as u8, (y * 9) as u8, 70])
        });
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&rgb)
            .unwrap();
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [10 20 310 220] /Rotate {rotate} /Resources << /XObject << /Im1 5 0 R >> /Font << /F1 6 0 R >> /ExtGState << /GS1 << /ca 0.5 >> >> >> /Contents 4 0 R >>").into_bytes(),
            [format!("<< /Length {} >>\nstream\n", content.len()).as_bytes(), content.as_bytes(), b"\nendstream"].concat(),
            [format!("<< /Type /XObject /Subtype /Image /Width 32 /Height 24 /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /DCTDecode {} /Length {} >>\nstream\n", if masked { "/SMask 7 0 R" } else { "" }, jpeg.len()).as_bytes(), &jpeg, b"\nendstream"].concat(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            [b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 1 >>\nstream\n".as_slice(), &[127], b"\nendstream"].concat(),
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            writeln!(&mut bytes, "{} 0 obj", i + 1).unwrap();
            bytes.extend(object);
            bytes.extend(b"\nendobj\n");
        }
        let xref = bytes.len();
        writeln!(
            &mut bytes,
            "xref\n0 {}\n0000000000 65535 f ",
            objects.len() + 1
        )
        .unwrap();
        for offset in offsets {
            writeln!(&mut bytes, "{offset:010} 00000 n ").unwrap();
        }
        write!(
            &mut bytes,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .unwrap();
        bytes
    }

    #[test]
    fn viewport_preserves_pixels_with_rotation_clipping_and_transparency() {
        let content = "q 30 30 250 170 re W n 0.2 0.4 0.7 rg 40 50 180 100 re f /GS1 gs 160 0 0 120 70 50 cm /Im1 Do Q BT /F1 20 Tf 65 95 Td (Viewport) Tj ET";
        for rotation in [0, 90, 180, 270] {
            let pdf = Pdf::new(fixture(rotation, content, true)).unwrap();
            let page = &pdf.pages()[0];
            let cache = RenderCache::with_outline_budget(4096);
            let settings = PageRasterSettings {
                x_scale: 1.5,
                y_scale: 1.5,
                bg_color: WHITE,
                ..PageRasterSettings::default()
            };
            let full = render_page(page, &cache, &interpreter_settings(), &settings);
            let local = render_page_region(
                page,
                &cache,
                &interpreter_settings(),
                &PageRasterSettings {
                    width: Some(180),
                    height: Some(160),
                    ..settings
                },
                (70, 65),
            );
            // Ignore the viewport boundary; production saves crops inside a
            // four-pixel guard. Geometry must match; alpha compositing may
            // round a small number of channels one level differently.
            let mut max_delta = 0;
            let mut total_delta = 0u64;
            for y in 4..156usize {
                for x in 4..176usize {
                    let a = (y * 180 + x) * 4;
                    let b = ((y + 65) * usize::from(full.width()) + x + 70) * 4;
                    for (a, b) in local.data_as_u8_slice()[a..a + 4]
                        .iter()
                        .zip(&full.data_as_u8_slice()[b..b + 4])
                    {
                        let delta = a.abs_diff(*b);
                        max_delta = max_delta.max(delta);
                        total_delta += u64::from(delta);
                    }
                }
            }
            assert!(max_delta <= 2, "rotation {rotation}: max delta {max_delta}");
            assert!(
                total_delta < 1000,
                "rotation {rotation}: total delta {total_delta}"
            );
        }
    }

    #[test]
    fn render_cache_is_bounded_across_pages_without_changing_pixels() {
        let pdf = Pdf::new(fixture(
            0,
            "BT /F1 20 Tf 40 100 Td (ABCDEFGHIJKLMNOPQRSTUVWXYZ) Tj ET",
            false,
        ))
        .unwrap();
        let page = &pdf.pages()[0];
        let settings = PageRasterSettings {
            bg_color: WHITE,
            ..PageRasterSettings::default()
        };
        let expected = render_page(
            page,
            &RenderCache::new(),
            &interpreter_settings(),
            &settings,
        );
        for budget in [0, 256, 4096] {
            let mut cache = RenderCache::with_outline_budget(budget);
            for _ in 0..3 {
                cache.begin_page();
                let image = render_page(page, &cache, &interpreter_settings(), &settings);
                assert_eq!(image.data_as_u8_slice(), expected.data_as_u8_slice());
                assert!(cache.retained_outline_bytes() <= budget);
                if budget == 0 {
                    assert_eq!(cache.retained_outline_bytes(), 0);
                }
            }
            if budget == 4096 {
                assert!(cache.retained_outline_bytes() > 0);
            }
        }
    }

    #[test]
    fn direct_export_preserves_native_image_and_rejects_overlays_and_masks() {
        let content = "q 160 0 0 120 70 50 cm /Im1 Do Q";
        let publication =
            super::super::super::open(fixture(0, content, false), "test.pdf").unwrap();
        let page = extract::page(&publication.pdf, 0, &HashMap::new());
        assert!(page.encoded_images[0].is_some());
        let r = page.images[0];
        let crop = layout::Crop {
            path: "image.png".into(),
            bounds: Rect::new(r[0], r[1], r[2], r[3]).inflate(2.0, 2.0),
            image_sources: vec![0],
        };
        let root = std::env::temp_dir().join(format!(
            "torto-direct-image-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(
            export_simple_images(
                &publication,
                0,
                &page,
                std::slice::from_ref(&crop),
                &root,
                &AtomicBool::new(false),
                1.0,
                (1.0, 1.0)
            )
            .unwrap()
            .contains(&crop.path)
        );
        let saved = image::open(root.join("image.png")).unwrap().to_rgba8();
        let full = publication.render_page_pixmap(0, 300.0).unwrap();
        let full = image::RgbaImage::from_raw(
            u32::from(full.width()),
            u32::from(full.height()),
            full.data_as_u8_slice().to_vec(),
        )
        .unwrap();
        let expected = image::imageops::crop_imm(
            &full,
            crop.bounds.x0 as u32,
            crop.bounds.y0 as u32,
            crop.bounds.width() as u32,
            crop.bounds.height() as u32,
        )
        .to_image();
        assert_eq!(saved, expected);
        fs::remove_dir_all(&root).unwrap();
        for (content, masked, rotation) in [
            (
                format!("{content} BT /F1 12 Tf 90 100 Td (Overlay) Tj ET"),
                false,
                0,
            ),
            (format!("{content} 0 0 0 rg 90 90 20 20 re f"), false, 0),
            (format!("q /GS1 gs {content} Q"), false, 0),
            (
                format!("q 60 40 m 250 40 l 100 190 l h W n {content} Q"),
                false,
                0,
            ),
            (content.into(), true, 0),
            (content.into(), false, 90),
        ] {
            let pdf = Pdf::new(fixture(rotation, &content, masked)).unwrap();
            let native = extract::page(&pdf, 0, &HashMap::new());
            let r = native.images[0];
            let crop = layout::Crop {
                path: "image.png".into(),
                bounds: Rect::new(r[0], r[1], r[2], r[3]).inflate(2.0, 2.0),
                image_sources: vec![0],
            };
            assert!(
                direct_candidate(&native, &crop).is_none(),
                "{content}, masked {masked}, rotation {rotation}"
            );
        }
    }

    #[test]
    #[ignore = "requires TORTO_REFLOW_TEST_PDF pointing to local Pick, Click, Flick! PDF"]
    fn local_picture_exports_match_full_page_sampling() {
        let path = PathBuf::from(std::env::var("TORTO_REFLOW_TEST_PDF").unwrap());
        let publication = super::super::super::open(fs::read(path).unwrap(), "test.pdf").unwrap();
        let root = std::env::temp_dir().join(format!(
            "torto-raster-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut cache = RenderCache::with_outline_budget(8 * 1024 * 1024);
        let mut timings = ConversionTimings::default();
        let mut checked = 0;
        for physical in [
            98, 100, 101, 103, 106, 108, 135, 136, 137, 138, 143, 144, 202, 203,
        ] {
            let index = physical - 1;
            let page = extract::page(&publication.pdf, index, &HashMap::new());
            let crops = page
                .images
                .iter()
                .enumerate()
                .map(|(i, r)| layout::Crop {
                    path: format!("page-{physical}-image-{i}.png"),
                    bounds: Rect::new(r[0], r[1], r[2], r[3])
                        .inflate(2.0, 2.0)
                        .intersect(Rect::new(0.0, 0.0, page.width, page.height)),
                    image_sources: vec![i],
                })
                .collect::<Vec<_>>();
            save_regions(
                &publication,
                index,
                &page,
                &crops,
                &root,
                &AtomicBool::new(false),
                &mut cache,
                &mut timings,
            )
            .unwrap();
            let full = publication
                .render_page_pixmap(index, PAGE_MAX_DIMENSION)
                .unwrap();
            let sx = f64::from(full.width()) / page.width;
            let sy = f64::from(full.height()) / page.height;
            for crop in crops {
                let image = image::open(root.join(&crop.path)).unwrap().to_rgba8();
                let x0 = (crop.bounds.x0 * sx).floor() as usize;
                let y0 = (crop.bounds.y0 * sy).floor() as usize;
                let x1 = (crop.bounds.x1 * sx).ceil() as usize;
                let y1 = (crop.bounds.y1 * sy).ceil() as usize;
                assert_eq!(
                    (image.width(), image.height()),
                    ((x1 - x0) as u32, (y1 - y0) as u32)
                );
                let mut max_delta = 0;
                let mut total_delta = 0u64;
                for y in 0..image.height() as usize {
                    let actual = &image.as_raw()[y * (x1 - x0) * 4..(y + 1) * (x1 - x0) * 4];
                    let start = ((y + y0) * usize::from(full.width()) + x0) * 4;
                    let expected = &full.data_as_u8_slice()[start..start + actual.len()];
                    for (a, b) in actual.iter().zip(expected) {
                        let delta = a.abs_diff(*b);
                        max_delta = max_delta.max(delta);
                        total_delta += u64::from(delta);
                    }
                }
                let mean = total_delta as f64 / image.as_raw().len() as f64;
                println!("{}: max delta {max_delta}, mean delta {mean:.6}", crop.path);
                assert!(max_delta <= 4 && mean < 0.01, "{}", crop.path);
                fs::remove_file(root.join(crop.path)).unwrap();
                checked += 1;
            }
        }
        assert!(checked >= 14);
        // Some book pages include overlaid panel labels, so direct export
        // eligibility depends on the selected real fixtures.
        assert!(timings.region_pages > 0);
        println!(
            "checked {checked} images, {} direct exports",
            timings.direct_images
        );
        fs::remove_dir_all(root).unwrap();
    }
}
