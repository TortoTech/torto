mod branding;

use std::fs;
use std::path::PathBuf;

use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};

// cargo-bundle maps each source PNG to an icns entry by exact pixel size and
// fails with "No matching IconType" for anything outside this set.
const ICON_SIZES: [u32; 7] = [16, 32, 64, 128, 256, 512, 1024];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output_dir = root.join("assets/macos");
    fs::create_dir_all(&output_dir)?;

    let source = branding::Source::load(&root)?;

    for size in ICON_SIZES {
        let resized = source.render(size)?;
        let mut png = Vec::new();
        PngEncoder::new(&mut png).write_image(
            resized.as_raw(),
            size,
            size,
            ColorType::Rgba8.into(),
        )?;
        // cargo-bundle maps 1024x1024 only at retina density: the icns type
        // for 512@2x is selected by an "@2x" filename suffix, and there is no
        // 1024@1x type at all.
        let name = if size == 1024 {
            "torto-512@2x.png".to_owned()
        } else {
            format!("torto-{size}.png")
        };
        fs::write(output_dir.join(name), &png)?;
    }

    println!(
        "generated {} macOS icon sizes from assets/logo.svg",
        ICON_SIZES.len()
    );
    Ok(())
}
