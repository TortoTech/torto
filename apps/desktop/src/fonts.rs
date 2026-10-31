#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use peniko::Blob;
pub(crate) use rebook_app_core::fonts::*;
mod service;
pub(crate) use service::{FontService, Message, Status, family_label, publish_labels, request_family, take_request};
mod store {
    pub(super) use rebook_app_core::fonts::store::*;
    pub(super) fn directory() -> Result<std::path::PathBuf, String> {
        crate::smoke::project_dirs().map(|dirs| dirs.data_local_dir().join("fonts")).ok_or_else(|| "Font directory unavailable".into())
    }
}
// Deterministic, offline regression fixtures, linked only into the test binary.
#[cfg(test)]
pub(crate) fn embedded_reader_fonts() -> Arc<[Blob<u8>]> {
    [
        include_bytes!("../../../assets/fonts/LXGWWenKaiGBScreen.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/Literata-Italic-opsz-wght.ttf").as_slice(),
    ]
    .map(|data| Blob::new(Arc::new(data)))
    .into()
}
