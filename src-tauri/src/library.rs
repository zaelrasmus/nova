use crate::error::AppError;
use anyhow::Context;
use serde::Serialize;
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use std::path::PathBuf;
use tracing::{debug, info, instrument, warn};

#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    pub db_path: PathBuf,
    pub root_path: PathBuf,
}

/// Creates a new `.library` package — a directory containing the SQLite
/// database and an `assets/` folder for binary files.
///
/// Returns `AppError::LibraryAlreadyExists` when the target path is taken,
/// which lets the command layer surface a specific, actionable message.
/// On any other failure, the partially created directory is removed before
/// returning so no orphaned state is left on disk.
#[instrument(fields(location = %location, name = %name))]
pub async fn create_library(location: &str, name: &str) -> Result<PathBuf, AppError> {
    let library_root = PathBuf::from(location).join(format!("{}.library", name));

    if library_root.exists() {
        return Err(AppError::LibraryAlreadyExists);
    }

    debug!(root = ?library_root, "Starting library creation");

    // All setup steps run inside this block so a single `if let Err` can rollback cleanly.
    let setup_result: anyhow::Result<()> = async {
        tokio::fs::create_dir_all(library_root.join("assets"))
            .await
            .context("Failed to create assets directory")?;

        let db_path = library_root.join("library.db");
        debug!(db_path = ?db_path, "Initializing database");

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

    if let Err(e) = setup_result {
        if library_root.exists() {
            warn!(root = ?library_root, "Library creation failed — rolling back directory");
            if let Err(rm_err) = tokio::fs::remove_dir_all(&library_root).await {
                tracing::error!(
                    error = %rm_err,
                    "Rollback failed — orphaned directory may remain at {:?}",
                    library_root
                );
            }
        }
        return Err(AppError::from(e));
    }

    info!(root = ?library_root, "Library created successfully");
    Ok(library_root)
}
