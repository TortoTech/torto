//! Shared CPU image resizing for display and vision request preparation.
use fast_image_resize::{FilterType, IntoImageView, ResizeAlg, ResizeOptions, Resizer};
use image::{DynamicImage, imageops};

/// Fit inside the bounds, preserving aspect ratio. Like `image::resize`, this
/// may enlarge small inputs; display callers decide whether to shrink only.
pub fn resize(image: DynamicImage, bounds: [u32; 2], filter: imageops::FilterType) -> DynamicImage {
    if image.width() == 0 || image.height() == 0 {
        return image.resize(bounds[0], bounds[1], filter);
    }
    let scale = (f64::from(bounds[0]) / f64::from(image.width()))
        .min(f64::from(bounds[1]) / f64::from(image.height()));
    let width = (f64::from(image.width()) * scale).round().max(1.0) as u32;
    let height = (f64::from(image.height()) * scale).round().max(1.0) as u32;
    resize_exact(image, [width, height], filter)
}

/// Resize in the source pixel format before any conversion to RGBA. Alpha is
/// premultiplied during filtering so invisible colors cannot bleed into edges.
pub fn resize_exact(
    image: DynamicImage,
    size: [u32; 2],
    filter: imageops::FilterType,
) -> DynamicImage {
    if [image.width(), image.height()] == size {
        return image;
    }
    let algorithm = match filter {
        imageops::FilterType::Triangle => Some(ResizeAlg::Convolution(FilterType::Bilinear)),
        imageops::FilterType::Lanczos3 => Some(ResizeAlg::Convolution(FilterType::Lanczos3)),
        _ => None,
    };
    if let Some(algorithm) = algorithm
        && image.pixel_type().is_some()
        && !size.contains(&0)
    {
        let mut resized = DynamicImage::new(size[0], size[1], image.color());
        let options = ResizeOptions::new().resize_alg(algorithm);
        if resized.set_color_space(image.color_space()).is_ok()
            && Resizer::new()
                .resize(&image, &mut resized, &options)
                .is_ok()
        {
            return resized;
        }
    }
    image.resize_exact(size[0], size[1], filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_matches_image_dimensions_for_shrinking_and_enlarging() {
        for dimensions in [[33, 17], [1, 7], [1601, 301], [3, 2]] {
            for bounds in [[16, 16], [1600, 1000], [1, 1]] {
                let source = DynamicImage::new_rgb8(dimensions[0], dimensions[1]);
                let expected = source.resize(bounds[0], bounds[1], imageops::FilterType::Triangle);
                let actual = resize(source, bounds, imageops::FilterType::Triangle);
                assert_eq!(
                    [actual.width(), actual.height()],
                    [expected.width(), expected.height()]
                );
            }
        }
    }

    #[test]
    fn exact_resize_preserves_requested_formula_dimensions_and_alpha() {
        let source = image::RgbaImage::from_pixel(3, 7, image::Rgba([20, 40, 60, 128]));
        let resized = resize_exact(source.into(), [9, 21], imageops::FilterType::Lanczos3);
        assert_eq!([resized.width(), resized.height()], [9, 21]);
        assert_eq!(resized.into_rgba8().get_pixel(4, 10).0, [20, 40, 60, 128]);
    }
}
