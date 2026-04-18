use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::{debug, instrument, warn};
use walkdir::WalkDir;

#[instrument(fields(path = %path.display()))]
pub async fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        debug!(path = ?path, "Directory no exists. Creating directory again");
        tokio::fs::create_dir_all(path)
            .await
            .with_context(|| format!("Failed to create directory: {:?}", path))?;
    }
    Ok(())
}

#[instrument(fields(source = %source_dir.display()))]
pub fn scan_folder_structure(
    source_dir: &Path,
) -> (
    Vec<crate::assets::Folder>,
    std::collections::HashMap<PathBuf, String>,
) {
    let mut folders = Vec::new();

    let mut folder_map: std::collections::HashMap<PathBuf, String> =
        std::collections::HashMap::new();

    for entry in WalkDir::new(source_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| {
            e.inspect_err(|err| {
                tracing::warn!(error = %err, "WalkDir error while scanning folders, skipping entry")
            })
            .ok()
        })
        .filter(|e| e.file_type().is_dir())
    {
        let path = entry.path().to_path_buf();
        let id = uuid::Uuid::new_v4().to_string();

        let parent_id = path.parent().and_then(|p| folder_map.get(p).cloned());

        folders.push(crate::assets::Folder {
            id: id.clone(),
            name: entry.file_name().to_string_lossy().into_owned(),
            parent_id,
            order_by: "name".to_string(),
            is_ascending: "1".to_string(),
            original_path: path.to_string_lossy().into_owned(),
        });

        folder_map.insert(path, id);
    }
    debug!(
        folders = folders.len(),
        source = %source_dir.display(),
        "Folder structure scan complete"
    );
    (folders, folder_map)
}

/// Collects all file paths under `source_dir` recursively.
/// WalkDir errors for individual entries are logged and skipped,
/// not propagated — a single unreadable file should not abort the scan.
pub fn collect_file_paths(source_dir: &Path) -> Vec<PathBuf> {
    let paths: Vec<PathBuf> = WalkDir::new(source_dir)
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
        count = paths.len(),
        source = %source_dir.display(),
        "File path collection complete"
    );

    paths
}
