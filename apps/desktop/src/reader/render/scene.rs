use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use kurbo::{Affine, Rect, RoundedRect};
use peniko::{Color, Fill, ImageData};
use rebook_formats::BookFormat;
use rebook_layout::quote_accent_color;
use rebook_reader::{ReaderPosition, ReaderSectionPage};
use rebook_renderer::PageDisplayList;
use vello::Scene;

use super::super::DesktopReader;
use super::vello::VelloScene;

const PAGE_SCENE_CACHE_CAPACITY: usize = 32;
const PDF_PAGE_SCENE_CACHE_CAPACITY: usize = 4;
const ANNOTATION_MARK_COLOR: Color = Color::from_rgba8(96, 165, 250, 72);
const TEXT_SELECTION_COLOR: Color = Color::from_rgba8(68, 137, 103, 72);

fn focus_block_border_color() -> Color {
    let accent = crate::ui::palette().accent;
    Color::from_rgba8(accent.r(), accent.g(), accent.b(), accent.a())
}

fn focus_block_activation_color() -> Color {
    let accent = quote_accent_color(crate::ui::palette().dark);
    Color::from_rgba8(accent.red, accent.green, accent.blue, accent.alpha)
}

fn focus_unit_activation_color(structured: bool) -> Color {
    if structured {
        TEXT_SELECTION_COLOR
    } else {
        focus_block_activation_color()
    }
}

fn focus_footnote_icon_color() -> Color {
    let color = crate::ui::footnote_link_color();
    Color::from_rgba8(color.r(), color.g(), color.b(), color.a())
}

pub(in crate::reader) fn text_selection_fill() -> egui::Color32 {
    let rgba = TEXT_SELECTION_COLOR.to_rgba8();
    egui::Color32::from_rgba_unmultiplied(rgba.r, rgba.g, rgba.b, rgba.a)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PageSceneKey {
    section: usize,
    segment: usize,
    page: usize,
}

pub(crate) struct PageSceneLayers {
    underlay: Arc<Scene>,
    content: Arc<Scene>,
    images: Arc<[ImageData]>,
}

pub(crate) struct ReaderScene {
    pub(crate) scene: Arc<Scene>,
    pub(crate) images: Arc<[ImageData]>,
    pub(crate) refresh_image_atlas: bool,
}

impl ReaderScene {
    fn new(scene: Scene, images: Arc<[ImageData]>) -> Self {
        // Vello's persistent image atlas must be refreshed whenever a rendered
        // scene references images. This is a scene-content invariant, not a
        // reading-mode behavior: classic and focus modes replay the same
        // ImageData through the same renderer.
        let refresh_image_atlas = !images.is_empty();
        Self {
            scene: Arc::new(scene),
            images,
            refresh_image_atlas,
        }
    }
}

fn evict_page_scene<T>(
    scenes: &mut HashMap<PageSceneKey, T>,
    lru: &mut VecDeque<PageSceneKey>,
    position: ReaderPosition,
) -> bool {
    let key = PageSceneKey {
        section: position.section_index,
        segment: position.segment_index,
        page: position.page_index,
    };
    lru.retain(|entry| *entry != key);
    scenes.remove(&key).is_some()
}

impl DesktopReader {
    pub(crate) fn page_scene(&mut self) -> ReaderScene {
        if self.is_scroll_mode() {
            return self.scroll_page_scene();
        }
        let layers = self.page_scene_layers();
        let mut scene = Scene::new();
        scene.append(&layers.underlay, None);
        match self.reader.current_spread() {
            Ok(spread) => {
                let mut bridge = VelloScene::new(&mut scene);
                self.paint_page_overlays(&spread.primary, &mut bridge, spread.primary_offset_x);
                if let Some(secondary) = spread.secondary {
                    self.paint_page_overlays(&secondary, &mut bridge, spread.secondary_offset_x);
                }
            }
            Err(error) => self.error = Some(format!("组合双页失败：{error}")),
        }
        scene.append(&layers.content, None);
        match self.reader.current_spread() {
            Ok(spread) => {
                let mut bridge = VelloScene::new(&mut scene);
                self.paint_focus_table_border(
                    &spread.primary,
                    &mut bridge,
                    spread.primary_offset_x,
                );
                if let Some(secondary) = spread.secondary {
                    self.paint_focus_table_border(
                        &secondary,
                        &mut bridge,
                        spread.secondary_offset_x,
                    );
                }
            }
            Err(error) => self.error = Some(format!("组合双页失败：{error}")),
        }
        ReaderScene::new(scene, Arc::clone(&layers.images))
    }

    fn scroll_page_scene(&mut self) -> ReaderScene {
        let Some(viewport) = self.scroll_viewport else {
            return ReaderScene::new(Scene::new(), Arc::from([]));
        };
        let layout = match self.current_scroll_layout() {
            Ok(layout) => layout,
            Err(error) => {
                self.error = Some(format!("生成滑动章节失败：{error}"));
                return ReaderScene::new(Scene::new(), Arc::from([]));
            }
        };
        let content_padding = self.scroll_content_padding(viewport.size.y);
        let visible_bottom = viewport.offset_y + viewport.size.y;
        let mut scene = Scene::new();
        if self.is_focus_mode()
            && let Some(rect) = self
                .focus_units
                .get(self.focus_unit_index)
                .and_then(|unit| unit.rectangular_activation_rect)
        {
            let background = RoundedRect::from_rect(
                Rect::new(
                    f64::from(rect.left()),
                    f64::from(rect.top() + content_padding - viewport.offset_y),
                    f64::from(rect.right()),
                    f64::from(rect.bottom() + content_padding - viewport.offset_y),
                ),
                7.0,
            );
            let color = self
                .focus_units
                .get(self.focus_unit_index)
                .map_or_else(focus_block_activation_color, |unit| {
                    focus_unit_activation_color(unit.structured_activation)
                });
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &background);
        }
        let mut images = Vec::new();
        for (index, entry) in layout.pages.iter().enumerate() {
            let top = layout.page_tops[index] + content_padding;
            let bottom = top + layout.page_heights[index];
            if bottom <= viewport.offset_y || top >= visible_bottom {
                continue;
            }
            let layers = self.scroll_page_layers(entry);
            images.extend(layers.images.iter().cloned());
            let mut page_scene = Scene::new();
            let clip = Rect::new(
                0.0,
                f64::from(layout.page_origins[index]),
                f64::from(entry.page.width()),
                f64::from(layout.page_origins[index] + layout.page_heights[index]),
            );
            page_scene.push_clip_layer(peniko::Fill::NonZero, Affine::IDENTITY, &clip);
            page_scene.append(&layers.underlay, None);
            self.paint_page_overlays(&entry.page, &mut VelloScene::new(&mut page_scene), 0.0);
            page_scene.append(&layers.content, None);
            self.paint_focus_table_border(&entry.page, &mut VelloScene::new(&mut page_scene), 0.0);
            page_scene.pop_layer();
            scene.append(
                &page_scene,
                Some(Affine::translate((
                    0.0,
                    f64::from(top - viewport.offset_y - layout.page_origins[index]),
                ))),
            );
        }
        for bridge in &layout.quote_bridges {
            let top = bridge.top + content_padding - viewport.offset_y;
            let bottom = bridge.bottom + content_padding - viewport.offset_y;
            if bottom < 0.0 || top > viewport.size.y {
                continue;
            }
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                bridge.style.color,
                None,
                &Rect::new(
                    f64::from(bridge.style.x),
                    f64::from(top - 0.5),
                    f64::from(bridge.style.x + bridge.style.width),
                    f64::from(bottom + 0.5),
                ),
            );
        }
        ReaderScene::new(scene, images.into())
    }

    fn scroll_page_layers(&mut self, entry: &ReaderSectionPage) -> Arc<PageSceneLayers> {
        let key = PageSceneKey {
            section: entry.position.section_index,
            segment: entry.position.segment_index,
            page: entry.position.page_index,
        };
        if let Some(layers) = self.page_scenes.get(&key).cloned() {
            self.touch_page_scene(key);
            return layers;
        }

        let mut underlay = Scene::new();
        let mut underlay_bridge = VelloScene::new(&mut underlay);
        entry.page.paint_images_at(&mut underlay_bridge, 0.0);
        let mut content = Scene::new();
        entry
            .page
            .paint_non_image_content_at(&mut VelloScene::new(&mut content), 0.0);
        let layers = Arc::new(PageSceneLayers {
            underlay: Arc::new(underlay),
            content: Arc::new(content),
            images: entry.page.image_data().cloned().collect(),
        });
        self.page_scenes.insert(key, Arc::clone(&layers));
        self.touch_page_scene(key);
        while self.page_scenes.len() > PAGE_SCENE_CACHE_CAPACITY {
            let Some(oldest) = self.page_scene_lru.pop_front() else {
                break;
            };
            if oldest != key {
                self.page_scenes.remove(&oldest);
            }
        }
        layers
    }

    fn page_scene_layers(&mut self) -> Arc<PageSceneLayers> {
        let key = PageSceneKey {
            section: self.snapshot.location.section_index,
            segment: self.snapshot.location.segment_index,
            page: self.snapshot.location.page_index,
        };
        if let Some(layers) = self.page_scenes.get(&key).cloned() {
            self.touch_page_scene(key);
            return layers;
        }

        let mut underlay = Scene::new();
        let mut content = Scene::new();
        let mut images = Vec::new();
        match self.reader.current_spread() {
            Ok(spread) => {
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

                let mut content_bridge = VelloScene::new(&mut content);
                spread
                    .primary
                    .paint_non_image_content_at(&mut content_bridge, spread.primary_offset_x);
                if let Some(secondary) = spread.secondary {
                    secondary
                        .paint_non_image_content_at(&mut content_bridge, spread.secondary_offset_x);
                }
            }
            Err(error) => {
                self.error = Some(format!("组合双页失败：{error}"));
                images.extend(self.reader.current_page().image_data().cloned());
                self.reader
                    .current_page()
                    .paint(&mut VelloScene::new(&mut underlay));
            }
        }
        let layers = Arc::new(PageSceneLayers {
            underlay: Arc::new(underlay),
            content: Arc::new(content),
            images: images.into(),
        });
        self.page_scenes.insert(key, Arc::clone(&layers));
        self.touch_page_scene(key);
        let cache_capacity = match self.format {
            BookFormat::Pdf => PDF_PAGE_SCENE_CACHE_CAPACITY,
            _ => PAGE_SCENE_CACHE_CAPACITY,
        };
        while self.page_scenes.len() > cache_capacity {
            let Some(oldest) = self.page_scene_lru.pop_front() else {
                break;
            };
            if oldest != key {
                self.page_scenes.remove(&oldest);
            }
        }
        layers
    }

    fn paint_page_overlays(
        &self,
        page: &PageDisplayList,
        scene: &mut VelloScene<'_>,
        offset_x: f32,
    ) {
        let focus_unit = self
            .is_focus_mode()
            .then(|| self.focus_units.get(self.focus_unit_index))
            .flatten();
        if let Some(unit) =
            focus_unit.filter(|unit| unit.rectangular_activation && !self.is_scroll_mode())
        {
            page.paint_source_block_background(
                scene,
                &unit.paint_ranges,
                focus_unit_activation_color(unit.structured_activation),
                offset_x,
            );
        }
        if let Some(unit) = focus_unit {
            page.paint_footnote_icons(
                scene,
                &unit.paint_ranges,
                focus_footnote_icon_color(),
                offset_x,
            );
        } else if !self.is_focus_mode() {
            page.paint_all_footnote_icons(scene, focus_footnote_icon_color(), offset_x);
        }
        for highlight in &self.highlights {
            page.paint_source_ranges(scene, &highlight.ranges, ANNOTATION_MARK_COLOR, offset_x);
        }
        if let Some(mark) = &self.focused_mark {
            page.paint_source_ranges(scene, &mark.ranges, mark.color(), offset_x);
        }
        if let Some(selection) = &self.selection {
            page.paint_source_ranges(scene, &selection.ranges, TEXT_SELECTION_COLOR, offset_x);
        }
        if let Some(unit) = focus_unit
            && !unit.is_table
            && !unit.rectangular_activation
        {
            page.paint_source_ranges(scene, &unit.paint_ranges, TEXT_SELECTION_COLOR, offset_x);
        }
    }

    fn paint_focus_table_border(
        &self,
        page: &PageDisplayList,
        scene: &mut VelloScene<'_>,
        offset_x: f32,
    ) {
        if self.is_focus_mode()
            && let Some(unit) = self
                .focus_units
                .get(self.focus_unit_index)
                .filter(|unit| unit.is_table)
        {
            page.paint_source_table_borders(
                scene,
                &unit.paint_ranges,
                focus_block_border_color(),
                offset_x,
            );
        }
    }

    fn touch_page_scene(&mut self, key: PageSceneKey) {
        if let Some(position) = self.page_scene_lru.iter().position(|entry| *entry == key) {
            self.page_scene_lru.remove(position);
        }
        self.page_scene_lru.push_back(key);
    }

    pub(in crate::reader) fn bump_scene_revision(&mut self) {
        self.scene_revision = self.scene_revision.wrapping_add(1);
    }

    pub(in crate::reader) fn invalidate_page_scenes(&mut self) {
        self.page_scenes.clear();
        self.page_scene_lru.clear();
        self.scroll_section = None;
        self.bump_scene_revision();
    }

    pub(in crate::reader) fn invalidate_page_scene(&mut self, position: ReaderPosition) {
        evict_page_scene(&mut self.page_scenes, &mut self.page_scene_lru, position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_focus_cards_reuse_the_text_highlight_fill() {
        assert_eq!(focus_unit_activation_color(true), TEXT_SELECTION_COLOR);
        assert_ne!(focus_unit_activation_color(false), TEXT_SELECTION_COLOR);
    }

    #[test]
    fn evicting_an_image_page_removes_only_its_cached_scene() {
        let image_page = ReaderPosition {
            section_index: 2,
            segment_index: 1,
            page_index: 3,
        };
        let text_page = ReaderPosition {
            page_index: 4,
            ..image_page
        };
        let image_key = PageSceneKey {
            section: image_page.section_index,
            segment: image_page.segment_index,
            page: image_page.page_index,
        };
        let text_key = PageSceneKey {
            section: text_page.section_index,
            segment: text_page.segment_index,
            page: text_page.page_index,
        };
        let mut scenes = HashMap::from([(image_key, "image"), (text_key, "text")]);
        let mut lru = VecDeque::from([image_key, text_key]);

        assert!(evict_page_scene(&mut scenes, &mut lru, image_page));
        assert!(!scenes.contains_key(&image_key));
        assert_eq!(scenes.get(&text_key), Some(&"text"));
        assert_eq!(lru, VecDeque::from([text_key]));
        assert!(!evict_page_scene(&mut scenes, &mut lru, image_page));
        assert_eq!(lru, VecDeque::from([text_key]));
    }

    #[test]
    fn every_scene_with_images_refreshes_the_vello_atlas() {
        let image = ImageData {
            data: peniko::Blob::new(Arc::new(vec![255, 255, 255, 255])),
            format: peniko::ImageFormat::Rgba8,
            alpha_type: peniko::ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        };

        assert!(!ReaderScene::new(Scene::new(), Arc::from([])).refresh_image_atlas);
        assert!(ReaderScene::new(Scene::new(), vec![image].into()).refresh_image_atlas);
    }
}
