//! Core of Lazee Viewer: the photo library index, metadata, RAW previews and
//! the thumbnail cache. Everything the UI shows comes from here, and nothing
//! in this crate ever writes to the user's photo folders.

pub mod db;
pub mod formats;
pub mod hash;
pub mod library;
pub mod meta;
pub mod raw;
pub mod scanner;
pub mod thumbs;
pub mod volume;

pub use library::{FullImage, Library, PhotoInfo, PhotoList, SortOrder, Source};
pub use scanner::{ScanControl, ScanPhase, ScanProgress, ScanSummary};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not decode image: {0}")]
    Decode(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
