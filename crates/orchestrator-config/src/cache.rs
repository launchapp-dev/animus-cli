//! Hot-path read caches for orchestrator-config.
//!
//! This module owns the on-disk and in-process caches for the workflow
//! YAML compile pipeline. The caches are intentionally best-effort —
//! every read path falls through to the live compile on any
//! deserialize, I/O, or hash mismatch failure so a corrupt file never
//! masks a real config change.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::workflow_config::LoadedWorkflowConfig;

pub const WORKFLOW_CACHE_SCHEMA: &str = "animus.cache.workflow-config.v1";

/// Bumped on any change to the compile pipeline that affects output
/// for unchanged YAML/manifest inputs (e.g. built-in workflow base
/// version, pack-overlay merge order, asset canonicalization rules).
/// Mixed into every cache key so an older serialized entry is treated
/// as a miss after an Animus upgrade.
pub const WORKFLOW_CACHE_COMPILER_VERSION: &str = concat!("animus-compiler:", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Serialize, Deserialize)]
struct WorkflowCacheFile {
    schema: String,
    source_hash: String,
    compiled: LoadedWorkflowConfig,
}

#[derive(Debug, Clone)]
pub struct WorkflowCacheInput {
    pub paths: Vec<PathBuf>,
    pub bytes: Vec<Vec<u8>>,
}

impl WorkflowCacheInput {
    pub fn new() -> Self {
        Self { paths: Vec::new(), bytes: Vec::new() }
    }

    pub fn push(&mut self, path: PathBuf, content_bytes: Vec<u8>) {
        self.paths.push(path);
        self.bytes.push(content_bytes);
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn hash(&self) -> String {
        let mut indices: Vec<usize> = (0..self.paths.len()).collect();
        indices.sort_by(|a, b| self.paths[*a].cmp(&self.paths[*b]));

        let mut hasher = Sha256::new();
        // Compiler-version salt: invalidates every cached entry after
        // an Animus upgrade so a stale compiled config from an older
        // binary cannot be served by a newer one.
        hasher.update(WORKFLOW_CACHE_COMPILER_VERSION.as_bytes());
        hasher.update(b"\0");
        for idx in indices {
            let path = &self.paths[idx];
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update(b"\0");
            let meta = std::fs::metadata(path).ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime_secs = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i128)
                .unwrap_or(-1);
            hasher.update(size.to_le_bytes());
            hasher.update(mtime_secs.to_le_bytes());
            let mut content_hasher = Sha256::new();
            content_hasher.update(&self.bytes[idx]);
            hasher.update(content_hasher.finalize());
        }
        format!("{:x}", hasher.finalize())
    }
}

impl Default for WorkflowCacheInput {
    fn default() -> Self {
        Self::new()
    }
}

static NO_CACHE_FLAG: OnceLock<bool> = OnceLock::new();

pub fn set_no_cache_flag(value: bool) {
    let _ = NO_CACHE_FLAG.set(value);
}

fn no_cache_runtime_flag() -> bool {
    *NO_CACHE_FLAG.get().unwrap_or(&false)
}

fn is_truthy(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

pub fn workflow_cache_enabled() -> bool {
    if no_cache_runtime_flag() {
        return false;
    }
    match std::env::var("ANIMUS_DISABLE_WORKFLOW_CACHE") {
        Ok(value) => !is_truthy(&value),
        Err(_) => true,
    }
}

pub fn workflow_cache_path(project_root: &Path) -> PathBuf {
    let base = protocol::scoped_state_root(project_root).unwrap_or_else(|| project_root.join(".animus"));
    base.join("cache").join("workflow-config.compiled.v1.json")
}

pub fn read_workflow_cache(project_root: &Path, expected_hash: &str) -> Option<LoadedWorkflowConfig> {
    if !workflow_cache_enabled() {
        return None;
    }
    let path = workflow_cache_path(project_root);
    let bytes = std::fs::read(&path).ok()?;
    let parsed: WorkflowCacheFile = serde_json::from_slice(&bytes).ok()?;
    if parsed.schema != WORKFLOW_CACHE_SCHEMA {
        return None;
    }
    if parsed.source_hash != expected_hash {
        return None;
    }
    Some(parsed.compiled)
}

pub fn write_workflow_cache(project_root: &Path, source_hash: &str, compiled: &LoadedWorkflowConfig) -> Result<()> {
    if !workflow_cache_enabled() {
        return Ok(());
    }
    let path = workflow_cache_path(project_root);
    let file = WorkflowCacheFile {
        schema: WORKFLOW_CACHE_SCHEMA.to_string(),
        source_hash: source_hash.to_string(),
        compiled: compiled.clone(),
    };
    let bytes = serde_json::to_vec(&file).context("failed to serialize workflow cache")?;
    write_atomic(&path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("cache path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("failed to create dir {}", parent.display()))?;
    let pid = std::process::id();
    let nonce = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let file_name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "cache".to_string());
    let tmp = parent.join(format!(".{}.tmp.{}.{}", file_name, pid, nonce));
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("failed to create {}", tmp.display()))?;
        f.write_all(bytes).with_context(|| format!("failed to write {}", tmp.display()))?;
        f.flush().ok();
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("failed to rename {} -> {}: {}", tmp.display(), path.display(), err));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn hash_changes_with_content() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("a.yaml");
        fs::write(&path, b"x: 1").unwrap();
        let mut input = WorkflowCacheInput::new();
        input.push(path.clone(), b"x: 1".to_vec());
        let h1 = input.hash();

        fs::write(&path, b"x: 2").unwrap();
        let mut input2 = WorkflowCacheInput::new();
        input2.push(path, b"x: 2".to_vec());
        let h2 = input2.hash();
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_stable_for_same_inputs() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("a.yaml");
        fs::write(&path, b"x: 1").unwrap();
        let mut input1 = WorkflowCacheInput::new();
        input1.push(path.clone(), b"x: 1".to_vec());
        let mut input2 = WorkflowCacheInput::new();
        input2.push(path, b"x: 1".to_vec());
        assert_eq!(input1.hash(), input2.hash());
    }
}
