use crate::error::AppError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, instrument, warn};

#[derive(Clone)]
pub struct LibraryHandle {
    pub pool: SqlitePool,
    pub root: PathBuf,
}

pub struct DbState {
    inner: Arc<RwLock<Option<LibraryHandle>>>,
    /// Held for the duration of a background thumbnail run. `try_lock` failing
    /// means a run is already in flight, so a duplicate request is a no-op.
    pub thumb_gen: Arc<Mutex<()>>,
}

impl DbState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            thumb_gen: Arc::new(Mutex::new(())),
        }
    }

    // Full handle (pool + root path). Errors if no library is open
    pub async fn acquire(&self) -> Result<LibraryHandle, AppError> {
        let lock = self.inner.read().await;
        lock.as_ref().cloned().ok_or(AppError::NoLibrary)
    }
    /// Convenience for callers that only need the pool
    pub async fn acquire_pool(&self) -> Result<SqlitePool, AppError> {
        Ok(self.acquire().await?.pool)
    }

    #[instrument(skip(self, path), fields(library_path = %path.as_ref().display()))]
    pub async fn connect<P: AsRef<Path>>(&self, path: P) -> Result<(), AppError> {
        let root = path.as_ref().to_path_buf();
        let db_path = root.join("library.db");

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
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5))
            .pragma("cache_size", "-65536") // 64 MiB page cache (negative = KiB)
            .pragma("temp_store", "MEMORY")
            .pragma("mmap_size", "268435456"); // 256 MiB memory mapped reads

        let new_pool = SqlitePool::connect_with(options).await?;

        // Run migrations on connect
        sqlx::migrate!().run(&new_pool).await.map_err(|e| {
            tracing::error!(error = %e, "Failed to run migrations on connect");
            AppError::Internal(e.into())
        })?;

        let handle = LibraryHandle {
            pool: new_pool,
            root,
        };

        let mut lock = self.inner.write().await;

        if let Some(old_handle) = lock.take() {
            warn!("Replacing existing library connection. Closing old pool.");
            old_handle.pool.close().await;
        }

        *lock = Some(handle);

        info!(db_path = ?db_path, "Library connected successfully");
        Ok(())
    }
}

impl Default for DbState {
    fn default() -> Self {
        Self::new()
    }
}
