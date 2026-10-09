//! Small, disposable CPU renders for authored HTML cover pages.

use std::io::Cursor;

use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use rebook_layout::{LayoutEngine, LayoutViewport, ReaderStyle, SpreadMode};
use rebook_publication::{Block, BookSource, ImageLength, Rgba};
use rebook_renderer::DisplayListCompiler;

pub(super) fn page_thumbnail(source: &dyn BookSource) -> Option<Vec<u8>> {
    let mut section = source.cover_section().ok()??;
    // A single-image cover page should retain its original quality. Never pick
    // one image from a composed title/portrait/logo page.
    if let [Block::Image(image)] = section.blocks.as_slice()
        && let Ok(resource) = source.resource(&image.href)
        && resource.media_type.starts_with("image/")
    {
        return Some(resource.bytes.to_vec());
    }
    if section.blocks.is_empty() || section.blocks.len() > 128 {
        return None;
    }
    // Use a taller logical page, then fit its entire contents into the thumbnail.
    // Percentage image heights in a reflowable cover must not consume the whole
    // thumbnail or push its title/portrait onto a discarded second page.
    fn constrain_images(blocks: &mut [Block]) {
        for block in blocks {
            let constrain = |image: &mut rebook_publication::ImageBlock| {
                image.style.max_height = Some(ImageLength::Pixels(180.0));
            };
            match block {
                Block::Image(image) => constrain(image),
                Block::Figure(figure) => figure.images.iter_mut().for_each(constrain),
                Block::Note(note) => constrain_images(&mut note.blocks),
                _ => {}
            }
        }
    }
    constrain_images(&mut section.blocks);
    // Raster dimensions, parsed content, and worker concurrency are bounded.
    let viewport = LayoutViewport {
        width: 420,
        height: 1200,
        raster_scale: 1.0,
    };
    let mut style = ReaderStyle {
        spread: SpreadMode::Single,
        writing_system: source.book().metadata.writing_system(),
        horizontal_margin: 24.0,
        top_margin: 20.0,
        bottom_margin: 20.0,
        background: Rgba {
            red: 255,
            green: 255,
            blue: 255,
            alpha: 255,
        },
        foreground: Rgba::BLACK,
        ..Default::default()
    };
    style.typography.font_size = 16.0;
    style.typography.minimum_font_size = 8.0;
    let mut engine = LayoutEngine::new();
    let layout = engine
        .layout_section(source, &section, viewport, &style)
        .ok()?;
    let page = layout.pages.first()?;
    let display = DisplayListCompiler.compile(page);
    if layout.pages.len() != 1 {
        return None;
    }
    let content_height = display.content_bottom().unwrap_or(600.0) + 20.0;
    let scale = (600.0 / content_height).min(1.0);
    let offset_x = (420.0 / scale - 420.0) * 0.5;
    let mut renderer = VelloCpuImageRenderer::new(420, 600);
    let mut pixels = Vec::new();
    renderer.render_to_vec(
        |scene| display.paint_scaled_at(scene, scale, offset_x, 0.0),
        &mut pixels,
    );
    for pixel in pixels.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            pixel.copy_from_slice(&[255; 4]);
        }
    }
    let image = image::RgbaImage::from_raw(420, 600, pixels)?;
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

#[cfg(test)]
mod tests {
    #[test]
    fn bilinear_image_sampling_keeps_taps_aligned_at_boundaries() {
        // Regress the u8 image-sampling rounding failure fixed by Vello #1950.
        // The repeated two-color image must sample red at this texel boundary.
        use hayro::vello_cpu::color::palette::css::{BLUE, RED};
        use hayro::vello_cpu::peniko::{Extend, ImageQuality, ImageSampler};
        use hayro::vello_cpu::{Image, ImageSource, Pixmap, RenderContext, Resources};
        use kurbo::{Affine, Rect};
        use std::sync::Arc;

        let mut image = Pixmap::new(2, 1);
        image.set_pixel(0, 0, RED.premultiply().to_rgba8());
        image.set_pixel(1, 0, BLUE.premultiply().to_rgba8());
        let mut context = RenderContext::new(100, 100);
        context.set_paint(Image {
            image: ImageSource::Pixmap(Arc::new(image)),
            sampler: ImageSampler {
                x_extend: Extend::Repeat,
                y_extend: Extend::Pad,
                quality: ImageQuality::Medium,
                alpha: 1.0,
            },
        });
        let image_from_scene = Affine::translate((f64::from(0.5_f32.next_down()), 0.25))
            * Affine::scale_non_uniform(1.0e-12, 1.0)
            * Affine::translate((-10.5, -10.5));
        context.set_paint_transform(image_from_scene.inverse());
        context.fill_rect(&Rect::new(10.0, 10.0, 25.0, 90.0));
        context.flush();
        let mut target = Pixmap::new(100, 100);
        context.render(&mut target, &mut Resources::new());
        for y in 11..89 {
            for x in 11..24 {
                assert_eq!(target.sample(x, y), RED.premultiply().to_rgba8());
            }
        }
    }

    #[test]
    #[ignore = "requires TORTO_PERF_BOOK local EPUB"]
    fn profile_local_references() {
        let book = crate::open_file(std::path::PathBuf::from(
            std::env::var_os("TORTO_PERF_BOOK").unwrap(),
        ))
        .unwrap();
        let started = std::time::Instant::now();
        let section = book.source().parse_section(19).unwrap();
        let elapsed = started.elapsed();
        if let Some(path) = std::env::var_os("TORTO_PERF_SECTION_OUTPUT") {
            std::fs::write(path, format!("{section:#?}")).unwrap();
        }
        println!(
            "References blocks={} elapsed_ms={:.2}",
            section.blocks.len(),
            elapsed.as_secs_f64() * 1000.0
        );
    }

    #[test]
    #[ignore = "requires TORTO_DIAG_EPUB and TORTO_DIAG_OUTPUT local paths"]
    fn diagnose_local_rtl_websites() {
        use anyrender::ImageRenderer;
        use rebook_publication::{Block, TextDirection};
        let publication = crate::open_file(std::env::var("TORTO_DIAG_EPUB").unwrap()).unwrap();
        let source = publication.source();
        let index = publication
            .book()
            .sections
            .iter()
            .position(|section| section.href.path().ends_with("copyright.xhtml"))
            .unwrap();
        let section = source.parse_section(index).unwrap();
        for block in &section.blocks {
            if let Block::Text(text) = block {
                assert_eq!(text.style.direction, TextDirection::Rtl);
            }
        }
        let viewport = rebook_layout::LayoutViewport {
            width: 1000,
            height: 1000,
            raster_scale: 1.0,
        };
        let style = rebook_layout::ReaderStyle {
            website_icons: true,
            ..Default::default()
        };
        let layout = rebook_layout::LayoutEngine::new()
            .layout_section(source.as_ref(), &section, viewport, &style)
            .unwrap();
        let mut websites = 0;
        for page in &layout.pages {
            for item in &page.items {
                if let rebook_layout::PageItem::Text(text) = item {
                    for citation in text
                        .citations
                        .iter()
                        .filter(|citation| citation.website.is_some())
                    {
                        let line = text
                            .layout
                            .lines()
                            .find(|line| line.text_range().contains(&citation.range.start))
                            .unwrap();
                        assert!(
                            (line.metrics().offset + line.metrics().advance
                                - line.metrics().inline_max_coord)
                                .abs()
                                < 1.0,
                            "website should be right aligned: {:?}",
                            line.metrics()
                        );
                        websites += 1;
                    }
                }
            }
        }
        assert_eq!(websites, 2);
        let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
        let mut renderer =
            anyrender_vello_cpu::VelloCpuImageRenderer::new(viewport.width, viewport.height);
        let mut pixels = Vec::new();
        renderer.render_to_vec(|scene| display.paint(scene), &mut pixels);
        let image = image::RgbaImage::from_raw(viewport.width, viewport.height, pixels).unwrap();
        image
            .save(
                std::path::PathBuf::from(std::env::var("TORTO_DIAG_OUTPUT").unwrap())
                    .join("rtl-websites.png"),
            )
            .unwrap();
    }

    #[test]
    #[ignore = "requires TORTO_DIAG_EPUB and TORTO_DIAG_OUTPUT local paths"]
    fn diagnose_local_ruby_and_cover() {
        use anyrender::ImageRenderer;
        use rebook_publication::{Block, Inline};
        let path = std::env::var("TORTO_DIAG_EPUB").unwrap();
        let output = std::path::PathBuf::from(std::env::var("TORTO_DIAG_OUTPUT").unwrap());
        let publication = crate::open_file(path).unwrap();
        let bytes = publication.cover_bytes().expect("cover must be generated");
        std::fs::write(output.join("kusamakura-cover.png"), bytes).unwrap();
        let source = publication.source();
        let mut count = 0;
        let mut chapter = None;
        fn count_ruby(blocks: &[Block]) -> usize {
            let count_text = |text: &rebook_publication::TextBlock| {
                text.content
                    .iter()
                    .filter(|inline| matches!(inline, Inline::Ruby(_)))
                    .count()
            };
            blocks
                .iter()
                .map(|block| match block {
                    Block::Text(text) => count_text(text),
                    Block::Quote(quote) => quote
                        .body
                        .iter()
                        .chain(quote.attribution.iter())
                        .map(count_text)
                        .sum(),
                    Block::Note(note) => count_ruby(&note.blocks),
                    Block::Table(table) => table.text_blocks().map(count_text).sum(),
                    Block::Figure(figure) => figure.captions.iter().map(count_text).sum(),
                    Block::Separator(separator) => separator.text.as_ref().map_or(0, count_text),
                    _ => 0,
                })
                .sum()
        }
        for index in 0..publication.book().sections.len() {
            let section = source.parse_section(index).unwrap();
            let rubies = count_ruby(&section.blocks);
            count += rubies;
            if rubies > 0 && chapter.is_none() {
                chapter = Some(section);
            }
        }
        let mut engine = rebook_layout::LayoutEngine::new();
        let viewport = rebook_layout::LayoutViewport {
            width: 780,
            height: 1000,
            raster_scale: 1.0,
        };
        let mut style = rebook_layout::ReaderStyle::default();
        style.writing_system = source.book().metadata.writing_system();
        let start = std::time::Instant::now();
        let layout = engine
            .layout_section(source.as_ref(), &chapter.unwrap(), viewport, &style)
            .unwrap();
        println!(
            "ruby count={count}, first chapter pages={}, layout_ms={}",
            layout.pages.len(),
            start.elapsed().as_millis()
        );
        assert_eq!(count, 4603);
        let display = rebook_renderer::DisplayListCompiler.compile(&layout.pages[0]);
        let mut renderer =
            anyrender_vello_cpu::VelloCpuImageRenderer::new(viewport.width, viewport.height);
        let mut pixels = Vec::new();
        renderer.render_to_vec(|scene| display.paint(scene), &mut pixels);
        let image = image::RgbaImage::from_raw(viewport.width, viewport.height, pixels).unwrap();
        image.save(output.join("kusamakura-ruby.png")).unwrap();
    }
}
