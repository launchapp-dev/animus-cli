use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use tempfile::{NamedTempFile, TempPath};

pub fn project_state_dir(project_root: &str) -> PathBuf {
    if let Some(scoped) = protocol::scoped_state_root(Path::new(project_root)) {
        return scoped.join("state");
    }
    Path::new(project_root).join(".ao").join("state")
}

pub fn read_json_or_default<T>(path: &Path) -> Result<T>
where
    T: Default + DeserializeOwned,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let content = std::fs::read_to_string(path)?;
    let parsed = serde_json::from_str::<T>(&content)?;
    Ok(parsed)
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let payload = serde_json::to_vec_pretty(value)?;
    let mut temp_file =
        NamedTempFile::new_in(parent).with_context(|| format!("failed to create temp file for {}", path.display()))?;
    temp_file.write_all(&payload).with_context(|| format!("failed to write temp file for {}", path.display()))?;
    temp_file.flush().with_context(|| format!("failed to flush temp file for {}", path.display()))?;
    temp_file.as_file().sync_all().with_context(|| format!("failed to sync temp file for {}", path.display()))?;

    persist_temp_path(temp_file.into_temp_path(), path)
}

pub fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_json_atomic(path, value)
}

pub fn write_json_if_missing<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if path.exists() {
        return Ok(());
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn persist_temp_path(temp_path: TempPath, path: &Path) -> Result<()> {
    match temp_path.persist(path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let tempfile::PathPersistError { error, path: temp_path } = error;
            if path.exists() {
                std::fs::remove_file(path)
                    .with_context(|| format!("failed to replace {} after rename failure", path.display()))?;
                temp_path
                    .persist(path)
                    .with_context(|| format!("failed to atomically move temp file to {}", path.display()))?;
                Ok(())
            } else {
                Err(error).with_context(|| format!("failed to atomically move temp file to {}", path.display()))
            }
        }
    }
}
