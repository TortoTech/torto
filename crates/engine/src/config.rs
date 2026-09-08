use std::sync::Arc;

use rebook_layout::{LayoutViewport, ReaderFontBlob, ReaderStyle};
use rebook_publication::LocatorV1;

#[derive(Clone, Default)]
pub struct EngineConfig {
    pub fonts: Arc<[ReaderFontBlob]>,
}

#[derive(Clone)]
pub struct ReaderConfig {
    pub viewport: LayoutViewport,
    pub style: ReaderStyle,
    pub locator: Option<LocatorV1>,
}
