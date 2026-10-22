//! Headless engine of forhunt-viewer: opens multi-GB CSV files read-only,
//! indexes rows in one parallel pass, and serves rows, filters and sorts
//! without loading the file into memory.

pub mod cache;
pub mod dialect;
pub mod index;
pub mod progress;
pub mod scan;
pub mod source;
pub mod table;
pub mod time;

pub use dialect::{Dialect, Encoding};
pub use progress::{Phase, Progress};
pub use source::Access;
pub use table::{OpenOptions, Rows, Table};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("index cache: {0}")]
    Cache(String),
    #[error("internal lock poisoned")]
    Poisoned,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
