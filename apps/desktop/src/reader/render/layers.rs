//! Ordered, document-coordinate layers. Scroll offsets never enter the content
//! identity: the GPU can move retained pixels without replaying their glyphs.
use std::sync::Arc;

#[cfg(test)]
use kurbo::Affine;
use kurbo::Rect;
use peniko::ImageData;
use rebook_renderer::PageDisplayList;
use vello::Scene;

use super::super::DesktopReader;
use super::vello::VelloScene;

pub(crate) enum LayerContent {
    Scene(Arc<Scene>),
    Images {
        page: Arc<PageDisplayList>,
        offset_x: f32,
    },
}

pub(crate) struct ReaderLayer {
    /// None denotes interaction geometry; retained layers use an allocation ID,
    /// never a pointer address which could be reused after an eviction.
    pub(crate) identity: Option<(u64, u8)>,
    pub(crate) content: LayerContent,
    pub(crate) clip: Rect,
    pub(crate) offset: (f64, f64),
}

pub(crate) struct LayeredReaderScene {
    pub(crate) layers: Vec<ReaderLayer>,
}

impl ReaderLayer {
    pub(crate) fn paint(&self, clip: Rect) -> (Arc<Scene>, Vec<ImageData>) {
        match &self.content {
            LayerContent::Scene(scene) => (Arc::clone(scene), Vec::new()),
            LayerContent::Images { page, offset_x } => {
                let mut scene = Scene::new();
                let images = page.paint_image_layer_in(
                    &mut VelloScene::new(&mut scene),
                    *offset_x,
                    Some(clip),
                );
                (Arc::new(scene), images)
            }
        }
    }
}

impl LayeredReaderScene {
    fn dynamic(&mut self, scene: Scene, clip: Rect, offset: (f64, f64)) {
        if !scene.encoding().draw_tags.is_empty() {
            self.layers.push(ReaderLayer {
                identity: None,
                content: LayerContent::Scene(Arc::new(scene)),
                clip,
                offset,
            });
        }
    }

    fn retained(
        &mut self,
        identity: (u64, u8),
        content: LayerContent,
        clip: Rect,
        offset: (f64, f64),
    ) {
        self.layers.push(ReaderLayer {
            identity: Some(identity),
            content,
            clip,
            offset,
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::preferences::ReadingMode;
    use crate::reader::{ReaderFramePlan, ScrollViewportState};
    use rebook_layout::LayoutViewport;

    #[test]
    #[ignore = "requires a native GPU; checks pixels exposed before resize reflow finishes"]
    fn vertical_resize_preview_paints_exposed_text_without_moving_existing_pixels() {
        let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
        let installed = reader.reader.viewport();
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        reader.select_focus_unit(0);
        let old_size = egui::vec2(800.0, 240.0);
        reader.scroll_viewport = Some(ScrollViewportState {
            size: old_size,
            offset_y: reader.scroll_content_padding(old_size.y),
        });
        let mut plan = ReaderFramePlan {
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, old_size),
            scene_id: reader.scene_id,
            scene_revision: reader.scene_revision,
            background: peniko::Color::from_rgb8(248, 247, 243),
            content_left: 0.0,
            preview_left: 0.0,
            reflow_pending: false,
            live_resize_preview: true,
        };
        let mut old_audit = crate::platform::LayerAudit::new([800, 240]);
        old_audit.layered(&reader.layered_scene().unwrap(), plan, 1.0);
        let before = old_audit.pixels();

        let new_size = egui::vec2(800.0, 620.0);
        reader.retain_viewport_reflow_anchor(new_size);
        reader.request_viewport_reflow(LayoutViewport::new(800, 620).unwrap());
        reader.scroll_viewport = Some(ScrollViewportState {
            size: new_size,
            offset_y: reader
                .viewport_preview_offset(new_size.y, layout.content_height)
                .unwrap(),
        });
        reader.bump_scene_revision();
        plan.rect = egui::Rect::from_min_size(egui::Pos2::ZERO, new_size);
        plan.scene_revision = reader.scene_revision;
        plan.reflow_pending = true;
        let mut new_audit = crate::platform::LayerAudit::new([800, 620]);
        new_audit.layered(&reader.layered_scene().unwrap(), plan, 1.0);
        let after = new_audit.pixels();
        assert_eq!(
            reader.reader.viewport(),
            installed,
            "no new layout was installed"
        );
        assert!(
            before.iter().zip(&after).all(|(a, b)| a.abs_diff(*b) <= 1),
            "the original visible pixels must remain stationary"
        );
        let ink = after[before.len()..]
            .chunks_exact(4)
            .filter(|p| p[0] < 100 && p[1] < 100 && p[2] < 100)
            .count();
        assert!(
            ink > 100,
            "the newly exposed area must already contain text: {ink}"
        );
        println!("RESIZE_PREVIEW exposed_ink_pixels={ink} installed={installed:?}");
    }

    #[test]
    #[ignore = "requires a native GPU; checks clipping while width and height change together"]
    fn diagonal_resize_preview_translates_before_clipping_the_new_canvas() {
        let (mut reader, _, _) = crate::reader::semantic_layout::tests::fixture();
        reader
            .reader
            .resize(LayoutViewport::new(1600, 600).unwrap())
            .unwrap();
        let installed = reader.reader.viewport();
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        reader.select_focus_unit(0);
        reader.scroll_viewport = Some(ScrollViewportState {
            size: egui::vec2(1600.0, 240.0),
            offset_y: reader.scroll_content_padding(240.0),
        });
        reader.retain_viewport_reflow_anchor(egui::vec2(1200.0, 620.0));
        let offset_y = reader
            .viewport_preview_offset(620.0, layout.content_height)
            .unwrap();
        reader.scroll_viewport = Some(ScrollViewportState {
            size: egui::vec2(1600.0, 620.0),
            offset_y,
        });
        let mut plan = ReaderFramePlan {
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 620.0)),
            scene_id: reader.scene_id,
            scene_revision: reader.scene_revision,
            background: peniko::Color::from_rgb8(248, 247, 243),
            content_left: reader.preview_content_left(1600.0),
            preview_left: reader.preview_content_left(1600.0),
            reflow_pending: true,
            live_resize_preview: true,
        };
        let mut source_audit = crate::platform::LayerAudit::new([1600, 620]);
        source_audit.layered(&reader.layered_scene().unwrap(), plan, 1.0);
        let source_pixels = source_audit.pixels();
        for width in [900_u32, 2000] {
            reader.request_viewport_reflow(LayoutViewport::new(width, 620).unwrap());
            reader.scroll_viewport = Some(ScrollViewportState {
                size: egui::vec2(width as f32, 620.0),
                offset_y,
            });
            let shift = reader.preview_offset_x(width as f32);
            assert!(
                shift.abs() > 10.0 && shift.fract() == 0.0,
                "fixture must exercise horizontal translation: {shift}"
            );
            plan.rect =
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, 620.0));
            plan.content_left = reader.preview_content_left(width as f32);
            plan.preview_left = plan.content_left;
            let mut audit = crate::platform::LayerAudit::new([width, 620]);
            audit.layered(&reader.layered_scene().unwrap(), plan, 1.0);
            let pixels = audit.pixels();
            let mut right_ink = 0;
            let mut compared = 0;
            let mut changed = 0;
            let mut missing_ink = 0;
            for y in 0..620_usize {
                for x in 0..width as usize {
                    let source_x = x as i32 - shift as i32;
                    if !(0..1600).contains(&source_x) {
                        continue;
                    }
                    let expected = &source_pixels[(y * 1600 + source_x as usize) * 4..][..4];
                    let actual = &pixels[(y * width as usize + x) * 4..][..4];
                    compared += 1;
                    // Different atlas packing can change a few antialiased
                    // edge samples. Use the existing layer-audit parity budget
                    // and separately reject lost dark text pixels.
                    if actual.iter().zip(expected).any(|(a, b)| a.abs_diff(*b) > 8) {
                        changed += 1;
                    }
                    if expected[..3].iter().all(|v| *v < 100)
                        && actual[..3].iter().all(|v| *v > 200)
                    {
                        missing_ink += 1;
                    }
                    if shift < 0.0
                        && x >= (width as f32 + shift) as usize
                        && actual[..3].iter().all(|value| *value < 100)
                    {
                        right_ink += 1;
                    }
                }
            }
            if shift < 0.0 {
                assert!(
                    right_ink > 100,
                    "the formerly blank right strip must contain text"
                );
            }
            let fraction = changed as f64 / compared as f64;
            println!(
                "DIAGONAL_PARITY width={width} changed_fraction={fraction:.6} missing_ink={missing_ink}"
            );
            assert!(fraction < 0.005, "preview geometry changed: {fraction:.6}");
            assert!(
                missing_ink <= compared / 10000 + 8,
                "preview lost text instead of merely changing antialiasing: {missing_ink}"
            );
            assert_eq!(reader.reader.viewport(), installed);
            println!(
                "DIAGONAL_PREVIEW width={width} height=620 shift={shift} right_ink={right_ink}"
            );
        }
    }

    #[test]
    #[ignore = "requires TORTO_HIGHLIGHT_BOOK local EPUB and native GPU; checks cross-page highlight pixels"]
    fn local_cross_page_highlight_has_no_seam() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let store = crate::sync::SyncStore::open_at(
            std::env::temp_dir().join(format!("highlight-audit-{}.sqlite3", uuid::Uuid::new_v4())),
            "highlight-audit",
        )
        .unwrap();
        let mut reader = crate::reader::open_reader_in_viewport(
            &std::path::PathBuf::from(std::env::var_os("TORTO_HIGHLIGHT_BOOK").unwrap()),
            crate::fonts::embedded_reader_fonts(),
            None,
            None,
            store,
            LayoutViewport::new(900, 240).unwrap(),
        )
        .unwrap();
        reader.progress_store = None;
        let mut style = reader.reader.style().clone();
        style.spread = rebook_layout::SpreadMode::Scroll;
        style.typesetting = rebook_layout::ReaderTypesetting::unified();
        reader.reader.set_style(style).unwrap();
        let target = reader
            .reader
            .toc_items()
            .iter()
            .find(|item| item.label.contains("2.3 N-GRAM"))
            .unwrap()
            .target
            .clone()
            .unwrap();
        reader.reader.go_to_href(&target).unwrap();
        reader.reading_mode = ReadingMode::Focus;
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        reader.focus_unit_index = reader
            .focus_units
            .iter()
            .position(|unit| unit.text.contains("The results are shown"))
            .unwrap();
        let ranges = reader.focus_units[reader.focus_unit_index]
            .paint_ranges
            .clone();
        let mut rects = Vec::new();
        let mut pages = Vec::new();
        for (index, entry) in layout.pages.iter().enumerate() {
            let local = entry.page.source_rects(&ranges);
            if local.is_empty() {
                continue;
            }
            pages.push(index);
            rects.extend(local.into_iter().map(|rect| {
                rect + kurbo::Vec2::new(
                    0.,
                    f64::from(layout.page_tops[index] - layout.page_origins[index]),
                )
            }));
        }
        assert!(
            pages.len() > 1,
            "fixture must split the active paragraph across logical pages"
        );
        let edge = layout.page_tops[pages[1]];
        println!("HIGHLIGHT_PAGE_EDGE {edge}, pages={pages:?}");
        let padding = reader.scroll_content_padding(240.);
        reader.scroll_viewport = Some(ScrollViewportState {
            offset_y: edge + padding - 100.325,
            size: egui::vec2(900., 240.),
        });
        let mut layers = reader.layered_scene().unwrap();
        layers.layers.retain(|layer| layer.identity.is_none());
        let plan = ReaderFramePlan {
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900., 240.)),
            scene_id: 1,
            scene_revision: 1,
            background: peniko::Color::from_rgb8(212, 210, 206),
            content_left: 0.0,
            preview_left: 0.0,
            reflow_pending: false,
            live_resize_preview: false,
        };
        let mut audit = crate::platform::LayerAudit::new([1400, 400]);
        // Keep the former page-mask composition as a diagnostic reference.
        let mut page_masked = vello::Scene::new();
        let viewport = reader.scroll_viewport.unwrap();
        for index in &pages {
            let page = &layout.pages[*index].page;
            let origin = layout.page_origins[*index];
            let clip = kurbo::Rect::new(
                0.,
                f64::from(origin),
                f64::from(page.width()),
                f64::from(origin + layout.page_heights[*index]),
            );
            let mut fragment = vello::Scene::new();
            fragment.push_clip_layer(peniko::Fill::NonZero, kurbo::Affine::IDENTITY, &clip);
            reader.paint_page_overlays(page, &mut super::VelloScene::new(&mut fragment), 0.);
            fragment.pop_layer();
            page_masked.append(
                &fragment,
                Some(kurbo::Affine::translate((
                    0.,
                    f64::from(layout.page_tops[*index] + padding - viewport.offset_y - origin),
                ))),
            );
        }
        for scale in [0.925, 1., 1.25, 1.5] {
            let scene = crate::reader::ReaderScene {
                scene: std::sync::Arc::new(layers.flatten()),
                images: std::sync::Arc::from([]),
                refresh_image_atlas: false,
            };
            audit.direct(&scene, plan, scale);
            let pixels = audit.pixels();
            let pair = rects
                .windows(2)
                .find(|pair| {
                    (pair[0].y1 - f64::from(edge)).abs() < 1.
                        && (pair[1].y0 - f64::from(edge)).abs() < 1.
                })
                .unwrap();
            let x = ((pair[0].x0.max(pair[1].x0) + pair[0].x1.min(pair[1].x1))
                * 0.5
                * f64::from(scale)) as usize;
            let sample = |y: usize| &pixels[(y * 1400 + x) * 4..(y * 1400 + x) * 4 + 4];
            let boundary = (100.325 * f64::from(scale)) as usize;
            let expected = sample(boundary - 6);
            let legacy = crate::reader::ReaderScene {
                scene: std::sync::Arc::new(page_masked.clone()),
                images: std::sync::Arc::from([]),
                refresh_image_atlas: false,
            };
            audit.direct(&legacy, plan, scale);
            let legacy_pixels = audit.pixels();
            let legacy_sample =
                &legacy_pixels[(boundary * 1400 + x) * 4..(boundary * 1400 + x) * 4 + 4];
            println!(
                "HIGHLIGHT_PIXELS scale={scale} page_masked={legacy_sample:?} combined={:?} interior={expected:?}",
                sample(boundary)
            );
            for y in boundary - 1..=boundary + 1 {
                assert!(
                    sample(y)
                        .iter()
                        .zip(expected)
                        .all(|(a, b)| a.abs_diff(*b) <= 1),
                    "scale={scale} boundary={boundary} y={y}: {:?} != {expected:?}",
                    sample(y)
                );
            }
            for _ in 0..64 {
                if !audit.layered(&layers, plan, scale).0.pending {
                    break;
                }
            }
            audit.parity(&pixels, &format!("highlight-seam-{scale}"));
        }
    }

    #[test]
    #[ignore = "requires a local publication and native GPU; verifies real-book pixels and scrolling"]
    fn local_book_layers_match_direct_rendering_and_reuse_scrolling_tiles() {
        let path = std::path::PathBuf::from(std::env::var_os("TORTO_LAYER_BOOK").unwrap());
        let store = crate::sync::SyncStore::open_at(
            std::env::temp_dir().join(format!("layer-audit-{}.sqlite3", uuid::Uuid::new_v4())),
            "layer-audit",
        )
        .unwrap();
        let mut reader = crate::reader::open_reader_in_viewport(
            &path,
            crate::fonts::embedded_reader_fonts(),
            None,
            None,
            store,
            LayoutViewport::new(1216, 789).unwrap(),
        )
        .unwrap();
        reader.progress_store = None;
        if reader.reader.style().spread != rebook_layout::SpreadMode::Scroll {
            let mut style = reader.reader.style().clone();
            style.spread = rebook_layout::SpreadMode::Scroll;
            reader.reader.set_style(style).unwrap();
            reader.invalidate_page_scenes();
        }
        if let Ok(label) = std::env::var("TORTO_LAYER_TOC") {
            let target = reader
                .reader
                .toc_items()
                .iter()
                .find(|item| item.label.contains(&label))
                .unwrap()
                .target
                .clone()
                .unwrap();
            reader.reader.go_to_href(&target).unwrap();
        }
        reader.reading_mode = ReadingMode::Focus;
        let layout = reader.current_scroll_layout().unwrap();
        reader.rebuild_focus_units(&layout);
        for entry in &layout.pages {
            for (request, _) in entry.page.deferred_rasters() {
                request.load(reader.source.as_ref()).unwrap();
            }
        }
        let size = egui::vec2(1216.0, 789.0);
        let plan = ReaderFramePlan {
            rect: egui::Rect::from_min_size(egui::Pos2::ZERO, size),
            scene_id: reader.scene_id,
            scene_revision: reader.scene_revision,
            background: peniko::Color::from_rgb8(248, 247, 243),
            content_left: 0.0,
            preview_left: 0.0,
            reflow_pending: false,
            live_resize_preview: false,
        };
        let mut audit = crate::platform::LayerAudit::new([1216, 789]);
        let mut direct_times = Vec::new();
        let mut retained_times = Vec::new();
        let mut offsets = vec![0.0, 64.0, 128.0, 192.0, 128.0, 64.0, 0.0];
        // Also exercise actual pictures/formulas, rather than only the prose at
        // the beginning of a chapter. Decode is outside the measured paint step.
        for (index, entry) in layout.pages.iter().enumerate() {
            for (_, bounds) in entry.page.deferred_rasters() {
                let offset =
                    layout.page_tops[index] - layout.page_origins[index] + bounds.center().y as f32;
                offsets.push(offset.max(0.0));
                if offsets.len() >= 9 {
                    break;
                }
            }
            if offsets.len() >= 9 {
                break;
            }
        }
        for (index, offset) in offsets.into_iter().enumerate() {
            reader.scroll_viewport = Some(ScrollViewportState {
                offset_y: offset,
                size,
            });
            let direct = reader.page_scene();
            let elapsed = audit.direct(&direct, plan, 1.0);
            let baseline = audit.pixels();
            let layers = reader.layered_scene().unwrap();
            let mut stabilized = false;
            for frame in 0..64 {
                let (stats, _) = audit.layered(&layers, plan, 1.0);
                if frame == 0 {
                    audit.parity(&baseline, &format!("book-{index}-first"));
                }
                if !stats.pending {
                    stabilized = true;
                    break;
                }
            }
            assert!(stabilized, "cache warming must converge");
            audit.parity(&baseline, &format!("book-{index}"));
            let (stats, cached) = audit.layered(&layers, plan, 1.0);
            assert_eq!(stats.tiles, 0);
            assert!(!stats.pending);
            assert_eq!(
                stats.image_bytes, 0,
                "retained pictures must not be reuploaded"
            );
            direct_times.push(elapsed);
            retained_times.push(cached);
            println!(
                "LAYER_SCROLL index={index} offset={offset} direct_complete_ms={elapsed:.3} retained_complete_ms={cached:.3} cache_hits={} rasterized_pixels={} uploaded_image_bytes={}",
                stats.hits, stats.rasterized_pixels, stats.image_bytes
            );
        }
        direct_times.sort_by(f32::total_cmp);
        retained_times.sort_by(f32::total_cmp);
        println!(
            "LAYER_BENCH direct_complete_median_ms={:.3} retained_complete_median_ms={:.3}",
            direct_times[direct_times.len() / 2],
            retained_times[retained_times.len() / 2]
        );
        // Paginated modes share this path, including native PDF page images.
        reader.reading_mode = ReadingMode::Classic;
        let mut style = reader.reader.style().clone();
        style.spread = rebook_layout::SpreadMode::Single;
        reader.reader.set_style(style).unwrap();
        reader.invalidate_page_scenes();
        let direct = reader.page_scene();
        audit.direct(&direct, plan, 1.0);
        let baseline = audit.pixels();
        let layers = reader.layered_scene().unwrap();
        for _ in 0..64 {
            if !audit.layered(&layers, plan, 1.0).0.pending {
                break;
            }
        }
        audit.parity(&baseline, "paginated");
    }
}

impl DesktopReader {
    pub(crate) fn layered_scene(&mut self) -> Option<LayeredReaderScene> {
        if self.completion.is_some() || self.pdf_zoom_enabled() {
            return None;
        }
        let mut result = LayeredReaderScene { layers: Vec::new() };
        if self.is_scroll_mode() {
            let viewport = self.scroll_viewport?;
            let layout = self.current_scroll_layout().ok()?;
            let padding = self.scroll_content_padding(viewport.size.y);
            let preview_x = f64::from(self.preview_offset_x(viewport.size.x));
            // Translate the document before clipping to the current viewport.
            // Moving a viewport-sized image afterwards discards the right edge
            // while narrowing and leaves a blank strip until reflow completes.
            let window = Rect::new(
                -preview_x,
                0.0,
                f64::from(viewport.size.x) - preview_x,
                f64::from(viewport.size.y),
            );
            let mut backdrop = Scene::new();
            let bounds = self.paint_scroll_backdrop(&mut backdrop, viewport, padding);
            result.dynamic(
                backdrop,
                measured_clip(window, Some(bounds)),
                (preview_x, 0.0),
            );
            let mut text_layers = LayeredReaderScene { layers: Vec::new() };
            let mut foreground_layers = LayeredReaderScene { layers: Vec::new() };
            for (index, entry) in layout.pages.iter().enumerate() {
                let top = layout.page_tops[index] + padding;
                let bottom = top + layout.page_heights[index];
                if bottom <= viewport.offset_y || top >= viewport.offset_y + viewport.size.y {
                    continue;
                }
                let cached = self.scroll_page_layers(entry);
                let origin = layout.page_origins[index];
                let clip = Rect::new(
                    0.0,
                    f64::from(origin),
                    f64::from(entry.page.width()),
                    f64::from(origin + layout.page_heights[index]),
                );
                let offset = (preview_x, f64::from(top - viewport.offset_y - origin));
                result.retained(
                    (cached.generation, 0),
                    LayerContent::Images {
                        page: Arc::clone(&entry.page),
                        offset_x: 0.0,
                    },
                    clip,
                    offset,
                );
                text_layers.retained(
                    (cached.generation, 1),
                    LayerContent::Scene(Arc::clone(&cached.content)),
                    clip,
                    offset,
                );
                let mut foreground = Scene::new();
                let mut bridge = VelloScene::measuring(&mut foreground);
                self.paint_page_foreground(&entry.page, &mut bridge, 0.0);
                let border_clip = Rect::new(clip.x0, clip.y0 - 4.0, clip.x1, clip.y1 + 4.0);
                let foreground_clip = measured_clip(border_clip, bridge.painted_bounds());
                foreground_layers.dynamic(foreground, foreground_clip, offset);
            }
            let mut overlays = Scene::new();
            let bounds = self.paint_scroll_overlays(&mut overlays, &layout, viewport, padding);
            result.dynamic(
                overlays,
                measured_clip(window, Some(bounds)),
                (preview_x, 0.0),
            );
            result.layers.extend(text_layers.layers);
            result.layers.extend(foreground_layers.layers);
            let mut bridges = Scene::new();
            let bounds = self.paint_scroll_bridges(&mut bridges, &layout, viewport, padding);
            result.dynamic(
                bridges,
                measured_clip(window, Some(bounds)),
                (preview_x, 0.0),
            );
            let mut surroundings = Scene::new();
            self.paint_focus_surroundings(&mut surroundings, viewport, padding);
            result.dynamic(surroundings, window, (preview_x, 0.0));
        } else {
            let cached = self.page_scene_layers();
            let spread = self.reader.current_spread().ok()?;
            let width = (spread.primary_offset_x + spread.primary.width() as f32).max(
                spread
                    .secondary
                    .as_ref()
                    .map_or(0.0, |p| spread.secondary_offset_x + p.width() as f32),
            );
            let clip = Rect::new(
                0.0,
                0.0,
                f64::from(width),
                f64::from(spread.primary.height()),
            );
            result.retained(
                (cached.generation, 2),
                LayerContent::Scene(Arc::clone(&cached.underlay)),
                clip,
                (0.0, 0.0),
            );
            for (page, offset_x, kind) in [
                (Some(spread.primary.clone()), spread.primary_offset_x, 0),
                (spread.secondary.clone(), spread.secondary_offset_x, 3),
            ] {
                if let Some(page) = page {
                    result.retained(
                        (cached.generation, kind),
                        LayerContent::Images {
                            page: Arc::clone(&page),
                            offset_x,
                        },
                        clip,
                        (0.0, 0.0),
                    );
                    let mut overlays = Scene::new();
                    let mut bridge = VelloScene::measuring(&mut overlays);
                    self.paint_page_overlays(&page, &mut bridge, offset_x);
                    let overlay_clip = measured_clip(clip, bridge.painted_bounds());
                    result.dynamic(overlays, overlay_clip, (0.0, 0.0));
                }
            }
            result.retained(
                (cached.generation, 1),
                LayerContent::Scene(Arc::clone(&cached.content)),
                clip,
                (0.0, 0.0),
            );
            let mut foreground = Scene::new();
            let mut bridge = VelloScene::measuring(&mut foreground);
            self.paint_page_foreground(&spread.primary, &mut bridge, spread.primary_offset_x);
            if let Some(page) = spread.secondary {
                self.paint_page_foreground(&page, &mut bridge, spread.secondary_offset_x);
            }
            let foreground_clip = measured_clip(clip, bridge.painted_bounds());
            result.dynamic(foreground, foreground_clip, (0.0, 0.0));
        }
        Some(result)
    }
}

#[cfg(test)]
impl LayeredReaderScene {
    /// Same vector ordering as the direct renderer, useful for parity checks.
    pub(crate) fn flatten(&self) -> Scene {
        let mut result = Scene::new();
        for layer in &self.layers {
            let (scene, _) = layer.paint(layer.clip);
            let mut clipped = Scene::new();
            clipped.push_clip_layer(peniko::Fill::NonZero, Affine::IDENTITY, &layer.clip);
            clipped.append(&scene, None);
            clipped.pop_layer();
            result.append(&clipped, Some(Affine::translate(layer.offset)));
        }
        result
    }
}

fn measured_clip(original: Rect, bounds: Option<Rect>) -> Rect {
    bounds.map_or(original, |bounds| {
        original.intersect(bounds.inflate(2.0, 2.0))
    })
}
