use super::{
    Face, FontSet,
    catalog::{Asset, catalog},
};
use fontdb::{Database, Family, Query, Style, Weight};
use peniko::Blob;
use skrifa::raw::TableProvider;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn directory() -> Result<PathBuf, String> {
    crate::smoke::project_dirs()
        .map(|dirs| dirs.data_local_dir().join("fonts"))
        .ok_or_else(|| "无法确定字体保存目录 / Font directory unavailable".into())
}
pub(super) fn path(root: &Path, asset: &Asset) -> PathBuf {
    root.join(&catalog().version)
        .join(format!("{}-{}.ttf", asset.id, asset.sha256))
}
pub(super) fn validate(asset: &Asset, bytes: Vec<u8>, pinned: bool) -> Result<Face, String> {
    if bytes.is_empty() || bytes.len() > 40 * 1024 * 1024 {
        return Err("字体文件大小无效 / Invalid font size".into());
    }
    if pinned {
        crate::http::verify_sized_sha256(&bytes, asset.size as u64, 40 * 1024 * 1024, &asset.sha256)
            .map_err(|_| "字体校验失败 / Font checksum mismatch".to_owned())?;
    }
    let mut db = Database::new();
    db.load_font_data(bytes);
    let id = db
        .query(&Query {
            families: &[Family::Name(&asset.family)],
            style: if asset.italic {
                Style::Italic
            } else {
                Style::Normal
            },
            weight: Weight::NORMAL,
            ..Query::default()
        })
        .ok_or("字体名称无效 / Unexpected font family")?;
    if db
        .face(id)
        .is_none_or(|face| (face.style == Style::Italic) != asset.italic)
    {
        return Err("字体样式无效 / Unexpected font style".into());
    }
    db.with_face_data(id, |data, index| {
        let font =
            skrifa::FontRef::from_index(data, index).map_err(|_| "无法解析字体 / Invalid font")?;
        if font.maxp().map_or(true, |maxp| maxp.num_glyphs() == 0)
            || font.table_data(skrifa::Tag::new(b"glyf")).is_none()
                && font.table_data(skrifa::Tag::new(b"CFF ")).is_none()
                && font.table_data(skrifa::Tag::new(b"CFF2")).is_none()
        {
            return Err("字体没有可用轮廓 / Font outlines unavailable".into());
        }
        Ok(Face {
            data: Blob::new(Arc::new(data.to_vec())),
            index,
        })
    })
    .ok_or("字体数据无效 / Invalid font data")?
}
pub(super) fn load_cached(root: &Path, asset: &Asset) -> Option<Face> {
    let file = path(root, asset);
    if fs::metadata(&file).ok()?.len() != asset.size as u64 {
        return None;
    }
    validate(asset, fs::read(file).ok()?, true).ok()
}
pub(super) fn load(root: &Path, mut set: FontSet) -> FontSet {
    // Include license records with external fonts without blocking offline use
    // when a previously downloaded directory has since become read-only.
    let _ = crate::persistence::write_bytes_atomic(
        &root.join("OFL-1.1.txt"),
        include_bytes!("../../../../assets/fonts/OFL-1.1.txt"),
    );
    let _ = crate::persistence::write_bytes_atomic(
        &root.join("README.md"),
        include_bytes!("../../../../assets/fonts/README.md"),
    );
    for (index, asset) in catalog().fonts.iter().enumerate() {
        if set.faces[index].is_none() {
            set.faces[index] = load_cached(root, asset);
        }
    }
    if set.faces.iter().all(Option::is_some) {
        return set;
    }
    let mut system = Database::new();
    system.load_system_fonts();
    for (index, asset) in catalog().fonts.iter().enumerate() {
        if set.faces[index].is_some() {
            continue;
        }
        if let Some(id) = system.query(&Query {
            families: &[Family::Name(&asset.family)],
            style: if asset.italic {
                Style::Italic
            } else {
                Style::Normal
            },
            ..Query::default()
        }) {
            set.faces[index] = system
                .with_face_data(id, |bytes, _| validate(asset, bytes.to_vec(), false).ok())
                .flatten();
        }
    }
    set
}
pub(super) fn save(root: &Path, asset: &Asset, bytes: Vec<u8>) -> Result<Face, String> {
    // Invalid downloads never replace a usable file. AtomicFile also provides
    // unique temporary files when two Torto processes download concurrently.
    let face = validate(asset, bytes, true)?;
    crate::persistence::write_bytes_atomic(&path(root, asset), face.data.as_ref())
        .map_err(|error| error.to_string())?;
    Ok(face)
}
