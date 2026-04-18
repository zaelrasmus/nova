use crate::error::AppError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

/// Global database connection state managed by Tauri.
///
/// `RwLock` is used instead of `Mutex` so concurrent reads (e.g. `fetch_assets`
/// running while a background task holds a reference) do not block each other.
pub struct DbState {
    pool: Arc<RwLock<Option<SqlitePool>>>,
}

impl DbState {
    pub fn new() -> Self {
        Self {
            pool: Arc::new(RwLock::new(None)),
        }
    }

    /// Returns a clone of the active connection pool.
    ///
    /// Cloning `SqlitePool` is cheap — it increments an `Arc` reference count.
    /// Returns `AppError::NoLibrary` if no library has been connected yet.
    pub async fn acquire_pool(&self) -> Result<SqlitePool, AppError> {
        let lock = self.pool.read().await;
        lock.as_ref().cloned().ok_or(AppError::NoLibrary)
    }

    /// Opens a WAL-mode SQLite connection to `{path}/library.db`.
    ///
    /// WAL + Normal synchronous is the right tradeoff for a local asset manager:
    /// concurrent reads, fast writes, and crash-safe enough for non-critical data.
    /// If a connection is already open, it is gracefully closed before the new
    /// one is established.
    #[instrument(skip(self, path), fields(library_path = %path.as_ref().display()))]
    pub async fn connect<P: AsRef<Path>>(&self, path: P) -> Result<(), AppError> {
        let db_path = path.as_ref().join("library.db");

        if !db_path.exists() {
            return Err(AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("library.db not found at {:?}", db_path),
            )));
        }

        debug!(db_path = ?db_path, "Opening SQLite connection pool");

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let new_pool = SqlitePool::connect_with(options).await?;

        let mut lock = self.pool.write().await;

        if let Some(old_pool) = lock.take() {
            warn!("Replacing existing library connection. Closing old pool.");
            old_pool.close().await;
        }

        *lock = Some(new_pool);

        info!(db_path = ?db_path, "Library connected successfully");
        Ok(())
    }
}

impl Default for DbState {
    fn default() -> Self {
        Self::new()
    }
}
