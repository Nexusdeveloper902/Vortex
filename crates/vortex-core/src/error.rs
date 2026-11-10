//! Error and Result types shared by every core module.
//!
//! GTK shows `Error`'s `Display` in a toast or dialog, so messages are written for users.
//! Diagnostic findings are data (`diagnose::Finding`), not errors.

use std::path::PathBuf;

/// Database row id of an environment (prefix).
pub type EnvId = i64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this Vortex supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("{0}")]
    Validation(String),
    #[error("runtime at {} is not usable: {reason}", path.display())]
    RuntimeInvalid { path: PathBuf, reason: String },
    #[error("environment {0} already has an active run")]
    Busy(EnvId),
    #[error("download failed: {0}")]
    Download(String),
    #[error("cancelled")]
    Cancelled,
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Builds an `Io` error carrying the path; use as `.map_err(Error::io(path))`.
    pub fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
        let path = path.into();
        move |source| Error::Io { path, source }
    }
}
