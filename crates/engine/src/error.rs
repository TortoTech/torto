use rebook_formats::FormatError;
use rebook_publication::PublicationError;
use rebook_reader::ReaderError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error(transparent)]
    Reader(#[from] ReaderError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
