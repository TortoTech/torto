//! Downloaded reading fonts. Production builds contain no reading font binaries.
//! Workers own discovery, I/O and validation; UI adopts immutable snapshots.
use arc_swap::ArcSwap;
use peniko::Blob;
use std::sync::{Arc, OnceLock};

pub mod catalog;
pub mod download;
pub mod store;

#[derive(Clone)]
pub struct Face {
    pub data: Blob<u8>,
    pub index: u32,
}

#[derive(Clone, Default)]
pub struct FontSet {
    pub faces: [Option<Face>; 3],
}

impl FontSet {
    pub fn reader_fonts(&self) -> Arc<[Blob<u8>]> {
        let mut blobs: Vec<Blob<u8>> = Vec::new();
        for face in self.faces.iter().flatten() {
            if !blobs.iter().any(|blob| blob.id() == face.data.id()) {
                blobs.push(face.data.clone());
            }
        }
        blobs.into()
    }
    pub fn cjk(&self) -> Option<Face> {
        self.faces[0].clone()
    }
}

fn current() -> &'static ArcSwap<FontSet> {
    static CURRENT: OnceLock<ArcSwap<FontSet>> = OnceLock::new();
    CURRENT.get_or_init(|| ArcSwap::from_pointee(FontSet::default()))
}

pub fn install(fonts: Arc<FontSet>) {
    rebook_formats::set_pdf_cjk_fallback_font(fonts.cjk().map(|face| (face.data, face.index)));
    current().store(fonts);
}

pub fn reader_fonts() -> Arc<[Blob<u8>]> {
    current().load().reader_fonts()
}
pub fn cjk_face() -> Option<Face> {
    current().load().cjk()
}

#[cfg(test)]
mod tests;

// Deterministic, offline regression fixtures, linked only into the test binary.
#[cfg(test)]
pub fn embedded_reader_fonts() -> Arc<[Blob<u8>]> {
    [
        include_bytes!("../../../assets/fonts/LXGWWenKaiGBScreen.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/Literata-opsz-wght.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/Literata-Italic-opsz-wght.ttf").as_slice(),
    ]
    .map(|data| Blob::new(Arc::new(data)))
    .into()
}

/// Reuse a system font file in platform widgets without copying a font binary.
pub fn system_font_file(family: &str) -> Option<(std::path::PathBuf, u32)> {
    static DATABASE: OnceLock<fontdb::Database> = OnceLock::new();
    let database = DATABASE.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    });
    let families = [if family == "monospace" {
        fontdb::Family::Monospace
    } else {
        fontdb::Family::Name(family)
    }];
    let id = database.query(&fontdb::Query {
        families: &families,
        ..Default::default()
    })?;
    let face = database.face(id)?;
    match &face.source {
        fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
            Some((path.clone(), face.index))
        }
        fontdb::Source::Binary(_) => None,
    }
}
