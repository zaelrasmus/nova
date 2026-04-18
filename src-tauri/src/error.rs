//! Application-level error types.
//!
//! ## Strategy
//!
//! - [`AppError`] is the only type that crosses the Tauri IPC boundary.
//! - Internal service functions use `anyhow::Result` for ergonomic `?` chaining.
//! - `From<anyhow::Error>` on `AppError` means `?` works transparently at command
//!   boundaries with zero manual `.map_err` calls.
//! - [`AppError::Serialize`] sends only the generic frontend message over the wire.
//!   Full error detail is logged by the command layer before serialization occurs.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    /// No library is open. Returned by any command that requires an active pool.
    #[error("No library connected")]
    NoLibrary,

    /// The target path already contains a library. Returned by `create_library`.
    #[error("A library already exists at the given location")]
    LibraryAlreadyExists,

    /// A SQLx database error. Typed so CI test assertions can match on DB failures.
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    /// A filesystem I/O error. Typed for the same reason as `Database`.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// A spawned Tokio task panicked or was cancelled.
    #[error("Async task failed: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),

    /// Catch-all for internal `anyhow` errors. The full context chain is
    /// preserved for logs via `{0:#}` but never reaches the frontend.
    #[error("{0:#}")]
    Internal(anyhow::Error),
}

impl AppError {
    /// Returns a safe, user-facing string for a frontend toast notification.
    ///
    /// Never exposes internal details such as file paths, SQL queries, or stack
    /// information. Add a new arm here when a variant needs a distinct message.
    pub fn frontend_message(&self) -> &'static str {
        match self {
            Self::NoLibrary => "No library is currently open. Please open or create one.",
            Self::LibraryAlreadyExists => {
                "A library already exists at this location. Choose a different folder."
            }
            Self::Database(_) => "A database error occurred. Please try again or restart the app.",
            Self::Io(_) => "A file system error occurred. Please check folder permissions.",
            Self::TaskJoin(_) | Self::Internal(_) => {
                "An unexpected error occurred. Please try again or restart the app."
            }
        }
    }
}

/// Allows `?` to convert `anyhow::Error` into `AppError` at command boundaries.
///
/// Internal service functions return `anyhow::Result` freely; the command layer
/// pays zero conversion cost — the compiler inserts this `From` impl automatically.
impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

/// Serializes only the generic frontend message over the Tauri IPC wire.
///
/// Full technical detail is always logged by the command layer *before* this
/// point, so no information is lost — it stays in the structured logs.
impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.frontend_message())
    }
}
