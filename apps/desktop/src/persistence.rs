pub(crate) use rebook_app_core::persistence::*;

#[cfg(test)]
mod tests {
    use std::fs;
    use sha2::{Digest, Sha256};
    use super::*;

    #[test]
    fn book_paths_preserve_legacy_names_and_hash_unsafe_identities() {
        for book_id in ["book-1_2", "../book", "中文书籍", ""] {
            let safe = safe_book_id(book_id);
            let path = book_data_path("generated-toc", book_id, "json").unwrap();
            assert_eq!(
                path.file_name().unwrap().to_str().unwrap(),
                format!("{safe}.json")
            );
            assert_eq!(path.parent().unwrap().file_name().unwrap(), "generated-toc");
            if book_id == "../book" || book_id == "中文书籍" {
                assert_eq!(safe, format!("{:x}", Sha256::digest(book_id.as_bytes())));
            } else {
                assert_eq!(safe, book_id);
            }
        }
    }

    #[test]
    fn atomic_write_replaces_existing_contents() {
        let path = std::env::temp_dir().join(format!(
            "rebook-atomic-write-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        write_bytes_atomic(&path, b"old").unwrap();
        write_bytes_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_file(path).unwrap();
    }
}
