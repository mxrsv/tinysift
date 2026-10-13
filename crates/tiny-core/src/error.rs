use std::fmt::Display;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    Operation(String),
    #[error("{0}")]
    Unsupported(String),
    /// macOS denied Apple Events (Automation) access, e.g. Finder for Trash.
    #[error("automation permission denied: {0}")]
    AutomationDenied(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Attach operation context while keeping the engine independent of anyhow.
pub trait Context<T> {
    fn context(self, message: impl Display) -> Result<T>;
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T>;
}
impl<T, E: Display> Context<T> for std::result::Result<T, E> {
    fn context(self, message: impl Display) -> Result<T> {
        self.map_err(|error| Error::Operation(format!("{message}: {error}")))
    }
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|error| Error::Operation(format!("{}: {error}", message())))
    }
}
impl<T> Context<T> for Option<T> {
    fn context(self, message: impl Display) -> Result<T> {
        self.ok_or_else(|| Error::InvalidInput(message.to_string()))
    }
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T> {
        self.ok_or_else(|| Error::InvalidInput(message()))
    }
}
#[macro_export]
macro_rules! engine_error {
    ($($arg:tt)*) => { $crate::error::Error::Operation(format!($($arg)*)) };
}
