use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("No library connected")]
    NoLibrary,

    #[error("A library already exists at the given location")]
    LibraryAlreadyExists,

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Async task failed: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),

    #[error("{0:#}")]
    Internal(anyhow::Error),
}

impl AppError {
    pub fn frontend_message(&self) -> &'static str {
        match self {
            Self::NoLibrary => {
                "No library is currently open. Please open or create one."
            }
            Self::LibraryAlreadyExists => {
                "A library already exists at the given location. Choose a different folder."
            }
            Self::Database(_) => {
                "A database error occurred. Please try again or restart the app."
            }
            Self::Io(_) => {
                "A file system error ocurred. Please try again or restart the app or check folder permissions."
            }
            Self::TaskJoin(_) | Self::Internal(_) => {
                "An unexpected error occurred. Please try again or restart the app."
            }
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.frontend_message())
    }
}
