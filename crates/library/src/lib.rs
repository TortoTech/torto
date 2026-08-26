//! UI-independent local library storage shared by Torto desktop frontends.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use atomicwrites::{AllowOverwrite, AtomicFile};
use directories::ProjectDirs;
use rebook_formats::open_bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const LIBRARY_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "library.json";
const BOOKS_DIRECTORY: &str = "books";
const COVERS_DIRECTORY: &str = "covers";

pub type LibraryResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// One managed publication shown by any desktop frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryBook {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub file_name: String,
    pub path: PathBuf,
    pub cover_bytes: Option<Vec<u8>>,
    pub added_at: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub imported: usize,
    pub duplicates: usize,
}

/// Fully materialized book downloaded by the sync layer.
#[derive(Clone, Debug)]
pub struct RemoteLibraryBook {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub file_name: String,
    pub content_sha256: String,
    pub added_at: u64,
    pub content: Vec<u8>,
    pub cover: Option<Vec<u8>>,
}

/// Transactional local library model with no UI-toolkit dependencies.
pub struct LocalLibrary {
    root: PathBuf,
    books: Vec<LibraryBook>,
}

#[derive(Serialize, Deserialize)]
struct StoredLibrary {
    version: u32,
    #[serde(default)]
    books: Vec<StoredBook>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredBook {
    id: String,
    title: String,
    #[serde(default)]
    authors: Vec<String>,
    file_name: String,
    storage_name: String,
    cover_name: Option<String>,
    added_at: u64,
}

impl LocalLibrary {
    /// Opens the production library directory used by every Torto desktop UI.
    pub fn load_default() -> LibraryResult<Self> {
        let project = ProjectDirs::from("com", "Rebook", "Rebook")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法确定本地书架数据目录"))?;
        Self::load_from(project.data_local_dir().join("library"))
    }

    /// Opens a library rooted at an explicit directory.
    ///
    /// This is useful to embedders and tests that must not touch production
    /// application data.
    pub fn load_from(root: PathBuf) -> LibraryResult<Self> {
        fs::create_dir_all(root.join(BOOKS_DIRECTORY))?;
        fs::create_dir_all(root.join(COVERS_DIRECTORY))?;
        let manifest_path = root.join(MANIFEST_FILE);
        let stored = if manifest_path.exists() {
            let stored: StoredLibrary = serde_json::from_slice(&fs::read(&manifest_path)?)?;
            if stored.version != LIBRARY_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("不支持的本地书架版本：{}", stored.version),
                )
                .into());
            }
            stored
        } else {
            StoredLibrary {
                version: LIBRARY_VERSION,
                books: Vec::new(),
            }
        };

        let mut books = stored
            .books
            .into_iter()
            .map(|book| {
                let cover_bytes = book
                    .cover_name
                    .as_ref()
                    .and_then(|name| fs::read(root.join(COVERS_DIRECTORY).join(name)).ok());
                LibraryBook {
                    id: book.id,
                    title: book.title,
                    authors: book.authors,
                    file_name: book.file_name,
                    path: root.join(BOOKS_DIRECTORY).join(book.storage_name),
                    cover_bytes,
                    added_at: book.added_at,
                }
            })
            .collect::<Vec<_>>();
        books.sort_by_key(|book| std::cmp::Reverse(book.added_at));
        Ok(Self { root, books })
    }

    pub fn books(&self) -> &[LibraryBook] {
        &self.books
    }

    pub fn import_files(&mut self, paths: &[PathBuf]) -> LibraryResult<ImportSummary> {
        let mut summary = ImportSummary::default();
        for path in paths {
            if self.import_file(path)? {
                summary.imported += 1;
            } else {
                summary.duplicates += 1;
            }
        }
        Ok(summary)
    }

    pub fn import_for_open(&mut self, source_path: &Path) -> LibraryResult<(LibraryBook, bool)> {
        if let Some(book) = self.books.iter().find(|book| book.path == source_path) {
            return Ok((book.clone(), false));
        }

        let bytes = fs::read(source_path)?;
        let id = format!("{:x}", Sha256::digest(&bytes));
        if let Some(book) = self.books.iter().find(|book| book.id == id) {
            return Ok((book.clone(), false));
        }

        let file_name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "书籍文件名不是有效 Unicode")
            })?
            .to_owned();
        let publication = open_bytes(bytes.clone(), &file_name)?;
        let metadata = &publication.book().metadata;
        let title = if metadata.title.trim().is_empty() {
            title_from_file_name(source_path)
        } else {
            metadata.title.trim().to_owned()
        };
        let format = publication.format();
        let storage_name = format!("{id}.{}", format.storage_extension());
        let cover_bytes = publication.cover_bytes().map(<[u8]>::to_vec);
        let cover_name = cover_bytes.as_ref().map(|_| format!("{id}.cover"));
        let book = LibraryBook {
            id,
            title,
            authors: metadata.authors.clone(),
            file_name,
            path: self.root.join(BOOKS_DIRECTORY).join(storage_name),
            cover_bytes,
            added_at: unix_timestamp_millis(),
        };
        self.commit_import(&book, &bytes, cover_name.as_deref())?;
        Ok((book, true))
    }

    pub fn import_remote(&mut self, remote: RemoteLibraryBook) -> LibraryResult<bool> {
        if self.books.iter().any(|book| book.id == remote.id) {
            return Ok(false);
        }
        let digest = format!("{:x}", Sha256::digest(&remote.content));
        if digest != remote.id || digest != remote.content_sha256 {
            return Err(
                io::Error::new(io::ErrorKind::InvalidData, "远端书籍内容哈希与清单不一致").into(),
            );
        }
        let publication = open_bytes(remote.content.clone(), &remote.file_name)?;
        let metadata = &publication.book().metadata;
        let title = if remote.title.trim().is_empty() {
            metadata.title.trim().to_owned()
        } else {
            remote.title.trim().to_owned()
        };
        let authors = if remote.authors.is_empty() {
            metadata.authors.clone()
        } else {
            remote.authors.clone()
        };
        let storage_name = format!("{}.{}", remote.id, publication.format().storage_extension());
        let cover_bytes = remote
            .cover
            .or_else(|| publication.cover_bytes().map(<[u8]>::to_vec));
        let cover_name = cover_bytes.as_ref().map(|_| format!("{}.cover", remote.id));
        let book = LibraryBook {
            id: remote.id,
            title,
            authors,
            file_name: remote.file_name,
            path: self.root.join(BOOKS_DIRECTORY).join(storage_name),
            cover_bytes,
            added_at: remote.added_at,
        };
        self.commit_import(&book, &remote.content, cover_name.as_deref())?;
        Ok(true)
    }

    fn import_file(&mut self, source_path: &Path) -> LibraryResult<bool> {
        self.import_for_open(source_path)
            .map(|(_, imported)| imported)
    }

    pub fn remove(&mut self, id: &str) -> LibraryResult<bool> {
        let Some(index) = self.books.iter().position(|book| book.id == id) else {
            return Ok(false);
        };
        let book = self.books[index].clone();
        let mut books = self.books.clone();
        books.remove(index);
        self.persist_books(&books)?;
        self.books = books;
        self.cleanup_book_files(&book);
        Ok(true)
    }

    pub fn update_metadata(
        &mut self,
        id: &str,
        title: &str,
        authors: &[String],
    ) -> LibraryResult<bool> {
        let Some(index) = self.books.iter().position(|book| book.id == id) else {
            return Ok(false);
        };
        let mut books = self.books.clone();
        let book = &mut books[index];
        let title = title.trim();
        let mut normalized_authors = authors
            .iter()
            .map(|author| author.trim().to_owned())
            .filter(|author| !author.is_empty())
            .collect::<Vec<_>>();
        normalized_authors.dedup();
        let mut changed = false;
        if !title.is_empty() && book.title != title {
            title.clone_into(&mut book.title);
            changed = true;
        }
        if !normalized_authors.is_empty() && book.authors != normalized_authors {
            book.authors = normalized_authors;
            changed = true;
        }
        if !changed {
            return Ok(false);
        }
        self.persist_books(&books)?;
        self.books = books;
        Ok(true)
    }

    #[cfg(test)]
    fn persist(&self) -> LibraryResult<()> {
        self.persist_books(&self.books)
    }

    fn persist_books(&self, books: &[LibraryBook]) -> LibraryResult<()> {
        let stored = StoredLibrary {
            version: LIBRARY_VERSION,
            books: books
                .iter()
                .map(|book| StoredBook {
                    id: book.id.clone(),
                    title: book.title.clone(),
                    authors: book.authors.clone(),
                    file_name: book.file_name.clone(),
                    storage_name: book
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_owned(),
                    cover_name: book
                        .cover_bytes
                        .as_ref()
                        .map(|_| format!("{}.cover", book.id)),
                    added_at: book.added_at,
                })
                .collect(),
        };
        write_json_atomic(&self.root.join(MANIFEST_FILE), &stored)?;
        Ok(())
    }

    fn commit_import(
        &mut self,
        book: &LibraryBook,
        content: &[u8],
        cover_name: Option<&str>,
    ) -> LibraryResult<()> {
        write_bytes_atomic(&book.path, content)?;
        let cover_path = cover_name.map(|name| self.root.join(COVERS_DIRECTORY).join(name));
        if let (Some(path), Some(cover)) = (&cover_path, &book.cover_bytes)
            && let Err(error) = write_bytes_atomic(path, cover)
        {
            remove_if_exists_warn(&book.path);
            return Err(error.into());
        }

        let mut books = self.books.clone();
        books.push(book.clone());
        books.sort_by_key(|entry| std::cmp::Reverse(entry.added_at));
        if let Err(error) = self.persist_books(&books) {
            remove_if_exists_warn(&book.path);
            if let Some(path) = &cover_path {
                remove_if_exists_warn(path);
            }
            return Err(error);
        }
        self.books = books;
        Ok(())
    }

    fn cleanup_book_files(&self, book: &LibraryBook) {
        remove_if_exists_warn(&book.path);
        remove_if_exists_warn(
            &self
                .root
                .join(COVERS_DIRECTORY)
                .join(format!("{}.cover", book.id)),
        );
    }
}

fn title_from_file_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("未命名书籍")
        .to_owned()
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn write_json_atomic<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    write_bytes_atomic(path, &bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "持久化路径没有父目录"))?;
    fs::create_dir_all(parent)?;
    AtomicFile::new(path, AllowOverwrite)
        .write(|file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .map_err(io::Error::other)
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_if_exists_warn(path: &Path) {
    if let Err(error) = remove_if_exists(path) {
        tracing::warn!(%error, path = %path.display(), "failed to clean unreferenced library file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    const FIXTURE_ENTRIES: [&str; 5] = [
        "META-INF/container.xml",
        "OPS/package.opf",
        "OPS/nav.xhtml",
        "OPS/Styles/book.css",
        "OPS/Text/chapter.xhtml",
    ];

    #[test]
    fn manifest_round_trip_preserves_book_order_and_metadata() {
        let root = test_directory("manifest-round-trip");
        let mut library = LocalLibrary::load_from(root.clone()).unwrap();
        let managed_path = root.join(BOOKS_DIRECTORY).join("first.epub");
        fs::write(&managed_path, b"fixture").unwrap();
        library.books.push(LibraryBook {
            id: "first".into(),
            title: "第一本书".into(),
            authors: vec!["作者".into()],
            file_name: "source.epub".into(),
            path: managed_path,
            cover_bytes: None,
            added_at: 42,
        });
        library.persist().unwrap();

        let loaded = LocalLibrary::load_from(root.clone()).unwrap();
        assert_eq!(loaded.books.len(), 1);
        assert_eq!(loaded.books[0].title, "第一本书");
        assert_eq!(loaded.books[0].authors, ["作者"]);
        assert_eq!(loaded.books[0].file_name, "source.epub");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_deletes_only_the_managed_book() {
        let root = test_directory("remove-managed-book");
        let mut library = LocalLibrary::load_from(root.clone()).unwrap();
        let managed_path = root.join(BOOKS_DIRECTORY).join("managed.epub");
        fs::write(&managed_path, b"fixture").unwrap();
        library.books.push(LibraryBook {
            id: "managed".into(),
            title: "Managed".into(),
            authors: Vec::new(),
            file_name: "original.epub".into(),
            path: managed_path.clone(),
            cover_bytes: None,
            added_at: 1,
        });
        library.persist().unwrap();

        assert!(library.remove("managed").unwrap());
        assert!(!managed_path.exists());
        assert!(library.books.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn import_extracts_metadata_copies_content_and_skips_duplicates() {
        let root = test_directory("import-epub");
        let source = root.join("source.epub");
        build_fixture(&source);
        let mut library = LocalLibrary::load_from(root.join("data")).unwrap();

        let first = library.import_files(std::slice::from_ref(&source)).unwrap();
        assert_eq!(first.imported, 1);
        assert_eq!(first.duplicates, 0);
        assert_eq!(library.books[0].title, "Rebook 原生渲染样板");
        assert_eq!(library.books[0].authors, ["Rebook"]);
        assert!(library.books[0].path.exists());
        assert_ne!(library.books[0].path, source);
        let publication = open_bytes(fs::read(&source).unwrap(), "source.epub").unwrap();
        assert_eq!(publication.book().id.as_str(), library.books[0].id);

        let second = library.import_files(std::slice::from_ref(&source)).unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.duplicates, 1);
        assert_eq!(library.books.len(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_update_is_persisted_without_empty_replacements() {
        let root = test_directory("metadata-update");
        let mut library = LocalLibrary::load_from(root.clone()).unwrap();
        let managed_path = root.join(BOOKS_DIRECTORY).join("book.pdf");
        fs::write(&managed_path, b"fixture").unwrap();
        library.books.push(LibraryBook {
            id: "book".into(),
            title: "filename".into(),
            authors: Vec::new(),
            file_name: "book.pdf".into(),
            path: managed_path,
            cover_bytes: None,
            added_at: 1,
        });
        library.persist().unwrap();

        assert!(
            library
                .update_metadata("book", "Recognized title", &[" Author ".into()])
                .unwrap()
        );
        assert!(
            !library
                .update_metadata("book", "", &Vec::<String>::new())
                .unwrap()
        );

        let loaded = LocalLibrary::load_from(root.clone()).unwrap();
        assert_eq!(loaded.books[0].title, "Recognized title");
        assert_eq!(loaded.books[0].authors, ["Author"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opening_an_external_book_imports_once_and_returns_the_managed_copy() {
        let root = test_directory("open-import-epub");
        let source = root.join("source.epub");
        build_fixture(&source);
        let mut library = LocalLibrary::load_from(root.join("data")).unwrap();

        let (imported, was_imported) = library.import_for_open(&source).unwrap();
        assert!(was_imported);
        assert_ne!(imported.path, source);
        assert!(imported.path.exists());
        assert_eq!(library.books().len(), 1);

        let (duplicate, was_imported) = library.import_for_open(&source).unwrap();
        assert!(!was_imported);
        assert_eq!(duplicate.id, imported.id);
        assert_eq!(duplicate.path, imported.path);
        assert_eq!(library.books().len(), 1);

        let (managed, was_imported) = library.import_for_open(&imported.path).unwrap();
        assert!(!was_imported);
        assert_eq!(managed.id, imported.id);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_manifest_commit_rolls_back_imported_files_and_memory() {
        let root = test_directory("import-rollback");
        let source = root.join("source.epub");
        build_fixture(&source);
        let data_root = root.join("data");
        let mut library = LocalLibrary::load_from(data_root.clone()).unwrap();
        fs::create_dir(data_root.join(MANIFEST_FILE)).unwrap();

        assert!(library.import_files(&[source]).is_err());
        assert!(library.books().is_empty());
        assert_eq!(
            fs::read_dir(data_root.join(BOOKS_DIRECTORY))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(data_root.join(COVERS_DIRECTORY))
                .unwrap()
                .count(),
            0
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_remove_commit_keeps_book_and_managed_file() {
        let root = test_directory("remove-rollback");
        let mut library = LocalLibrary::load_from(root.clone()).unwrap();
        let managed_path = root.join(BOOKS_DIRECTORY).join("managed.epub");
        fs::write(&managed_path, b"fixture").unwrap();
        library.books.push(LibraryBook {
            id: "managed".into(),
            title: "Managed".into(),
            authors: Vec::new(),
            file_name: "original.epub".into(),
            path: managed_path.clone(),
            cover_bytes: None,
            added_at: 1,
        });
        library.persist().unwrap();
        fs::remove_file(root.join(MANIFEST_FILE)).unwrap();
        fs::create_dir(root.join(MANIFEST_FILE)).unwrap();

        assert!(library.remove("managed").is_err());
        assert_eq!(library.books().len(), 1);
        assert!(managed_path.exists());

        fs::remove_dir_all(root).unwrap();
    }

    fn build_fixture(output: &Path) {
        let fixture_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-data/minimal-epub");
        let mut archive = ZipWriter::new(fs::File::create(output).unwrap());
        archive
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(b"application/epub+zip").unwrap();
        for entry in FIXTURE_ENTRIES {
            archive
                .start_file(
                    entry,
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
                )
                .unwrap();
            archive
                .write_all(&fs::read(fixture_root.join(entry)).unwrap())
                .unwrap();
        }
        archive.finish().unwrap();
    }

    fn test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "rebook-library-{name}-{}-{}",
            std::process::id(),
            unix_timestamp_millis()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
