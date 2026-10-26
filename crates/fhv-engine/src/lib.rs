//! Headless engine of forhunt-viewer: opens multi-GB CSV files read-only,
//! indexes rows in one parallel pass, and serves rows, filters and sorts
//! without loading the file into memory.

pub mod cache;
pub mod columns;
pub mod dialect;
pub mod export;
pub mod filter;
pub mod index;
pub mod progress;
pub mod query;
pub mod rownum;
pub mod scan;
pub mod sort;
pub mod source;
pub mod table;
pub mod time;
pub mod view;

pub use dialect::{Dialect, Encoding};
pub use filter::{Engine, Expr, Op, Pred, Target};
pub use progress::{Phase, Progress};
pub use sort::SortKey;
pub use source::Access;
pub use table::{OpenOptions, Rows, Table};
pub use view::{Query, View};

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
