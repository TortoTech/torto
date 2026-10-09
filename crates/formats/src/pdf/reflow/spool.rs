//! Conversion-only page spool. Published sections and manifests remain JSON.
//! Fixed-width little-endian floats preserve the extracted coordinate bits.
use super::{MAX_SECTION_BYTES, NativePage};
use bincode::Options;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_SECTION_BYTES)
        .reject_trailing_bytes()
}

pub(super) fn write(path: &Path, page: &NativePage) -> Result<(), String> {
    let file = fs::File::create(path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    codec()
        .serialize_into(&mut writer, page)
        .map_err(|e| format!("PDF page spool write: {e}"))?;
    // Propagate the final buffered write error, rather than losing it in Drop.
    writer.flush().map_err(|e| e.to_string())
}

pub(super) fn read(path: &Path) -> Result<NativePage, String> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_SECTION_BYTES {
        return Err("PDF page spool exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SECTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_SECTION_BYTES {
        return Err("PDF page spool exceeds size limit".into());
    }
    codec()
        .deserialize(&bytes)
        .map_err(|e| format!("PDF page spool read: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::reflow::{EncodedImage, NativeGlyph, NativeHeading};

    #[test]
    fn page_spool_preserves_coordinates_text_and_source_metadata() {
        let coordinate = f64::from_bits(0x406d_71f3_b27a_819d);
        let page = NativePage {
            width: coordinate,
            height: 800.0,
            glyphs: vec![NativeGlyph {
                text: "中é👩‍💻".into(),
                rect: [coordinate, -0.0, 99.0, 120.0],
                baseline: [3.0, 4.0],
                advance: [5.0, 6.0],
                size: 12.0,
                bold: true,
                italic: true,
                rotated: true,
                tag: Some("Caption".into()),
                mcid: Some(-3),
                link: Some("https://example.com/中文".into()),
                index: 37,
                unmapped: true,
            }],
            images: vec![[1.0, 2.0, 3.0, 4.0]],
            encoded_images: vec![
                Some(EncodedImage {
                    object: [27, 1],
                    width: 250,
                    height: 600,
                }),
                None,
            ],
            raster_decode_bytes: 123456,
            image_obstacles: vec![[2.0, 3.0, 4.0, 5.0]],
            rules: vec![[3.0, 4.0, 5.0, 6.0]],
            graphics: vec![[4.0, 5.0, 6.0, 7.0]],
            unmapped: 1,
            invisible: 2,
            headings: vec![NativeHeading {
                ordinal: vec![0],
                title: vec![1, 2],
                level: 3,
            }],
        };
        let bytes = codec().serialize(&page).unwrap();
        let decoded: NativePage = codec().deserialize(&bytes).unwrap();
        // Re-encoding compares every field, including float bits and signed zero.
        assert_eq!(bytes, codec().serialize(&decoded).unwrap());
        assert_eq!(decoded.width.to_bits(), coordinate.to_bits());
        assert_eq!(decoded.glyphs[0].rect[1].to_bits(), (-0.0f64).to_bits());
        assert_eq!(decoded.glyphs[0].text, "中é👩‍💻");
        assert!(
            codec()
                .deserialize::<NativePage>(&bytes[..bytes.len() - 1])
                .is_err()
        );
        let mut trailing = bytes;
        trailing.push(0);
        assert!(codec().deserialize::<NativePage>(&trailing).is_err());
    }

    #[test]
    fn page_spool_rejects_oversized_lengths() {
        let mut bytes = codec().serialize(&NativePage::default()).unwrap();
        // width + height, then the glyph vector length.
        bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(codec().deserialize::<NativePage>(&bytes).is_err());
    }
}
