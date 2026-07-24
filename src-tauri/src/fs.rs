use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{debug, instrument, warn};
use walkdir::WalkDir;

/// Streaming read size for hashing. Large enough that syscall overhead vanishes,
/// small enough that a 4 GB video costs a buffer rather than memory.
const HASH_CHUNK: usize = 128 * 1024;

/// BLAKE3 fingerprint of a file's bytes, hex-encoded — the identity import
/// dedups on.
///
/// `None` on any read failure, which the caller treats as "import it, but never
/// dedup it". A file we cannot hash is still the user's file; refusing to import
/// it because a fingerprint failed would trade a duplicate for data loss.
///
/// Deliberately single-threaded per file. Callers hash files in parallel across
/// Rayon's pool already, so blake3's own multithreading would oversubscribe the
/// same cores it's competing for.
pub fn hash_file(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path)
        .inspect_err(|e| warn!(path = ?path, error = %e, "Could not open file to hash"))
        .ok()?;

    let mut reader = std::io::BufReader::with_capacity(HASH_CHUNK, file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; HASH_CHUNK];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            // A signal interrupted the read; the bytes are still there.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                warn!(path = ?path, error = %e, "Read failed mid-hash; asset will skip dedup");
                return None;
            }
        }
    }

    Some(hasher.finalize().to_hex().to_string())
}

/// Creates a directory and all missing parents if it does not already exist.
#[instrument(fields(path = %path.display()))]
pub async fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        debug!(path = ?path, "Directory missing, creating");
        tokio::fs::create_dir_all(path)
            .await
            .with_context(|| format!("Failed to create directory: {:?}", path))?;
    }
    Ok(())
}

#[instrument(fields(source = %source_dir.display()))]
pub fn scan_directories(
    source_dir: &Path,
) -> (
    Vec<crate::assets::Folder>,
    std::collections::HashMap<PathBuf, String>,
) {
    let mut folders = Vec::new();
    let mut folder_id_by_path: std::collections::HashMap<PathBuf, String> =
        std::collections::HashMap::new();

    for entry in WalkDir::new(source_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| {
            e.inspect_err(|err| {
                tracing::warn!(error = %err, "WalkDir error while scanning directories, skipping entry")
            })
            .ok()
        })
        .filter(|e| e.file_type().is_dir())
    {
        let path = entry.path().to_path_buf();
        let id = uuid::Uuid::new_v4().to_string();
        let parent_id = path.parent().and_then(|p| folder_id_by_path.get(p).cloned());

        folders.push(crate::assets::Folder {
            id: id.clone(),
            name: entry.file_name().to_string_lossy().into_owned(),
            parent_id,
            position: folders.len() as f64, // discovery order; siblings stay monotonic
            order_by: crate::assets::OrderBy::Manual,
            is_ascending: true,
            notes: None,
            // When the folder entered THIS library, not the source directory's
            // mtime — a scanned folder is created here and now, same as one made
            // by hand. (The source's own timestamp isn't a property of the folder
            // as the library understands it.)
            created_at: crate::assets::now_stamp(),
        });

        folder_id_by_path.insert(path, id);
    }

    debug!(
        folders = folders.len(),
        source = %source_dir.display(),
        "Directory scan complete"
    );

    (folders, folder_id_by_path)
}

#[instrument(fields(source = %source_dir.display()))]
pub fn collect_files(source_dir: &Path) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = WalkDir::new(source_dir)
        .into_iter()
        .filter_map(|e| {
            e.inspect_err(|err| {
                tracing::warn!(error = %err, "WalkDir error while collecting files, skipping entry")
            })
            .ok()
        })
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();

    debug!(
        count = files.len(),
        source = %source_dir.display(),
        "File collection complete"
    );

    files
}
