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
    pub async fn get_pool(&self) -> Result<SqlitePool, String> {
        let lock = self.pool.read().await;
        lock.as_ref().cloned().ok_or_else(|| {
            "No active library connected. Please open or create a library first.".to_string()
        })
    }

    // establishing a connection to the SQlite database library
    pub async fn connect<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let db_path = path.as_ref().join("library.db");

        if !db_path.exists() {
            return Err(format!("Database file not found at {:?}", db_path));
        }

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);

        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| format!("Failed to connect to SQLite: {}", e))?;

        let mut lock = self.pool.write().await;

        // If a pool is already connected, disconnect it first
        if let Some(old_pool) = lock.take() {
            old_pool.close().await;
        }

        *lock = Some(pool);
        Ok(())
    }
}
