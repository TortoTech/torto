mod scene;
mod vello;

pub(crate) use scene::{PageSceneKey, PageSceneLayers, ReaderScene};
pub(in crate::reader) use scene::{active_footnote_marker_color, text_selection_fill};
