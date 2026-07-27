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

/// Marker for an error whose message was written FOR the user.
///
/// The default for an internal `anyhow` error is to be replaced by a generic
/// sentence, because it may carry a path, a SQL statement or an OS message. This
/// opts a specific error out of that: the string is one the app authored — a
/// shortcut conflict, a malformed rename pattern, a folder dropped into itself —
/// and replacing it with "an unexpected error occurred" throws away the only
/// thing that would have told the user what to do.
///
/// The distinction is already structural in this codebase: `reject!` carries a
/// sentence, `.context()` carries a breadcrumb. This makes the compiler aware of
/// it.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct Rejected(pub String);

/// Build a rejection as an `anyhow::Error`, for `ok_or_else` and friends.
pub fn rejected(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Rejected(message.into()))
}

/// `bail!`, but the message reaches the user instead of being swallowed.
///
/// Do NOT wrap a `reject!` in `.context(...)`: the downcast finds the marker and
/// returns ITS message, so the context is added to the log and lost from the
/// screen. If the caller has the better sentence, the caller should reject —
/// `.map_err(|_| rejected("A folder needs a name"))`.
#[macro_export]
macro_rules! reject {
    ($($arg:tt)*) => {
        return ::core::result::Result::Err($crate::error::rejected(format!($($arg)*)))
    };
}

#[derive(Debug, Error)]
pub enum AppError {
    /// A message the app wrote for the user. See [`Rejected`].
    #[error("{0}")]
    Rejected(String),

    /// No library is open. Returned by any command that requires an active pool.
    #[error("No library connected")]
    NoLibrary,

    /// The target path already contains a library. Returned by `create_library`.
    #[error("A library already exists at the given location")]
    LibraryAlreadyExists,

    /// The library's schema doesn't match this build's migration set — either it
    /// was written by a newer Nova, or a shipped migration file was edited.
    ///
    /// Typed separately from `Database` because it is the ONE failure a user can
    /// act on: the fix is "get the right version of Nova", and the generic
    /// "a database error occurred, try restarting" would send them in circles
    /// forever. See the freeze note at the top of the migration.
    #[error("Library schema is incompatible with this build: {0}")]
    LibraryVersion(String),

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
    ///
    /// `&str` rather than `&'static str` so [`Self::Rejected`] can return the
    /// sentence it carries. Every other arm is still a literal, so the exhaustive
    /// match still forces a decision when a variant is added.
    pub fn frontend_message(&self) -> &str {
        match self {
            // The only arm that is not a fixed string — and the only one whose
            // content came from inside the app rather than from this file. Safe
            // by construction: `Rejected` is only ever built from an authored
            // sentence, never from a path or a driver error.
            Self::Rejected(message) => message,
            Self::NoLibrary => "No library is currently open. Please open or create one.",
            Self::LibraryAlreadyExists => {
                "A library already exists at this location. Choose a different folder."
            }
            Self::LibraryVersion(_) => {
                "This library was created by a different version of Nova and can't be opened. \
                 Update Nova, or open it with the version that made it. Your files are untouched."
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
        // A `Rejected` anywhere in the chain wins. `downcast` searches through
        // `.context()` wrappers, so a rejection stays a rejection even if a
        // caller added a breadcrumb on the way up — the breadcrumb goes to the
        // log and the sentence goes to the screen, which is the right split.
        match e.downcast::<Rejected>() {
            Ok(Rejected(message)) => Self::Rejected(message),
            Err(other) => Self::Internal(other),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point: a sentence written for the user survives the trip to the
    /// wire instead of being replaced by the generic apology.
    #[test]
    fn a_rejection_keeps_its_message() {
        let err: AppError = rejected("Ctrl+Shift+1 is already used by \"Archive old\"").into();
        assert_eq!(
            err.frontend_message(),
            "Ctrl+Shift+1 is already used by \"Archive old\""
        );
    }

    /// …and it survives a `.context()` added by a caller on the way up. The
    /// breadcrumb belongs in the log; the sentence still belongs on screen.
    #[test]
    fn a_rejection_survives_being_wrapped_in_context() {
        use anyhow::Context;
        let wrapped = Err::<(), _>(rejected("A folder needs a name"))
            .context("Failed to create folder")
            .unwrap_err();
        let err: AppError = wrapped.into();
        assert_eq!(err.frontend_message(), "A folder needs a name");
    }

    /// The guarantee that makes the variant safe: an ordinary internal error is
    /// still replaced, so a path or a SQL statement can never reach the webview.
    #[test]
    fn an_internal_error_is_still_generic() {
        let err: AppError = anyhow::anyhow!("UNIQUE constraint failed: assets.content_hash").into();
        assert_eq!(
            err.frontend_message(),
            "An unexpected error occurred. Please try again or restart the app."
        );
    }

    /// What actually crosses the IPC boundary is the message, not the Debug form.
    #[test]
    fn serialization_sends_the_frontend_message() {
        let err: AppError = rejected("The pattern is empty").into();
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            "\"The pattern is empty\""
        );
    }
}

