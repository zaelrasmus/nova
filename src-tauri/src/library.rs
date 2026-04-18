use crate::error::AppError;
use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    pub db_path: PathBuf,
    pub root_path: PathBuf,
}

// #[instrument(fields(location = %location, name = %name))]
pub async fn perform_create_library(location: &str, name: &str) -> Result<PathBuf, AppError> {
    let root = PathBuf::from(location).join(format!("{}.library", name));

    if root.exists() {
        // Typed error: the command can display a specific message for this case.
        return Err(AppError::LibraryAlreadyExists);
    }

    // debug!(root = ?root, "Starting library creation");

    // All setups steps run in a nested block so we can rollback cleanly on failure
    let setup: anyhow::Result<()> = async {
        tokio::fs::create_dir_all(root.join("assets"))
            .await
            .context("Failed to create assets directory")?;

        let db_path = root.join("library.db");
        // debug!(db_path = ?db_path, "Initializing database");
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        let pool = SqlitePool::connect_with(options)
            .await
            .context("Failed to open SQLite pool for new library")?;

        sqlx::migrate!()
            .run(&pool)
            .await
            .context("Failed to run database migrations")?;

        pool.close().await;
        Ok(())
    }
    .await;

    if let Err(e) = setup {
        // Rollback: remove anything we created before the failure.
        if root.exists() {
            // warn!(root = ?root, "Library creation failed, rolling back directory");
            if let Err(rm_err) = tokio::fs::remove_dir_all(&root).await {
                // Log the rollback failure but still surface the original error.
                // tracing::error!(error = %rm_err, "Rollback failed — orphaned directory may remain");
            }
        }
        // Wrap anyhow error into AppError::Internal for the command layer.
        return Err(AppError::from(e));
    }

    // info!(root = ?root, "Library created successfully");
    Ok(root)
}
