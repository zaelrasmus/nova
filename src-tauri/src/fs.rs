use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{debug, instrument};
use walkdir::WalkDir;

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

/// Recursively walks `source_dir` and returns:
/// - A flat list of `Folder` values preserving parent hierarchy.
/// - A map of `PathBuf → folder_id` used to build `ImportResult::path_links`.
///
/// Both are built in a single pass. WalkDir errors for individual entries are
/// logged and skipped — a single unreadable directory should not abort the scan.
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
            order_by: "name".to_string(),
            is_ascending: "1".to_string(),
            original_path: path.to_string_lossy().into_owned(),
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

/// Recursively collects all file paths under `source_dir`.
///
/// WalkDir errors for individual entries are logged and skipped — a single
/// unreadable file should not prevent the rest of the scan from completing.
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
