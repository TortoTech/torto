//! Immutable per-publication PDF fallback snapshots. Downloads can affect the
//! next open without mixing font generations in existing page/cover caches.
use arc_swap::ArcSwapOption;
use fontdb::{Database, Family, Query};
use hayro::hayro_interpret::{
    InterpreterSettings,
    font::{FontData, FontQuery},
};
use rebook_layout::ReaderFontBlob;
use skrifa::MetadataProvider;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

struct Face {
    data: ReaderFontBlob,
    index: u32,
}
fn downloaded() -> &'static ArcSwapOption<Face> {
    static FONT: OnceLock<ArcSwapOption<Face>> = OnceLock::new();
    FONT.get_or_init(ArcSwapOption::empty)
}
pub fn set_pdf_cjk_fallback_font(font: Option<(ReaderFontBlob, u32)>) {
    downloaded().store(font.map(|(data, index)| Arc::new(Face { data, index })));
}

fn system_font() -> Option<Arc<Face>> {
    static SYSTEM: OnceLock<Option<Arc<Face>>> = OnceLock::new();
    SYSTEM
        .get_or_init(|| {
            let mut db = Database::new();
            db.load_system_fonts();
            for family in [
                "PingFang SC",
                "Microsoft YaHei",
                "Noto Sans CJK SC",
                "Noto Sans SC",
                "Source Han Sans SC",
                "Heiti SC",
            ] {
                let Some(id) = db.query(&Query {
                    families: &[Family::Name(family)],
                    ..Query::default()
                }) else {
                    continue;
                };
                let face = db
                    .with_face_data(id, |bytes, index| {
                        let font = skrifa::FontRef::from_index(bytes, index).ok()?;
                        let charmap = font.charmap();
                        if !"中文阅读".chars().all(|ch| charmap.map(ch).is_some())
                            || font.table_data(skrifa::Tag::new(b"glyf")).is_none()
                                && font.table_data(skrifa::Tag::new(b"CFF ")).is_none()
                                && font.table_data(skrifa::Tag::new(b"CFF2")).is_none()
                        {
                            return None;
                        }
                        Some(Arc::new(Face {
                            data: ReaderFontBlob::new(Arc::new(bytes.to_vec())),
                            index,
                        }))
                    })
                    .flatten();
                if face.is_some() {
                    return face;
                }
            }
            None
        })
        .clone()
}

pub(super) fn snapshot() -> (InterpreterSettings, Arc<AtomicBool>) {
    snapshot_with_face(downloaded().load_full().or_else(system_font))
}

fn snapshot_with_face(cjk: Option<Arc<Face>>) -> (InterpreterSettings, Arc<AtomicBool>) {
    let mut settings = InterpreterSettings::default();
    let standard = settings.font_resolver.clone();
    let missing = Arc::new(AtomicBool::new(false));
    let detected = missing.clone();
    settings.font_resolver = Arc::new(move |query| match query {
        FontQuery::Fallback(fallback) if fallback.character_collection.is_some() => {
            if cjk.is_none() {
                detected.store(true, Ordering::Release);
            }
            cjk.as_ref().map(|face| {
                let data: FontData = Arc::new(face.data.clone());
                (data, face.index)
            })
            // A Latin standard-font fallback cannot replace CJK: explicitly
            // report missing coverage rather than caching a blank PDF cover.
        }
        _ => standard(query),
    });
    (settings, missing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::PdfPublication;
    use rebook_publication::{BookSource, PublicationUrl};

    fn pdf() -> PdfPublication {
        let content = "BT /F1 24 Tf 20 70 Td <4E2D6587> Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".into(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H /DescendantFonts [6 0 R] >>".into(),
            "<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> /FontDescriptor 7 0 R /DW 1000 >>".into(),
            "<< /Type /FontDescriptor /FontName /STSong-Light /Flags 4 /FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 >>".into(),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes(),
        );
        for offset in &offsets[1..] {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        crate::pdf::open(bytes, "missing-cjk.pdf").unwrap()
    }

    #[test]
    fn missing_pdf_fallback_is_reported_and_incomplete_cover_is_not_cached() {
        let mut publication = pdf();
        (publication.interpreter, publication.missing_fonts) = snapshot_with_face(None);
        let cover = PublicationUrl::parse(crate::pdf::COVER_PATH).unwrap();
        assert!(publication.resource(&cover).is_err());
        assert!(publication.missing_font_resources());
        assert!(publication.lock_cache().unwrap().cover.is_none());
    }

    #[test]
    fn pdf_font_snapshot_remains_usable_and_preserves_cjk_without_embedding_in_app() {
        let font = ReaderFontBlob::new(Arc::new(
            include_bytes!("../../../../assets/fonts/LXGWWenKaiGBScreen.ttf").as_slice(),
        ));
        let mut publication = pdf();
        (publication.interpreter, publication.missing_fonts) =
            snapshot_with_face(Some(Arc::new(Face {
                data: font,
                index: 0,
            })));
        let cover = PublicationUrl::parse(crate::pdf::COVER_PATH).unwrap();
        assert!(publication.resource(&cover).is_ok());
        assert!(!publication.missing_font_resources());
        assert!(publication.parse_section(0).unwrap().blocks.iter().any(|block|
            matches!(block, rebook_publication::Block::Image(image) if image.text_layer.as_ref().is_some_and(|layer| layer.text.contains("中文")))));
        // Creating another unavailable snapshot does not mutate this publication.
        let _unavailable = snapshot_with_face(None);
        assert!(publication.resource(&cover).is_ok());
        assert!(!publication.missing_font_resources());
    }
}
