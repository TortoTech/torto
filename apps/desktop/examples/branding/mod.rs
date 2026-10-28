use std::error::Error;
use std::fs;
use std::path::Path;

use image::{ImageBuffer, RgbaImage};
use resvg::{tiny_skia, usvg};

pub struct Source {
    tree: usvg::Tree,
}

impl Source {
    pub fn load(root: &Path) -> Result<Self, Box<dyn Error>> {
        let svg = fs::read(root.join("assets/logo.svg"))?;
        let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())?;
        Ok(Self { tree })
    }

    pub fn render(&self, size: u32) -> Result<RgbaImage, Box<dyn Error>> {
        // Render the vector at each target size, avoiding resampling fringes
        // around the transparent tile and retaining the small facial details.
        let mut pixels =
            tiny_skia::Pixmap::new(size, size).ok_or("could not allocate logo pixels")?;
        let scale = size as f32 / self.tree.size().width();
        resvg::render(
            &self.tree,
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixels.as_mut(),
        );
        // tiny-skia stores premultiplied RGBA; PNG/image use straight alpha.
        // Unpremultiply edges to avoid dark fringes on light backgrounds.
        let rgba: Vec<u8> = pixels
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let color = pixel.demultiply();
                [color.red(), color.green(), color.blue(), color.alpha()]
            })
            .collect();
        let image =
            ImageBuffer::from_raw(size, size, rgba).ok_or("invalid logo pixel dimensions")?;
        Ok(image)
    }
}
