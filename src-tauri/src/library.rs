use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    pub db_path: PathBuf,
    pub root_path: PathBuf,
}

pub async fn perform_create_library(location: &str, name: &str) -> Result<PathBuf> {
    let root = PathBuf::from(location).join(format!("{}.library", name));

    if root.exists() {
        anyhow::bail!("Library path already exists")
    }

    let workspace_res: anyhow::Result<()> = async {
        tokio::fs::create_dir_all(root.join("assets"))
            .await
            .context("Cannot create assets dir")?;

        let db_path = root.join("library.db");
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

        let pool = SqlitePool::connect_with(options)
            .await
            .context("Cannot open pool Database SQlite")?;

        sqlx::migrate!()
            .run(&pool)
            .await
            .context("Error executing migrations in database")?;

        pool.close().await;

        Ok(())
    }
    .await;

    if let Err(e) = workspace_res {
        // Rollback: remove the root directory if creation failed
        if root.exists() {
            let _ = tokio::fs::remove_dir_all(&root).await;
        }
        return Err(e).context("Error creating library");
    }

    Ok(root)
}
