use super::*;
use rebook_publication::{Block, Inline, TableOfContentsOrigin};
use std::io::Write;

const OPF: &str = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="uid">urn:uuid:00112233-4455-6677-8899-aabbccddeeff</dc:identifier><dc:title>Primary title</dc:title><dc:language>en</dc:language></metadata><manifest><item id="body" href="body.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="ncx"><itemref idref="body"/></spine></package>"#;
const BODY: &str = "<html><body><h1 id='one'>Chapter</h1><p>Readable content.</p></body></html>";
const NAV: &str = "<html xmlns:epub='http://www.idpf.org/2007/ops'><body><nav epub:type='toc'><ol><li><a href='body.xhtml#one'>EPUB 3 chapter</a></li></ol></nav></body></html>";
const NCX: &str = "<ncx><navMap><navPoint><navLabel><text>NCX chapter</text></navLabel><content src='body.xhtml#one'/></navPoint></navMap></ncx>";

fn archive(opf: &str, body: &str, nav: Option<&str>, extras: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut put = |name: &str, bytes: &[u8]| {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(bytes).unwrap();
    };
    put("mimetype", b"application/epub+zip");
    put(
        "META-INF/container.xml",
        b"<container><rootfiles><rootfile full-path='book.opf'/></rootfiles></container>",
    );
    put("book.opf", opf.as_bytes());
    put("body.xhtml", body.as_bytes());
    if let Some(nav) = nav {
        put("nav.xhtml", nav.as_bytes());
    }
    put("toc.ncx", NCX.as_bytes());
    for (name, bytes) in extras {
        put(name, bytes);
    }
    zip.finish().unwrap().into_inner()
}
fn png() -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[test]
fn broken_missing_and_unlinked_nav_fall_back_to_ncx_without_losing_prose() {
    for nav in [
        None,
        Some("<html><body><nav>"),
        Some(
            "<html xmlns:epub='http://www.idpf.org/2007/ops'><nav epub:type='toc'><ol><li><span>Only text</span></li></ol></nav></html>",
        ),
    ] {
        let source = EpubPublication::open_bytes(archive(OPF, BODY, nav, &[])).unwrap();
        assert_eq!(source.book().table_of_contents[0].label, "NCX chapter");
        assert_eq!(
            source.table_of_contents_origin(),
            TableOfContentsOrigin::Embedded
        );
        assert_eq!(source.parse_section(0).unwrap().blocks.len(), 2);
    }
    let valid = EpubPublication::open_bytes(archive(OPF, BODY, Some(NAV), &[])).unwrap();
    assert_eq!(valid.book().table_of_contents[0].label, "EPUB 3 chapter");
}

#[test]
fn fallback_navigation_keeps_spine_order_and_skips_nonlinear_documents() {
    let opf = OPF.replace("application/x-dtbncx+xml", "application/octet-stream").replace(" toc=\"ncx\"", "")
        .replace("</manifest>", "<item id='two' href='two.xhtml' media-type='application/xhtml+xml'/><item id='note' href='note.xhtml' media-type='application/xhtml+xml'/></manifest>")
        .replace("</spine>", "<itemref idref='note' linear='no'/><itemref idref='two'/></spine>");
    let source = EpubPublication::open_bytes(archive(
        &opf,
        BODY,
        None,
        &[
            ("two.xhtml", BODY.as_bytes()),
            ("note.xhtml", BODY.as_bytes()),
        ],
    ))
    .unwrap();
    assert_eq!(
        source.table_of_contents_origin(),
        TableOfContentsOrigin::Fallback
    );
    let paths = source
        .book()
        .table_of_contents
        .iter()
        .map(|t| t.href.as_ref().unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["body.xhtml", "two.xhtml"]);
}

#[test]
fn fallback_chain_and_noncanonical_media_types_keep_readable_sections() {
    let opf = OPF.replace(
        "application/xhtml+xml",
        "Application/XHTML+XML; charset=utf-8",
    );
    let source = EpubPublication::open_bytes(archive(&opf, BODY, Some(NAV), &[])).unwrap();
    assert_eq!(source.parse_section(0).unwrap().blocks.len(), 2);
    let opf = OPF.replace("</manifest>", "<item id='foreign' href='foreign.bin' media-type='application/x-foreign' fallback='body'/></manifest>")
        .replace("idref=\"body\"", "idref=\"foreign\"");
    let source = EpubPublication::open_bytes(archive(&opf, BODY, Some(NAV), &[])).unwrap();
    assert_eq!(source.book.sections[0].id.as_str(), "foreign");
    assert_eq!(source.parse_section(0).unwrap().blocks.len(), 2);
    let cyclic = opf.replace("fallback='body'", "fallback='foreign'");
    assert!(EpubPublication::open_bytes(archive(&cyclic, BODY, Some(NAV), &[])).is_err());
}

#[test]
fn cover_candidates_accept_legacy_hrefs_and_literal_archive_names() {
    let image = png();
    for (name, reference) in [
        ("Images/cover.png", "Images/cover.png"),
        ("Images/cover%20literal.png", "Images/cover%2520literal.png"),
        ("Images/cover#1.png", "Images/cover%231.png"),
    ] {
        let opf = OPF.replace(
            "</metadata>",
            &format!("<meta name='cover' content='{reference}'/></metadata>"),
        );
        let opened = crate::open_bytes(
            archive(&opf, BODY, Some(NAV), &[(name, &image)]),
            "cover.epub",
        )
        .unwrap();
        assert_eq!(opened.cover_bytes(), Some(image.as_slice()), "{name}");
    }
    let opened = crate::open_bytes(
        archive(OPF, BODY, Some(NAV), &[("Images/cover.png", &image)]),
        "cover.epub",
    )
    .unwrap();
    assert_eq!(opened.cover_bytes(), Some(image.as_slice()));
    let opened = crate::open_bytes(
        archive(OPF, BODY, Some(NAV), &[("Images/chart.png", &image)]),
        "cover.epub",
    )
    .unwrap();
    assert!(
        opened.cover_bytes().is_none(),
        "an arbitrary illustration is not a cover"
    );
}

#[test]
fn broken_declared_cover_tries_the_next_candidate() {
    let image = png();
    let opf = OPF.replace("</manifest>", "<item id='broken' href='broken.png' media-type='image/png' properties='cover-image'/></manifest>");
    let opened = crate::open_bytes(
        archive(
            &opf,
            BODY,
            Some(NAV),
            &[
                ("broken.png", b"not an image"),
                ("Images/cover.png", &image),
            ],
        ),
        "cover.epub",
    )
    .unwrap();
    assert_eq!(opened.cover_bytes(), Some(image.as_slice()));
}

#[test]
fn svg_spine_and_svg_cover_are_renderable_and_cover_is_rasterized() {
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg' width='300' height='400'><rect width='300' height='400' fill='green'/></svg>";
    let opf = OPF.replace(
        "href=\"body.xhtml\" media-type=\"application/xhtml+xml\"",
        "href='cover.svg' media-type='image/svg+xml' properties='cover-image'",
    );
    let opened = crate::open_bytes(
        archive(&opf, BODY, Some(NAV), &[("cover.svg", svg)]),
        "svg.epub",
    )
    .unwrap();
    assert!(matches!(
        opened.source().parse_section(0).unwrap().blocks.as_slice(),
        [Block::Image(_)]
    ));
    let cover = image::load_from_memory(opened.cover_bytes().unwrap()).unwrap();
    assert_eq!((cover.width(), cover.height()), (420, 600));
}

#[test]
fn svg_metadata_and_pixels_share_archive_image_resolution_and_auto_geometry() {
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><image href='photo.png' x='10' y='20' width='120' height='80'/></svg>";
    let image = png();
    let opf = OPF.replace(
        "</manifest>",
        "<item id='svg' href='cover.svg' media-type='image/svg+xml'/></manifest>",
    );
    let source = EpubPublication::open_bytes(archive(
        &opf,
        BODY,
        Some(NAV),
        &[("cover.svg", svg), ("photo.png", &image)],
    ))
    .unwrap();
    let href = PublicationUrl::parse("cover.svg").unwrap();
    let dims = source.raster_resource_dimensions(&href).unwrap().unwrap();
    let raster = source.raster_resource(&href).unwrap().unwrap();
    assert_eq!(dims, [raster.width, raster.height]);
    assert!(raster.pixels.chunks_exact(4).any(|p| p[3] != 0));
}

#[test]
fn metadata_retains_roles_details_and_the_original_primary_title() {
    let opf = OPF.replace("</metadata>", "<dc:title xmlns:dc='http://purl.org/dc/elements/1.1/' id='alt'>Alternate main</dc:title><meta property='title-type' refines='#alt'>main</meta><dc:creator xmlns:dc='http://purl.org/dc/elements/1.1/' id='a'>Author</dc:creator><dc:creator xmlns:dc='http://purl.org/dc/elements/1.1/' id='t'>Translator</dc:creator><meta refines='#a' property='role'>aut</meta><meta refines='#t' property='role'>trl</meta><dc:publisher xmlns:dc='http://purl.org/dc/elements/1.1/'>Publisher</dc:publisher><meta name='calibre:series' content='Series'/><meta name='calibre:series_index' content='2.1'/></metadata>");
    let source = EpubPublication::open_bytes(archive(&opf, BODY, Some(NAV), &[])).unwrap();
    let metadata = &source.book.metadata;
    assert_eq!(metadata.title, "Primary title");
    assert_eq!(metadata.authors, ["Author"]);
    assert_eq!(metadata.details.contributors[1].roles, ["trl"]);
    assert_eq!(metadata.details.titles[1].kind.as_deref(), Some("main"));
    assert_eq!(metadata.details.publisher.as_deref(), Some("Publisher"));
    assert_eq!(metadata.details.series_position.as_deref(), Some("2.1"));
}

#[test]
fn deobfuscated_font_is_shared_by_resource_loading_and_intrinsic_style_detection() {
    let mut font = vec![0; 106];
    font[..4].copy_from_slice(&0x00010000u32.to_be_bytes());
    font[4..6].copy_from_slice(&1u16.to_be_bytes());
    font[12..16].copy_from_slice(b"OS/2");
    font[20..24].copy_from_slice(&28u32.to_be_bytes());
    font[24..28].copy_from_slice(&78u32.to_be_bytes());
    font[32..34].copy_from_slice(&700u16.to_be_bytes());
    let id = "urn:uuid:00112233-4455-6677-8899-aabbccddeeff";
    for (algorithm, decoder) in [
        (
            "http://www.idpf.org/2008/embedding",
            crate::font_data::Decoder::idpf(id),
        ),
        (
            "http://ns.adobe.com/pdf/enc#RC",
            crate::font_data::Decoder::adobe(id).unwrap(),
        ),
    ] {
        let mut encoded = font.clone();
        decoder.apply(&mut encoded);
        let enc = format!(
            "<encryption><EncryptedData><EncryptionMethod Algorithm='{algorithm}'/><CipherData><CipherReference URI='font.ttf'/></CipherData></EncryptedData></encryption>"
        );
        let body = "<html><head><style>@font-face {font-family:Book; src:url('font.ttf')} p {font-family:Book}</style></head><body><p>Bold font.</p></body></html>";
        let source = EpubPublication::open_bytes(archive(
            OPF,
            body,
            Some(NAV),
            &[
                ("font.ttf", &encoded),
                ("META-INF/encryption.xml", enc.as_bytes()),
            ],
        ))
        .unwrap();
        assert_eq!(
            source
                .resource(&PublicationUrl::parse("font.ttf").unwrap())
                .unwrap()
                .bytes
                .as_ref(),
            font
        );
        let section = source.parse_section(0).unwrap();
        let Block::Text(text) = &section.blocks[0] else {
            panic!("text block")
        };
        assert!(
            text.content
                .iter()
                .any(|i| matches!(i, Inline::Text(r) if r.style.bold))
        );
        let warm = EpubPublication::from_reading_index(
            source.archive.bytes.clone(),
            source.reading_index(),
        )
        .unwrap();
        assert_eq!(
            warm.resource(&PublicationUrl::parse("font.ttf").unwrap())
                .unwrap()
                .bytes
                .as_ref(),
            font
        );
    }
}

#[test]
fn auxiliary_navigation_survives_descriptor_cache_without_becoming_chapter_toc() {
    let opf = OPF.replace(
        "<spine toc=\"ncx\">",
        "<spine toc='ncx' page-progression-direction='rtl'>",
    );
    let nav = NAV.replace("</body>", "<nav epub:type='page-list'><ol><li><a href='body.xhtml#one'>12</a></li></ol></nav><nav epub:type='landmarks'><ol><li><a epub:type='bodymatter' href='body.xhtml'>Start</a></li></ol></nav></body>");
    let source = EpubPublication::open_bytes(archive(&opf, BODY, Some(&nav), &[])).unwrap();
    assert_eq!(source.book.table_of_contents.len(), 1);
    let meta = source.navigation_metadata().unwrap();
    assert_eq!(meta.page_list[0].label, "12");
    assert_eq!(meta.landmarks[0].kinds, ["bodymatter"]);
    assert_eq!(
        meta.page_progression,
        Some(rebook_publication::TextDirection::Rtl)
    );
    let warm =
        EpubPublication::from_reading_index(source.archive.bytes.clone(), source.reading_index())
            .unwrap();
    assert_eq!(warm.navigation_metadata(), Some(meta));
}

#[test]
fn usable_nonspine_targets_are_retained_as_nonlinear_and_fallback_aliases_resolve() {
    let opf = OPF.replace("</manifest>", "<item id='extra' href='extra.xhtml' media-type='application/xhtml+xml'/><item id='foreign' href='foreign.bin' media-type='application/x-foreign' fallback='body'/></manifest>");
    let nav = NAV
        .replace("body.xhtml#one", "foreign.bin#one")
        .replace("</ol>", "<li><a href='extra.xhtml'>Extra</a></li></ol>");
    let source = EpubPublication::open_bytes(archive(
        &opf,
        BODY,
        Some(&nav),
        &[("extra.xhtml", BODY.as_bytes())],
    ))
    .unwrap();
    assert_eq!(source.book.sections.len(), 2);
    assert!(!source.book.sections[1].linear);
    assert_eq!(
        source.book.table_of_contents[0]
            .href
            .as_ref()
            .unwrap()
            .to_string(),
        "body.xhtml#one"
    );
    assert_eq!(source.parse_section(1).unwrap().blocks.len(), 2);
}
