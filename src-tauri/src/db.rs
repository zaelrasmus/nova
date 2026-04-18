use crate::error::AppError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct DbState {
    // We use RwLock for better concurrent read performance
    pub pool: Arc<RwLock<Option<SqlitePool>>>,
}

impl DbState {
    pub fn new() -> Self {
        Self {
            pool: Arc::new(RwLock::new(None)),
        }
    }

    /// Senior helper: Centralizes pool access and error handling
    pub async fn get_pool(&self) -> Result<SqlitePool, AppError> {
        let lock = self.pool.read().await;
        lock.as_ref().cloned().ok_or(AppError::NoLibrary)
    }

    // Establishing a connection to the SQlite database library
    // #[instrument(skip(self), fields(library_path = %path.as_ref().display()))]
    pub async fn connect<P: AsRef<Path>>(&self, path: P) -> Result<(), AppError> {
        let db_path = path.as_ref().join("library.db");

        if !db_path.exists() {
            // return Err(format!("Database file not found at {:?}", db_path);
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("library.db not found at {:?}", db_path),
            )));
        }

        // debug!(db_path = ?db_path, "Opening SQLite connection pool");

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        // sqlx::Error is #[from] on AppError, so ? converts directly.
        let pool = SqlitePool::connect_with(options).await?;

        let mut lock = self.pool.write().await;

        // If a pool is already connected, disconnect it first
        if let Some(old_pool) = lock.take() {
            // warn!("Replacing existing library connection. Closing old pool.");
            old_pool.close().await;
        }

        *lock = Some(pool);

        // info!(db_path = ?db_path, "Library connected sucessfully");
        Ok(())
    }
}

impl Default for DbState {
    fn default() -> Self {
        Self::new()
    }
}
