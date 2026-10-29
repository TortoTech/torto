//! Downloaded reading fonts. Production builds contain no reading font binaries.
//! Workers own discovery, I/O and validation; UI adopts immutable snapshots.
use arc_swap::ArcSwap;
use peniko::Blob;
use std::sync::{Arc, OnceLock};

mod catalog;
mod download;
mod service;
mod store;
pub(crate) use service::{FontService, Message, Status};
pub(crate) use service::{family_label, publish_labels, request_family, take_request};

#[derive(Clone)]
pub(crate) struct Face {
    pub(crate) data: Blob<u8>,
    pub(crate) index: u32,
}

#[derive(Clone, Default)]
pub(crate) struct FontSet {
    pub(crate) faces: [Option<Face>; 3],
}

impl FontSet {
    pub(crate) fn reader_fonts(&self) -> Arc<[Blob<u8>]> {
        let mut blobs: Vec<Blob<u8>> = Vec::new();
        for face in self.faces.iter().flatten() {
            if !blobs.iter().any(|blob| blob.id() == face.data.id()) {
                blobs.push(face.data.clone());
            }
        }
        blobs.into()
    }
    pub(crate) fn cjk(&self) -> Option<Face> {
        self.faces[0].clone()
    }
}

fn current() -> &'static ArcSwap<FontSet> {
    static CURRENT: OnceLock<ArcSwap<FontSet>> = OnceLock::new();
    CURRENT.get_or_init(|| ArcSwap::from_pointee(FontSet::default()))
}

pub(crate) fn install(fonts: Arc<FontSet>) {
    rebook_formats::set_pdf_cjk_fallback_font(fonts.cjk().map(|face| (face.data, face.index)));
    current().store(fonts);
}

pub(crate) fn reader_fonts() -> Arc<[Blob<u8>]> {
    current().load().reader_fonts()
}
pub(crate) fn cjk_face() -> Option<Face> {
    current().load().cjk()
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

#[cfg(test)]
mod tests;
