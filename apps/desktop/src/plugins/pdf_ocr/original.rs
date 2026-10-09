//! The OCR descriptor is sufficient until an explicit original-page request.
use super::*;

pub(crate) fn cache_pdf_original_source(
    path: &Path,
    source: Arc<dyn BookSource>,
) -> Arc<PdfModeSource> {
    let path = path.to_owned();
    let book_id = source.book().id.to_string();
    Arc::new(PdfModeSource::new(
        source.book().clone(),
        source.table_of_contents_origin(),
        Some(source),
        move || {
            rebook_formats::open_file_for_reading(&path, Some(&book_id))
                .map(|publication| publication.source())
                .map_err(|error| error.to_string())
        },
    ))
}

/// Called on the book-opening worker. Legacy results are upgraded once and the
/// temporary PDF source is dropped before returning the lightweight descriptor.
pub(crate) fn open_cached_pdf_ocr_original(
    path: &Path,
    book_id: &str,
) -> io::Result<Option<Arc<PdfModeSource>>> {
    if let Some(source) = super::super::pdf_native::cached_original(path, book_id)? {
        return Ok(Some(source));
    }
    let Some(mut document) = load_document(book_id)? else {
        return Ok(None);
    };
    let mode = load_pdf_ocr_view_mode(book_id, document.view_mode)?;
    let mut loaded = None;
    if !original_resources_ready(&document)? {
        let publication =
            rebook_formats::open_file_for_reading(path, Some(book_id)).map_err(io::Error::other)?;
        document = cache_original_resources(book_id, publication.source().as_ref(), document)?;
        if mode == PdfOcrViewMode::Original {
            loaded = Some(publication.source());
        }
    }
    let book = document.original_book.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "OCR original descriptor is missing",
        )
    })?;
    let path = path.to_owned();
    let book_id = book_id.to_owned();
    Ok(Some(Arc::new(PdfModeSource::new(
        book,
        document.original_toc_origin,
        loaded,
        move || {
            rebook_formats::open_file_for_reading(&path, Some(&book_id))
                .map(|publication| publication.source())
                .map_err(|error| error.to_string())
        },
    ))))
}

fn special_page_href(physical_page: usize) -> String {
    format!("OcrResources/pdf-original-page-{physical_page:05}.png")
}

fn original_resources_ready(document: &StoredPdfOcrDocument) -> io::Result<bool> {
    let Some(book) = &document.original_book else {
        return Ok(false);
    };
    if book.id.as_str() != document.book_id
        || book.metadata.layout != RenditionLayout::PrePaginated
        || book.sections.len() != document.pages.len()
    {
        return Ok(false);
    }
    let directory = book_directory(&document.book_id)?.join("resources");
    for assignment in &document.page_roles {
        let href = special_page_href(assignment.physical_page);
        let Some(resource) = document
            .resources
            .iter()
            .find(|resource| resource.href == href)
        else {
            return Ok(false);
        };
        validate_sync_resource_name(&resource.file_name)?;
        if !directory.join(&resource.file_name).try_exists()? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn cache_original_resources(
    book_id: &str,
    original: &dyn BookSource,
    document: StoredPdfOcrDocument,
) -> io::Result<StoredPdfOcrDocument> {
    if original_resources_ready(&document)? {
        return Ok(document);
    }
    let tasks = PDF_OCR_TASKS.get_or_init(|| Mutex::new(HashMap::new()));
    let _tasks = tasks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Re-read under the same lock as cloud imports and OCR completion.
    let mut document = load_document(book_id)?.unwrap_or(document);
    document.original_book = Some(original.book().clone());
    document.original_toc_origin = original.table_of_contents_origin();
    store_special_page_images(
        &book_directory(book_id)?.join("resources"),
        original,
        &mut document,
    )?;
    write_json_atomic(&document_path(book_id)?, &document)?;
    crate::sync::mark_derived_dirty(book_id, crate::sync::DerivedDataKind::Ocr)?;
    Ok(document)
}

pub(super) fn store_special_page_images(
    directory: &Path,
    original: &dyn BookSource,
    document: &mut StoredPdfOcrDocument,
) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    for assignment in &document.page_roles {
        let page = assignment.physical_page;
        if page == 0 || page > original.book().sections.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "OCR special page is out of range",
            ));
        }
        let href = special_page_href(page);
        if let Some(resource) = document
            .resources
            .iter()
            .find(|resource| resource.href == href)
        {
            validate_sync_resource_name(&resource.file_name)?;
            if directory.join(&resource.file_name).try_exists()? {
                continue;
            }
        }
        // Use the original renderer's bounded 2048px page image, without
        // extracting a text layer or keeping its decoded raster in OCR memory.
        let page_href = PublicationUrl::parse(&format!("Pages/page-{page:05}.png"))
            .map_err(io::Error::other)?;
        let image = original.resource(&page_href).map_err(io::Error::other)?;
        let file_name = format!("pdf-original-page-{page:05}.png");
        write_bytes_atomic(&directory.join(&file_name), image.bytes.as_ref())?;
        document.resources.retain(|resource| resource.href != href);
        document.resources.push(StoredOcrResource {
            href,
            file_name,
            media_type: "image/png".into(),
        });
    }
    let active = document
        .page_roles
        .iter()
        .map(|assignment| special_page_href(assignment.physical_page))
        .collect::<BTreeSet<_>>();
    document.resources.retain(|resource| {
        !resource.href.starts_with("OcrResources/pdf-original-page-")
            || active.contains(&resource.href)
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    struct Fixture {
        id: String,
        directory: PathBuf,
        pdf: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let id = format!("ocr-original-{label}-{}", uuid::Uuid::new_v4());
            let directory = book_directory(&id).unwrap();
            fs::create_dir_all(&directory).unwrap();
            let pdf = directory.join("original.pdf");
            fs::write(&pdf, minimal_pdf()).unwrap();
            Self { id, directory, pdf }
        }

        fn save(&self, special: bool) {
            let source = rebook_formats::open_file_for_reading(&self.pdf, Some(&self.id))
                .unwrap()
                .source();
            if special {
                save_pdf_ocr_page_roles(
                    &self.id,
                    &[PdfOcrPageRoleAssignment {
                        physical_page: 1,
                        role: PdfOcrPageRole::Cover,
                    }],
                    source.as_ref(),
                )
                .unwrap();
            }
            save_document_with_source(
                &self.id,
                ParsedOcrDocument {
                    provider: PdfOcrProviderKind::PaddleOcr,
                    model: "test".into(),
                    pages: vec![StoredOcrPage {
                        markdown: "Cached OCR body.".into(),
                        ..Default::default()
                    }],
                    resources: Vec::new(),
                },
                Some(source),
            )
            .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    #[test]
    fn eagerly_opened_pdf_can_be_retired_and_reloaded() {
        let fixture = Fixture::new("eager-reload");
        fixture.save(false);
        let source = rebook_formats::open_file_for_reading(&fixture.pdf, Some(&fixture.id))
            .unwrap()
            .source();
        let weak = Arc::downgrade(&source);
        let cache = cache_pdf_original_source(&fixture.pdf, source);
        let loaded = load_pdf_ocr_source(cache.clone(), false, Some(cache.clone())).unwrap();
        assert!(
            weak.upgrade().is_none(),
            "inactive eagerly loaded PDF remained resident"
        );
        let controller = loaded.controller.unwrap();
        controller.prepare_mode(PdfOcrViewMode::Original).unwrap();
        controller.set_mode(PdfOcrViewMode::Original);
        drop(controller.retire_inactive());
        assert!(controller.parse_section(0).is_ok());
    }

    #[test]
    fn cached_text_and_special_images_work_without_reading_the_pdf() {
        for special in [false, true] {
            let fixture = Fixture::new(if special { "cover" } else { "body" });
            fixture.save(special);
            let bytes = fs::read(&fixture.pdf).unwrap();
            fs::remove_file(&fixture.pdf).unwrap();
            let source = open_cached_pdf_ocr_original(&fixture.pdf, &fixture.id)
                .unwrap()
                .unwrap();
            let loaded = load_pdf_ocr_source(source.clone(), false, Some(source)).unwrap();
            let parsed = loaded.source.parse_section(0).unwrap();
            if special {
                let image = parsed
                    .blocks
                    .iter()
                    .find_map(|block| match block {
                        rebook_publication::Block::Image(image) => Some(image),
                        _ => None,
                    })
                    .unwrap();
                assert!(
                    loaded
                        .source
                        .resource(&image.href)
                        .unwrap()
                        .bytes
                        .starts_with(b"\x89PNG")
                );
            } else {
                assert!(parsed.blocks.iter().any(|block| matches!(block,
                    rebook_publication::Block::Text(text) if crate::plugins::text_block_text(text).contains("Cached OCR body"))));
                let store = crate::sync::SyncStore::open_at(
                    fixture.directory.join("reader-test.sqlite3"),
                    "ocr-reader-test",
                )
                .unwrap();
                let reader = crate::reader::open_reader(
                    &fixture.pdf,
                    crate::fonts::embedded_reader_fonts(),
                    Some(crate::reader::BookDisplayMetadata {
                        id: fixture.id.clone(),
                        title: "Cached OCR test".into(),
                        authors: Vec::new(),
                    }),
                    None,
                    store,
                )
                .unwrap();
                assert_eq!(
                    reader.progress_locator().publication_id.as_str(),
                    fixture.id
                );
                drop(reader);
            }
            // Unknown OCR resources cannot fall through and open the PDF.
            assert!(
                loaded
                    .source
                    .resource(&PublicationUrl::parse("Pages/page-00001.png").unwrap())
                    .is_err()
            );
            fs::write(&fixture.pdf, bytes).unwrap();
            let controller = loaded.controller.unwrap();
            assert!(
                controller
                    .prepare_mode(PdfOcrViewMode::Original)
                    .unwrap()
                    .parse_section(0)
                    .is_ok()
            );
            controller.set_mode(PdfOcrViewMode::Original);
            assert_eq!(
                controller.book().metadata.layout,
                RenditionLayout::PrePaginated
            );
            assert!(controller.parse_section(0).is_ok());
            controller.prepare_mode(PdfOcrViewMode::Reflow).unwrap();
            controller.set_mode(PdfOcrViewMode::Reflow);
            drop(controller.retire_inactive());
            assert!(controller.parse_section(0).is_ok());
        }
    }

    #[test]
    fn legacy_results_upgrade_once_and_special_images_sync_with_the_document() {
        let fixture = Fixture::new("legacy");
        fixture.save(true);
        let mut document = load_document(&fixture.id).unwrap().unwrap();
        document.original_book = None;
        document.resources.clear();
        fs::remove_file(
            fixture
                .directory
                .join("resources/pdf-original-page-00001.png"),
        )
        .unwrap();
        write_json_atomic(&document_path(&fixture.id).unwrap(), &document).unwrap();
        let source = open_cached_pdf_ocr_original(&fixture.pdf, &fixture.id)
            .unwrap()
            .unwrap();
        let repaired = load_document(&fixture.id).unwrap().unwrap();
        assert!(original_resources_ready(&repaired).unwrap());
        fs::remove_file(&fixture.pdf).unwrap();
        assert!(
            load_pdf_ocr_source(source.clone(), false, Some(source))
                .unwrap()
                .source
                .parse_section(0)
                .is_ok()
        );
        let exported = export_pdf_ocr_sync_data(&fixture.id).unwrap().unwrap();
        assert_eq!(exported.resources.len(), 1);
        assert!(exported.resources[0].1.starts_with(b"\x89PNG"));
        let receiver = Fixture::new("synced");
        let mut synced: StoredPdfOcrDocument = serde_json::from_slice(&exported.document).unwrap();
        synced.book_id.clone_from(&receiver.id);
        synced.original_book.as_mut().unwrap().id =
            rebook_publication::PublicationId::new(&receiver.id).unwrap();
        import_pdf_ocr_sync_data(
            &receiver.id,
            PdfOcrSyncData {
                document: serde_json::to_vec(&synced).unwrap(),
                resources: exported.resources,
            },
        )
        .unwrap();
        set_pdf_ocr_view_mode(&receiver.id, PdfOcrViewMode::Reflow).unwrap();
        fs::remove_file(&receiver.pdf).unwrap();
        let source = open_cached_pdf_ocr_original(&receiver.pdf, &receiver.id)
            .unwrap()
            .unwrap();
        let loaded = load_pdf_ocr_source(source.clone(), false, Some(source)).unwrap();
        assert!(loaded.source.parse_section(0).is_ok());
    }

    #[test]
    fn changing_page_roles_adds_and_removes_derived_images() {
        let fixture = Fixture::new("roles");
        fixture.save(false);
        let original = rebook_formats::open_file_for_reading(&fixture.pdf, Some(&fixture.id))
            .unwrap()
            .source();
        save_pdf_ocr_page_roles(
            &fixture.id,
            &[PdfOcrPageRoleAssignment {
                physical_page: 1,
                role: PdfOcrPageRole::TitlePage,
            }],
            original.as_ref(),
        )
        .unwrap();
        assert_eq!(
            export_pdf_ocr_sync_data(&fixture.id)
                .unwrap()
                .unwrap()
                .resources
                .len(),
            1
        );
        save_pdf_ocr_page_roles(&fixture.id, &[], original.as_ref()).unwrap();
        assert!(
            export_pdf_ocr_sync_data(&fixture.id)
                .unwrap()
                .unwrap()
                .resources
                .is_empty()
        );
        set_pdf_ocr_view_mode(&fixture.id, PdfOcrViewMode::Original).unwrap();
        assert!(
            open_cached_pdf_ocr_original(&fixture.pdf, &fixture.id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn both_modes_release_their_payload_and_reload_from_disk() {
        for initial_mode in [PdfOcrViewMode::Original, PdfOcrViewMode::Reflow] {
            let fixture = Fixture::new("switch-release");
            fixture.save(true);
            set_pdf_ocr_view_mode(&fixture.id, initial_mode).unwrap();
            let original = open_cached_pdf_ocr_original(&fixture.pdf, &fixture.id)
                .unwrap()
                .unwrap();
            let loaded =
                load_pdf_ocr_source(original.clone(), false, Some(original.clone())).unwrap();
            let controller = loaded.controller.unwrap();
            for mode in [
                PdfOcrViewMode::Original,
                PdfOcrViewMode::Reflow,
                PdfOcrViewMode::Original,
                PdfOcrViewMode::Reflow,
            ] {
                controller.prepare_mode(mode).unwrap();
                controller.set_mode(mode);
                let original_weak = if mode == PdfOcrViewMode::Original {
                    Some(Arc::downgrade(&original.prepare().unwrap()))
                } else {
                    None
                };
                let ocr_weak = if mode == PdfOcrViewMode::Reflow {
                    Some(Arc::downgrade(
                        &controller.reflow_cache.as_ref().unwrap().prepare().unwrap(),
                    ))
                } else {
                    None
                };
                assert!(controller.parse_section(0).is_ok());
                let next = if mode == PdfOcrViewMode::Original {
                    PdfOcrViewMode::Reflow
                } else {
                    PdfOcrViewMode::Original
                };
                controller.prepare_mode(next).unwrap();
                controller.set_mode(next);
                drop(controller.retire_inactive());
                if let Some(weak) = original_weak {
                    assert!(weak.upgrade().is_none(), "PDF remained resident");
                }
                if let Some(weak) = ocr_weak {
                    assert!(weak.upgrade().is_none(), "OCR remained resident");
                }
            }
        }
    }

    fn minimal_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 120 160] /Contents 4 0 R >>",
            "<< /Length 0 >>\nstream\n\nendstream",
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            write!(&mut bytes, "{} 0 obj\n{object}\nendobj\n", index + 1).unwrap();
        }
        let xref = bytes.len();
        write!(&mut bytes, "xref\n0 5\n0000000000 65535 f \n").unwrap();
        for offset in offsets {
            writeln!(&mut bytes, "{offset:010} 00000 n ").unwrap();
        }
        write!(
            &mut bytes,
            "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .unwrap();
        bytes
    }
}
