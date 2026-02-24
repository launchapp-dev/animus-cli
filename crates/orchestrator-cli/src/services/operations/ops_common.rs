use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(super) fn project_state_dir(project_root: &str) -> PathBuf {
    Path::new(project_root).join(".ao").join("state")
}

pub(super) fn read_json_or_default<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).with_context(|| {
        format!(
            "failed to parse JSON at {}; file is likely corrupt",
            path.display()
        )
    })
}

pub(super) fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state.json");
    let tmp_path = path.with_file_name(format!("{file_name}.{}.tmp", Uuid::new_v4()));
    let payload = serde_json::to_string_pretty(value)?;

    fs::write(&tmp_path, payload)?;
    match fs::rename(&tmp_path, path) {
        Ok(()) => {}
        Err(original_error) => {
            if path.exists() {
                fs::remove_file(path).with_context(|| {
                    format!("failed to replace {} after rename failure", path.display())
                })?;
                fs::rename(&tmp_path, path).with_context(|| {
                    format!(
                        "failed to atomically move temp file {} to {}",
                        tmp_path.display(),
                        path.display()
                    )
                })?;
            } else {
                return Err(original_error).with_context(|| {
                    format!(
                        "failed to atomically move temp file {} to {}",
                        tmp_path.display(),
                        path.display()
                    )
                });
            }
        }
    }

    Ok(())
}
