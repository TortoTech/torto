use super::*;
use rebook_publication::ImageContainerWidth;
fn paginator() -> Paginator {
    Paginator::new(
        LayoutViewport::new(800, 700).unwrap(),
        Rgba::BLACK,
        PageGeometry {
            left: 0.0,
            top: 0.0,
            width: 800.0,
            bottom: 700.0,
            visible_pages: 1,
            continuation_offset_x: 0.0,
        },
        false,
        0.0,
    )
}
fn raster() -> RasterImage {
    RasterImage {
        deferred: None,
        origin: None,
        blob: None,
        width: 192,
        height: 216,
        pixels: Vec::new().into(),
    }
}
fn zero() -> ImageContainerWidth {
    ImageContainerWidth {
        fraction: Some(0.0),
        pixels: None,
    }
}
#[test]
fn auto_image_can_overflow_zero_width_but_percentage_and_clipping_remain_real_zeroes() {
    let mut p = paginator();
    let style = ImageStyle {
        container_width: Some(zero()),
        ..Default::default()
    };
    assert_eq!(p.image_display_size(&raster(), style), (192.0, 216.0));
    assert_eq!(
        p.image_display_size(
            &raster(),
            ImageStyle {
                width: Some(ImageLength::Fraction(1.0)),
                ..style
            }
        ),
        (0.0, 0.0)
    );
    p.push_image(
        raster(),
        ImageStyle {
            clip_width: Some(zero()),
            ..style
        },
        None,
        None,
    );
    assert!(p.items.is_empty());
    p.push_image(raster(), style, None, None);
    assert!(
        matches!(&p.items[0],PageItem::Image(i) if i.width==192.0 && i.height==216.0 && i.clip.is_none())
    );
}
#[test]
fn percentage_height_requires_a_definite_containing_height_and_clipping_does_not_rescale_pixels() {
    let mut p = paginator();
    let mut style = ImageStyle {
        height: Some(ImageLength::Fraction(0.5)),
        ..Default::default()
    };
    assert_eq!(p.image_display_size(&raster(), style), (192.0, 216.0));
    style.container_height = Some(ImageContainerWidth {
        fraction: None,
        pixels: Some(200.0),
    });
    let (w, h) = p.image_display_size(&raster(), style);
    assert_eq!(h, 100.0);
    assert!((w - 192.0 / 216.0 * 100.0).abs() < 0.001);
    p.push_image(
        raster(),
        ImageStyle {
            clip_width: Some(ImageContainerWidth {
                fraction: None,
                pixels: Some(48.0),
            }),
            ..Default::default()
        },
        None,
        None,
    );
    let PageItem::Image(i) = &p.items[0] else {
        panic!()
    };
    assert_eq!(i.width, 192.0);
    assert_eq!(i.clip.unwrap().width, 48.0);
}
#[test]
fn css_min_and_max_dimensions_are_resolved_before_reader_page_fitting() {
    let p = paginator();
    let (w, h) = p.image_display_size(
        &raster(),
        ImageStyle {
            min_width: Some(ImageLength::Pixels(384.0)),
            ..Default::default()
        },
    );
    assert_eq!((w, h), (384.0, 432.0));
    assert_eq!(
        p.image_display_size(
            &raster(),
            ImageStyle {
                max_width: Some(ImageLength::Pixels(96.0)),
                ..Default::default()
            }
        ),
        (96.0, 108.0)
    );
}
