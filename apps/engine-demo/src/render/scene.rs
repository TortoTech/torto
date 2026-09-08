use std::sync::Arc;

use peniko::ImageData;
use rebook_publication::SourceRange;
use rebook_reader::ReaderPosition;
use vello::Scene;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageSceneKey {
    pub position: ReaderPosition,
    pub layout_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpreadSceneKey {
    pub primary: PageSceneKey,
    pub secondary: Option<PageSceneKey>,
    pub width: u32,
    pub height: u32,
}

pub struct StaticSpreadLayers {
    pub underlay: Arc<Scene>,
    pub content: Arc<Scene>,
    pub images: Arc<[ImageData]>,
    pub key: SpreadSceneKey,
}

pub struct StaticSpreadScene {
    pub scene: Arc<Scene>,
    pub images: Arc<[ImageData]>,
    pub key: SpreadSceneKey,
}

#[derive(Debug, Clone, Default)]
pub struct OverlaySet {
    pub highlights: Vec<SourceRange>,
    pub selection: Vec<SourceRange>,
    pub focus: Vec<SourceRange>,
}
