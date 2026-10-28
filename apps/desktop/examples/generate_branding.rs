mod branding;

use std::fs;
use std::path::PathBuf;

use image::{DynamicImage, Rgba, RgbaImage};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = branding::Source::load(&root)?;
    source.render(1024)?.save(root.join("assets/logo.png"))?;

    let output = root.join("assets/windows/installer");
    fs::create_dir_all(&output)?;

    // WiX overlays its heading on the left of the banner. Keep the logo in
    // the rightmost 58-pixel square and leave the heading area plain white.
    let mut banner = RgbaImage::from_pixel(493, 58, Rgba([255, 255, 255, 255]));
    image::imageops::overlay(&mut banner, &source.render(48)?, 440, 5);
    DynamicImage::ImageRgba8(banner)
        .to_rgb8()
        .save(output.join("installer-banner.bmp"))?;

    // Only the left 164 pixels are artwork: the welcome/exit dialog places
    // localized text and controls over the entire right-hand white region.
    let mut dialog = RgbaImage::from_pixel(493, 312, Rgba([255, 255, 255, 255]));
    for y in 0..312 {
        for x in 0..164 {
            dialog.put_pixel(x, y, Rgba([234, 245, 240, 255]));
        }
    }
    image::imageops::overlay(&mut dialog, &source.render(144)?, 10, 84);
    DynamicImage::ImageRgba8(dialog)
        .to_rgb8()
        .save(output.join("installer-dialog.bmp"))?;

    println!("generated master PNG and 24-bit WiX banner/dialog from assets/logo.svg");
    Ok(())
}
