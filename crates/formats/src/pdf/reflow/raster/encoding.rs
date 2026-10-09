//! Lossless crop storage; rendering resolution and RGBA pixels stay unchanged.
use image::{
    ExtendedColorType, ImageEncoder,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

pub(super) fn save(
    path: &Path,
    mut pixels: Vec<u8>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let (len, color) = compact_channels(&mut pixels, width, height)?;
    pixels.truncate(len);
    let mut writer = BufWriter::new(File::create(path).map_err(|e| e.to_string())?);
    // The previous default was Fast RGBA even for opaque monochrome crops.
    // Default compression and adaptive filtering also help photographic PNGs
    // without introducing JPEG artifacts around text, labels or fine lines.
    PngEncoder::new_with_quality(&mut writer, CompressionType::Default, FilterType::Adaptive)
        .write_image(&pixels, width, height, color)
        .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

fn compact_channels(
    pixels: &mut [u8],
    width: u32,
    height: u32,
) -> Result<(usize, ExtendedColorType), String> {
    let count = usize::try_from(u64::from(width) * u64::from(height)).map_err(|e| e.to_string())?;
    if count.checked_mul(4) != Some(pixels.len()) || count == 0 {
        return Err("Invalid PDF crop pixels".into());
    }
    let opaque = pixels.chunks_exact(4).all(|p| p[3] == 255);
    if !opaque {
        return Ok((pixels.len(), ExtendedColorType::Rgba8));
    }
    let gray = pixels.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2]);
    let channels = if gray { 1 } else { 3 };
    for i in 0..count {
        for c in 0..channels {
            pixels[i * channels + c] = pixels[i * 4 + c];
        }
    }
    Ok((
        count * channels,
        if gray {
            ExtendedColorType::L8
        } else {
            ExtendedColorType::Rgb8
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_gray_color_and_transparency_round_trip_pixel_exactly() {
        let root = std::env::temp_dir().join(format!(
            "torto-crop-encoding-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for (pixels, color) in [
            (
                vec![0, 0, 0, 255, 127, 127, 127, 255],
                ExtendedColorType::L8,
            ),
            (vec![1, 2, 3, 255, 4, 5, 6, 255], ExtendedColorType::Rgb8),
            (vec![1, 2, 3, 123, 4, 5, 6, 255], ExtendedColorType::Rgba8),
        ] {
            let mut compact = pixels.clone();
            assert_eq!(compact_channels(&mut compact, 2, 1).unwrap().1, color);
            save(&root, pixels.clone(), 2, 1).unwrap();
            let decoded = image::open(&root).unwrap().into_rgba8();
            assert_eq!(decoded.as_raw(), &pixels);
        }
        std::fs::remove_file(root).unwrap();
        assert!(compact_channels(&mut [0; 7], 2, 1).is_err());
    }
}
