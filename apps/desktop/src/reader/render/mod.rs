mod scene;
mod vello;
#[cfg(test)]
pub(in crate::reader) use vello::VelloScene;

pub(crate) use scene::scene_encoding_bytes;
pub(crate) use scene::{PageSceneKey, PageSceneLayers, ReaderScene};
pub(in crate::reader) use scene::{active_footnote_marker_color, text_selection_fill};
