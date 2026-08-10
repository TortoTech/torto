//! Format detection and direct publication sources for desktop e-book formats.
//!
//! Every parser implements the format-neutral [`BookSource`] boundary. EPUB
//! retains its archive-backed source, while MOBI/KF8, FB2, and CBZ construct
//! their publication descriptors, lazy sections, and resources directly.

mod cbz;
mod chm;
mod epub;
mod fb2;
mod kf8;
mod mobi;
mod pdf;
mod source;
mod xml;

use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use rebook_publication::{Book, BookSource, PublicationError};
use thiserror::Error;

use self::epub::{EpubError, EpubPublication};

pub use self::pdf::cjk_fallback_font_bytes;

/// E-book formats supported by the desktop application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookFormat {
    Epub,
    Mobi,
    Azw,
    Azw3,
    Fb2,
    Fbz,
    Cbz,
    Chm,
    Pdf,
}

impl BookFormat {
    /// Detects a supported format from a source file name.
    pub fn from_file_name(file_name: &str) -> Option<Self> {
        let lower = file_name.to_ascii_lowercase();
        if lower.ends_with(".fb2.zip") {
            return Some(Self::Fbz);
        }
        let extension = Path::new(&lower).extension()?.to_str()?;
        match extension {
            "epub" => Some(Self::Epub),
            "mobi" => Some(Self::Mobi),
            "azw" => Some(Self::Azw),
            "azw3" => Some(Self::Azw3),
            "fb2" => Some(Self::Fb2),
            "fbz" => Some(Self::Fbz),
            "cbz" => Some(Self::Cbz),
            "chm" => Some(Self::Chm),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }

    /// Short display label used by the shelf and reader sidebar.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Epub => "EPUB",
            Self::Mobi => "MOBI",
            Self::Azw => "AZW",
            Self::Azw3 => "AZW3",
            Self::Fb2 => "FB2",
            Self::Fbz => "FBZ",
            Self::Cbz => "CBZ",
            Self::Chm => "CHM",
            Self::Pdf => "PDF",
        }
    }

    /// Extension used for the managed library copy.
    pub const fn storage_extension(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Mobi => "mobi",
            Self::Azw => "azw",
            Self::Azw3 => "azw3",
            Self::Fb2 => "fb2",
            Self::Fbz => "fbz",
            Self::Cbz => "cbz",
            Self::Chm => "chm",
            Self::Pdf => "pdf",
        }
    }
}

impl fmt::Display for BookFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// A detected source opened against the common desktop publication contract.
pub struct OpenedPublication {
    format: BookFormat,
    source: Arc<dyn BookSource>,
    cover_bytes: Option<Vec<u8>>,
}

impl OpenedPublication {
    pub const fn format(&self) -> BookFormat {
        self.format
    }

    pub fn book(&self) -> &Book {
        self.source.book()
    }

    pub fn source(&self) -> Arc<dyn BookSource> {
        Arc::clone(&self.source)
    }

    pub fn cover_bytes(&self) -> Option<&[u8]> {
        self.cover_bytes.as_deref()
    }
}

/// Opens a supported local e-book file.
pub fn open_file(path: impl AsRef<Path>) -> Result<OpenedPublication, FormatError> {
    open_file_with_options(path, true, None)
}

/// Opens a shelf-managed book without reloading its cover or recomputing the
/// content-derived PDF identity established during import.
pub fn open_file_for_reading(
    path: impl AsRef<Path>,
    known_publication_id: Option<&str>,
) -> Result<OpenedPublication, FormatError> {
    open_file_with_options(path, false, known_publication_id)
}

fn open_file_with_options(
    path: impl AsRef<Path>,
    load_cover: bool,
    known_publication_id: Option<&str>,
) -> Result<OpenedPublication, FormatError> {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| FormatError::UnsupportedFormat(path.display().to_string()))?;
    let format = BookFormat::from_file_name(file_name)
        .ok_or_else(|| FormatError::UnsupportedFormat(file_name.to_owned()))?;
    let source: Arc<dyn BookSource> = match format {
        BookFormat::Chm => Arc::new(chm::open_path(path, file_name)?),
        BookFormat::Pdf => {
            let bytes = fs::read(path)?;
            Arc::new(if let Some(publication_id) = known_publication_id {
                pdf::open_with_id(bytes, file_name, publication_id)?
            } else {
                pdf::open(bytes, file_name)?
            })
        }
        _ => open_shared_source(fs::read(path)?.into(), file_name, format)?,
    };
    Ok(finish_open(format, source, load_cover))
}

/// Opens immutable source bytes using a file name for format detection.
pub fn open_bytes(
    bytes: impl Into<Arc<[u8]>>,
    file_name: &str,
) -> Result<OpenedPublication, FormatError> {
    let format = BookFormat::from_file_name(file_name)
        .ok_or_else(|| FormatError::UnsupportedFormat(file_name.to_owned()))?;
    let bytes = bytes.into();
    let source = match format {
        BookFormat::Chm => {
            Arc::new(chm::open_bytes(bytes.as_ref(), file_name)?) as Arc<dyn BookSource>
        }
        BookFormat::Pdf => Arc::new(pdf::open_shared(bytes, file_name)?) as Arc<dyn BookSource>,
        _ => open_shared_source(bytes, file_name, format)?,
    };
    Ok(finish_open(format, source, true))
}

fn open_shared_source(
    bytes: Arc<[u8]>,
    file_name: &str,
    format: BookFormat,
) -> Result<Arc<dyn BookSource>, FormatError> {
    let source: Arc<dyn BookSource> = match format {
        BookFormat::Epub => Arc::new(EpubPublication::open_bytes(bytes)?),
        BookFormat::Mobi | BookFormat::Azw | BookFormat::Azw3 => {
            Arc::new(mobi::open(bytes.as_ref(), file_name, format)?)
        }
        BookFormat::Fb2 | BookFormat::Fbz => Arc::new(fb2::open(bytes.as_ref(), file_name)?),
        BookFormat::Cbz => Arc::new(cbz::open(bytes.as_ref(), file_name)?),
        BookFormat::Chm => Arc::new(chm::open_bytes(bytes.as_ref(), file_name)?),
        BookFormat::Pdf => unreachable!("PDF bytes use the zero-copy owned/shared paths"),
    };
    Ok(source)
}

fn finish_open(
    format: BookFormat,
    source: Arc<dyn BookSource>,
    load_cover: bool,
) -> OpenedPublication {
    let cover_bytes = source
        .book()
        .cover
        .as_ref()
        .filter(|_| load_cover)
        .and_then(|href| source.resource(href).ok())
        .map(|resource| resource.bytes.to_vec());
    OpenedPublication {
        format,
        source,
        cover_bytes,
    }
}

#[derive(Debug, Error)]
pub enum FormatError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("EPUB parsing failed: {0}")]
    Epub(String),
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error("不支持的电子书格式：{0}")]
    UnsupportedFormat(String),
    #[error("{format} 解析失败：{message}")]
    Conversion { format: BookFormat, message: String },
}

impl From<EpubError> for FormatError {
    fn from(error: EpubError) -> Self {
        Self::Epub(error.to_string())
    }
}

pub(crate) fn conversion_error(format: BookFormat, error: impl fmt::Display) -> FormatError {
    FormatError::Conversion {
        format,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_every_supported_extension() {
        assert_eq!(
            BookFormat::from_file_name("book.EPUB"),
            Some(BookFormat::Epub)
        );
        assert_eq!(
            BookFormat::from_file_name("book.mobi"),
            Some(BookFormat::Mobi)
        );
        assert_eq!(
            BookFormat::from_file_name("book.azw"),
            Some(BookFormat::Azw)
        );
        assert_eq!(
            BookFormat::from_file_name("book.azw3"),
            Some(BookFormat::Azw3)
        );
        assert_eq!(
            BookFormat::from_file_name("book.fb2"),
            Some(BookFormat::Fb2)
        );
        assert_eq!(
            BookFormat::from_file_name("book.fbz"),
            Some(BookFormat::Fbz)
        );
        assert_eq!(
            BookFormat::from_file_name("book.fb2.zip"),
            Some(BookFormat::Fbz)
        );
        assert_eq!(
            BookFormat::from_file_name("book.cbz"),
            Some(BookFormat::Cbz)
        );
        assert_eq!(
            BookFormat::from_file_name("book.pdf"),
            Some(BookFormat::Pdf)
        );
        assert_eq!(
            BookFormat::from_file_name("book.CHM"),
            Some(BookFormat::Chm)
        );
    }
}
