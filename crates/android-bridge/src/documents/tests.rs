use super::*;

pub(crate) async fn image_reference_preview(core: &Arc<MobileCore>, root: &std::path::Path) {
    use rebook_publication::media_references::{MediaKey, MediaKind};
    use std::io::Write;
    for tall in [false, true] {
        let path = root.join(format!("image-reference-refresh-{tall}.epub"));
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(
            80,
            if tall { 600 } else { 60 },
            image::Rgba([30, 120, 70, 255]),
        )
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let body = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>See Figure 1.1.</p><div><img src="image.png" style="width:60%"/><p style="font-size:3em">Figure 1.1 Caption</p><p style="font-size:3em;text-indent:6em">Explanation inside the figure.</p><p style="font-size:3em;margin-left:6em;font-weight:bold">© Author</p></div></body></html>"#;
        let title = format!("Image reference refresh {tall}");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name,bytes) in [
        ("mimetype",b"application/epub+zip".as_slice()),
        ("META-INF/container.xml",br#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="book.opf"/></rootfiles></container>"#.as_slice()),
        ("book.opf",br#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">image-reference-refresh</dc:identifier><dc:title>Image reference refresh</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="image" href="image.png" media-type="image/png"/></manifest><spine><itemref idref="body"/></spine></package>"#.as_slice()),
        ("body.xhtml",body.as_bytes()),("image.png",png.get_ref().as_slice()),
    ] {
        zip.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();
        if name=="book.opf" {
            zip.write_all(std::str::from_utf8(bytes).unwrap().replace("Image reference refresh",&title).as_bytes()).unwrap();
        } else {zip.write_all(bytes).unwrap();}
    }
        zip.finish().unwrap();
        let book = core
            .clone()
            .import_book(path.to_string_lossy().into_owned())
            .await
            .unwrap()
            .into_iter()
            .find(|b| b.title == title)
            .unwrap();
        let viewport = Viewport {
            width: 360,
            height: 640,
            scale: 1.0,
        };
        let reader = core
            .clone()
            .open_book(book.id, viewport.clone())
            .await
            .unwrap();
        for unified in [true, false] {
            reader
                .clone()
                .configure(20.0, unified, false)
                .await
                .unwrap();
            for focus in [false, true] {
                reader.clone().focus_mode(focus).await.unwrap();
                reader.clone().prepare_media_references().await.unwrap();
                reader.clone().refresh_media_references().await.unwrap();
                let action = reader
                    .with(|s| {
                        let section = s.source.parse_section(0).map_err(error)?;
                        let origin =
                            rebook_app_core::reading_text::block_source_range(&section.blocks[0])
                                .unwrap();
                        let target = s
                            .media
                            .resolve(
                                origin,
                                &MediaKey {
                                    kind: MediaKind::Figure,
                                    number: "1.1".into(),
                                },
                            )
                            .unwrap();
                        documents::reference(s, target)
                    })
                    .unwrap();
                let metrics = |pages: &[Arc<PageDisplayList>]| {
                    ["Caption", "Explanation", "© Author"].map(|needle| {
                        pages
                            .iter()
                            .find_map(|page| {
                                (0..page.text_region_count()).find_map(|i| {
                                    let text = page.text_region_text(i)?;
                                    if !text.contains(needle) {
                                        return None;
                                    }
                                    let source = page.text_region_source_range(i, 0..text.len())?;
                                    Some(
                                        page.source_rects(&[source])
                                            .iter()
                                            .map(|r| r.height())
                                            .fold(0.0, f64::max),
                                    )
                                })
                            })
                            .expect(needle)
                    })
                };
                let (body_image, body_typography, body_profile, body_metrics) = reader
                    .with(|s| {
                        let pages = s.session.current_reading_unit_pages().map_err(error)?;
                        let image = pages.iter().find_map(|p| p.page.image_bounds()).unwrap();
                        let style = s.session.style();
                        let pages = pages.iter().map(|p| p.page.clone()).collect::<Vec<_>>();
                        Ok((image, style.typography, style.typesetting, metrics(&pages)))
                    })
                    .unwrap();
                if tall {
                    assert!(
                        body_image.height() > 480.0,
                        "Tall fixture reaches the body height cap: {body_image:?}"
                    );
                } else {
                    assert!((body_image.width() - 320.0).abs() < 0.1);
                }
                let mut previous = None;
                for refresh in [false, true] {
                    if refresh {
                        reader
                            .with(|s| {
                                reflow::refresh(s)?;
                                assert!(documents::refresh(s)?);
                                Ok(())
                            })
                            .unwrap();
                    }
                    // The sheet is narrower and shorter than the reading viewport; its
                    // visible height must not change image scaling or text sizing.
                    let popup = Viewport {
                        width: 320,
                        height: 220,
                        scale: 1.0,
                    };
                    let preview = reader
                        .clone()
                        .reference_page(action.target.clone(), 0, popup, 0xff000000, 0xffffffff)
                        .await
                        .unwrap();
                    reader
                        .with(|s| {
                            let d = s.document.as_ref().unwrap();
                            let style = d.session.as_ref().unwrap().style();
                            assert_eq!(
                                style.typography.minimum_font_size,
                                body_typography.minimum_font_size,
                                "Caption font floor must match the body"
                            );
                            assert_eq!(style.typography.font_size, body_typography.font_size);
                            assert_eq!(style.typography.font_weight, body_typography.font_weight);
                            assert_eq!(
                                metrics(&d.pages),
                                body_metrics,
                                "Caption, description and credit line metrics must match the body"
                            );
                            assert_eq!(
                                style.typesetting, body_profile,
                                "Preview must retain the body's actual presentation mode"
                            );
                            let image = d.pages.iter().find_map(|p| p.image_bounds()).unwrap();
                            assert!(
                                (image.width() - body_image.width()).abs() < 0.1,
                                "Image width differs: body={body_image:?}, preview={image:?}"
                            );
                            assert!(
                                (image.height() - body_image.height()).abs() < 0.1,
                                "Image height differs: body={body_image:?}, preview={image:?}"
                            );
                            Ok(())
                        })
                        .unwrap();
                    let image_target = reader.with(|s| {
                        Ok(s.document.as_ref().unwrap().pages.iter().enumerate().find_map(|(index, page)| {
                            page.image_bounds().map(|bounds| (
                                index as u32,
                                ((bounds.x0 + bounds.x1) / 2.) as f32,
                                ((bounds.y0 + bounds.y1) / 2.) as f32,
                            ))
                        }).unwrap())
                    }).unwrap();
                    let image = reader.clone().document_activate(
                        preview.id.clone(), image_target.0, image_target.1, image_target.2,
                    ).await.unwrap();
                    assert_eq!(image.kind, "image");
                    assert!(image.media.is_some());
                    reader.with(|s| {
                        assert_eq!(s.document.as_ref().unwrap().id, preview.id, "tapping a raster leaves its parent document open");
                        Ok(())
                    }).unwrap();
                    let page = &preview.page;
                    let mut left = page.width();
                    let mut right = 0;
                    let mut top = page.height();
                    let mut bottom = 0;
                    for (i, pixel) in page.pixels.chunks_exact(4).enumerate() {
                        if pixel[0] < 40
                            && (110..=130).contains(&pixel[1])
                            && (60..=80).contains(&pixel[2])
                        {
                            let x = i as u32 % page.width();
                            let y = i as u32 / page.width();
                            left = left.min(x);
                            right = right.max(x);
                            top = top.min(y);
                            bottom = bottom.max(y);
                        }
                    }
                    assert!(
                        right > left && bottom > top,
                        "The native popup contains the image"
                    );
                    let size = (right - left + 1, bottom - top + 1);
                    assert!(
                        (size.0 as f64 - body_image.width()).abs() <= 2.0,
                        "Preview matches the body image: {size:?}"
                    );
                    if let Some(before) = previous {
                        assert_eq!(size, before, "Popup refresh must keep the same size");
                    }
                    previous = Some(size);
                    let info = page.info();
                    if (focus || unified) && !tall {
                        for text in ["Caption", "Explanation", "© Author"] {
                            let region = info
                                .regions
                                .iter()
                                .find(|r| r.text.contains(text))
                                .expect(text);
                            assert!(
                                region.bounds.iter().all(|r| r.height < 35.0),
                                "{text} uses unified caption sizing, not authored 3em"
                            );
                            if text == "Explanation" {
                                assert!(
                                    region.bounds[0].x < 10.0,
                                    "Description has no authored 6em indent"
                                );
                            }
                            if text == "© Author" {
                                let mut ink = 255;
                                for rect in &region.bounds {
                                    for y in (rect.y.max(0.0) as u32)
                                        ..((rect.y + rect.height).ceil() as u32).min(page.height())
                                    {
                                        for x in (rect.x.max(0.0) as u32)
                                            ..((rect.x + rect.width).ceil() as u32)
                                                .min(page.width())
                                        {
                                            ink = ink.min(
                                                page.pixels[((y * page.width() + x) * 4) as usize],
                                            );
                                        }
                                    }
                                }
                                assert!(
                                    (30..230).contains(&ink),
                                    "Credits use secondary ink: {ink}"
                                );
                            }
                        }
                    }
                }
                reader.clone().close_document(action.target).await.unwrap();
            }
        }
        reader.shutdown().await.unwrap();
    }
}
