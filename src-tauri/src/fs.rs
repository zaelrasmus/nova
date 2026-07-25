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

/// Scan source directory trees into `Folder` rows, parent-before-child, plus the
/// path→id map that attaches each asset to the folder its file lived in.
///
/// Two knobs, because a dialog import and a drop want different things from the
/// same walk:
///
/// * `include_roots` — whether each source directory becomes a folder itself.
///   The dialog says NO: you picked that folder, so its *contents* are the
///   import and recreating it adds a level nobody asked for. A drop says YES:
///   drag `Photos/` into Nova and a `Photos` folder is precisely what you expect
///   to appear.
/// * `parent_id` — an existing folder the whole scan nests beneath, which is how
///   dropping onto a folder row nests instead of dumping at the top level.
///
/// `root_position` seeds the sibling order for the top level so dropped folders
/// land AFTER whatever the target already contains rather than tying with it.
#[instrument(skip(roots), fields(roots = roots.len(), include_roots, parent_id))]
pub fn scan_directories(
    roots: &[PathBuf],
    include_roots: bool,
    parent_id: Option<&str>,
    root_position: f64,
) -> (
    Vec<crate::assets::Folder>,
    std::collections::HashMap<PathBuf, String>,
) {
    let mut folders = Vec::new();
    let mut folder_id_by_path: std::collections::HashMap<PathBuf, String> =
        std::collections::HashMap::new();

    // Sibling order is per-parent, so one shared counter would leave gaps and
    // ties across levels. Keyed by resolved parent; the top level starts at
    // `root_position` so it continues an existing folder's children.
    let mut next_position: std::collections::HashMap<Option<String>, f64> =
        std::collections::HashMap::new();
    next_position.insert(parent_id.map(str::to_string), root_position);

    for root in roots {
        for entry in WalkDir::new(root)
            .min_depth(if include_roots { 0 } else { 1 })
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

            // A directory whose parent was scanned nests under it; anything else
            // is a top-level entry of this import and hangs off `parent_id`.
            // That single fallback covers both the included root and the
            // depth-1 children of an excluded one.
            let resolved_parent = path
                .parent()
                .and_then(|p| folder_id_by_path.get(p).cloned())
                .or_else(|| parent_id.map(str::to_string));

            let position = next_position.entry(resolved_parent.clone()).or_insert(0.0);

            folders.push(crate::assets::Folder {
                id: id.clone(),
                name: entry.file_name().to_string_lossy().into_owned(),
                parent_id: resolved_parent,
                position: *position, // discovery order; siblings stay monotonic
                order_by: crate::assets::OrderBy::Manual,
                is_ascending: true,
                notes: None,
                // When the folder entered THIS library, not the source directory's
                // mtime — a scanned folder is created here and now, same as one made
                // by hand. (The source's own timestamp isn't a property of the folder
                // as the library understands it.)
                created_at: crate::assets::now_stamp(),
                // Scanned folders arrive unpinned: an import of 200 directories
                // must not fill the sidebar's curated list.
                color: None,
                pin_position: None,
            });
            *position += 1.0;

            folder_id_by_path.insert(path, id);
        }
    }

    debug!(folders = folders.len(), "Directory scan complete");

    (folders, folder_id_by_path)
}

/// Every file under `roots`, recursively.
///
/// Takes a slice because a drop hands over a mix of files and directories.
/// WalkDir over a plain file yields that one file, so both kinds flow through
/// here without a special case.
#[instrument(skip(roots), fields(roots = roots.len()))]
pub fn collect_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let files: Vec<PathBuf> = roots
        .iter()
        .flat_map(|root| {
            WalkDir::new(root)
                .into_iter()
                .filter_map(|e| {
                    e.inspect_err(|err| {
                        tracing::warn!(error = %err, "WalkDir error while collecting files, skipping entry")
                    })
                    .ok()
                })
                .filter(|e| e.file_type().is_file())
                .map(|e| e.into_path())
        })
        .collect();

    debug!(count = files.len(), "File collection complete");

    files
}
