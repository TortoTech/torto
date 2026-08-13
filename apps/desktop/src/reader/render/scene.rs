use std::sync::Arc;

use kurbo::Affine;
use peniko::Color;
use rebook_formats::BookFormat;
use rebook_reader::ReaderSectionPage;
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
}

impl DesktopReader {
    pub(crate) fn page_scene(&mut self) -> Arc<Scene> {
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
        Arc::new(scene)
    }

    fn scroll_page_scene(&mut self) -> Arc<Scene> {
        let Some(viewport) = self.scroll_viewport else {
            return Arc::new(Scene::new());
        };
        let layout = match self.current_scroll_layout() {
            Ok(layout) => layout,
            Err(error) => {
                self.error = Some(format!("生成滑动章节失败：{error}"));
                return Arc::new(Scene::new());
            }
        };
        let content_padding = self.scroll_content_padding(viewport.size.y);
        let visible_bottom = viewport.offset_y + viewport.size.y;
        let mut scene = Scene::new();
        for (index, entry) in layout.pages.iter().enumerate() {
            let top = layout.page_tops[index] + content_padding;
            let bottom = top + layout.page_heights[index];
            if bottom <= viewport.offset_y || top >= visible_bottom {
                continue;
            }
            let layers = self.scroll_page_layers(entry);
            let mut page_scene = Scene::new();
            page_scene.append(&layers.underlay, None);
            self.paint_page_overlays(&entry.page, &mut VelloScene::new(&mut page_scene), 0.0);
            page_scene.append(&layers.content, None);
            self.paint_focus_table_border(&entry.page, &mut VelloScene::new(&mut page_scene), 0.0);
            scene.append(
                &page_scene,
                Some(Affine::translate((
                    0.0,
                    f64::from(top - viewport.offset_y - layout.page_origins[index]),
                ))),
            );
        }
        Arc::new(scene)
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
        match self.reader.current_spread() {
            Ok(spread) => {
                let mut underlay_bridge = VelloScene::new(&mut underlay);
                spread.primary.paint_background(&mut underlay_bridge);
                spread
                    .primary
                    .paint_images_at(&mut underlay_bridge, spread.primary_offset_x);
                if let Some(secondary) = &spread.secondary {
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
                self.reader
                    .current_page()
                    .paint(&mut VelloScene::new(&mut underlay));
            }
        }
        let layers = Arc::new(PageSceneLayers {
            underlay: Arc::new(underlay),
            content: Arc::new(content),
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
        for highlight in &self.highlights {
            page.paint_source_ranges(scene, &highlight.ranges, ANNOTATION_MARK_COLOR, offset_x);
        }
        if let Some(mark) = &self.focused_mark {
            page.paint_source_ranges(scene, &mark.ranges, mark.color(), offset_x);
        }
        if let Some(selection) = &self.selection {
            page.paint_source_ranges(scene, &selection.ranges, TEXT_SELECTION_COLOR, offset_x);
        }
        if self.is_focus_mode()
            && let Some(unit) = self.focus_units.get(self.focus_unit_index)
            && !unit.is_table
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
}
