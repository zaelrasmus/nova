use crate::error::AppError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

pub struct DbState {
    pool: Arc<RwLock<Option<SqlitePool>>>,
}

impl DbState {
    pub fn new() -> Self {
        Self {
            pool: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn acquire_pool(&self) -> Result<SqlitePool, AppError> {
        let lock = self.pool.read().await;
        lock.as_ref().cloned().ok_or(AppError::NoLibrary)
    }

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
