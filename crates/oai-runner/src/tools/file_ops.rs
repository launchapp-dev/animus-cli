use anyhow::{bail, Result};
use std::path::Path;

pub fn read_file(working_dir: &Path, path: &str, offset: Option<usize>, limit: Option<usize>) -> Result<String> {
    let full_path = resolve_path(working_dir, path)?;
    let content = std::fs::read_to_string(&full_path).map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path, e))?;

    let lines: Vec<&str> = content.lines().collect();
    let start = offset.unwrap_or(1).saturating_sub(1);
    let end = match limit {
        Some(n) => (start + n).min(lines.len()),
        None => lines.len(),
    };

    if start >= lines.len() {
        return Ok(String::new());
    }

    let selected: Vec<String> =
        lines[start..end].iter().enumerate().map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line)).collect();

    Ok(selected.join("\n"))
}

pub fn write_file(working_dir: &Path, path: &str, content: &str) -> Result<String> {
    let full_path = resolve_path(working_dir, path)?;

    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("Failed to create directories for {}: {}", path, e))?;
    }

    std::fs::write(&full_path, content).map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path, e))?;

    Ok(format!("Successfully wrote {} bytes to {}", content.len(), path))
}

pub fn edit_file(working_dir: &Path, path: &str, old_text: &str, new_text: &str) -> Result<String> {
    let full_path = resolve_path(working_dir, path)?;
    let content = std::fs::read_to_string(&full_path).map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path, e))?;

    let count = content.matches(old_text).count();
    if count == 0 {
        bail!("old_text not found in {}. Make sure the text matches exactly including whitespace.", path);
    }

    let new_content = content.replacen(old_text, new_text, 1);
    std::fs::write(&full_path, &new_content).map_err(|e| anyhow::anyhow!("Failed to write {}: {}", path, e))?;

    Ok(format!("Successfully edited {} ({} occurrence(s) found, replaced first)", path, count))
}

pub fn list_files(working_dir: &Path, pattern: &str, base_path: Option<&str>) -> Result<String> {
    let base = match base_path {
        Some(p) => resolve_path(working_dir, p)?,
        None => working_dir.to_path_buf(),
    };

    let canonical_wd = working_dir.canonicalize().unwrap_or_else(|_| working_dir.to_path_buf());
    let glob_pattern = base.join(pattern).to_string_lossy().to_string();
    let mut results = Vec::new();

    for entry in glob::glob(&glob_pattern).map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {}", pattern, e))? {
        match entry {
            Ok(path) => {
                let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
                if !canonical_path.starts_with(&canonical_wd) {
                    continue;
                }
                if let Ok(rel) = path.strip_prefix(working_dir) {
                    results.push(rel.to_string_lossy().to_string());
                } else {
                    results.push(path.to_string_lossy().to_string());
                }
            }
            Err(e) => {
                results.push(format!("Error: {}", e));
            }
        }
    }

    if results.is_empty() {
        Ok("No files found matching the pattern.".to_string())
    } else {
        results.sort();
        if results.len() > 500 {
            results.truncate(500);
            results.push("... (truncated, showing 500 of many results)".to_string());
        }
        Ok(results.join("\n"))
    }
}

fn canonicalize_for_validation(path: &Path) -> std::path::PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let mut base = path.to_path_buf();
    let mut tail = std::path::PathBuf::new();
    loop {
        if let Ok(canonical_base) = base.canonicalize() {
            return canonical_base.join(&tail);
        }
        match base.file_name() {
            Some(name) => {
                tail = std::path::PathBuf::from(name).join(&tail);
                match base.parent() {
                    Some(parent) => base = parent.to_path_buf(),
                    None => return path.to_path_buf(),
                }
            }
            None => return path.to_path_buf(),
        }
    }
}

pub(crate) fn resolve_path(working_dir: &Path, path: &str) -> Result<std::path::PathBuf> {
    let resolved = if Path::new(path).is_absolute() { std::path::PathBuf::from(path) } else { working_dir.join(path) };

    let canonical_wd = working_dir.canonicalize().unwrap_or_else(|_| working_dir.to_path_buf());
    let canonical_resolved = canonicalize_for_validation(&resolved);

    if !canonical_resolved.starts_with(&canonical_wd) {
        bail!("Path '{}' attempts to escape the working directory. Paths must stay within the project root.", path);
    }

    Ok(resolved)
}
