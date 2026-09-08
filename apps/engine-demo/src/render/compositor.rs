use std::sync::Arc;

use kurbo::Affine;
use peniko::Color;
use rebook_reader::ReaderSpread;
use vello::Scene;

use super::scene::{OverlaySet, SpreadSceneKey, StaticSpreadLayers};
use super::vello::VelloScene;

const TEXT_SELECTION_COLOR: Color = Color::from_rgba8(68, 137, 103, 72);
const ANNOTATION_MARK_COLOR: Color = Color::from_rgba8(96, 165, 250, 72);

pub struct ReaderCompositor;

impl ReaderCompositor {
    pub fn build_static_layers(spread: &ReaderSpread, key: SpreadSceneKey) -> StaticSpreadLayers {
        let mut underlay = Scene::new();
        let mut content = Scene::new();
        let mut images = Vec::new();

        // Underlay: background + images
        {
            images.extend(spread.primary.image_data().cloned());
            let mut underlay_bridge = VelloScene::new(&mut underlay);
            spread.primary.paint_background(&mut underlay_bridge);
            spread
                .primary
                .paint_images_at(&mut underlay_bridge, spread.primary_offset_x);
            if let Some(secondary) = &spread.secondary {
                images.extend(secondary.image_data().cloned());
                secondary.paint_images_at(&mut underlay_bridge, spread.secondary_offset_x);
            }
        }

        // Content: non-image content (text, rules, etc.)
        {
            let mut content_bridge = VelloScene::new(&mut content);
            spread
                .primary
                .paint_non_image_content_at(&mut content_bridge, spread.primary_offset_x);
            if let Some(secondary) = &spread.secondary {
                secondary
                    .paint_non_image_content_at(&mut content_bridge, spread.secondary_offset_x);
            }
        }

        StaticSpreadLayers {
            underlay: Arc::new(underlay),
            content: Arc::new(content),
            images: images.into(),
            key,
        }
    }

    pub fn compose_spread_scene(
        layers: &StaticSpreadLayers,
        spread: &ReaderSpread,
        overlays: &OverlaySet,
        transform: Option<Affine>,
    ) -> Scene {
        let mut scene = Scene::new();

        // 1. Static Underlay
        scene.append(&layers.underlay, transform);

        // 2. Dynamic Overlays
        if !overlays.highlights.is_empty()
            || !overlays.selection.is_empty()
            || !overlays.focus.is_empty()
        {
            let mut overlay_scene = Scene::new();
            {
                let mut bridge = VelloScene::new(&mut overlay_scene);
                Self::paint_overlays(
                    &spread.primary,
                    &mut bridge,
                    overlays,
                    spread.primary_offset_x,
                );
                if let Some(secondary) = &spread.secondary {
                    Self::paint_overlays(
                        secondary,
                        &mut bridge,
                        overlays,
                        spread.secondary_offset_x,
                    );
                }
            }
            scene.append(&overlay_scene, transform);
        }

        // 3. Static Foreground Content
        scene.append(&layers.content, transform);

        scene
    }

    fn paint_overlays(
        page: &rebook_renderer::PageDisplayList,
        scene: &mut VelloScene<'_>,
        overlays: &OverlaySet,
        offset_x: f32,
    ) {
        if !overlays.highlights.is_empty() {
            page.paint_source_ranges(scene, &overlays.highlights, ANNOTATION_MARK_COLOR, offset_x);
        }
        if !overlays.selection.is_empty() {
            page.paint_source_ranges(scene, &overlays.selection, TEXT_SELECTION_COLOR, offset_x);
        }
        if !overlays.focus.is_empty() {
            page.paint_source_ranges(scene, &overlays.focus, TEXT_SELECTION_COLOR, offset_x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::scene::PageSceneKey;
    use rebook_layout::PageLayout;
    use rebook_renderer::DisplayListCompiler;

    #[test]
    fn transforms_compose_without_rebuilding_static_layers() {
        let compiler = DisplayListCompiler;
        let page_layout = PageLayout {
            viewport: rebook_layout::LayoutViewport {
                width: 800,
                height: 1000,
            },
            background: rebook_publication::Rgba {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 255,
            },
            leading_gap: 0.0,
            items: Vec::new(),
        };
        let page_display_list = Arc::new(compiler.compile(&page_layout));
        let spread = ReaderSpread {
            primary: Arc::clone(&page_display_list),
            secondary: None,
            primary_offset_x: 0.0,
            secondary_offset_x: 0.0,
        };

        let key = SpreadSceneKey {
            primary: PageSceneKey {
                position: rebook_reader::ReaderPosition {
                    section_index: 0,
                    segment_index: 0,
                    page_index: 0,
                },
                layout_generation: 0,
            },
            secondary: None,
            width: 800,
            height: 1000,
        };
        let layers = ReaderCompositor::build_static_layers(&spread, key);

        let t1 = Some(Affine::translate((10.0, 0.0)));
        let t2 = Some(Affine::translate((20.0, 0.0)));

        let _scene1 =
            ReaderCompositor::compose_spread_scene(&layers, &spread, &OverlaySet::default(), t1);
        let _scene2 =
            ReaderCompositor::compose_spread_scene(&layers, &spread, &OverlaySet::default(), t2);

        assert_eq!(Arc::strong_count(&layers.underlay), 1);
        assert_eq!(Arc::strong_count(&layers.content), 1);
    }
}
