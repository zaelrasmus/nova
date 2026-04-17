use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub async fn ensure_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        tokio::fs::create_dir_all(path)
            .await
            .context(format!("No se pudo crear el directorio: {:?}", path))?;
    }
    Ok(())
}

pub fn scan_folder_structure(
    source_dir: &Path,
) -> (
    Vec<crate::assets::Folder>,
    std::collections::HashMap<PathBuf, String>,
) {
    let mut folders = Vec::new();
    let mut folder_map = std::collections::HashMap::new();

    for entry in WalkDir::new(source_dir)
        .min_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() {
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
    }
    (folders, folder_map)
}

pub fn collect_file_paths(source_dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(source_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect()
}
