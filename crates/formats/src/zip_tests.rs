//! Exercise parser entry points with archives whose two headers disagree.
use std::io::{Cursor, Write};

use zip::{ZipWriter, write::SimpleFileOptions};

pub(crate) fn zip_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, body) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(body).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

pub(crate) fn central_header_mut<'a>(bytes: &'a mut [u8], name: &str) -> &'a mut [u8] {
    let offset = bytes
        .windows(4)
        .enumerate()
        .find_map(|(offset, magic)| {
            (magic == b"PK\x01\x02"
                && bytes.get(offset + 46..offset + 46 + name.len()) == Some(name.as_bytes()))
            .then_some(offset)
        })
        .expect("central directory entry");
    &mut bytes[offset..offset + 46 + name.len()]
}

#[test]
fn cbz_rejects_a_page_renamed_only_in_the_central_directory() {
    let mut bytes = zip_entries(&[("page1.jpg", b"image")]);
    assert!(crate::cbz::open(&bytes, "book.cbz").is_ok());
    central_header_mut(&mut bytes, "page1.jpg")[46..].copy_from_slice(b"page2.jpg");
    assert!(crate::cbz::open(&bytes, "book.cbz").is_err());
}

#[test]
fn fbz_does_not_skip_an_inconsistent_entry_and_open_another_book() {
    let xml = b"<FictionBook><body><section><p>Text</p></section></body></FictionBook>";
    let mut bytes = zip_entries(&[("book1.fb2", xml), ("book2.fb2", xml)]);
    assert!(crate::fb2::open(&bytes, "book.fbz").is_ok());
    central_header_mut(&mut bytes, "book1.fb2")[46..].copy_from_slice(b"book0.fb2");
    assert!(crate::fb2::open(&bytes, "book.fbz").is_err());
}
