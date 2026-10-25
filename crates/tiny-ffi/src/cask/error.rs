//! The error every cask call can end with.

use tiny_core::cask::{CaskFailure, UpdateRequestError};

use super::records::FfiCaskFailure;
use crate::FfiError;

/// Why a cask call did not give its answer. A separate type from
/// [`FfiError`] so the cask screens handle their own cases and Clean's and
/// Uninstall's `FfiError` handling is untouched. `Failed` carries the typed
/// reason (`FfiCaskFailure.kind` is the contract; its text is a diagnostic
/// the client may replace); the other cases are the session's.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiCaskError {
    /// Another operation holds the session gate. Nothing was started.
    #[error("another operation is already running")]
    Busy,
    /// The caller's token was cancelled before anything started.
    #[error("operation cancelled")]
    Cancelled,
    /// The preview is unknown, replaced, expired or already used, or there is
    /// no current update list to preview from.
    #[error("preview is no longer valid: {detail}")]
    PreviewInvalid { detail: String },
    /// The request names something the preview does not hold (an unknown or
    /// repeated token, or nothing). The preview stays usable.
    #[error("invalid input: {detail}")]
    InvalidInput { detail: String },
    /// Homebrew or the request failed in a way the user can read and act on.
    /// For `AppRunning` and `AppCheckFailed` raised by `cask_execute` nothing
    /// ran and the preview was not spent.
    #[error("{}", failure.problem)]
    Failed { failure: FfiCaskFailure },
    /// Reading the Mac's apps failed, or another unexpected error.
    #[error("operation failed: {detail}")]
    Operation { detail: String },
}

impl From<FfiError> for FfiCaskError {
    fn from(error: FfiError) -> Self {
        match error {
            FfiError::Busy => Self::Busy,
            FfiError::Cancelled => Self::Cancelled,
            FfiError::PreviewInvalid { detail } => Self::PreviewInvalid { detail },
            FfiError::InvalidInput { detail } => Self::InvalidInput { detail },
            other => Self::Operation {
                detail: other.to_string(),
            },
        }
    }
}

/// A core failure as the error a call returns.
pub(super) fn failed(failure: CaskFailure) -> FfiCaskError {
    FfiCaskError::Failed {
        failure: (&failure).into(),
    }
}

impl From<UpdateRequestError> for FfiCaskError {
    fn from(error: UpdateRequestError) -> Self {
        match error {
            UpdateRequestError::Invalid(detail) => Self::InvalidInput { detail },
            UpdateRequestError::Failed(failure) => failed(failure),
        }
    }
}
