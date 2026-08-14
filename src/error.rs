use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),

    #[error("I/O failed: {0}")]
    Io(#[from] io::Error),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Cloud Storage API request failed ({status}): {message}")]
    Storage { status: u16, message: String },

    #[error("Cloud command failed: {operation} (exit status: {status}){details}")]
    Command {
        operation: String,
        status: String,
        details: String,
    },

    #[error("Manual recovery required for {paths} after {operation}; {details}")]
    Recovery {
        paths: String,
        operation: String,
        details: String,
    },

    #[error("Normalized names collide: {0}")]
    Collision(String),

    #[error("{original}; rollback failed: {details}")]
    Rollback {
        original: Box<AppError>,
        details: String,
    },

    #[error("Operation interrupted")]
    Interrupted,

    #[error("Atomic no-replace rename is not supported on {0}")]
    UnsupportedPlatform(String),

    #[error("Expected a Cloud Storage URI: {0:?}")]
    InvalidStorageUri(String),
}

impl AppError {
    pub(crate) fn rollback(original: AppError, errors: Vec<AppError>) -> Self {
        if errors.is_empty() {
            return original;
        }

        let details = errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        Self::Rollback {
            original: Box::new(original),
            details,
        }
    }

    pub(crate) fn status(&self) -> Option<u16> {
        match self {
            Self::Storage { status, .. } => Some(*status),
            _ => None,
        }
    }
}
