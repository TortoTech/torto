#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use peniko::Blob;
pub(crate) use rebook_app_core::fonts::*;
mod service;
pub(crate) use service::{
    CustomFont, CustomImportMessage, CustomImportTarget, FontService, Message, Status,
    any_downloading, custom_cjk_families, custom_latin_families, family_label, family_status,
    is_family_ready, publish_labels, request_custom_import, request_family,
    request_family_with_apply, take_apply_request, take_custom_import_request, take_requests,
};
mod store {
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
