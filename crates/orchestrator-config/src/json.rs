use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use uuid::Uuid;

pub fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state.json");
    let tmp_path = path.with_file_name(format!("{file_name}.{}.tmp", Uuid::new_v4()));
    let payload = serde_json::to_string_pretty(value)?;

    std::fs::write(&tmp_path, payload)?;
    match std::fs::rename(&tmp_path, path) {
        Ok(()) => Ok(()),
        Err(original_error) => {
            if path.exists() {
                std::fs::remove_file(path).with_context(|| {
                    format!("failed to replace {} after rename failure", path.display())
                })?;
                std::fs::rename(&tmp_path, path).with_context(|| {
                    format!(
                        "failed to atomically move temp file {} to {}",
                        tmp_path.display(),
                        path.display()
                    )
                })?;
                Ok(())
            } else {
                Err(original_error).with_context(|| {
                    format!(
                        "failed to atomically move temp file {} to {}",
                        tmp_path.display(),
                        path.display()
                    )
                })
            }
        }
    }
}
