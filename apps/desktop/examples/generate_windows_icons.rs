use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ColorType, ImageEncoder};

const ICON_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];
const STORE_ASSETS: [(&str, u32); 3] = [
    ("StoreLogo.png", 50),
    ("Square44x44Logo.png", 44),
    ("Square150x150Logo.png", 150),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_path = root.join("assets/logo.png");
    let output_dir = root.join("assets/windows");
    let store_output_dir = output_dir.join("store");
    fs::create_dir_all(&output_dir)?;
    fs::create_dir_all(&store_output_dir)?;

    let source = image::open(&source_path)?.into_rgba8();
    let (width, height) = source.dimensions();
    if width != height {
        return Err(format!("logo must be square, got {width}x{height}").into());
    }

    let mut frames = Vec::with_capacity(ICON_SIZES.len());
    for size in ICON_SIZES {
        let resized = image::imageops::resize(&source, size, size, FilterType::Lanczos3);
        let mut png = Vec::new();
        PngEncoder::new(&mut png).write_image(
            resized.as_raw(),
            size,
            size,
            ColorType::Rgba8.into(),
        )?;
        fs::write(output_dir.join(format!("torto-{size}.png")), &png)?;
        frames.push((size, png));
    }

    for (name, size) in STORE_ASSETS {
        let resized = image::imageops::resize(&source, size, size, FilterType::Lanczos3);
        let mut png = Vec::new();
        PngEncoder::new(&mut png).write_image(
            resized.as_raw(),
            size,
            size,
            ColorType::Rgba8.into(),
        )?;
        fs::write(store_output_dir.join(name), png)?;
    }

    write_ico(&output_dir.join("torto.ico"), &frames)?;
    println!(
        "generated {} Windows icon sizes from {}x{} source",
        frames.len(),
        width,
        height
    );
    Ok(())
}

fn write_ico(path: &std::path::Path, frames: &[(u32, Vec<u8>)]) -> io::Result<()> {
    let count = u16::try_from(frames.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many icon frames"))?;
    let directory_size = 6_u32 + u32::from(count) * 16;
    let mut offset = directory_size;
    let mut output = Vec::new();

    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&count.to_le_bytes());

    for (size, png) in frames {
        output.push(if *size == 256 {
            0
        } else {
            u8::try_from(*size).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "icon frame is too large")
            })?
        });
        output.push(if *size == 256 {
            0
        } else {
            u8::try_from(*size).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "icon frame is too large")
            })?
        });
        output.extend_from_slice(&[0, 0]);
        output.extend_from_slice(&1_u16.to_le_bytes());
        output.extend_from_slice(&32_u16.to_le_bytes());
        output.extend_from_slice(
            &u32::try_from(png.len())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "icon is too large"))?
                .to_le_bytes(),
        );
        output.extend_from_slice(&offset.to_le_bytes());
        offset =
            offset
                .checked_add(u32::try_from(png.len()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "icon is too large")
                })?)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "icon is too large"))?;
    }

    for (_, png) in frames {
        output.write_all(png)?;
    }
    fs::write(path, output)
}
