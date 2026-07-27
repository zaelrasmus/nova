use crate::error::AppError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};

#[derive(Clone)]
pub struct LibraryHandle {
    pub pool: SqlitePool,
    pub root: PathBuf,
}

pub struct DbState {
    inner: Arc<RwLock<Option<LibraryHandle>>>,
    /// Guards the thumbnail/colour pipeline. A reader/writer lock rather than a
    /// plain mutex, because the two kinds of run are not peers:
    ///
    ///   * on-view generation (`generate_thumbnails_for_ids`) takes the SHARED
    ///     side. Many of these overlap by design — the grid fires one per
    ///     visible window as you scroll — and they only ever fill in rows where
    ///     `thumb_hash IS NULL`, so they cannot conflict with each other.
    ///   * `rebuild_thumbnails` and `analyze_colors` take the EXCLUSIVE side.
    ///     A rebuild deletes the entire `thumbnails/` directory and NULLs every
    ///     `thumb_hash`; an on-view batch running through that would be writing
    ///     files into a directory being removed and re-created, and both would
    ///     be updating the same rows.
    ///
    /// Both sides use the `try_` form, so neither ever blocks a command: a
    /// rejected exclusive request means "already running" and a rejected shared
    /// request means "a rebuild is regenerating everything anyway".
    pub thumb_gen: Arc<RwLock<()>>,

    /// Bumped by every `stream_manifest` call; each request keeps the value it
    /// got and stops as soon as it no longer matches.
    ///
    /// The frontend already discards superseded RESULTS via its own load token,
    /// but that only stops them being rendered — the query kept running to
    /// completion. Clicking through five folders in a 100k library meant five
    /// full manifest scans in flight against a ten-connection pool, four of
    /// which nobody would ever look at. This is the backend half of the same
    /// idea, and it is deliberately the same shape so the two stay legible
    /// together.
    ///
    /// A counter rather than a `CancellationToken` because the rule really is
    /// "only the newest matters" — there is no per-request handle to hold, and
    /// nothing else needs to trigger the cancel.
    pub manifest_gen: Arc<AtomicU64>,
}

impl DbState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            thumb_gen: Arc::new(RwLock::new(())),
            manifest_gen: Arc::new(AtomicU64::new(0)),
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

        // Snapshot the database before migrations touch it. Best-effort: a
        // library we can't back up is still a library we should open, and the
        // common failure (no space, read-only volume) is not one the user can
        // fix from inside a modal. But when a migration DOES go wrong, this file
        // is the difference between "restore this" and "sorry".
        let backup = root.join("library.db.bak");
        if let Err(e) = std::fs::copy(&db_path, &backup) {
            warn!(error = %e, path = ?backup, "Pre-migration backup failed (non-fatal)");
        }

        let new_pool = SqlitePool::connect_with(options).await?;

        // Run migrations on connect.
        //
        // A version failure is called out separately from every other database
        // error because it is the one the user can actually act on — the answer
        // is "get the matching build of Nova", and the generic "try restarting"
        // would loop them forever on a library that will never open. The pool is
        // closed first so a failed connect leaves no handle behind.
        if let Err(e) = sqlx::migrate!().run(&new_pool).await {
            tracing::error!(error = %e, "Failed to run migrations on connect");
            new_pool.close().await;
            return Err(match e {
                sqlx::migrate::MigrateError::VersionMismatch(v) => {
                    AppError::LibraryVersion(format!("migration {v} has a different checksum"))
                }
                sqlx::migrate::MigrateError::VersionMissing(v) => {
                    AppError::LibraryVersion(format!("migration {v} is unknown to this build"))
                }
                other => AppError::Internal(other.into()),
            });
        }

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
