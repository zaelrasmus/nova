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

        // Backfill the search index for a library that just gained the table on
        // migration. No-op once populated. Non-fatal: search degrades, the rest
        // of the app doesn't, and a manual rebuild can recover it.
        if let Err(e) = crate::search::ensure_indexed(&new_pool).await {
            warn!(error = %e, "Search index backfill failed (non-fatal)");
        }

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

// Tests disabled for now (kept, not deleted). Re-enable by removing this block
// comment. These guard the FTS5-availability and "trigram is substring, not
// fuzzy" decisions, so they're worth turning back on before the search feature
// ships.
/*
#[cfg(test)]
mod fts5_probe {
    //! S0 — Search-engine feasibility probe.
    //!
    //! Verifies the bundled SQLite actually has what the search design assumes:
    //! FTS5 compiled in AND the trigram tokenizer present. If either is missing
    //! the whole Option-A design changes, so this gates the feature. It also
    //! pins the behaviour we DECIDED on — trigram is substring, not typo-tolerant
    //! — so a future SQLite bump that changed that would fail loudly here.

    use sqlx::SqlitePool;

    async fn mem() -> SqlitePool {
        SqlitePool::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite")
    }

    #[tokio::test]
    async fn fts5_and_trigram_are_available() {
        let pool = mem().await;

        // Fails here if FTS5 isn't compiled in, or the trigram tokenizer is
        // absent (needs SQLite >= 3.34).
        sqlx::query("CREATE VIRTUAL TABLE probe USING fts5(x, tokenize='trigram')")
            .execute(&pool)
            .await
            .expect("FTS5 with trigram tokenizer must be available");

        sqlx::query("INSERT INTO probe(x) VALUES ('Photoshop Document.psd')")
            .execute(&pool)
            .await
            .expect("insert into fts5 table");

        // Substring / infix match — the actual feature. Standard FTS is
        // prefix-only; this is the trigram win.
        let hits: i64 = sqlx::query_scalar("SELECT count(*) FROM probe WHERE probe MATCH 'shop'")
            .fetch_one(&pool)
            .await
            .expect("run a MATCH query");
        assert_eq!(hits, 1, "trigram must match a substring in the middle of a word");
    }

    #[tokio::test]
    async fn trigram_is_substring_not_typo_tolerant() {
        let pool = mem().await;
        sqlx::query("CREATE VIRTUAL TABLE probe USING fts5(x, tokenize='trigram')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO probe(x) VALUES ('photoshop')")
            .execute(&pool)
            .await
            .unwrap();

        // The decision, made executable: a typo does NOT match. If this ever
        // starts returning 1, our "substring, not fuzzy" premise broke and the
        // UI copy / expectations need revisiting.
        let hits: i64 =
            sqlx::query_scalar("SELECT count(*) FROM probe WHERE probe MATCH 'phatoshop'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(hits, 0, "trigram MATCH is substring-only, not edit-distance");
    }

    #[tokio::test]
    async fn column_filters_scope_the_match() {
        // Validates the mechanism the scope toggles rely on: restricting a MATCH
        // to named columns via `{col ...} : term`.
        let pool = mem().await;
        sqlx::query(
            "CREATE VIRTUAL TABLE probe USING fts5(name, note, tokenize='trigram')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO probe(name, note) VALUES ('sunset', 'a beach at dusk')")
            .execute(&pool)
            .await
            .unwrap();

        // 'each' is a substring of 'beach' — present in note, absent in name.
        let in_note: i64 =
            sqlx::query_scalar("SELECT count(*) FROM probe WHERE probe MATCH '{note} : each'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(in_note, 1, "column-scoped match should find it in note");

        let in_name: i64 =
            sqlx::query_scalar("SELECT count(*) FROM probe WHERE probe MATCH '{name} : each'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(in_name, 0, "same term scoped to name must NOT match");
    }
}
*/
