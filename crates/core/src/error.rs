//! Crate error type.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid hash: {0}")]
    BadHash(String),

    #[error("chunker: {0}")]
    Chunker(String),

    #[error("manifest: {0}")]
    Manifest(String),

    #[error("verification failed: {0}")]
    Verify(String),
}
