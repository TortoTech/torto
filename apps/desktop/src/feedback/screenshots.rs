use super::model::Screenshot;
use crate::async_task::TaskResult;

pub(super) use rebook_app_core::feedback::images::MAX_IMAGES;

#[derive(Clone)]
pub(super) enum Source {
    Clipboard,
    Pick,
    Restore(Vec<Screenshot>),
}

pub(crate) type Message = TaskResult<Vec<Prepared>>;
pub(crate) type PreviewMessage = TaskResult<egui::ColorImage>;
pub(crate) struct Prepared {
    pub(super) screenshot: Screenshot,
    pub(super) thumbnail: egui::ColorImage,
}

pub(super) async fn prepare(source: Source) -> Result<Vec<Prepared>, String> {
    if let Source::Pick = source {
        let paths = rfd::AsyncFileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"])
            .pick_files()
            .await;
        return tokio::task::spawn_blocking(move || {
            let Some(paths) = paths else {
                return Ok(Vec::new());
            };
            if paths.len() > MAX_IMAGES {
                return Err("limit".into());
            }
            paths
                .into_iter()
                .map(|path| {
                    if std::fs::metadata(path.path()).map_err(|_| "image")?.len() > 20 * 1024 * 1024
                    {
                        return Err("image".into());
                    }
                    let mut reader = image::ImageReader::open(path.path())
                        .map_err(|_| "image")?
                        .with_guessed_format()
                        .map_err(|_| "image")?;
                    let mut limits = image::Limits::default();
                    limits.max_alloc = Some(64 * 1024 * 1024);
                    limits.max_image_width = Some(8192);
                    limits.max_image_height = Some(8192);
                    reader.limits(limits);
                    encode(reader.decode().map_err(|_| "image")?)
                })
                .collect()
        })
        .await
        .map_err(|_| "image".to_owned())?;
    }
    tokio::task::spawn_blocking(move || match source {
        Source::Clipboard => {
            let images = crate::platform::feedback_images();
            if images.len() > MAX_IMAGES {
                return Err("limit".into());
            }
            images
                .into_iter()
                .map(|pixels| {
                    if u64::from(pixels.width) * u64::from(pixels.height) > 16 * 1024 * 1024 {
                        return Err("image".into());
                    }
                    let image =
                        image::RgbaImage::from_raw(pixels.width, pixels.height, pixels.rgba)
                            .ok_or("image")?;
                    encode(image::DynamicImage::ImageRgba8(image))
                })
                .collect()
        }
        Source::Restore(images) => {
            if images.len() > MAX_IMAGES {
                return Err("image".into());
            }
            images.into_iter().map(restore).collect()
        }
        Source::Pick => unreachable!(),
    })
    .await
    .map_err(|_| "image".to_owned())?
}

pub(super) fn restore(screenshot: Screenshot) -> Result<Prepared, String> {
    let image = decode(&screenshot)?;
    Ok(Prepared {
        screenshot,
        thumbnail: thumbnail(&image),
    })
}

pub(super) async fn preview(screenshot: Screenshot) -> Result<egui::ColorImage, String> {
    tokio::task::spawn_blocking(move || {
        let pixels = decode(&screenshot)?.to_rgba8();
        Ok(egui::ColorImage::from_rgba_unmultiplied(
            [pixels.width() as usize, pixels.height() as usize],
            pixels.as_raw(),
        ))
    })
    .await
    .map_err(|_| "image".to_owned())?
}

fn decode(screenshot:&Screenshot)->Result<image::DynamicImage,String> {
    rebook_app_core::feedback::images::decode(screenshot)
}
pub(super) fn encode(image:image::DynamicImage)->Result<Prepared,String> {
    let screenshot=rebook_app_core::feedback::images::encode(image)?;
    restore(screenshot)
}

fn thumbnail(image: &image::DynamicImage) -> egui::ColorImage {
    let pixels = image.thumbnail(160, 112).to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied(
        [pixels.width() as usize, pixels.height() as usize],
        pixels.as_raw(),
    )
}
