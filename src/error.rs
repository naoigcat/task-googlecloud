use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
/// Errors are classified by whether a request may have changed remote state so
/// callers can choose confirmation, rollback, or immediate failure safely.
pub enum AppError {
    #[error("{0}")]
    Message(String),

    #[error("I/O failed: {0}")]
    Io(#[from] io::Error),

    /// Raised while reading a local upload source, before any request leaves the
    /// process, so Cloud Storage cannot have been changed by the attempt.
    #[error("Cannot read the upload source: {0}")]
    UploadSource(io::Error),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Cloud Storage API request failed ({status}): {message}")]
    Storage { status: u16, message: String },

    #[error("{0}")]
    StorageResponse(String),

    /// The bucket lock already belongs to another compliant writer.
    #[error("Cloud Storage bucket lock conflict: {0}")]
    BucketLockConflict(Box<AppError>),

    #[error("Cloud command failed: {operation} (exit status: {status}){details}")]
    Command {
        operation: String,
        status: String,
        details: String,
    },

    /// Access-token retrieval failed. The boolean records whether an earlier
    /// request in the same operation may already have reached Cloud Storage.
    #[error("Cloud Storage token retrieval failed: {0}")]
    Token(Box<AppError>, bool),

    #[error("Manual recovery required for {paths} after {operation}; {details}")]
    Recovery {
        paths: String,
        operation: String,
        details: String,
    },

    #[error("Normalized names collide: {0}")]
    Collision(String),

    /// Combines an operation failure with failures encountered while restoring
    /// local or remote state.
    #[error("{original}; rollback failed: {details}")]
    Rollback {
        original: Box<AppError>,
        details: String,
    },

    #[error("Operation interrupted")]
    Interrupted,

    /// A move was interrupted after source deletion, but rollback restored the
    /// source under a new generation that the transaction must record.
    #[error("Operation interrupted after restoring a moved object")]
    InterruptedAfterMoveRollback { restored_generation: String },

    /// The request may have reached Cloud Storage before the interruption was observed.
    #[error("Operation interrupted during a Cloud Storage request")]
    InterruptedAfterRequest,

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

    /// Whether the failure may have left Cloud Storage changed. Upload-source
    /// failures, pre-request interruptions, and token failures before any
    /// request cannot have; token failures after an earlier request are marked
    /// as reached.
    pub(crate) fn reached_storage(&self) -> bool {
        match self {
            Self::UploadSource(_) | Self::Command { .. } | Self::Interrupted => false,
            Self::Token(_, reached_storage) => *reached_storage,
            _ => true,
        }
    }

    pub(crate) fn is_interrupted(&self) -> bool {
        match self {
            Self::Interrupted
            | Self::InterruptedAfterMoveRollback { .. }
            | Self::InterruptedAfterRequest => true,
            Self::Token(error, _) => error.is_interrupted(),
            _ => false,
        }
    }

    pub(crate) fn interrupted_after_move_rollback(restored_generation: String) -> Self {
        Self::InterruptedAfterMoveRollback {
            restored_generation,
        }
    }

    pub(crate) fn restored_move_generation(&self) -> Option<&str> {
        match self {
            Self::InterruptedAfterMoveRollback {
                restored_generation,
            } => Some(restored_generation),
            Self::Rollback { original, .. } => original.restored_move_generation(),
            _ => None,
        }
    }

    pub(crate) fn is_bucket_lock_conflict(&self) -> bool {
        match self {
            Self::BucketLockConflict(_) => true,
            Self::Rollback { original, .. } => original.is_bucket_lock_conflict(),
            _ => false,
        }
    }

    pub(crate) fn token(error: Self) -> Self {
        Self::Token(Box::new(error), false)
    }

    pub(crate) fn mark_reached_storage(self) -> Self {
        match self {
            Self::Token(error, _) => Self::Token(error, true),
            error => error,
        }
    }

    pub(crate) fn may_have_sent_storage_request(&self) -> bool {
        match self {
            Self::Http(_)
            | Self::Storage { .. }
            | Self::StorageResponse(_)
            | Self::InterruptedAfterRequest => true,
            Self::Token(_, true) => true,
            Self::Rollback { original, .. } => original.may_have_sent_storage_request(),
            _ => false,
        }
    }

    pub(crate) fn status(&self) -> Option<u16> {
        match self {
            Self::Storage { status, .. } => Some(*status),
            _ => None,
        }
    }
}
